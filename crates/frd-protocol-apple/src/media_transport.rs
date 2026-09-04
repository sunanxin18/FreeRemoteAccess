//! Generation-bound UDP media transport.
//!
//! This layer owns sockets, control-state ordering, and activation of the
//! negotiated SRTP/SRTCP data plane. Wire serialization, key derivation, and
//! audio codec details remain isolated in their protocol-specific modules.

use anyhow::{bail, ensure, Context, Result};
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

#[cfg(debug_assertions)]
use sha2::{Digest as _, Sha256};

use crate::audio_codec::{ARD_AUDIO_RTP_PAYLOAD_TYPE, ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT};
use crate::media_negotiation::MediaNegotiatorMode;
use crate::media_negotiation::{
    encode_client_media_stream_configuration_with_capabilities, AudioMediaFlow,
    ClientMediaStreamCapabilities, ClientMediaStreamConfiguration, MediaStreamAnswer,
    MediaStreamConfigurationEntry,
};
use crate::media_protocol::{MediaStreamPortAnnouncement, MediaStreamPortDescriptor};
use crate::srtp::{
    build_compound_rtcp_receiver_report_with_block_and_picture_loss, classify_rtp_mux_packet,
    derive_session_keys, parse_rtp_header, secure_packet_discard_kind, RtpMuxPacketKind,
    RtpReceptionReportState, SecurePacketDiscardKind, SrtcpReceiver, SrtcpSender, SrtpPacketKind,
    SrtpReceiver,
};
use crate::srtp::{
    parse_rtcp_reception_reports, protect_rtp_packet, RtcpReceptionReport, SrtpSessionKeys,
    RTP_FIXED_HEADER_LEN, RTP_MARKER_MASK, RTP_VERSION, RTP_VERSION_SHIFT,
};

pub const MAX_MEDIA_DATAGRAM_BYTES: usize = 65_507;
pub const MAX_MEDIA_DATAGRAMS_PER_ROLE_PER_POLL: usize = 256;
/// High Performance 视频会产生高码率突发流量。为每个媒体角色独立请求至少 4 MiB
/// 的内核 UDP 接收队列，避免音频和视频共享容量或互相挤占。
pub const MEDIA_SOCKET_RECEIVE_BUFFER_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MEDIA_CONTROL_REPORT_STARTUP_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const SRTP_UNAMBIGUOUS_INITIAL_SEQUENCE_MASK: u16 = u16::MAX >> 1;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MediaRole {
    Audio,
    VideoStream1,
    VideoStream2,
}

impl MediaRole {
    const fn rtp_clock_rate(self) -> u32 {
        match self {
            Self::Audio => 48_000,
            Self::VideoStream1 | Self::VideoStream2 => 90_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaTransportPhase {
    Idle,
    PortsAnnounced,
    LocalSocketsReady,
    ConfigSent,
    AnswerAccepted,
    Active,
    Closing,
    Failed,
}

struct BoundMediaSocket {
    role: MediaRole,
    socket: UdpSocket,
    receive_buffer_actual_bytes: usize,
    remote: SocketAddr,
    outbound_control: Option<OutboundControlStream>,
    outbound_rtp: Option<OutboundRtpStream>,
    inbound_crypto: Option<InboundCryptoStream>,
    reception_report: RtpReceptionReportState,
    pending_picture_loss_media_ssrc: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MediaSocketReceiveBufferCapacity {
    pub role: MediaRole,
    pub requested_bytes: usize,
    pub actual_bytes: usize,
}

struct OutboundControlStream {
    local_ssrc: u32,
    sender: SrtcpSender,
}

struct OutboundRtpStream {
    local_ssrc: u32,
    keys: SrtpSessionKeys,
    next_sequence: u16,
    next_timestamp: u32,
    rollover_counter: u32,
    marker_pending: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutboundRtpPacketMetadata {
    pub sequence: u16,
    pub extended_sequence: u32,
    pub timestamp: u32,
    pub ssrc: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutboundAudioSentRange {
    pub ssrc: u32,
    pub first_extended_sequence: u32,
    pub last_extended_sequence: u32,
    pub packets_sent: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AudioReceptionEvidence {
    #[default]
    NotObserved,
    MatchingReport {
        extended_highest_sequence: u32,
        cumulative_packets_lost: i32,
    },
    Confirmed {
        extended_highest_sequence: u32,
        cumulative_packets_lost: i32,
    },
}

#[derive(Debug, Default)]
struct OutboundAudioEvidenceTracker {
    sent_range: Option<OutboundAudioSentRange>,
    evidence: AudioReceptionEvidence,
}

impl OutboundAudioEvidenceTracker {
    fn record_sent(&mut self, metadata: OutboundRtpPacketMetadata) -> Result<()> {
        match self.sent_range.as_mut() {
            Some(range) => {
                ensure!(
                    range.ssrc == metadata.ssrc,
                    "出站音频 RTP SSRC 在同一 generation 内发生变化"
                );
                ensure!(
                    metadata.extended_sequence > range.last_extended_sequence,
                    "出站音频 RTP 扩展序号没有单调递增"
                );
                range.packets_sent = range
                    .packets_sent
                    .checked_add(1)
                    .context("出站音频 RTP 发送计数溢出")?;
                range.last_extended_sequence = metadata.extended_sequence;
            }
            None => {
                self.sent_range = Some(OutboundAudioSentRange {
                    ssrc: metadata.ssrc,
                    first_extended_sequence: metadata.extended_sequence,
                    last_extended_sequence: metadata.extended_sequence,
                    packets_sent: 1,
                });
            }
        }
        Ok(())
    }

    fn observe_reports(&mut self, reports: Vec<RtcpReceptionReport>) {
        let Some(sent_range) = self.sent_range else {
            return;
        };
        for report in reports {
            if report.source_ssrc != sent_range.ssrc
                || matches!(self.evidence, AudioReceptionEvidence::Confirmed { .. })
            {
                continue;
            }
            let evidence = AudioReceptionEvidence::MatchingReport {
                extended_highest_sequence: report.extended_highest_sequence,
                cumulative_packets_lost: report.cumulative_packets_lost,
            };
            if (sent_range.first_extended_sequence..=sent_range.last_extended_sequence)
                .contains(&report.extended_highest_sequence)
                && i64::from(report.cumulative_packets_lost) < i64::from(sent_range.packets_sent)
            {
                self.evidence = AudioReceptionEvidence::Confirmed {
                    extended_highest_sequence: report.extended_highest_sequence,
                    cumulative_packets_lost: report.cumulative_packets_lost,
                };
            } else {
                self.evidence = evidence;
            }
        }
    }
}

impl OutboundRtpStream {
    fn new(local_ssrc: u32, keys: SrtpSessionKeys) -> Result<Self> {
        let mut sequence = [0u8; size_of::<u16>()];
        let mut timestamp = [0u8; size_of::<u32>()];
        getrandom::getrandom(&mut sequence).context("生成音频 RTP 初始序号失败")?;
        getrandom::getrandom(&mut timestamp).context("生成音频 RTP 初始时间戳失败")?;
        Ok(Self::with_initial(
            local_ssrc,
            keys,
            u16::from_ne_bytes(sequence) & SRTP_UNAMBIGUOUS_INITIAL_SEQUENCE_MASK,
            u32::from_ne_bytes(timestamp),
        ))
    }

    fn with_initial(
        local_ssrc: u32,
        keys: SrtpSessionKeys,
        next_sequence: u16,
        next_timestamp: u32,
    ) -> Self {
        Self {
            local_ssrc,
            keys,
            next_sequence,
            next_timestamp,
            rollover_counter: 0,
            marker_pending: true,
        }
    }

    fn protect_audio_access_unit(
        &mut self,
        access_unit: &[u8],
    ) -> Result<(Vec<u8>, OutboundRtpPacketMetadata)> {
        ensure!(!access_unit.is_empty(), "AAC-ELD access unit 不能为空");
        let next_rollover_counter = if self.next_sequence == u16::MAX {
            Some(
                self.rollover_counter
                    .checked_add(1)
                    .context("音频 SRTP rollover counter 溢出")?,
            )
        } else {
            None
        };
        let extended_sequence = self
            .rollover_counter
            .checked_mul(1 << u16::BITS)
            .and_then(|base| base.checked_add(u32::from(self.next_sequence)))
            .context("音频 RTCP 扩展序号溢出")?;
        let mut plaintext = Vec::with_capacity(RTP_FIXED_HEADER_LEN + access_unit.len());
        plaintext.push(RTP_VERSION << RTP_VERSION_SHIFT);
        plaintext.push(
            ARD_AUDIO_RTP_PAYLOAD_TYPE
                | if self.marker_pending {
                    RTP_MARKER_MASK
                } else {
                    0
                },
        );
        plaintext.extend_from_slice(&self.next_sequence.to_be_bytes());
        plaintext.extend_from_slice(&self.next_timestamp.to_be_bytes());
        plaintext.extend_from_slice(&self.local_ssrc.to_be_bytes());
        plaintext.extend_from_slice(access_unit);
        let protected = protect_rtp_packet(&plaintext, &self.keys, self.rollover_counter)
            .context("保护音频 SRTP 数据报失败")?;
        let metadata = OutboundRtpPacketMetadata {
            sequence: self.next_sequence,
            extended_sequence,
            timestamp: self.next_timestamp,
            ssrc: self.local_ssrc,
        };
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.next_timestamp = self
            .next_timestamp
            .wrapping_add(ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT as u32);
        if let Some(next_rollover_counter) = next_rollover_counter {
            self.rollover_counter = next_rollover_counter;
        }
        self.marker_pending = false;
        Ok((protected, metadata))
    }
}

struct InboundCryptoStream {
    rtp_receiver: SrtpReceiver,
    rtcp_receiver: SrtcpReceiver,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaDatagram {
    Rtp(Vec<u8>),
    Rtcp(Vec<u8>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceivedMediaDatagram {
    pub datagram: MediaDatagram,
    /// UDP `recv_from` 返回后立刻记录的本机时刻；后续认证和分发不得重采样。
    pub received_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaDiscardReason {
    UnexpectedSource,
    EmptyDatagram,
    TruncatedHeader,
    MalformedPacket,
    AuthenticationFailed,
    ReplayOrTooOld,
}

#[derive(Debug, Eq, PartialEq)]
pub enum MediaReceiveOutcome {
    Empty,
    Accepted(ReceivedMediaDatagram),
    Discarded(MediaDiscardReason),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MediaDiscardCounters {
    pub unexpected_source: u64,
    pub empty_datagram: u64,
    pub truncated_header: u64,
    pub malformed_packet: u64,
    pub authentication_failed: u64,
    pub replay_or_too_old: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MediaRolePollStats {
    pub processed: usize,
    pub accepted: usize,
    pub discarded: usize,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct MediaPollSummary {
    pub accepted_total: usize,
    pub discarded_total: usize,
    pub role_order: Vec<MediaRole>,
    pub roles: Vec<(MediaRole, MediaRolePollStats)>,
}

impl MediaPollSummary {
    pub fn per_role(&self, role: MediaRole) -> MediaRolePollStats {
        self.roles
            .iter()
            .find_map(|(candidate, stats)| (*candidate == role).then_some(*stats))
            .unwrap_or_default()
    }
}

impl MediaDiscardCounters {
    fn count(self, reason: MediaDiscardReason) -> u64 {
        match reason {
            MediaDiscardReason::UnexpectedSource => self.unexpected_source,
            MediaDiscardReason::EmptyDatagram => self.empty_datagram,
            MediaDiscardReason::TruncatedHeader => self.truncated_header,
            MediaDiscardReason::MalformedPacket => self.malformed_packet,
            MediaDiscardReason::AuthenticationFailed => self.authentication_failed,
            MediaDiscardReason::ReplayOrTooOld => self.replay_or_too_old,
        }
    }
}

pub struct MediaTransport {
    generation: u64,
    server_address: IpAddr,
    phase: MediaTransportPhase,
    announcement: Option<MediaStreamPortAnnouncement>,
    sockets: Vec<BoundMediaSocket>,
    configuration: Option<ClientMediaStreamConfiguration>,
    answer: Option<MediaStreamAnswer>,
    next_control_report_at: Option<Instant>,
    audio_flow: AudioMediaFlow,
    discard_counters: MediaDiscardCounters,
    next_poll_role_index: usize,
    outbound_audio_evidence: OutboundAudioEvidenceTracker,
}

impl MediaTransport {
    pub fn new(generation: u64, server_address: IpAddr) -> Self {
        Self {
            generation,
            server_address,
            phase: MediaTransportPhase::Idle,
            announcement: None,
            sockets: Vec::new(),
            configuration: None,
            answer: None,
            next_control_report_at: None,
            audio_flow: AudioMediaFlow::MacToPc,
            discard_counters: MediaDiscardCounters::default(),
            next_poll_role_index: 0,
            outbound_audio_evidence: OutboundAudioEvidenceTracker::default(),
        }
    }

    pub fn phase(&self) -> MediaTransportPhase {
        self.phase
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Atomically retire the previous generation's sockets and negotiation
    /// state before accepting media for a newer display generation.
    pub fn reset_generation(&mut self, generation: u64) -> Result<()> {
        ensure!(
            generation > self.generation,
            "媒体传输 generation 必须单调递增: {generation} <= {}",
            self.generation
        );
        self.sockets.clear();
        self.announcement = None;
        self.configuration = None;
        self.answer = None;
        self.next_control_report_at = None;
        self.discard_counters = MediaDiscardCounters::default();
        self.next_poll_role_index = 0;
        self.reset_outbound_audio_evidence();
        self.generation = generation;
        self.phase = MediaTransportPhase::Idle;
        Ok(())
    }

    /// 仅当当前 generation 的 mode-4 Audio 数据面完整激活时允许启动 P5 探针。
    pub fn pc_to_mac_audio_probe_ready(&self, generation: u64) -> bool {
        self.validate_generation(generation).is_ok()
            && self.phase == MediaTransportPhase::Active
            && self.audio_flow == AudioMediaFlow::PcToMac
            && self
                .sockets
                .iter()
                .any(|bound| bound.role == MediaRole::Audio)
            && self.configuration.as_ref().is_some_and(|configuration| {
                configuration.audio.offer.mode == MediaNegotiatorMode::RemoteMicrophone
            })
            && self.answer.is_some()
    }

    pub fn outbound_audio_sent_range(&self) -> Option<OutboundAudioSentRange> {
        self.outbound_audio_evidence.sent_range
    }

    pub fn audio_reception_evidence(&self) -> AudioReceptionEvidence {
        self.outbound_audio_evidence.evidence
    }

    fn reset_outbound_audio_evidence(&mut self) {
        {
            self.outbound_audio_evidence = OutboundAudioEvidenceTracker::default();
        }
    }

    #[cfg(debug_assertions)]
    pub fn diagnostic_audio_material_fingerprints(&self) -> Option<(String, String)> {
        fn fingerprint(material: &crate::media_negotiation::SrtpMasterMaterial) -> String {
            // Apple 的统一日志只显示 NSData 的前 16 字节和末 8 字节。复现同一
            // 不可逆摘要以校准 Receive/Send 方向，绝不输出原始密钥材料。
            const APPLE_LOG_PREFIX_BYTES: usize = 16;
            const APPLE_LOG_SUFFIX_BYTES: usize = 8;
            let mut wire_material =
                Vec::with_capacity(material.master_key.len() + material.master_salt.len());
            wire_material.extend_from_slice(&material.master_key);
            wire_material.extend_from_slice(&material.master_salt);
            let digest = Sha256::new()
                .chain_update(&wire_material[..APPLE_LOG_PREFIX_BYTES])
                .chain_update(&wire_material[wire_material.len() - APPLE_LOG_SUFFIX_BYTES..])
                .finalize();
            digest[..8]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        }

        self.configuration.as_ref().map(|configuration| {
            (
                fingerprint(&configuration.audio.viewer_to_server),
                fingerprint(&configuration.audio.server_to_viewer),
            )
        })
    }

    pub fn set_audio_flow(&mut self, audio_flow: AudioMediaFlow) -> Result<()> {
        ensure!(
            self.phase == MediaTransportPhase::Idle,
            "只能在 UDP 媒体协商开始前选择音频方向"
        );
        self.audio_flow = audio_flow;
        Ok(())
    }

    fn validate_generation(&self, generation: u64) -> Result<()> {
        ensure!(
            generation == self.generation,
            "媒体传输 generation 过期: {generation} != {}",
            self.generation
        );
        Ok(())
    }

    fn descriptors(
        announcement: &MediaStreamPortAnnouncement,
    ) -> [(MediaRole, MediaStreamPortDescriptor); 3] {
        [
            (MediaRole::Audio, announcement.audio),
            (MediaRole::VideoStream1, announcement.video_stream_1),
            (MediaRole::VideoStream2, announcement.video_stream_2),
        ]
    }

    pub fn accept_port_announcement(
        &mut self,
        generation: u64,
        announcement: MediaStreamPortAnnouncement,
    ) -> Result<()> {
        self.validate_generation(generation)?;
        ensure!(
            self.phase == MediaTransportPhase::Idle,
            "端口公告只能在 Idle 状态接受"
        );

        let validation = (|| {
            let mut announced_ports = HashSet::new();
            for (_, descriptor) in Self::descriptors(&announcement) {
                if descriptor.is_announced() {
                    ensure!(
                        announced_ports.insert(descriptor.port),
                        "媒体端口角色发生冲突: {}",
                        descriptor.port
                    );
                }
            }
            ensure!(!announced_ports.is_empty(), "端口公告没有已发出的媒体角色");
            Ok(())
        })();
        if let Err(error) = validation {
            self.phase = MediaTransportPhase::Failed;
            self.reset_outbound_audio_evidence();
            return Err(error);
        }

        self.announcement = Some(announcement);
        self.phase = MediaTransportPhase::PortsAnnounced;
        Ok(())
    }

    pub fn bind_local_sockets(&mut self, generation: u64, bind_address: IpAddr) -> Result<()> {
        self.validate_generation(generation)?;
        ensure!(
            self.phase == MediaTransportPhase::PortsAnnounced,
            "本地媒体 socket 只能在 PortsAnnounced 状态绑定"
        );
        let announcement = self
            .announcement
            .as_ref()
            .context("PortsAnnounced 缺少端口公告")?;

        let binding = (|| {
            let mut sockets = Vec::new();
            for (role, descriptor) in Self::descriptors(announcement) {
                if !descriptor.is_announced() {
                    continue;
                }
                let socket = UdpSocket::bind(SocketAddr::new(bind_address, descriptor.port))
                    .with_context(|| format!("绑定 {role:?} UDP socket 失败"))?;
                let socket_ref = socket2::SockRef::from(&socket);
                socket_ref
                    .set_recv_buffer_size(MEDIA_SOCKET_RECEIVE_BUFFER_REQUEST_BYTES)
                    .with_context(|| {
                        format!(
                            "设置 {role:?} UDP 接收缓冲区为至少 {} 字节失败",
                            MEDIA_SOCKET_RECEIVE_BUFFER_REQUEST_BYTES
                        )
                    })?;
                let receive_buffer_actual_bytes = socket_ref
                    .recv_buffer_size()
                    .with_context(|| format!("读取 {role:?} UDP 实际接收缓冲区容量失败"))?;
                socket
                    .set_nonblocking(true)
                    .with_context(|| format!("设置 {role:?} UDP 非阻塞模式失败"))?;
                eprintln!(
                    "[media] {role:?} UDP 接收缓冲区：请求 {} 字节，实际 {} 字节",
                    MEDIA_SOCKET_RECEIVE_BUFFER_REQUEST_BYTES, receive_buffer_actual_bytes
                );
                sockets.push(BoundMediaSocket {
                    role,
                    socket,
                    receive_buffer_actual_bytes,
                    remote: SocketAddr::new(self.server_address, descriptor.port),
                    outbound_control: None,
                    outbound_rtp: None,
                    inbound_crypto: None,
                    reception_report: RtpReceptionReportState::new(role.rtp_clock_rate()),
                    pending_picture_loss_media_ssrc: None,
                });
            }
            Ok::<_, anyhow::Error>(sockets)
        })();

        match binding {
            Ok(sockets) => {
                self.sockets = sockets;
                self.phase = MediaTransportPhase::LocalSocketsReady;
                Ok(())
            }
            Err(error) => {
                self.sockets.clear();
                self.phase = MediaTransportPhase::Failed;
                self.reset_outbound_audio_evidence();
                Err(error)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn bind_test_loopback_sockets(
        &mut self,
        generation: u64,
        peers: &[(MediaRole, &UdpSocket)],
    ) -> Result<()> {
        self.validate_generation(generation)?;
        ensure!(
            self.phase == MediaTransportPhase::PortsAnnounced,
            "测试媒体 socket 只能在 PortsAnnounced 状态绑定"
        );
        let announcement = self
            .announcement
            .as_ref()
            .context("PortsAnnounced 缺少端口公告")?;

        let binding = (|| {
            let mut sockets = Vec::new();
            for (role, descriptor) in Self::descriptors(announcement) {
                if !descriptor.is_announced() {
                    continue;
                }
                let peer = peers
                    .iter()
                    .find_map(|(peer_role, socket)| (*peer_role == role).then_some(*socket))
                    .with_context(|| format!("测试媒体角色 {role:?} 缺少已绑定 loopback peer"))?;
                let remote = peer
                    .local_addr()
                    .with_context(|| format!("读取测试媒体角色 {role:?} peer 地址失败"))?;
                ensure!(
                    remote.ip() == IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                    "测试媒体角色 {role:?} 必须绑定到 127.0.0.1"
                );
                ensure!(
                    remote.port() == descriptor.port,
                    "测试媒体角色 {role:?} peer 端口与公告不一致"
                );
                let socket = crate::bind_test_udp_loopback();
                let socket_ref = socket2::SockRef::from(&socket);
                socket_ref
                    .set_recv_buffer_size(MEDIA_SOCKET_RECEIVE_BUFFER_REQUEST_BYTES)
                    .with_context(|| format!("设置测试媒体角色 {role:?} 接收缓冲区失败"))?;
                let receive_buffer_actual_bytes = socket_ref
                    .recv_buffer_size()
                    .with_context(|| format!("读取测试媒体角色 {role:?} 实际接收缓冲区容量失败"))?;
                socket
                    .set_nonblocking(true)
                    .with_context(|| format!("设置测试媒体角色 {role:?} 非阻塞模式失败"))?;
                sockets.push(BoundMediaSocket {
                    role,
                    socket,
                    receive_buffer_actual_bytes,
                    remote,
                    outbound_control: None,
                    outbound_rtp: None,
                    inbound_crypto: None,
                    reception_report: RtpReceptionReportState::new(role.rtp_clock_rate()),
                    pending_picture_loss_media_ssrc: None,
                });
            }
            ensure!(
                sockets.len() == peers.len(),
                "测试媒体 peer 包含未公告的角色"
            );
            Ok::<_, anyhow::Error>(sockets)
        })();

        match binding {
            Ok(sockets) => {
                self.sockets = sockets;
                self.phase = MediaTransportPhase::LocalSocketsReady;
                Ok(())
            }
            Err(error) => {
                self.sockets.clear();
                self.phase = MediaTransportPhase::Failed;
                self.reset_outbound_audio_evidence();
                Err(error)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn poll_test_receive_decrypted(
        &mut self,
        generation: u64,
        role: MediaRole,
    ) -> Result<MediaReceiveOutcome> {
        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            let outcome = self.try_recv_decrypted(generation, role)?;
            if outcome != MediaReceiveOutcome::Empty || Instant::now() >= deadline {
                return Ok(outcome);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub fn receive_buffer_capacities(
        &self,
        generation: u64,
    ) -> Result<Vec<MediaSocketReceiveBufferCapacity>> {
        self.validate_generation(generation)?;
        Ok(self
            .sockets
            .iter()
            .map(|bound| MediaSocketReceiveBufferCapacity {
                role: bound.role,
                requested_bytes: MEDIA_SOCKET_RECEIVE_BUFFER_REQUEST_BYTES,
                actual_bytes: bound.receive_buffer_actual_bytes,
            })
            .collect())
    }

    fn transition(
        &mut self,
        generation: u64,
        expected: MediaTransportPhase,
        next: MediaTransportPhase,
    ) -> Result<()> {
        self.validate_generation(generation)?;
        if self.phase != expected {
            let actual = self.phase;
            self.phase = MediaTransportPhase::Failed;
            self.reset_outbound_audio_evidence();
            bail!("媒体传输状态顺序错误: 期望 {expected:?}，实际 {actual:?}");
        }
        self.phase = next;
        Ok(())
    }

    pub fn prepare_configuration(&mut self, generation: u64) -> Result<Vec<u8>> {
        self.prepare_configuration_with_capabilities(
            generation,
            ClientMediaStreamCapabilities::default(),
        )
    }

    pub fn prepare_configuration_with_capabilities(
        &mut self,
        generation: u64,
        capabilities: ClientMediaStreamCapabilities,
    ) -> Result<Vec<u8>> {
        self.validate_generation(generation)?;
        ensure!(
            self.phase == MediaTransportPhase::LocalSocketsReady,
            "媒体配置只能在 LocalSocketsReady 状态生成"
        );
        ensure!(self.configuration.is_none(), "媒体配置已生成，禁止重复换钥");
        let configuration =
            ClientMediaStreamConfiguration::generate_one_video_with_audio_flow(self.audio_flow)
                .context("生成客户端媒体流配置失败")?;
        let wire = encode_client_media_stream_configuration_with_capabilities(
            &configuration,
            capabilities,
        )
        .context("序列化客户端媒体流配置失败")?;
        self.configuration = Some(configuration);
        Ok(wire)
    }

    pub fn mark_configuration_sent(&mut self, generation: u64) -> Result<()> {
        ensure!(self.configuration.is_some(), "媒体配置尚未生成");
        self.transition(
            generation,
            MediaTransportPhase::LocalSocketsReady,
            MediaTransportPhase::ConfigSent,
        )
    }

    pub fn accept_answer(&mut self, generation: u64, answer: MediaStreamAnswer) -> Result<()> {
        self.transition(
            generation,
            MediaTransportPhase::ConfigSent,
            MediaTransportPhase::AnswerAccepted,
        )?;
        self.answer = Some(answer);
        Ok(())
    }

    pub fn activate(&mut self, generation: u64) -> Result<()> {
        self.validate_generation(generation)?;
        ensure!(
            self.phase == MediaTransportPhase::AnswerAccepted,
            "媒体数据面只能在 AnswerAccepted 状态激活"
        );
        ensure!(
            self.answer.is_some(),
            "激活媒体数据面前缺少 Message 2 answer"
        );
        self.reset_outbound_audio_evidence();
        let configuration = self
            .configuration
            .as_ref()
            .context("激活媒体数据面前缺少客户端配置")?;
        let activation_now = Instant::now();
        let activation = self.sockets.iter_mut().try_for_each(|bound| {
            let entry = configuration_entry(configuration, bound.role)?;
            let rtcp_keys = derive_session_keys(&entry.viewer_to_server, SrtpPacketKind::Rtcp);
            bound.outbound_control = Some(OutboundControlStream {
                local_ssrc: entry.offer.local_ssrc,
                sender: SrtcpSender::new(rtcp_keys),
            });
            {
                bound.outbound_rtp = Some(OutboundRtpStream::new(
                    entry.offer.local_ssrc,
                    derive_session_keys(&entry.viewer_to_server, SrtpPacketKind::Rtp),
                )?);
            }
            bound.inbound_crypto = Some(InboundCryptoStream {
                rtp_receiver: SrtpReceiver::new(derive_session_keys(
                    &entry.server_to_viewer,
                    SrtpPacketKind::Rtp,
                )),
                rtcp_receiver: SrtcpReceiver::new(derive_session_keys(
                    &entry.server_to_viewer,
                    SrtpPacketKind::Rtcp,
                )),
            });
            send_control_report(bound, activation_now)?;
            Ok::<_, anyhow::Error>(())
        });
        match activation {
            Ok(()) => {
                self.phase = MediaTransportPhase::Active;
                self.next_control_report_at =
                    Some(activation_now + MEDIA_CONTROL_REPORT_STARTUP_RETRY_INTERVAL);
                Ok(())
            }
            Err(error) => {
                self.phase = MediaTransportPhase::Failed;
                self.reset_outbound_audio_evidence();
                Err(error)
            }
        }
    }

    pub fn service_control_reports_at(&mut self, generation: u64, now: Instant) -> Result<usize> {
        self.validate_generation(generation)?;
        ensure!(
            self.phase == MediaTransportPhase::Active,
            "SRTCP 控制报告只能在 Active 状态发送"
        );
        let due_at = self
            .next_control_report_at
            .context("Active 媒体传输缺少 SRTCP 调度状态")?;
        if now < due_at {
            return Ok(0);
        }

        let result = self
            .sockets
            .iter_mut()
            .try_for_each(|bound| send_control_report(bound, now));
        match result {
            Ok(()) => {
                self.next_control_report_at =
                    Some(now + MEDIA_CONTROL_REPORT_STARTUP_RETRY_INTERVAL);
                Ok(self.sockets.len())
            }
            Err(error) => {
                self.phase = MediaTransportPhase::Failed;
                self.reset_outbound_audio_evidence();
                Err(error)
            }
        }
    }

    pub(crate) fn queue_picture_loss(
        &mut self,
        generation: u64,
        role: MediaRole,
        media_ssrc: u32,
    ) -> Result<()> {
        self.queue_picture_loss_at(generation, role, media_ssrc, Instant::now())
    }

    fn queue_picture_loss_at(
        &mut self,
        generation: u64,
        role: MediaRole,
        media_ssrc: u32,
        now: Instant,
    ) -> Result<()> {
        self.validate_generation(generation)?;
        ensure!(
            self.phase == MediaTransportPhase::Active,
            "仅激活的 Apple HP 媒体传输可请求视频恢复"
        );
        ensure!(
            matches!(role, MediaRole::VideoStream1 | MediaRole::VideoStream2),
            "PLI 只能绑定到 Apple HP 视频 socket"
        );
        let newly_queued = {
            let pending = &mut self
                .socket_mut(generation, role)?
                .pending_picture_loss_media_ssrc;
            if pending.is_some() {
                false
            } else {
                *pending = Some(media_ssrc);
                true
            }
        };
        if newly_queued {
            if let Some(due_at) = self.next_control_report_at.as_mut() {
                *due_at = (*due_at).min(now);
            }
        }
        Ok(())
    }

    fn socket(&self, generation: u64, role: MediaRole) -> Result<&BoundMediaSocket> {
        self.validate_generation(generation)?;
        self.sockets
            .iter()
            .find(|entry| entry.role == role)
            .with_context(|| format!("媒体角色 {role:?} 没有已绑定 socket"))
    }

    fn socket_mut(&mut self, generation: u64, role: MediaRole) -> Result<&mut BoundMediaSocket> {
        self.validate_generation(generation)?;
        self.sockets
            .iter_mut()
            .find(|entry| entry.role == role)
            .with_context(|| format!("媒体角色 {role:?} 没有已绑定 socket"))
    }

    pub fn local_addr(&self, generation: u64, role: MediaRole) -> Result<SocketAddr> {
        self.socket(generation, role)?
            .socket
            .local_addr()
            .with_context(|| format!("读取 {role:?} 本地 UDP 地址失败"))
    }

    pub fn active_roles(&self, generation: u64) -> Result<Vec<MediaRole>> {
        self.validate_generation(generation)?;
        Ok(self.sockets.iter().map(|entry| entry.role).collect())
    }

    pub fn drain_receive_round<F>(
        &mut self,
        generation: u64,
        mut handler: F,
    ) -> Result<MediaPollSummary>
    where
        F: FnMut(MediaRole, MediaDatagram) -> Result<()>,
    {
        self.drain_receive_round_received(generation, |role, received| {
            handler(role, received.datagram)
        })
    }

    pub fn drain_receive_round_received<F>(
        &mut self,
        generation: u64,
        handler: F,
    ) -> Result<MediaPollSummary>
    where
        F: FnMut(MediaRole, ReceivedMediaDatagram) -> Result<()>,
    {
        self.drain_receive_round_with_budget(
            generation,
            MAX_MEDIA_DATAGRAMS_PER_ROLE_PER_POLL,
            handler,
        )
    }

    fn drain_receive_round_with_budget<F>(
        &mut self,
        generation: u64,
        budget: usize,
        mut handler: F,
    ) -> Result<MediaPollSummary>
    where
        F: FnMut(MediaRole, ReceivedMediaDatagram) -> Result<()>,
    {
        ensure!(
            self.phase == MediaTransportPhase::Active,
            "媒体数据只能在 Active 状态接收"
        );
        let mut role_order = self.active_roles(generation)?;
        ensure!(!role_order.is_empty(), "Active 媒体传输没有已绑定角色");
        let start_index = self.next_poll_role_index % role_order.len();
        role_order.rotate_left(start_index);
        self.next_poll_role_index = (start_index + 1) % role_order.len();

        let mut summary = MediaPollSummary {
            role_order,
            ..MediaPollSummary::default()
        };
        for role in summary.role_order.clone() {
            let mut stats = MediaRolePollStats::default();
            for _ in 0..budget {
                match self.try_recv_decrypted(generation, role)? {
                    MediaReceiveOutcome::Empty => break,
                    MediaReceiveOutcome::Discarded(_) => {
                        stats.processed += 1;
                        stats.discarded += 1;
                    }
                    MediaReceiveOutcome::Accepted(datagram) => {
                        handler(role, datagram)?;
                        stats.processed += 1;
                        stats.accepted += 1;
                    }
                }
            }
            summary.roles.push((role, stats));
        }
        summary.accepted_total = summary
            .role_order
            .iter()
            .map(|role| summary.per_role(*role).accepted)
            .sum();
        summary.discarded_total = summary
            .role_order
            .iter()
            .map(|role| summary.per_role(*role).discarded)
            .sum();
        Ok(summary)
    }

    #[cfg(test)]
    pub fn send_opaque(&self, generation: u64, role: MediaRole, packet: &[u8]) -> Result<()> {
        ensure!(
            self.phase == MediaTransportPhase::Active,
            "媒体数据只能在 Active 状态发送"
        );
        ensure!(
            !packet.is_empty() && packet.len() <= MAX_MEDIA_DATAGRAM_BYTES,
            "媒体 UDP 数据报长度非法: {}",
            packet.len()
        );
        let entry = self.socket(generation, role)?;
        let written = entry
            .socket
            .send_to(packet, entry.remote)
            .with_context(|| format!("发送 {role:?} UDP 数据报失败"))?;
        ensure!(written == packet.len(), "UDP 数据报未完整发送");
        Ok(())
    }

    pub fn send_audio_access_unit(
        &mut self,
        generation: u64,
        access_unit: &[u8],
    ) -> Result<OutboundRtpPacketMetadata> {
        ensure!(
            self.phase == MediaTransportPhase::Active,
            "音频媒体只能在 Active 状态发送"
        );
        ensure!(
            self.audio_flow == AudioMediaFlow::PcToMac,
            "只有 PC→Mac 协商方向允许发送音频 RTP"
        );
        ensure!(!access_unit.is_empty(), "PC→Mac AAC-ELD raw AU 不能为空");
        let metadata = {
            let entry = self.socket_mut(generation, MediaRole::Audio)?;
            let (packet, metadata) = entry
                .outbound_rtp
                .as_mut()
                .context("Audio 缺少出站 SRTP 状态")?
                // AVConference mode 4 / stream mode 7 使用 bundling scheme 3：编码器
                // 产生的 AAC-ELD AU 必须逐字节作为 RTP payload，不能添加 scheme-2
                // RFC 3640 AU header。原生输出门禁通过前生产 PC→Mac 入口仍 fail-closed。
                .protect_audio_access_unit(access_unit)?;
            ensure!(
                packet.len() <= MAX_MEDIA_DATAGRAM_BYTES,
                "音频 SRTP 数据报超过 UDP 负载上限: {}",
                packet.len()
            );
            let written = entry
                .socket
                .send_to(&packet, entry.remote)
                .context("发送 Audio SRTP 数据报失败")?;
            ensure!(written == packet.len(), "Audio SRTP 数据报未完整发送");
            metadata
        };
        self.outbound_audio_evidence.record_sent(metadata)?;
        Ok(metadata)
    }

    pub fn try_recv_decrypted(
        &mut self,
        generation: u64,
        role: MediaRole,
    ) -> Result<MediaReceiveOutcome> {
        ensure!(
            self.phase == MediaTransportPhase::Active,
            "媒体数据只能在 Active 状态接收"
        );
        self.validate_generation(generation)?;
        let socket_index = self
            .sockets
            .iter()
            .position(|entry| entry.role == role)
            .with_context(|| format!("媒体角色 {role:?} 没有已绑定 socket"))?;
        let mut packet = vec![0u8; MAX_MEDIA_DATAGRAM_BYTES];
        let (received, source, received_at) =
            match self.sockets[socket_index].socket.recv_from(&mut packet) {
                Ok((received, source)) => (received, source, Instant::now()),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    return Ok(MediaReceiveOutcome::Empty);
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("接收 {role:?} UDP 数据报失败"));
                }
            };
        if source != self.sockets[socket_index].remote {
            return Ok(self.record_discard(role, MediaDiscardReason::UnexpectedSource, None));
        }
        packet.truncate(received);
        if packet.is_empty() {
            return Ok(self.record_discard(role, MediaDiscardReason::EmptyDatagram, None));
        }
        const RTP_RTCP_MUX_DISCRIMINATOR_LEN: usize = 2;
        if packet.len() < RTP_RTCP_MUX_DISCRIMINATOR_LEN {
            return Ok(self.record_discard(role, MediaDiscardReason::TruncatedHeader, None));
        }
        let packet_kind = match classify_rtp_mux_packet(&packet) {
            Ok(packet_kind) => packet_kind,
            Err(error) => {
                return Ok(self.record_discard(
                    role,
                    MediaDiscardReason::MalformedPacket,
                    Some(&error),
                ));
            }
        };
        let crypto_result = {
            let inbound = self.sockets[socket_index]
                .inbound_crypto
                .as_mut()
                .with_context(|| format!("{role:?} 缺少入站 SRTP 状态"))?;
            match packet_kind {
                RtpMuxPacketKind::Rtcp => inbound.rtcp_receiver.open(&packet),
                RtpMuxPacketKind::Rtp => inbound.rtp_receiver.open(&packet),
            }
        };
        match crypto_result {
            Ok(Some(plaintext)) => {
                if packet_kind == RtpMuxPacketKind::Rtcp {
                    if let Err(error) = self.sockets[socket_index]
                        .reception_report
                        .observe_sender_reports(&plaintext, received_at)
                    {
                        return Ok(self.record_discard(
                            role,
                            MediaDiscardReason::MalformedPacket,
                            Some(&error),
                        ));
                    }
                } else {
                    let header = match parse_rtp_header(&plaintext) {
                        Ok(header) => header,
                        Err(error) => {
                            return Ok(self.record_discard(
                                role,
                                MediaDiscardReason::MalformedPacket,
                                Some(&error),
                            ));
                        }
                    };
                    self.sockets[socket_index]
                        .reception_report
                        .observe_rtp(header, received_at);
                }
                if role == MediaRole::Audio && packet_kind == RtpMuxPacketKind::Rtcp {
                    let reports = match parse_rtcp_reception_reports(&plaintext) {
                        Ok(reports) => reports,
                        Err(error) => {
                            return Ok(self.record_discard(
                                role,
                                MediaDiscardReason::MalformedPacket,
                                Some(&error),
                            ));
                        }
                    };
                    self.outbound_audio_evidence.observe_reports(reports);
                }
                let datagram = match packet_kind {
                    RtpMuxPacketKind::Rtcp => MediaDatagram::Rtcp(plaintext),
                    RtpMuxPacketKind::Rtp => MediaDatagram::Rtp(plaintext),
                };
                Ok(MediaReceiveOutcome::Accepted(ReceivedMediaDatagram {
                    datagram,
                    received_at,
                }))
            }
            Ok(None) => Ok(self.record_discard(role, MediaDiscardReason::ReplayOrTooOld, None)),
            Err(error) => match secure_packet_discard_kind(&error) {
                Some(SecurePacketDiscardKind::MalformedPacket) => Ok(self.record_discard(
                    role,
                    MediaDiscardReason::MalformedPacket,
                    Some(&error),
                )),
                Some(SecurePacketDiscardKind::AuthenticationFailed) => Ok(self.record_discard(
                    role,
                    MediaDiscardReason::AuthenticationFailed,
                    Some(&error),
                )),
                None => Err(error).with_context(|| format!("打开 {role:?} 安全媒体数据报失败")),
            },
        }
    }

    pub fn discard_counters(&self) -> MediaDiscardCounters {
        self.discard_counters
    }

    fn record_discard(
        &mut self,
        role: MediaRole,
        reason: MediaDiscardReason,
        diagnostic: Option<&anyhow::Error>,
    ) -> MediaReceiveOutcome {
        let counter = match reason {
            MediaDiscardReason::UnexpectedSource => &mut self.discard_counters.unexpected_source,
            MediaDiscardReason::EmptyDatagram => &mut self.discard_counters.empty_datagram,
            MediaDiscardReason::TruncatedHeader => &mut self.discard_counters.truncated_header,
            MediaDiscardReason::MalformedPacket => &mut self.discard_counters.malformed_packet,
            MediaDiscardReason::AuthenticationFailed => {
                &mut self.discard_counters.authentication_failed
            }
            MediaDiscardReason::ReplayOrTooOld => &mut self.discard_counters.replay_or_too_old,
        };
        *counter = counter.saturating_add(1);
        let count = self.discard_counters().count(reason);
        if count.is_power_of_two() {
            if let Some(error) = diagnostic {
                eprintln!("[media] {role:?} 丢弃 {reason:?} 数据报（累计 {count}）: {error:#}");
            } else {
                eprintln!("[media] {role:?} 丢弃 {reason:?} 数据报（累计 {count}）");
            }
        }
        MediaReceiveOutcome::Discarded(reason)
    }

    pub fn close(&mut self, generation: u64) -> Result<()> {
        self.validate_generation(generation)?;
        self.phase = MediaTransportPhase::Closing;
        self.sockets.clear();
        self.configuration = None;
        self.answer = None;
        self.next_control_report_at = None;
        self.next_poll_role_index = 0;
        self.reset_outbound_audio_evidence();
        Ok(())
    }
}

fn send_control_report(bound: &mut BoundMediaSocket, now: Instant) -> Result<()> {
    let snapshot = bound.reception_report.report_snapshot(now);
    let control = bound
        .outbound_control
        .as_mut()
        .with_context(|| format!("{0:?} 缺少 SRTCP 发送状态", bound.role))?;
    let report = build_compound_rtcp_receiver_report_with_block_and_picture_loss(
        control.local_ssrc,
        snapshot.map(|snapshot| snapshot.report),
        bound.pending_picture_loss_media_ssrc,
    );
    let packet = control
        .sender
        .protect(&report)
        .with_context(|| format!("保护 {0:?} SRTCP Receiver Report 失败", bound.role))?;
    let written = bound
        .socket
        .send_to(&packet, bound.remote)
        .with_context(|| format!("发送 {0:?} SRTCP Receiver Report 失败", bound.role))?;
    ensure!(written == packet.len(), "SRTCP 数据报未完整发送");
    if let Some(snapshot) = snapshot {
        bound.reception_report.commit_report(snapshot);
    }
    bound.pending_picture_loss_media_ssrc = None;
    Ok(())
}

fn configuration_entry(
    configuration: &ClientMediaStreamConfiguration,
    role: MediaRole,
) -> Result<&MediaStreamConfigurationEntry> {
    match role {
        MediaRole::Audio => Ok(&configuration.audio),
        MediaRole::VideoStream1 => Ok(&configuration.video_stream_1),
        MediaRole::VideoStream2 => configuration
            .video_stream_2
            .as_ref()
            .context("端口公告启用了未配置的 video stream 2"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        send_control_report, AudioReceptionEvidence, BoundMediaSocket, MediaDatagram,
        MediaDiscardCounters, MediaDiscardReason, MediaReceiveOutcome, MediaRole, MediaTransport,
        MediaTransportPhase, OutboundControlStream, OutboundRtpStream, ReceivedMediaDatagram,
    };
    use crate::audio_codec::{ARD_AUDIO_RTP_PAYLOAD_TYPE, ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT};
    use crate::media_negotiation::{AudioMediaFlow, CompressedProtobufAnswer, MediaStreamAnswer};
    use crate::media_protocol::parse_media_stream_port_announcement;
    use crate::srtp::{
        build_compound_rtcp_receiver_report, derive_session_keys, parse_rtcp_reception_reports,
        parse_rtp_packet, protect_rtp_packet, RtpHeader, RtpReceptionReportState, SrtcpReceiver,
        SrtcpSender, SrtpPacketKind, SrtpReceiver, SrtpSessionKeys, RTP_FIXED_HEADER_LEN,
    };
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
    use std::time::{Duration, Instant};

    fn announcement_fixture(audio_port: u16, audio_flags: u32) -> [u8; 54] {
        let mut frame = [0u8; 54];
        frame[0..4].copy_from_slice(&1u32.to_be_bytes());
        frame[12..16].copy_from_slice(&0x03f2i32.to_be_bytes());
        frame[16..18].copy_from_slice(&36u16.to_be_bytes());
        frame[18..20].copy_from_slice(&1u16.to_be_bytes());
        frame[20..22].copy_from_slice(&1u16.to_be_bytes());
        frame[26..28].copy_from_slice(&audio_port.to_be_bytes());
        frame[28..32].copy_from_slice(&audio_flags.to_be_bytes());
        frame
    }

    fn three_role_announcement_fixture(ports: [u16; 3]) -> [u8; 54] {
        let mut frame = announcement_fixture(ports[0], 1);
        frame[32..34].copy_from_slice(&ports[1].to_be_bytes());
        frame[34..38].copy_from_slice(&1u32.to_be_bytes());
        frame[38..40].copy_from_slice(&ports[2].to_be_bytes());
        frame[40..44].copy_from_slice(&1u32.to_be_bytes());
        frame
    }

    fn answer_fixture() -> MediaStreamAnswer {
        let opaque = CompressedProtobufAnswer {
            compressed: vec![1],
            decompressed: vec![1],
        };
        MediaStreamAnswer {
            stream_1_supports_60_fps: true,
            stream_2_supports_60_fps: false,
            audio: opaque.clone(),
            video_stream_1: opaque,
            video_stream_2: None,
        }
    }

    struct PcToMacAudioLoopback {
        transport: MediaTransport,
        remote: UdpSocket,
        local: std::net::SocketAddr,
        rtcp_sender: SrtcpSender,
        outbound_receiver: SrtpReceiver,
    }

    impl PcToMacAudioLoopback {
        const GENERATION: u64 = 9;

        fn new() -> Self {
            let remote = crate::bind_test_udp_loopback();
            remote
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let announcement = parse_media_stream_port_announcement(&announcement_fixture(
                remote.local_addr().unwrap().port(),
                1,
            ))
            .unwrap();
            let mut transport =
                MediaTransport::new(Self::GENERATION, IpAddr::V4(Ipv4Addr::LOCALHOST));
            transport.set_audio_flow(AudioMediaFlow::PcToMac).unwrap();
            transport
                .accept_port_announcement(Self::GENERATION, announcement)
                .unwrap();
            transport
                .bind_test_loopback_sockets(Self::GENERATION, &[(MediaRole::Audio, &remote)])
                .unwrap();
            transport.prepare_configuration(Self::GENERATION).unwrap();
            let rtcp_sender = SrtcpSender::new(derive_session_keys(
                &transport
                    .configuration
                    .as_ref()
                    .unwrap()
                    .audio
                    .server_to_viewer,
                SrtpPacketKind::Rtcp,
            ));
            let outbound_receiver = SrtpReceiver::new(derive_session_keys(
                &transport
                    .configuration
                    .as_ref()
                    .unwrap()
                    .audio
                    .viewer_to_server,
                SrtpPacketKind::Rtp,
            ));
            transport.mark_configuration_sent(Self::GENERATION).unwrap();
            transport
                .accept_answer(Self::GENERATION, answer_fixture())
                .unwrap();
            transport.activate(Self::GENERATION).unwrap();
            let local = transport
                .local_addr(Self::GENERATION, MediaRole::Audio)
                .unwrap();
            let mut initial_report = [0u8; 128];
            remote.recv_from(&mut initial_report).unwrap();
            Self {
                transport,
                remote,
                local,
                rtcp_sender,
                outbound_receiver,
            }
        }

        fn receive_outbound_audio_payload(&mut self) -> Vec<u8> {
            let mut datagram = [0u8; 2_048];
            let (count, source) = self.remote.recv_from(&mut datagram).unwrap();
            assert_eq!(source, self.local);
            let plaintext = self
                .outbound_receiver
                .open(&datagram[..count])
                .unwrap()
                .expect("出站音频包不应被重放窗口丢弃");
            parse_rtp_packet(&plaintext).unwrap().payload.to_vec()
        }

        fn send_rtcp(&mut self, plaintext: &[u8]) {
            let protected = self.rtcp_sender.protect(plaintext).unwrap();
            self.remote.send_to(&protected, self.local).unwrap();
        }

        fn receive_rtcp(&mut self) -> MediaReceiveOutcome {
            self.transport
                .poll_test_receive_decrypted(Self::GENERATION, MediaRole::Audio)
                .unwrap()
        }
    }

    fn receiver_report(source_ssrc: u32, extended_highest_sequence: u32, lost: i32) -> Vec<u8> {
        let mut report = vec![0x81, 201, 0, 7];
        report.extend_from_slice(&0xaabb_ccdd_u32.to_be_bytes());
        report.extend_from_slice(&source_ssrc.to_be_bytes());
        report.push(0);
        report.extend_from_slice(&lost.to_be_bytes()[1..]);
        report.extend_from_slice(&extended_highest_sequence.to_be_bytes());
        report.extend_from_slice(&0u32.to_be_bytes());
        report.extend_from_slice(&0u32.to_be_bytes());
        report.extend_from_slice(&0u32.to_be_bytes());
        report
    }

    fn mono_raw_access_unit_fixture() -> Vec<u8> {
        let capture =
            crate::read_private_fixture_bytes("ard_capture/p4_udp/rust_aac_eld_mono.frdrtp");
        const CAPTURE_HEADER_BYTES: usize = 12;

        assert_eq!(&capture[..8], b"FRDRTP01");
        let packet_length = u32::from_be_bytes(capture[8..12].try_into().unwrap()) as usize;
        let packet = &capture[CAPTURE_HEADER_BYTES..CAPTURE_HEADER_BYTES + packet_length];
        parse_rtp_packet(packet).unwrap().payload.to_vec()
    }

    #[test]
    #[ignore = "需要未纳入公开仓库的本地授权 RTP fixture"]
    fn mode4_audio_sender_preserves_raw_access_units_in_decrypted_rtp_payload() {
        const DTX_ACCESS_UNIT: [u8; 4] = [0x00, 0x68, 0x34, 0x00];
        let mono_fixture = mono_raw_access_unit_fixture();
        assert_eq!(mono_fixture.len(), 143);
        let mut loopback = PcToMacAudioLoopback::new();

        loopback
            .transport
            .send_audio_access_unit(PcToMacAudioLoopback::GENERATION, &DTX_ACCESS_UNIT)
            .unwrap();
        let dtx_payload = loopback.receive_outbound_audio_payload();
        assert_eq!(dtx_payload.len(), DTX_ACCESS_UNIT.len());
        assert!(
            dtx_payload.as_slice() == DTX_ACCESS_UNIT,
            "mode-4 DTX RTP payload 必须逐字节保持 raw AU"
        );

        loopback
            .transport
            .send_audio_access_unit(PcToMacAudioLoopback::GENERATION, &mono_fixture)
            .unwrap();
        let mono_payload = loopback.receive_outbound_audio_payload();
        assert_eq!(mono_payload.len(), mono_fixture.len());
        assert!(
            mono_payload.as_slice() == mono_fixture.as_slice(),
            "mode-4 mono RTP payload 必须逐字节保持 raw AU"
        );
    }

    struct ThreeRoleLoopback {
        transport: MediaTransport,
        remotes: [(MediaRole, UdpSocket); 3],
        incoming_keys: [(MediaRole, SrtpSessionKeys); 3],
        incoming_control_senders: [(MediaRole, SrtcpSender); 3],
        control_receivers: [(MediaRole, SrtcpReceiver); 3],
    }

    impl ThreeRoleLoopback {
        const GENERATION: u64 = 7;

        fn new() -> Self {
            let remotes = [
                (MediaRole::Audio, crate::bind_test_udp_loopback()),
                (MediaRole::VideoStream1, crate::bind_test_udp_loopback()),
                (MediaRole::VideoStream2, crate::bind_test_udp_loopback()),
            ];
            for (_, remote) in &remotes {
                remote
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
            }
            let ports = [
                remotes[0].1.local_addr().unwrap().port(),
                remotes[1].1.local_addr().unwrap().port(),
                remotes[2].1.local_addr().unwrap().port(),
            ];
            let announcement =
                parse_media_stream_port_announcement(&three_role_announcement_fixture(ports))
                    .unwrap();
            let mut transport =
                MediaTransport::new(Self::GENERATION, IpAddr::V4(Ipv4Addr::LOCALHOST));
            transport
                .accept_port_announcement(Self::GENERATION, announcement)
                .unwrap();
            let peer_refs =
                std::array::from_fn::<_, 3, _>(|index| (remotes[index].0, &remotes[index].1));
            transport
                .bind_test_loopback_sockets(Self::GENERATION, &peer_refs)
                .unwrap();
            transport.prepare_configuration(Self::GENERATION).unwrap();
            let second_video_configuration = transport
                .configuration
                .as_ref()
                .unwrap()
                .video_stream_1
                .clone();
            transport.configuration.as_mut().unwrap().video_stream_2 =
                Some(second_video_configuration);
            let configuration = transport.configuration.as_ref().unwrap();
            let incoming_keys = [
                (
                    MediaRole::Audio,
                    derive_session_keys(&configuration.audio.server_to_viewer, SrtpPacketKind::Rtp),
                ),
                (
                    MediaRole::VideoStream1,
                    derive_session_keys(
                        &configuration.video_stream_1.server_to_viewer,
                        SrtpPacketKind::Rtp,
                    ),
                ),
                (
                    MediaRole::VideoStream2,
                    derive_session_keys(
                        &configuration
                            .video_stream_2
                            .as_ref()
                            .unwrap()
                            .server_to_viewer,
                        SrtpPacketKind::Rtp,
                    ),
                ),
            ];
            let control_receivers = [
                (
                    MediaRole::Audio,
                    SrtcpReceiver::new(derive_session_keys(
                        &configuration.audio.viewer_to_server,
                        SrtpPacketKind::Rtcp,
                    )),
                ),
                (
                    MediaRole::VideoStream1,
                    SrtcpReceiver::new(derive_session_keys(
                        &configuration.video_stream_1.viewer_to_server,
                        SrtpPacketKind::Rtcp,
                    )),
                ),
                (
                    MediaRole::VideoStream2,
                    SrtcpReceiver::new(derive_session_keys(
                        &configuration
                            .video_stream_2
                            .as_ref()
                            .unwrap()
                            .viewer_to_server,
                        SrtpPacketKind::Rtcp,
                    )),
                ),
            ];
            let incoming_control_senders = [
                (
                    MediaRole::Audio,
                    SrtcpSender::new(derive_session_keys(
                        &configuration.audio.server_to_viewer,
                        SrtpPacketKind::Rtcp,
                    )),
                ),
                (
                    MediaRole::VideoStream1,
                    SrtcpSender::new(derive_session_keys(
                        &configuration.video_stream_1.server_to_viewer,
                        SrtpPacketKind::Rtcp,
                    )),
                ),
                (
                    MediaRole::VideoStream2,
                    SrtcpSender::new(derive_session_keys(
                        &configuration
                            .video_stream_2
                            .as_ref()
                            .unwrap()
                            .server_to_viewer,
                        SrtpPacketKind::Rtcp,
                    )),
                ),
            ];
            transport.mark_configuration_sent(Self::GENERATION).unwrap();
            let mut answer = answer_fixture();
            answer.video_stream_2 = Some(answer.video_stream_1.clone());
            transport.accept_answer(Self::GENERATION, answer).unwrap();
            transport.activate(Self::GENERATION).unwrap();
            let mut initial_report = [0u8; 128];
            for (_, remote) in &remotes {
                remote.recv_from(&mut initial_report).unwrap();
            }
            Self {
                transport,
                remotes,
                incoming_keys,
                incoming_control_senders,
                control_receivers,
            }
        }

        fn send_rtp(&self, role: MediaRole, sequence: u16, payload: &[u8]) {
            const TEST_TIMESTAMP: u32 = 960;
            const TEST_SSRC: u32 = 0x5566_7788;
            let mut plaintext = vec![2 << 6, ARD_AUDIO_RTP_PAYLOAD_TYPE];
            plaintext.extend_from_slice(&sequence.to_be_bytes());
            plaintext.extend_from_slice(&TEST_TIMESTAMP.to_be_bytes());
            plaintext.extend_from_slice(&TEST_SSRC.to_be_bytes());
            plaintext.extend_from_slice(payload);
            let keys = &self
                .incoming_keys
                .iter()
                .find(|(candidate, _)| *candidate == role)
                .unwrap()
                .1;
            let protected = protect_rtp_packet(&plaintext, keys, 0).unwrap();
            let remote = &self
                .remotes
                .iter()
                .find(|(candidate, _)| *candidate == role)
                .unwrap()
                .1;
            let local = self.transport.local_addr(Self::GENERATION, role).unwrap();
            remote.send_to(&protected, local).unwrap();
        }

        fn send_tampered_rtp(&self, role: MediaRole, sequence: u16) {
            const TEST_TIMESTAMP: u32 = 960;
            const TEST_SSRC: u32 = 0x5566_7788;
            let mut plaintext = vec![2 << 6, ARD_AUDIO_RTP_PAYLOAD_TYPE];
            plaintext.extend_from_slice(&sequence.to_be_bytes());
            plaintext.extend_from_slice(&TEST_TIMESTAMP.to_be_bytes());
            plaintext.extend_from_slice(&TEST_SSRC.to_be_bytes());
            plaintext.extend_from_slice(b"tampered");
            let keys = &self
                .incoming_keys
                .iter()
                .find(|(candidate, _)| *candidate == role)
                .unwrap()
                .1;
            let mut protected = protect_rtp_packet(&plaintext, keys, 0).unwrap();
            *protected.last_mut().unwrap() ^= 0x01;
            let remote = &self
                .remotes
                .iter()
                .find(|(candidate, _)| *candidate == role)
                .unwrap()
                .1;
            let local = self.transport.local_addr(Self::GENERATION, role).unwrap();
            remote.send_to(&protected, local).unwrap();
        }

        fn send_empty_datagram(&self, role: MediaRole) {
            let remote = &self
                .remotes
                .iter()
                .find(|(candidate, _)| *candidate == role)
                .unwrap()
                .1;
            let local = self.transport.local_addr(Self::GENERATION, role).unwrap();
            remote.send_to(&[], local).unwrap();
        }

        fn receive_control_reports(
            &mut self,
            role: MediaRole,
        ) -> Vec<crate::srtp::RtcpReceptionReport> {
            let plaintext = self.receive_control_plaintext(role);
            parse_rtcp_reception_reports(&plaintext).unwrap()
        }

        fn receive_control_plaintext(&mut self, role: MediaRole) -> Vec<u8> {
            let remote = &self
                .remotes
                .iter()
                .find(|(candidate, _)| *candidate == role)
                .unwrap()
                .1;
            let mut protected = [0u8; 256];
            let (count, _) = remote.recv_from(&mut protected).unwrap();
            let plaintext = self
                .control_receivers
                .iter_mut()
                .find(|(candidate, _)| *candidate == role)
                .unwrap()
                .1
                .open(&protected[..count])
                .unwrap()
                .expect("出站 SRTCP 控制报告不应被重放窗口丢弃");
            plaintext
        }

        fn send_rtcp(&mut self, role: MediaRole, plaintext: &[u8]) {
            let protected = self
                .incoming_control_senders
                .iter_mut()
                .find(|(candidate, _)| *candidate == role)
                .unwrap()
                .1
                .protect(plaintext)
                .unwrap();
            let remote = &self
                .remotes
                .iter()
                .find(|(candidate, _)| *candidate == role)
                .unwrap()
                .1;
            let local = self.transport.local_addr(Self::GENERATION, role).unwrap();
            remote.send_to(&protected, local).unwrap();
        }
    }

    #[test]
    fn periodic_control_report_describes_only_authenticated_unique_inbound_rtp() {
        const REMOTE_SSRC: u32 = 0x5566_7788;
        let mut loopback = ThreeRoleLoopback::new();
        let local_audio_ssrc = loopback
            .transport
            .configuration
            .as_ref()
            .unwrap()
            .audio
            .offer
            .local_ssrc;
        let before_receive = Instant::now();
        loopback.send_rtp(MediaRole::Audio, 1, b"first");
        let MediaReceiveOutcome::Accepted(received) = loopback
            .transport
            .poll_test_receive_decrypted(ThreeRoleLoopback::GENERATION, MediaRole::Audio)
            .unwrap()
        else {
            panic!("authenticated RTP must be accepted");
        };
        let after_receive = Instant::now();
        assert!(matches!(received.datagram, MediaDatagram::Rtp(_)));
        assert!(received.received_at >= before_receive);
        assert!(received.received_at <= after_receive);
        let report_arrival = loopback
            .transport
            .sockets
            .iter()
            .find(|socket| socket.role == MediaRole::Audio)
            .and_then(|socket| socket.reception_report.previous_arrival());
        assert_eq!(
            report_arrival,
            Some(received.received_at),
            "the accepted envelope must preserve the recv_from timestamp used by RTP accounting"
        );
        loopback.send_rtp(MediaRole::Audio, 1, b"first");
        assert_eq!(
            loopback
                .transport
                .poll_test_receive_decrypted(ThreeRoleLoopback::GENERATION, MediaRole::Audio)
                .unwrap(),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::ReplayOrTooOld)
        );
        loopback.send_tampered_rtp(MediaRole::Audio, 2);
        assert_eq!(
            loopback
                .transport
                .poll_test_receive_decrypted(ThreeRoleLoopback::GENERATION, MediaRole::Audio)
                .unwrap(),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::AuthenticationFailed)
        );
        loopback.send_rtp(MediaRole::Audio, 3, b"third");
        assert!(matches!(
            loopback
                .transport
                .poll_test_receive_decrypted(ThreeRoleLoopback::GENERATION, MediaRole::Audio)
                .unwrap(),
            MediaReceiveOutcome::Accepted(ReceivedMediaDatagram {
                datagram: MediaDatagram::Rtp(_),
                ..
            })
        ));

        assert_eq!(
            loopback
                .transport
                .service_control_reports_at(
                    ThreeRoleLoopback::GENERATION,
                    Instant::now() + Duration::from_secs(2),
                )
                .unwrap(),
            3
        );
        let reports = loopback.receive_control_reports(MediaRole::Audio);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].reporter_ssrc, local_audio_ssrc);
        assert_eq!(reports[0].source_ssrc, REMOTE_SSRC);
        assert_eq!(reports[0].extended_highest_sequence, 3);
        assert_eq!(reports[0].cumulative_packets_lost, 1);
        assert_eq!(reports[0].fraction_lost, 85);
        assert!(loopback
            .receive_control_reports(MediaRole::VideoStream1)
            .is_empty());
    }

    #[test]
    fn authenticated_sender_report_populates_lsr_and_dlsr_in_next_control_report() {
        const REMOTE_SSRC: u32 = 0x5566_7788;
        let mut loopback = ThreeRoleLoopback::new();
        loopback.send_rtp(MediaRole::VideoStream1, 1, b"video");
        assert!(matches!(
            loopback
                .transport
                .poll_test_receive_decrypted(
                    ThreeRoleLoopback::GENERATION,
                    MediaRole::VideoStream1,
                )
                .unwrap(),
            MediaReceiveOutcome::Accepted(ReceivedMediaDatagram {
                datagram: MediaDatagram::Rtp(_),
                ..
            })
        ));

        let mut sender_report = vec![0x80, 200, 0, 6];
        sender_report.extend_from_slice(&REMOTE_SSRC.to_be_bytes());
        sender_report.extend_from_slice(&0x1122_3344u32.to_be_bytes());
        sender_report.extend_from_slice(&0x5566_7788u32.to_be_bytes());
        sender_report.extend_from_slice(&0u32.to_be_bytes());
        sender_report.extend_from_slice(&0u32.to_be_bytes());
        sender_report.extend_from_slice(&0u32.to_be_bytes());
        loopback.send_rtcp(MediaRole::VideoStream1, &sender_report);
        let MediaReceiveOutcome::Accepted(received) = loopback
            .transport
            .poll_test_receive_decrypted(ThreeRoleLoopback::GENERATION, MediaRole::VideoStream1)
            .unwrap()
        else {
            panic!("authenticated sender report must be accepted");
        };
        assert_eq!(received.datagram, MediaDatagram::Rtcp(sender_report));

        loopback
            .transport
            .service_control_reports_at(
                ThreeRoleLoopback::GENERATION,
                Instant::now() + Duration::from_millis(1_500),
            )
            .unwrap();
        let reports = loopback.receive_control_reports(MediaRole::VideoStream1);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].last_sender_report, 0x3344_5566);
        assert!(reports[0].delay_since_last_sender_report >= 0x0001_0000);
    }

    #[test]
    fn queued_picture_loss_expedites_control_report_and_coalesces_per_video_socket() {
        const REMOTE_MEDIA_SSRC: u32 = 0x5566_7788;
        let mut loopback = ThreeRoleLoopback::new();
        let local_sender_ssrc = loopback
            .transport
            .configuration
            .as_ref()
            .unwrap()
            .video_stream_1
            .offer
            .local_ssrc;
        loopback
            .transport
            .queue_picture_loss(
                ThreeRoleLoopback::GENERATION,
                MediaRole::VideoStream1,
                REMOTE_MEDIA_SSRC,
            )
            .unwrap();
        loopback
            .transport
            .queue_picture_loss(
                ThreeRoleLoopback::GENERATION,
                MediaRole::VideoStream1,
                REMOTE_MEDIA_SSRC,
            )
            .unwrap();

        let service_now = Instant::now();
        assert_eq!(
            loopback
                .transport
                .service_control_reports_at(ThreeRoleLoopback::GENERATION, service_now)
                .unwrap(),
            3,
            "首个 PLI 入队必须提前全局控制报告截止时间"
        );
        assert!(loopback
            .receive_control_plaintext(MediaRole::Audio)
            .windows(2)
            .all(|header| header != [0x81, 206]));
        let video = loopback.receive_control_plaintext(MediaRole::VideoStream1);
        assert_eq!(
            &video[video.len() - 12..],
            &[
                0x81,
                206,
                0,
                2,
                local_sender_ssrc.to_be_bytes()[0],
                local_sender_ssrc.to_be_bytes()[1],
                local_sender_ssrc.to_be_bytes()[2],
                local_sender_ssrc.to_be_bytes()[3],
                REMOTE_MEDIA_SSRC.to_be_bytes()[0],
                REMOTE_MEDIA_SSRC.to_be_bytes()[1],
                REMOTE_MEDIA_SSRC.to_be_bytes()[2],
                REMOTE_MEDIA_SSRC.to_be_bytes()[3],
            ]
        );
        assert_eq!(
            video
                .windows(2)
                .filter(|header| *header == [0x81, 206])
                .count(),
            1
        );
        assert!(loopback
            .receive_control_plaintext(MediaRole::VideoStream2)
            .windows(2)
            .all(|header| header != [0x81, 206]));

        assert_eq!(
            loopback
                .transport
                .service_control_reports_at(ThreeRoleLoopback::GENERATION, service_now)
                .unwrap(),
            0,
            "成功发送后必须清除 PLI，并恢复普通报告周期"
        );
        assert_eq!(
            loopback
                .transport
                .socket(ThreeRoleLoopback::GENERATION, MediaRole::VideoStream1)
                .unwrap()
                .pending_picture_loss_media_ssrc,
            None
        );
    }

    #[test]
    fn failed_control_send_does_not_commit_interval_but_successful_send_does() {
        const REMOTE_SSRC: u32 = 0x5566_7788;
        let now = Instant::now();
        let mut reception_report = RtpReceptionReportState::new(48_000);
        for sequence in [1, 3] {
            reception_report.observe_rtp(
                RtpHeader {
                    marker: false,
                    payload_type: ARD_AUDIO_RTP_PAYLOAD_TYPE,
                    sequence,
                    timestamp: u32::from(sequence) * 960,
                    ssrc: REMOTE_SSRC,
                    payload_offset: RTP_FIXED_HEADER_LEN,
                },
                now,
            );
        }
        let mut bound = BoundMediaSocket {
            role: MediaRole::VideoStream1,
            socket: UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
            receive_buffer_actual_bytes: 0,
            remote: SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9),
            outbound_control: Some(OutboundControlStream {
                local_ssrc: 0x1020_3040,
                sender: SrtcpSender::new(SrtpSessionKeys {
                    encryption_key: [0; 32],
                    salt: [0; 14],
                    authentication_key: [0; 20],
                }),
            }),
            outbound_rtp: None,
            inbound_crypto: None,
            reception_report,
            pending_picture_loss_media_ssrc: Some(REMOTE_SSRC),
        };

        assert_eq!(
            bound
                .reception_report
                .report_snapshot(now)
                .unwrap()
                .report
                .fraction_lost,
            85
        );
        assert!(send_control_report(&mut bound, now).is_err());
        assert_eq!(
            bound.pending_picture_loss_media_ssrc,
            Some(REMOTE_SSRC),
            "失败发送必须保留待发送的 Apple PLI"
        );
        assert_eq!(
            bound
                .reception_report
                .report_snapshot(now)
                .unwrap()
                .report
                .fraction_lost,
            85,
            "失败发送不得提交 interval prior"
        );

        let receiving = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        bound.remote = receiving.local_addr().unwrap();
        send_control_report(&mut bound, now).unwrap();
        assert_eq!(
            bound.pending_picture_loss_media_ssrc, None,
            "成功发送后必须清除待发送的 Apple PLI"
        );
        assert_eq!(
            bound
                .reception_report
                .report_snapshot(now)
                .unwrap()
                .report
                .fraction_lost,
            0,
            "成功发送后必须提交 interval prior"
        );
    }

    #[test]
    fn udp_poll_round_preserves_per_role_fairness_and_rotates_start() {
        let mut loopback = ThreeRoleLoopback::new();
        loopback.send_rtp(MediaRole::Audio, 1, b"audio-1");
        loopback.send_rtp(MediaRole::Audio, 2, b"audio-2");
        loopback.send_rtp(MediaRole::Audio, 3, b"audio-3");
        loopback.send_rtp(MediaRole::VideoStream1, 1, b"video-1");
        loopback.send_rtp(MediaRole::VideoStream2, 1, b"video-2");

        let mut accepted = Vec::new();
        let first = loopback
            .transport
            .drain_receive_round_with_budget(ThreeRoleLoopback::GENERATION, 2, |role, _| {
                accepted.push(role);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            first.role_order,
            vec![
                MediaRole::Audio,
                MediaRole::VideoStream1,
                MediaRole::VideoStream2
            ]
        );
        assert_eq!(first.per_role(MediaRole::Audio).processed, 2);
        assert_eq!(first.per_role(MediaRole::Audio).accepted, 2);
        assert_eq!(first.per_role(MediaRole::VideoStream1).accepted, 1);
        assert_eq!(first.per_role(MediaRole::VideoStream2).accepted, 1);
        assert_eq!(first.accepted_total, 4);
        assert_eq!(first.discarded_total, 0);
        assert_eq!(
            accepted,
            vec![
                MediaRole::Audio,
                MediaRole::Audio,
                MediaRole::VideoStream1,
                MediaRole::VideoStream2
            ]
        );

        let second = loopback
            .transport
            .drain_receive_round_with_budget(ThreeRoleLoopback::GENERATION, 2, |_, _| Ok(()))
            .unwrap();
        assert_eq!(
            second.role_order,
            vec![
                MediaRole::VideoStream1,
                MediaRole::VideoStream2,
                MediaRole::Audio
            ]
        );
        assert_eq!(second.per_role(MediaRole::Audio).accepted, 1);

        let third = loopback
            .transport
            .drain_receive_round_with_budget(ThreeRoleLoopback::GENERATION, 2, |_, _| Ok(()))
            .unwrap();
        assert_eq!(
            third.role_order,
            vec![
                MediaRole::VideoStream2,
                MediaRole::Audio,
                MediaRole::VideoStream1
            ]
        );
    }

    #[test]
    fn udp_poll_round_counts_discards_and_empty_only_stops_its_role() {
        let mut loopback = ThreeRoleLoopback::new();
        loopback.send_empty_datagram(MediaRole::Audio);
        loopback.send_empty_datagram(MediaRole::Audio);
        loopback.send_rtp(MediaRole::Audio, 1, b"behind-discard-budget");
        loopback.send_rtp(MediaRole::VideoStream2, 1, b"video-after-empty-role");

        let mut accepted = Vec::new();
        let first = loopback
            .transport
            .drain_receive_round_with_budget(ThreeRoleLoopback::GENERATION, 2, |role, _| {
                accepted.push(role);
                Ok(())
            })
            .unwrap();
        assert_eq!(first.per_role(MediaRole::Audio).processed, 2);
        assert_eq!(first.per_role(MediaRole::Audio).accepted, 0);
        assert_eq!(first.per_role(MediaRole::Audio).discarded, 2);
        assert_eq!(first.per_role(MediaRole::VideoStream1).processed, 0);
        assert_eq!(first.per_role(MediaRole::VideoStream2).accepted, 1);
        assert_eq!(first.accepted_total, 1);
        assert_eq!(first.discarded_total, 2);
        assert_eq!(accepted, vec![MediaRole::VideoStream2]);

        let second = loopback
            .transport
            .drain_receive_round_with_budget(ThreeRoleLoopback::GENERATION, 2, |role, _| {
                accepted.push(role);
                Ok(())
            })
            .unwrap();
        assert_eq!(second.per_role(MediaRole::Audio).accepted, 1);
        assert_eq!(accepted.last(), Some(&MediaRole::Audio));
    }

    #[test]
    fn udp_poll_round_propagates_contract_and_handler_errors() {
        let mut loopback = ThreeRoleLoopback::new();
        assert!(loopback
            .transport
            .drain_receive_round_with_budget(ThreeRoleLoopback::GENERATION + 1, 2, |_, _| Ok(()))
            .is_err());

        loopback.send_rtp(MediaRole::VideoStream1, 1, b"handler-error");
        let error = loopback
            .transport
            .drain_receive_round_with_budget(ThreeRoleLoopback::GENERATION, 2, |_, _| {
                anyhow::bail!("测试 handler 失败")
            })
            .unwrap_err();
        assert!(format!("{error:#}").contains("测试 handler 失败"));
    }

    #[test]
    fn udp_transport_prebound_loopback_preserves_role_and_generation() {
        let remote = crate::bind_test_udp_loopback();
        remote
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let remote_port = remote.local_addr().unwrap().port();
        let announcement =
            parse_media_stream_port_announcement(&announcement_fixture(remote_port, 1)).unwrap();

        let mut transport = MediaTransport::new(7, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(transport.phase(), MediaTransportPhase::Idle);
        transport.set_audio_flow(AudioMediaFlow::PcToMac).unwrap();
        transport.accept_port_announcement(7, announcement).unwrap();
        transport
            .bind_test_loopback_sockets(7, &[(MediaRole::Audio, &remote)])
            .unwrap();
        let local = transport.local_addr(7, MediaRole::Audio).unwrap();
        assert_ne!(local.port(), remote_port);
        let configuration = transport.prepare_configuration(7).unwrap();
        assert_eq!(configuration[0], 0x1c);
        let incoming_audio_material = transport
            .configuration
            .as_ref()
            .unwrap()
            .audio
            .server_to_viewer
            .clone();
        let outgoing_audio_material = transport
            .configuration
            .as_ref()
            .unwrap()
            .audio
            .viewer_to_server
            .clone();
        transport.mark_configuration_sent(7).unwrap();
        transport.accept_answer(7, answer_fixture()).unwrap();
        let local_audio_ssrc = transport
            .configuration
            .as_ref()
            .unwrap()
            .audio
            .offer
            .local_ssrc;
        transport.activate(7).unwrap();

        let mut initial_report = [0u8; 128];
        let (initial_count, initial_source) = remote.recv_from(&mut initial_report).unwrap();
        assert_eq!(initial_source, local);
        assert!(initial_count > 12);
        assert_eq!(&initial_report[..2], &[0x80, 201]);
        assert_eq!(&initial_report[2..4], &1u16.to_be_bytes());
        assert_eq!(&initial_report[4..8], &local_audio_ssrc.to_be_bytes());
        const SRTCP_INDEX_LEN: usize = 4;
        const SRTCP_AUTHENTICATION_TAG_LEN: usize = 10;
        let index_start = initial_count - SRTCP_AUTHENTICATION_TAG_LEN - SRTCP_INDEX_LEN;
        assert_eq!(
            &initial_report[index_start..index_start + SRTCP_INDEX_LEN],
            &0x8000_0001u32.to_be_bytes()
        );

        let first_outbound = transport
            .send_audio_access_unit(7, b"encoded-audio")
            .unwrap();
        let (outbound_count, outbound_source) = remote.recv_from(&mut initial_report).unwrap();
        assert_eq!(outbound_source, local);
        let mut outbound_receiver = SrtpReceiver::new(derive_session_keys(
            &outgoing_audio_material,
            SrtpPacketKind::Rtp,
        ));
        let first_plaintext = outbound_receiver
            .open(&initial_report[..outbound_count])
            .unwrap()
            .expect("首个出站音频包不应被重放窗口丢弃");
        let first_packet = parse_rtp_packet(&first_plaintext).unwrap();
        assert!(first_packet.header.marker);
        assert_eq!(first_packet.header.payload_type, ARD_AUDIO_RTP_PAYLOAD_TYPE);
        assert_eq!(first_packet.header.sequence, first_outbound.sequence);
        assert_eq!(first_packet.header.timestamp, first_outbound.timestamp);
        assert_eq!(first_packet.header.ssrc, local_audio_ssrc);
        assert_eq!(first_packet.payload, b"encoded-audio");

        let second_outbound = transport
            .send_audio_access_unit(7, b"second-audio")
            .unwrap();
        let (outbound_count, _) = remote.recv_from(&mut initial_report).unwrap();
        let second_plaintext = outbound_receiver
            .open(&initial_report[..outbound_count])
            .unwrap()
            .expect("第二个出站音频包不应被重放窗口丢弃");
        let second_packet = parse_rtp_packet(&second_plaintext).unwrap();
        assert!(!second_packet.header.marker);
        assert_eq!(
            second_packet.header.sequence,
            first_packet.header.sequence.wrapping_add(1)
        );
        assert_eq!(
            second_packet.header.timestamp,
            first_packet
                .header
                .timestamp
                .wrapping_add(ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT as u32)
        );
        assert_eq!(second_packet.header.sequence, second_outbound.sequence);
        assert_eq!(second_packet.header.timestamp, second_outbound.timestamp);
        assert_eq!(second_packet.payload, b"second-audio");

        assert_eq!(
            transport
                .service_control_reports_at(7, Instant::now() + Duration::from_secs(2))
                .unwrap(),
            1
        );
        let (retry_count, retry_source) = remote.recv_from(&mut initial_report).unwrap();
        assert_eq!(retry_source, local);
        let retry_index_start = retry_count - SRTCP_AUTHENTICATION_TAG_LEN - SRTCP_INDEX_LEN;
        assert_eq!(
            &initial_report[retry_index_start..retry_index_start + SRTCP_INDEX_LEN],
            &0x8000_0002u32.to_be_bytes()
        );

        transport
            .send_opaque(7, MediaRole::Audio, b"probe")
            .unwrap();
        let mut received = [0u8; 16];
        let (count, source) = remote.recv_from(&mut received).unwrap();
        assert_eq!(&received[..count], b"probe");
        assert_eq!(source, local);

        const RTP_VERSION_AND_FLAGS: u8 = 2 << 6;
        const AUDIO_PAYLOAD_TYPE: u8 = 101;
        const REMOTE_SEQUENCE: u16 = 1;
        const REMOTE_TIMESTAMP: u32 = 960;
        const REMOTE_SSRC: u32 = 0x5566_7788;
        let mut plaintext = vec![RTP_VERSION_AND_FLAGS, AUDIO_PAYLOAD_TYPE];
        plaintext.extend_from_slice(&REMOTE_SEQUENCE.to_be_bytes());
        plaintext.extend_from_slice(&REMOTE_TIMESTAMP.to_be_bytes());
        plaintext.extend_from_slice(&REMOTE_SSRC.to_be_bytes());
        plaintext.extend_from_slice(b"reply");
        let incoming_keys = derive_session_keys(&incoming_audio_material, SrtpPacketKind::Rtp);
        let protected = protect_rtp_packet(&plaintext, &incoming_keys, 0).unwrap();
        remote.send_to(&protected, local).unwrap();
        let MediaReceiveOutcome::Accepted(received) = transport
            .poll_test_receive_decrypted(7, MediaRole::Audio)
            .unwrap()
        else {
            panic!("authenticated RTP must be accepted");
        };
        assert_eq!(received.datagram, MediaDatagram::Rtp(plaintext));
        remote.send_to(&protected, local).unwrap();
        assert_eq!(
            transport
                .poll_test_receive_decrypted(7, MediaRole::Audio)
                .unwrap(),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::ReplayOrTooOld),
            "已认证的重复 SRTP 包应被丢弃，而不是终止媒体会话"
        );
        assert_eq!(
            transport.try_recv_decrypted(7, MediaRole::Audio).unwrap(),
            MediaReceiveOutcome::Empty
        );
        assert!(transport
            .send_opaque(8, MediaRole::Audio, b"stale")
            .is_err());
    }

    #[test]
    fn udp_transport_requests_high_rate_receive_capacity_and_reports_effective_value() {
        const FOUR_MEBIBYTES: usize = 4 * 1024 * 1024;

        let comparison = crate::bind_test_udp_loopback();
        let default_capacity = socket2::SockRef::from(&comparison)
            .recv_buffer_size()
            .unwrap();
        let remote = crate::bind_test_udp_loopback();
        let remote_port = remote.local_addr().unwrap().port();
        let announcement =
            parse_media_stream_port_announcement(&announcement_fixture(remote_port, 1)).unwrap();
        let mut transport = MediaTransport::new(7, IpAddr::V4(Ipv4Addr::LOCALHOST));
        transport.accept_port_announcement(7, announcement).unwrap();

        transport
            .bind_test_loopback_sockets(7, &[(MediaRole::Audio, &remote)])
            .unwrap();

        let capacities = transport.receive_buffer_capacities(7).unwrap();
        assert_eq!(capacities.len(), 1);
        let capacity = capacities[0];
        assert_eq!(capacity.role, MediaRole::Audio);
        assert!(capacity.requested_bytes >= FOUR_MEBIBYTES);
        assert!(capacity.actual_bytes > 0);
        let bound = transport.socket(7, MediaRole::Audio).unwrap();
        assert_eq!(
            socket2::SockRef::from(&bound.socket)
                .recv_buffer_size()
                .unwrap(),
            capacity.actual_bytes
        );
        if default_capacity < capacity.requested_bytes {
            assert!(
                capacity.actual_bytes > default_capacity,
                "当平台默认容量低于请求值时，媒体 socket 必须实际扩大接收缓冲区"
            );
        }
    }

    #[test]
    fn udp_transport_discards_untrusted_datagrams_and_accepts_the_next_valid_packet() {
        const BAD_TAG_SEQUENCE: u16 = 10;
        const ACCEPTED_SEQUENCE: u16 = 11;
        const REMOTE_TIMESTAMP: u32 = 960;
        const REMOTE_SSRC: u32 = 0x5566_7788;

        let remote = crate::bind_test_udp_loopback();
        remote
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let remote_port = remote.local_addr().unwrap().port();
        let announcement =
            parse_media_stream_port_announcement(&announcement_fixture(remote_port, 1)).unwrap();
        let mut transport = MediaTransport::new(7, IpAddr::V4(Ipv4Addr::LOCALHOST));
        transport.accept_port_announcement(7, announcement).unwrap();
        transport
            .bind_test_loopback_sockets(7, &[(MediaRole::Audio, &remote)])
            .unwrap();
        let local = transport.local_addr(7, MediaRole::Audio).unwrap();
        transport.prepare_configuration(7).unwrap();
        let incoming_audio_material = transport
            .configuration
            .as_ref()
            .unwrap()
            .audio
            .server_to_viewer
            .clone();
        transport.mark_configuration_sent(7).unwrap();
        transport.accept_answer(7, answer_fixture()).unwrap();
        transport.activate(7).unwrap();

        let mut initial_report = [0u8; 128];
        remote.recv_from(&mut initial_report).unwrap();

        let attacker = crate::bind_test_udp_loopback();
        attacker.send_to(b"wrong-source", local).unwrap();
        assert_eq!(
            transport
                .poll_test_receive_decrypted(7, MediaRole::Audio)
                .unwrap(),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::UnexpectedSource)
        );

        remote.send_to(&[], local).unwrap();
        assert_eq!(
            transport
                .poll_test_receive_decrypted(7, MediaRole::Audio)
                .unwrap(),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::EmptyDatagram)
        );

        remote.send_to(&[0x80], local).unwrap();
        assert_eq!(
            transport
                .poll_test_receive_decrypted(7, MediaRole::Audio)
                .unwrap(),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::TruncatedHeader)
        );

        remote.send_to(&[0x40, 100], local).unwrap();
        assert_eq!(
            transport
                .poll_test_receive_decrypted(7, MediaRole::Audio)
                .unwrap(),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::MalformedPacket)
        );

        let rtp_keys = derive_session_keys(&incoming_audio_material, SrtpPacketKind::Rtp);
        let rtp_packet = |sequence: u16, payload: &[u8]| {
            let mut plaintext = vec![2 << 6, ARD_AUDIO_RTP_PAYLOAD_TYPE];
            plaintext.extend_from_slice(&sequence.to_be_bytes());
            plaintext.extend_from_slice(&REMOTE_TIMESTAMP.to_be_bytes());
            plaintext.extend_from_slice(&REMOTE_SSRC.to_be_bytes());
            plaintext.extend_from_slice(payload);
            plaintext
        };
        let bad_tag_plaintext = rtp_packet(BAD_TAG_SEQUENCE, b"bad-tag");
        let mut bad_tag = protect_rtp_packet(&bad_tag_plaintext, &rtp_keys, 0).unwrap();
        *bad_tag.last_mut().unwrap() ^= 1;
        remote.send_to(&bad_tag, local).unwrap();
        assert_eq!(
            transport
                .poll_test_receive_decrypted(7, MediaRole::Audio)
                .unwrap(),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::AuthenticationFailed)
        );

        let plaintext = rtp_packet(ACCEPTED_SEQUENCE, b"reply");
        let protected = protect_rtp_packet(&plaintext, &rtp_keys, 0).unwrap();
        remote.send_to(&protected, local).unwrap();
        let MediaReceiveOutcome::Accepted(received) = transport
            .poll_test_receive_decrypted(7, MediaRole::Audio)
            .unwrap()
        else {
            panic!("authenticated RTP must be accepted");
        };
        assert_eq!(received.datagram, MediaDatagram::Rtp(plaintext));
        remote.send_to(&protected, local).unwrap();
        assert_eq!(
            transport
                .poll_test_receive_decrypted(7, MediaRole::Audio)
                .unwrap(),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::ReplayOrTooOld)
        );

        let rtcp_keys = derive_session_keys(&incoming_audio_material, SrtpPacketKind::Rtcp);
        let mut srtcp_sender = SrtcpSender::new(rtcp_keys);
        let rtcp_plaintext = build_compound_rtcp_receiver_report(REMOTE_SSRC);
        let protected_rtcp = srtcp_sender.protect(&rtcp_plaintext).unwrap();
        remote.send_to(&protected_rtcp, local).unwrap();
        let MediaReceiveOutcome::Accepted(received) = transport
            .poll_test_receive_decrypted(7, MediaRole::Audio)
            .unwrap()
        else {
            panic!("authenticated RTCP must be accepted");
        };
        assert_eq!(received.datagram, MediaDatagram::Rtcp(rtcp_plaintext));
        remote.send_to(&protected_rtcp, local).unwrap();
        assert_eq!(
            transport
                .poll_test_receive_decrypted(7, MediaRole::Audio)
                .unwrap(),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::ReplayOrTooOld)
        );

        assert_eq!(
            transport.try_recv_decrypted(7, MediaRole::Audio).unwrap(),
            MediaReceiveOutcome::Empty
        );
        assert_eq!(
            transport.discard_counters(),
            MediaDiscardCounters {
                unexpected_source: 1,
                empty_datagram: 1,
                truncated_header: 1,
                malformed_packet: 1,
                authentication_failed: 1,
                replay_or_too_old: 2,
            }
        );
        assert!(transport.try_recv_decrypted(8, MediaRole::Audio).is_err());
        assert_eq!(transport.phase(), MediaTransportPhase::Active);
    }

    #[test]
    fn udp_transport_discard_counters_saturate() {
        let mut transport = MediaTransport::new(1, IpAddr::V4(Ipv4Addr::LOCALHOST));
        transport.discard_counters.replay_or_too_old = u64::MAX;

        assert_eq!(
            transport.record_discard(MediaRole::Audio, MediaDiscardReason::ReplayOrTooOld, None,),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::ReplayOrTooOld)
        );
        assert_eq!(transport.discard_counters.replay_or_too_old, u64::MAX);
    }

    #[test]
    fn generation_reset_clears_nonzero_discard_counters() {
        let mut transport = MediaTransport::new(1, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(
            transport.record_discard(
                MediaRole::Audio,
                MediaDiscardReason::AuthenticationFailed,
                None,
            ),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::AuthenticationFailed)
        );
        assert_ne!(
            transport.discard_counters(),
            MediaDiscardCounters::default()
        );

        transport.reset_generation(2).unwrap();

        assert_eq!(transport.generation(), 2);
        assert_eq!(
            transport.discard_counters(),
            MediaDiscardCounters::default()
        );
    }

    #[test]
    fn announced_roles_cannot_share_a_remote_port() {
        let mut announcement =
            parse_media_stream_port_announcement(&announcement_fixture(9_999, 1)).unwrap();
        announcement.video_stream_1 = announcement.audio;
        let mut transport = MediaTransport::new(1, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(transport.accept_port_announcement(1, announcement).is_err());
        assert_eq!(transport.phase(), MediaTransportPhase::Failed);
    }

    #[test]
    fn outbound_audio_rtp_advances_rollover_counter_at_sequence_wrap() {
        let keys = SrtpSessionKeys {
            encryption_key: [0x11; 32],
            salt: [0x22; 14],
            authentication_key: [0x33; 20],
        };
        let mut sender = OutboundRtpStream::with_initial(
            0x1020_3040,
            keys.clone(),
            u16::MAX,
            u32::MAX - (ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT as u32 - 1),
        );
        let mut receiver = SrtpReceiver::new(keys);

        let (before_wrap, first) = sender.protect_audio_access_unit(b"first").unwrap();
        let (after_wrap, second) = sender.protect_audio_access_unit(b"second").unwrap();

        let first_plaintext = receiver
            .open(&before_wrap)
            .unwrap()
            .expect("回绕前数据报不应被丢弃");
        let second_plaintext = receiver
            .open(&after_wrap)
            .unwrap()
            .expect("回绕后数据报不应被丢弃");
        let first_packet = parse_rtp_packet(&first_plaintext).unwrap();
        let second_packet = parse_rtp_packet(&second_plaintext).unwrap();
        assert_eq!(first.sequence, u16::MAX);
        assert_eq!(second.sequence, 0);
        assert_eq!(first.timestamp, u32::MAX - 479);
        assert_eq!(second.timestamp, 0);
        assert_eq!(first_packet.payload, b"first");
        assert_eq!(second_packet.payload, b"second");
    }

    #[test]
    fn outbound_audio_evidence_exposes_monotonic_extended_sequences_across_rollover() {
        let keys = SrtpSessionKeys {
            encryption_key: [0x11; 32],
            salt: [0x22; 14],
            authentication_key: [0x33; 20],
        };
        let mut sender = OutboundRtpStream::with_initial(0x1020_3040, keys, u16::MAX, 0);

        let (_, first) = sender.protect_audio_access_unit(b"first").unwrap();
        let (_, last) = sender.protect_audio_access_unit(b"last").unwrap();

        assert_eq!(first.sequence, u16::MAX);
        assert_eq!(first.extended_sequence, 0x0000_ffff);
        assert_eq!(last.sequence, 0);
        assert_eq!(last.extended_sequence, 0x0001_0000);
        assert!(last.extended_sequence > first.extended_sequence);
    }

    #[test]
    fn outbound_audio_evidence_confirms_only_the_exact_sent_range() {
        let mut loopback = PcToMacAudioLoopback::new();
        assert_eq!(
            loopback.transport.audio_reception_evidence(),
            AudioReceptionEvidence::NotObserved
        );

        let first = loopback
            .transport
            .send_audio_access_unit(PcToMacAudioLoopback::GENERATION, b"first")
            .unwrap();
        let last = loopback
            .transport
            .send_audio_access_unit(PcToMacAudioLoopback::GENERATION, b"last")
            .unwrap();
        let sent = loopback.transport.outbound_audio_sent_range().unwrap();
        assert_eq!(sent.ssrc, first.ssrc);
        assert_eq!(sent.first_extended_sequence, first.extended_sequence);
        assert_eq!(sent.last_extended_sequence, last.extended_sequence);
        assert_eq!(sent.packets_sent, 2);

        loopback.send_rtcp(&receiver_report(
            first.ssrc.wrapping_add(1),
            first.extended_sequence,
            0,
        ));
        assert!(matches!(
            loopback.receive_rtcp(),
            MediaReceiveOutcome::Accepted(_)
        ));
        assert_eq!(
            loopback.transport.audio_reception_evidence(),
            AudioReceptionEvidence::NotObserved,
            "其他 SSRC 的报告不能确认出站音频"
        );

        loopback.send_rtcp(&receiver_report(first.ssrc, first.extended_sequence - 1, 0));
        assert!(matches!(
            loopback.receive_rtcp(),
            MediaReceiveOutcome::Accepted(_)
        ));
        assert_eq!(
            loopback.transport.audio_reception_evidence(),
            AudioReceptionEvidence::MatchingReport {
                extended_highest_sequence: first.extended_sequence - 1,
                cumulative_packets_lost: 0,
            }
        );

        loopback.send_rtcp(&receiver_report(first.ssrc, last.extended_sequence + 1, 0));
        assert!(matches!(
            loopback.receive_rtcp(),
            MediaReceiveOutcome::Accepted(_)
        ));
        assert_eq!(
            loopback.transport.audio_reception_evidence(),
            AudioReceptionEvidence::MatchingReport {
                extended_highest_sequence: last.extended_sequence + 1,
                cumulative_packets_lost: 0,
            }
        );

        loopback.send_rtcp(&receiver_report(
            first.ssrc,
            last.extended_sequence,
            sent.packets_sent as i32,
        ));
        assert!(matches!(
            loopback.receive_rtcp(),
            MediaReceiveOutcome::Accepted(_)
        ));
        assert_eq!(
            loopback.transport.audio_reception_evidence(),
            AudioReceptionEvidence::MatchingReport {
                extended_highest_sequence: last.extended_sequence,
                cumulative_packets_lost: sent.packets_sent as i32,
            }
        );

        loopback.send_rtcp(&receiver_report(first.ssrc, last.extended_sequence, 1));
        assert!(matches!(
            loopback.receive_rtcp(),
            MediaReceiveOutcome::Accepted(_)
        ));
        let confirmed = AudioReceptionEvidence::Confirmed {
            extended_highest_sequence: last.extended_sequence,
            cumulative_packets_lost: 1,
        };
        assert_eq!(loopback.transport.audio_reception_evidence(), confirmed);

        loopback.send_rtcp(&receiver_report(first.ssrc, last.extended_sequence, 1));
        assert!(matches!(
            loopback.receive_rtcp(),
            MediaReceiveOutcome::Accepted(_)
        ));
        assert_eq!(
            loopback.transport.audio_reception_evidence(),
            confirmed,
            "重复报告不能替换已锁存的确认"
        );
    }

    #[test]
    fn outbound_audio_evidence_ignores_malformed_and_unauthenticated_srtcp() {
        let mut loopback = PcToMacAudioLoopback::new();
        let sent = loopback
            .transport
            .send_audio_access_unit(PcToMacAudioLoopback::GENERATION, b"audio")
            .unwrap();

        loopback.send_rtcp(&[0x81, 201, 0, 1, 0, 0, 0, 0]);
        assert_eq!(
            loopback.receive_rtcp(),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::MalformedPacket)
        );

        let valid = receiver_report(sent.ssrc, sent.extended_sequence, 0);
        let mut unauthenticated = loopback.rtcp_sender.protect(&valid).unwrap();
        *unauthenticated.last_mut().unwrap() ^= 1;
        loopback
            .remote
            .send_to(&unauthenticated, loopback.local)
            .unwrap();
        assert_eq!(
            loopback.receive_rtcp(),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::AuthenticationFailed)
        );
        assert_eq!(
            loopback.transport.audio_reception_evidence(),
            AudioReceptionEvidence::NotObserved
        );
    }

    #[test]
    fn outbound_audio_evidence_confirms_matching_report_on_fresh_loopback() {
        let mut loopback = PcToMacAudioLoopback::new();
        let sent = loopback
            .transport
            .send_audio_access_unit(PcToMacAudioLoopback::GENERATION, b"audio")
            .unwrap();
        let report = receiver_report(sent.ssrc, sent.extended_sequence, 0);

        loopback.send_rtcp(&report);
        assert!(matches!(
            loopback.receive_rtcp(),
            MediaReceiveOutcome::Accepted(_)
        ));
        assert_eq!(
            loopback.transport.audio_reception_evidence(),
            AudioReceptionEvidence::Confirmed {
                extended_highest_sequence: sent.extended_sequence,
                cumulative_packets_lost: 0,
            }
        );
    }

    #[test]
    fn outbound_audio_evidence_replayed_matching_report_does_not_confirm_after_latch_clear() {
        let mut loopback = PcToMacAudioLoopback::new();
        let sent = loopback
            .transport
            .send_audio_access_unit(PcToMacAudioLoopback::GENERATION, b"audio")
            .unwrap();
        let report = receiver_report(sent.ssrc, sent.extended_sequence, 0);
        let replayed = loopback.rtcp_sender.protect(&report).unwrap();

        loopback.remote.send_to(&replayed, loopback.local).unwrap();
        assert!(matches!(
            loopback.receive_rtcp(),
            MediaReceiveOutcome::Accepted(_)
        ));
        assert!(matches!(
            loopback.transport.audio_reception_evidence(),
            AudioReceptionEvidence::Confirmed { .. }
        ));
        loopback.transport.outbound_audio_evidence.evidence = AudioReceptionEvidence::NotObserved;

        loopback.remote.send_to(&replayed, loopback.local).unwrap();
        assert_eq!(
            loopback.receive_rtcp(),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::ReplayOrTooOld)
        );
        assert_eq!(
            loopback.transport.audio_reception_evidence(),
            AudioReceptionEvidence::NotObserved
        );
    }

    #[test]
    fn outbound_audio_evidence_too_old_matching_report_does_not_confirm_after_latch_clear() {
        let mut loopback = PcToMacAudioLoopback::new();
        let sent = loopback
            .transport
            .send_audio_access_unit(PcToMacAudioLoopback::GENERATION, b"audio")
            .unwrap();
        let report = receiver_report(sent.ssrc, sent.extended_sequence, 0);
        let too_old = loopback.rtcp_sender.protect(&report).unwrap();

        loopback.remote.send_to(&too_old, loopback.local).unwrap();
        assert!(matches!(
            loopback.receive_rtcp(),
            MediaReceiveOutcome::Accepted(_)
        ));
        assert!(matches!(
            loopback.transport.audio_reception_evidence(),
            AudioReceptionEvidence::Confirmed { .. }
        ));

        for _ in 0..64 {
            let packet = loopback.rtcp_sender.protect(&report).unwrap();
            loopback.remote.send_to(&packet, loopback.local).unwrap();
            assert!(matches!(
                loopback.receive_rtcp(),
                MediaReceiveOutcome::Accepted(_)
            ));
        }
        loopback.transport.outbound_audio_evidence.evidence = AudioReceptionEvidence::NotObserved;
        loopback.remote.send_to(&too_old, loopback.local).unwrap();
        assert_eq!(
            loopback.receive_rtcp(),
            MediaReceiveOutcome::Discarded(MediaDiscardReason::ReplayOrTooOld)
        );
        assert_eq!(
            loopback.transport.audio_reception_evidence(),
            AudioReceptionEvidence::NotObserved
        );
    }
}
