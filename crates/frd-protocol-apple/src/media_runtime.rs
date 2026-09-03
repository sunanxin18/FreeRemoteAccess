use std::collections::VecDeque;
use std::net::IpAddr;
#[cfg(all(debug_assertions, not(test)))]
use std::sync::{
    atomic::{fence, AtomicBool, AtomicU64, AtomicU8, Ordering},
    Arc, Weak,
};
#[cfg(all(debug_assertions, not(test)))]
use std::time::Duration;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use frd_core::SessionId;
use frd_media_api::{MediaFrame, MediaStageDiagnostic, MediaStageTrace, VideoStreamIdentity};
use frd_protocol_api::{AudioState, ProtocolError, ProtocolRuntime, SessionEvent};

use crate::audio_codec::{
    ArdAudioReceiver, AudioReceiveOutcome, DecodedAudioPacket, ARD_AUDIO_CHANNEL_COUNT,
    ARD_AUDIO_SAMPLE_RATE_HZ,
};
use crate::connection::AppleWriterHandle;
use crate::hevc_access_unit::{
    HevcAccessUnitAssembler, HevcAccessUnitError, HevcAccessUnitLimits, HevcRtpPacket,
};
use crate::hevc_rtp::HevcRtpError;
use crate::high_performance_video::AppleHighPerformanceVideoAdapter;
use crate::media_negotiation::{AudioMediaFlow, MediaStreamAnswer};
use crate::media_protocol::MediaStreamPortAnnouncement;
use crate::media_transport::{MediaDatagram, MediaRole, MediaTransport, MediaTransportPhase};
use crate::srtp::parse_rtp_packet;

const MAX_PCM_PUBLICATION_SAMPLES: usize =
    ARD_AUDIO_SAMPLE_RATE_HZ as usize * ARD_AUDIO_CHANNEL_COUNT * 2;
const MAX_RETIRED_VIDEO_SSRCS: usize = 8;

pub(crate) fn publish_decoded_audio(
    runtime: &mut ProtocolRuntime,
    decoded: DecodedAudioPacket,
) -> Result<(), ProtocolError> {
    if decoded.pcm.len() > MAX_PCM_PUBLICATION_SAMPLES {
        return Err(ProtocolError::adapter(
            frd_core::ProtocolId::apple_hpss_mvs(),
            "apple_pcm_publication_too_large",
        ));
    }
    runtime.try_publish_optional_media(MediaFrame::Pcm {
        sample_rate_hz: ARD_AUDIO_SAMPLE_RATE_HZ,
        channels: ARD_AUDIO_CHANNEL_COUNT as u8,
        samples: decoded.pcm.into_boxed_slice(),
    })
}

pub(crate) struct ViewerMediaState {
    pub(crate) transport: MediaTransport,
    audio_receiver: ArdAudioReceiver,
    pub(crate) authenticated_audio_packets: u64,
    pub(crate) late_audio_packets: u64,
    pub(crate) audio_resynchronizations: u64,
    pub(crate) non_silent_audio_access_units: u64,
    pub(crate) concealed_audio_access_units: u64,
    pub(crate) authenticated_video_packets: u64,
    #[cfg(any(debug_assertions, test))]
    active_service_ticks: u64,
    video_stream_1: VideoReceiveState,
    video_stream_2: VideoReceiveState,
    audio_degraded: bool,
    control_stage_trace: MediaStageTrace,
    #[cfg(all(debug_assertions, not(test)))]
    debug_watchdog: Arc<DebugMediaWatchdog>,
}

#[cfg(all(debug_assertions, not(test)))]
#[derive(Clone, Copy)]
#[repr(u8)]
pub(crate) enum DebugMediaStage {
    ControlEnter = 1,
    ControlExit,
    DrainEnter,
    DrainExit,
    AudioEnter,
    AudioExit,
    Video1Enter,
    Video1Exit,
    Video2Enter,
    Video2Exit,
    ReaderEnter,
    ReaderExit,
    TcpReadEnter,
    TcpReadExit,
    FrameHandleEnter,
    FrameHandleExit,
}

#[cfg(all(debug_assertions, not(test)))]
impl DebugMediaStage {
    fn name_from_u8(value: u8) -> Option<&'static str> {
        DEBUG_MEDIA_STAGE_NAMES
            .get(usize::from(value))
            .copied()
            .filter(|name| !name.is_empty())
    }
}

#[cfg(all(debug_assertions, not(test)))]
const DEBUG_MEDIA_STAGE_NAMES: [&str; 17] = [
    "",
    "control_enter",
    "control_exit",
    "drain_enter",
    "drain_exit",
    "audio_enter",
    "audio_exit",
    "video1_enter",
    "video1_exit",
    "video2_enter",
    "video2_exit",
    "reader_enter",
    "reader_exit",
    "tcp_read_enter",
    "tcp_read_exit",
    "frame_handle_enter",
    "frame_handle_exit",
];

#[cfg(all(debug_assertions, not(test)))]
struct DebugMediaWatchdog {
    tick: AtomicU64,
    stage: AtomicU8,
    snapshot_version: AtomicU64,
    stop: AtomicBool,
}

#[cfg(all(debug_assertions, not(test)))]
#[derive(Clone, Copy)]
struct DebugMediaSnapshot {
    version: u64,
    tick: u64,
    stage: u8,
}

#[cfg(all(debug_assertions, not(test)))]
impl DebugMediaWatchdog {
    fn spawn() -> Arc<Self> {
        let watchdog = Arc::new(Self {
            tick: AtomicU64::new(0),
            stage: AtomicU8::new(0),
            snapshot_version: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        });
        let weak: Weak<Self> = Arc::downgrade(&watchdog);
        let _ = std::thread::Builder::new()
            .name("frd-media-watchdog".to_owned())
            .spawn(move || run_debug_media_watchdog(weak));
        watchdog
    }

    fn update(&self, tick: u64, stage: DebugMediaStage) {
        self.write_snapshot(tick, stage as u8);
    }

    fn disarm(&self) {
        self.write_snapshot(0, 0);
    }

    fn write_snapshot(&self, tick: u64, stage: u8) {
        let previous = self.snapshot_version.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(previous & 1, 0, "media watchdog seqlock writer overlapped");
        self.tick.store(tick, Ordering::Relaxed);
        self.stage.store(stage, Ordering::Relaxed);
        let writing = self.snapshot_version.fetch_add(1, Ordering::Release);
        debug_assert_eq!(writing & 1, 1, "media watchdog seqlock writer lost parity");
    }

    fn snapshot(&self) -> Option<DebugMediaSnapshot> {
        let before = self.snapshot_version.load(Ordering::Acquire);
        if before == 0 || before & 1 != 0 {
            return None;
        }
        let tick = self.tick.load(Ordering::Relaxed);
        let stage = self.stage.load(Ordering::Relaxed);
        fence(Ordering::Acquire);
        let after = self.snapshot_version.load(Ordering::Acquire);
        (before == after && after & 1 == 0).then_some(DebugMediaSnapshot {
            version: after,
            tick,
            stage,
        })
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[cfg(all(debug_assertions, not(test)))]
fn run_debug_media_watchdog(weak: Weak<DebugMediaWatchdog>) {
    const POLL_INTERVAL: Duration = Duration::from_millis(250);
    const STALL_INTERVAL: Duration = Duration::from_secs(2);

    let mut last_version = 0;
    let mut unchanged_since = Instant::now();
    let mut reported = false;
    loop {
        std::thread::sleep(POLL_INTERVAL);
        let Some(watchdog) = weak.upgrade() else {
            break;
        };
        if watchdog.stop.load(Ordering::Acquire) {
            break;
        }
        let Some(snapshot) = watchdog.snapshot() else {
            continue;
        };
        if snapshot.version != last_version {
            last_version = snapshot.version;
            unchanged_since = Instant::now();
            reported = false;
            continue;
        }
        if !reported && unchanged_since.elapsed() >= STALL_INTERVAL {
            let Some(confirmed) = watchdog.snapshot() else {
                continue;
            };
            if confirmed.version != snapshot.version {
                continue;
            }
            if let Some(stage) = DebugMediaStage::name_from_u8(confirmed.stage) {
                eprintln!("[frd-media-stall] tick={} phase={stage}", confirmed.tick);
                reported = true;
            }
        }
    }
}

#[cfg(all(debug_assertions, not(test)))]
struct DebugMediaStageExit<'a> {
    watchdog: &'a DebugMediaWatchdog,
    tick: u64,
    stage: DebugMediaStage,
}

#[cfg(all(debug_assertions, not(test)))]
impl<'a> DebugMediaStageExit<'a> {
    fn enter(
        watchdog: &'a DebugMediaWatchdog,
        tick: u64,
        enter: DebugMediaStage,
        exit: DebugMediaStage,
    ) -> Self {
        watchdog.update(tick, enter);
        Self {
            watchdog,
            tick,
            stage: exit,
        }
    }
}

#[cfg(all(debug_assertions, not(test)))]
impl Drop for DebugMediaStageExit<'_> {
    fn drop(&mut self) {
        self.watchdog.update(self.tick, self.stage);
    }
}

struct VideoReceiveState {
    assembler: HevcAccessUnitAssembler,
    adapter: AppleHighPerformanceVideoAdapter,
    stage_trace: MediaStageTrace,
    pending_recovery_request: Option<VideoRecoveryRequest>,
    active_ssrc: Option<u32>,
    retired_ssrcs: VecDeque<u32>,
    awaiting_replacement_irap: bool,
    #[cfg(any(debug_assertions, test))]
    authenticated_rtp_packets: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VideoRecoveryRequest {
    PictureLoss { media_ssrc: u32 },
}

impl VideoReceiveState {
    fn new(identity: VideoStreamIdentity, generation: u64) -> Result<Self> {
        Ok(Self {
            assembler: HevcAccessUnitAssembler::new(generation, HevcAccessUnitLimits::default())
                .context("创建 Apple HP HEVC AU 组装器失败")?,
            adapter: AppleHighPerformanceVideoAdapter::new(identity, generation),
            stage_trace: MediaStageTrace::default(),
            pending_recovery_request: None,
            active_ssrc: None,
            retired_ssrcs: VecDeque::new(),
            awaiting_replacement_irap: false,
            #[cfg(any(debug_assertions, test))]
            authenticated_rtp_packets: 0,
        })
    }

    fn reset(&mut self, generation: u64) {
        self.assembler.reset(generation);
        self.adapter.reset(generation);
        self.stage_trace = MediaStageTrace::default();
        self.pending_recovery_request = None;
        self.active_ssrc = None;
        self.retired_ssrcs.clear();
        self.awaiting_replacement_irap = false;
        #[cfg(any(debug_assertions, test))]
        {
            self.authenticated_rtp_packets = 0;
        }
    }

    fn observe_authenticated_rtp(&mut self, generation: u64) {
        #[cfg(any(debug_assertions, test))]
        {
            self.authenticated_rtp_packets = self.authenticated_rtp_packets.saturating_add(1);
        }
        self.stage_trace
            .observe(MediaStageDiagnostic::AuthenticatedVideoRtp {
                generation,
                stream_id: self.adapter.stream_id(),
            });
    }

    fn accept_rtp(
        &mut self,
        runtime: &mut ProtocolRuntime,
        generation: u64,
        datagram: &[u8],
    ) -> Result<()> {
        let packet = parse_rtp_packet(datagram).context("解析 Apple HP 视频 RTP 数据报失败")?;
        match self.active_ssrc {
            Some(previous) if previous != packet.header.ssrc => {
                if self.retired_ssrcs.contains(&packet.header.ssrc) {
                    return Ok(());
                }
                // Apple HP 每个媒体 role 同时只发布一个活动视频 SSRC。同一 display
                // generation 内不允许退回旧 SSRC；达到有界历史上限后忽略新的未知
                // SSRC，等待下一次 generation reset，而不是放宽成多活动流。
                if self.retired_ssrcs.len() == MAX_RETIRED_VIDEO_SSRCS {
                    return Ok(());
                }
                self.retired_ssrcs.push_back(previous);
                self.assembler.reset(generation);
                self.adapter.reset(generation);
                self.pending_recovery_request = None;
                self.active_ssrc = Some(packet.header.ssrc);
                self.awaiting_replacement_irap = true;
            }
            None => self.active_ssrc = Some(packet.header.ssrc),
            Some(_) => {}
        }
        let access_units = match self.assembler.push(HevcRtpPacket {
            generation,
            ssrc: packet.header.ssrc,
            sequence: packet.header.sequence,
            timestamp: packet.header.timestamp,
            marker: packet.header.marker,
            payload: packet.payload,
        }) {
            Ok(access_units) => access_units,
            Err(HevcAccessUnitError::ReorderWindowExceeded { .. }) => {
                self.pending_recovery_request
                    .get_or_insert(VideoRecoveryRequest::PictureLoss {
                        media_ssrc: packet.header.ssrc,
                    });
                return Ok(());
            }
            Err(HevcAccessUnitError::MissingInitialParameterSets)
                if self.awaiting_replacement_irap =>
            {
                self.assembler.reset(generation);
                return Ok(());
            }
            Err(HevcAccessUnitError::Depacketize(HevcRtpError::FuContinuationWithoutStart))
                if self.awaiting_replacement_irap =>
            {
                self.assembler.reset(generation);
                return Ok(());
            }
            Err(error) => return Err(error).context("组装 Apple HP HEVC 访问单元失败"),
        };
        for access_unit in access_units {
            if self.awaiting_replacement_irap {
                if !access_unit.keyframe {
                    continue;
                }
                self.awaiting_replacement_irap = false;
            }
            self.adapter
                .publish_access_unit(runtime, access_unit)
                .context("发布 Apple HP HEVC 访问单元失败")?;
        }
        Ok(())
    }

    fn take_recovery_request(&mut self) -> Option<VideoRecoveryRequest> {
        self.pending_recovery_request.take()
    }
}

impl ViewerMediaState {
    #[cfg(test)]
    pub(crate) fn new(
        audio_flow: AudioMediaFlow,
        generation: u64,
        server_address: IpAddr,
    ) -> Result<Self> {
        Self::new_for_session(
            SessionId::allocate(),
            audio_flow,
            generation,
            server_address,
        )
    }

    pub(crate) fn new_for_session(
        session_id: SessionId,
        audio_flow: AudioMediaFlow,
        generation: u64,
        server_address: IpAddr,
    ) -> Result<Self> {
        Self::new_for_session_with_transport_factory(session_id, audio_flow, || {
            MediaTransport::new(generation, server_address)
        })
    }

    #[cfg(test)]
    fn new_with_transport_factory<F>(audio_flow: AudioMediaFlow, factory: F) -> Result<Self>
    where
        F: FnOnce() -> MediaTransport,
    {
        Self::new_for_session_with_transport_factory(SessionId::allocate(), audio_flow, factory)
    }

    fn new_for_session_with_transport_factory<F>(
        session_id: SessionId,
        audio_flow: AudioMediaFlow,
        factory: F,
    ) -> Result<Self>
    where
        F: FnOnce() -> MediaTransport,
    {
        validate_hpss_audio_flow(audio_flow)?;
        let mut transport = factory();
        let generation = transport.generation();
        transport.set_audio_flow(audio_flow)?;
        Ok(Self {
            transport,
            audio_receiver: ArdAudioReceiver::new().context("创建 Mac→PC AAC-ELD 接收器失败")?,
            authenticated_audio_packets: 0,
            late_audio_packets: 0,
            audio_resynchronizations: 0,
            non_silent_audio_access_units: 0,
            concealed_audio_access_units: 0,
            authenticated_video_packets: 0,
            #[cfg(any(debug_assertions, test))]
            active_service_ticks: 0,
            video_stream_1: VideoReceiveState::new(
                VideoStreamIdentity {
                    session_id,
                    stream_id: 1,
                },
                generation,
            )?,
            video_stream_2: VideoReceiveState::new(
                VideoStreamIdentity {
                    session_id,
                    stream_id: 2,
                },
                generation,
            )?,
            audio_degraded: false,
            control_stage_trace: MediaStageTrace::default(),
            #[cfg(all(debug_assertions, not(test)))]
            debug_watchdog: DebugMediaWatchdog::spawn(),
        })
    }

    pub(crate) fn reset_generation(&mut self, generation: u64) -> Result<()> {
        // 先完成可能失败的接收器准备，随后才提交 transport generation；其后的本地
        // 状态替换和诊断归零均不再失败，避免摘要跨 generation 混合。
        let next_audio_receiver =
            ArdAudioReceiver::new().context("重建 Mac→PC AAC-ELD 接收器失败")?;
        self.transport.reset_generation(generation)?;
        #[cfg(all(debug_assertions, not(test)))]
        self.debug_watchdog.disarm();
        self.audio_receiver = next_audio_receiver;
        self.video_stream_1.reset(generation);
        self.video_stream_2.reset(generation);
        self.authenticated_video_packets = 0;
        #[cfg(any(debug_assertions, test))]
        {
            self.active_service_ticks = 0;
        }
        self.audio_degraded = false;
        self.control_stage_trace = MediaStageTrace::default();
        Ok(())
    }

    pub(crate) fn generation(&self) -> u64 {
        self.transport.generation()
    }

    pub(crate) fn handle_port_announcement(
        &mut self,
        generation: u64,
        announcement: MediaStreamPortAnnouncement,
        bind_address: IpAddr,
        writer: &AppleWriterHandle,
    ) -> Result<()> {
        if self.transport.phase() != MediaTransportPhase::Idle {
            return Ok(());
        }
        self.transport
            .accept_port_announcement(generation, announcement)?;
        self.transport
            .bind_local_sockets(generation, bind_address)?;
        let configuration = self.transport.prepare_configuration(generation)?;
        writer.send_private_message(&configuration)?;
        self.transport.mark_configuration_sent(generation)?;
        self.control_stage_trace
            .observe(MediaStageDiagnostic::Message1ConfigurationWritten { generation });
        Ok(())
    }

    pub(crate) fn handle_answer(
        &mut self,
        generation: u64,
        answer: MediaStreamAnswer,
    ) -> Result<()> {
        self.transport.accept_answer(generation, answer)?;
        self.transport.activate(generation)?;
        self.control_stage_trace
            .observe(MediaStageDiagnostic::Message2TransportActive { generation });
        Ok(())
    }

    pub(crate) fn service_active(
        &mut self,
        runtime: &mut ProtocolRuntime,
        generation: u64,
        now: Instant,
    ) -> Result<usize> {
        if self.transport.phase() != MediaTransportPhase::Active {
            return Ok(0);
        }
        #[cfg(any(debug_assertions, test))]
        if let Some(tick) = self.debug_advance_active_service_tick() {
            eprintln!("{}", self.debug_service_checkpoint_summary(tick));
        }
        #[cfg(all(debug_assertions, not(test)))]
        let debug_tick = self.active_service_ticks;
        #[cfg(all(debug_assertions, not(test)))]
        self.debug_watchdog
            .update(debug_tick, DebugMediaStage::ControlEnter);
        let control_result = self.transport.service_control_reports_at(generation, now);
        #[cfg(all(debug_assertions, not(test)))]
        self.debug_watchdog
            .update(debug_tick, DebugMediaStage::ControlExit);
        control_result?;
        let Self {
            transport,
            audio_receiver,
            authenticated_audio_packets,
            late_audio_packets,
            audio_resynchronizations,
            non_silent_audio_access_units,
            concealed_audio_access_units,
            authenticated_video_packets,
            video_stream_1,
            video_stream_2,
            audio_degraded,
            #[cfg(any(debug_assertions, test))]
                active_service_ticks: _,
            control_stage_trace: _,
            #[cfg(all(debug_assertions, not(test)))]
            debug_watchdog,
        } = self;
        #[cfg(all(debug_assertions, not(test)))]
        debug_watchdog.update(debug_tick, DebugMediaStage::DrainEnter);
        let summary_result = transport.drain_receive_round(generation, |role, datagram| {
            accept_datagram(
                audio_receiver,
                authenticated_audio_packets,
                late_audio_packets,
                audio_resynchronizations,
                non_silent_audio_access_units,
                concealed_audio_access_units,
                authenticated_video_packets,
                video_stream_1,
                video_stream_2,
                audio_degraded,
                runtime,
                generation,
                role,
                datagram,
                #[cfg(all(debug_assertions, not(test)))]
                debug_watchdog.as_ref(),
                #[cfg(all(debug_assertions, not(test)))]
                debug_tick,
            )
        });
        #[cfg(all(debug_assertions, not(test)))]
        debug_watchdog.update(debug_tick, DebugMediaStage::DrainExit);
        let summary = summary_result?;
        for (role, request) in [
            (
                MediaRole::VideoStream1,
                video_stream_1.take_recovery_request(),
            ),
            (
                MediaRole::VideoStream2,
                video_stream_2.take_recovery_request(),
            ),
        ] {
            if let Some(VideoRecoveryRequest::PictureLoss { media_ssrc }) = request {
                transport.queue_picture_loss(generation, role, media_ssrc)?;
            }
        }
        Ok(summary.accepted_total)
    }

    pub(crate) fn close(&mut self, generation: u64) -> Result<()> {
        #[cfg(all(debug_assertions, not(test)))]
        self.debug_watchdog.stop();
        self.transport.close(generation)
    }

    #[cfg(any(debug_assertions, test))]
    pub(crate) fn debug_close_summary(&self) -> String {
        self.debug_summary_with_prefix("[frd-media-summary]")
    }

    #[cfg(any(debug_assertions, test))]
    fn debug_advance_active_service_tick(&mut self) -> Option<u64> {
        self.active_service_ticks = self.active_service_ticks.saturating_add(1);
        matches!(self.active_service_ticks, 64 | 128 | 256 | 512 | 2048)
            .then_some(self.active_service_ticks)
    }

    #[cfg(any(debug_assertions, test))]
    fn debug_service_checkpoint_summary(&self, tick: u64) -> String {
        self.debug_summary_with_prefix(&format!("[frd-media-checkpoint] tick={tick}"))
    }

    #[cfg(all(debug_assertions, not(test)))]
    pub(crate) fn debug_update_stage(&self, stage: DebugMediaStage) {
        if self.transport.phase() == MediaTransportPhase::Active {
            self.debug_watchdog.update(self.active_service_ticks, stage);
        }
    }

    #[cfg(any(debug_assertions, test))]
    fn debug_summary_with_prefix(&self, prefix: &str) -> String {
        let discards = self.transport.discard_counters();
        let stream_1_assembler = self.video_stream_1.assembler.diagnostics();
        let stream_2_assembler = self.video_stream_2.assembler.diagnostics();
        let stream_1_adapter = self.video_stream_1.adapter.diagnostics();
        let stream_2_adapter = self.video_stream_2.adapter.diagnostics();
        format!(
            "{} active_service_ticks={} authenticated_video_rtp_stream_1={} authenticated_video_rtp_stream_2={} discard_unexpected_source={} discard_empty_datagram={} discard_truncated_header={} discard_malformed_packet={} discard_authentication_failed={} discard_replay_or_too_old={} stream_1_complete_configuration_access_units={} stream_1_reorder_window_exceeded={} stream_1_recovery_marker_resyncs={} stream_1_waiting_for_recovery_irap_drops={} stream_1_completed_access_units={} stream_2_complete_configuration_access_units={} stream_2_reorder_window_exceeded={} stream_2_recovery_marker_resyncs={} stream_2_waiting_for_recovery_irap_drops={} stream_2_completed_access_units={} stream_1_video_config_publications={} stream_1_encoded_video_publications={} stream_2_video_config_publications={} stream_2_encoded_video_publications={}",
            prefix,
            self.active_service_ticks,
            self.video_stream_1.authenticated_rtp_packets,
            self.video_stream_2.authenticated_rtp_packets,
            discards.unexpected_source,
            discards.empty_datagram,
            discards.truncated_header,
            discards.malformed_packet,
            discards.authentication_failed,
            discards.replay_or_too_old,
            stream_1_assembler.complete_configuration_access_units,
            stream_1_assembler.reorder_window_exceeded,
            stream_1_assembler.recovery_marker_resyncs,
            stream_1_assembler.waiting_for_recovery_irap_drops,
            stream_1_assembler.completed_access_units,
            stream_2_assembler.complete_configuration_access_units,
            stream_2_assembler.reorder_window_exceeded,
            stream_2_assembler.recovery_marker_resyncs,
            stream_2_assembler.waiting_for_recovery_irap_drops,
            stream_2_assembler.completed_access_units,
            stream_1_adapter.video_config_publications,
            stream_1_adapter.encoded_video_publications,
            stream_2_adapter.video_config_publications,
            stream_2_adapter.encoded_video_publications,
        )
    }

    #[cfg(debug_assertions)]
    pub(crate) fn emit_debug_close_summary(&self) {
        eprintln!("{}", self.debug_close_summary());
    }
}

#[allow(clippy::too_many_arguments)]
fn accept_datagram(
    audio_receiver: &mut ArdAudioReceiver,
    authenticated_audio_packets: &mut u64,
    late_audio_packets: &mut u64,
    audio_resynchronizations: &mut u64,
    non_silent_audio_access_units: &mut u64,
    concealed_audio_access_units: &mut u64,
    authenticated_video_packets: &mut u64,
    video_stream_1: &mut VideoReceiveState,
    video_stream_2: &mut VideoReceiveState,
    audio_degraded: &mut bool,
    runtime: &mut ProtocolRuntime,
    generation: u64,
    role: MediaRole,
    datagram: MediaDatagram,
    #[cfg(all(debug_assertions, not(test)))] debug_watchdog: &DebugMediaWatchdog,
    #[cfg(all(debug_assertions, not(test)))] debug_tick: u64,
) -> Result<()> {
    #[cfg(all(debug_assertions, not(test)))]
    let _debug_handler_exit = match role {
        MediaRole::Audio => DebugMediaStageExit::enter(
            debug_watchdog,
            debug_tick,
            DebugMediaStage::AudioEnter,
            DebugMediaStage::AudioExit,
        ),
        MediaRole::VideoStream1 => DebugMediaStageExit::enter(
            debug_watchdog,
            debug_tick,
            DebugMediaStage::Video1Enter,
            DebugMediaStage::Video1Exit,
        ),
        MediaRole::VideoStream2 => DebugMediaStageExit::enter(
            debug_watchdog,
            debug_tick,
            DebugMediaStage::Video2Enter,
            DebugMediaStage::Video2Exit,
        ),
    };
    if role == MediaRole::Audio && *audio_degraded {
        return Ok(());
    }
    match (role, datagram) {
        (MediaRole::Audio, MediaDatagram::Rtp(packet)) => {
            let outcome = match audio_receiver.decode_rtp_packet(&packet) {
                Err(_) => {
                    degrade_audio(audio_degraded, runtime);
                    return Ok(());
                }
                Ok(outcome) => outcome,
            };
            accept_audio_outcome(
                authenticated_audio_packets,
                late_audio_packets,
                audio_resynchronizations,
                non_silent_audio_access_units,
                concealed_audio_access_units,
                audio_degraded,
                runtime,
                outcome,
            );
        }
        (MediaRole::Audio, MediaDatagram::Rtcp(_)) => {}
        (MediaRole::VideoStream1, MediaDatagram::Rtp(packet)) => {
            video_stream_1.observe_authenticated_rtp(generation);
            video_stream_1.accept_rtp(runtime, generation, &packet)?;
            *authenticated_video_packets = authenticated_video_packets.saturating_add(1);
        }
        (MediaRole::VideoStream2, MediaDatagram::Rtp(packet)) => {
            video_stream_2.observe_authenticated_rtp(generation);
            video_stream_2.accept_rtp(runtime, generation, &packet)?;
            *authenticated_video_packets = authenticated_video_packets.saturating_add(1);
        }
        (MediaRole::VideoStream1 | MediaRole::VideoStream2, MediaDatagram::Rtcp(_)) => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn accept_audio_outcome(
    authenticated_audio_packets: &mut u64,
    late_audio_packets: &mut u64,
    audio_resynchronizations: &mut u64,
    non_silent_audio_access_units: &mut u64,
    concealed_audio_access_units: &mut u64,
    audio_degraded: &mut bool,
    runtime: &mut ProtocolRuntime,
    outcome: AudioReceiveOutcome,
) {
    if *audio_degraded {
        return;
    }
    let decoded = match outcome {
        AudioReceiveOutcome::DiscardedLate { .. } => {
            *late_audio_packets = late_audio_packets.saturating_add(1);
            return;
        }
        AudioReceiveOutcome::Decoded(decoded) => decoded,
        AudioReceiveOutcome::Resynchronized {
            decoded,
            skipped_access_units: _,
        } => {
            *audio_resynchronizations = audio_resynchronizations.saturating_add(1);
            decoded
        }
    };
    if decoded.pcm.iter().any(|sample| *sample != 0) {
        *non_silent_audio_access_units = non_silent_audio_access_units.saturating_add(1);
    }
    *concealed_audio_access_units =
        concealed_audio_access_units.saturating_add(decoded.concealed_access_units as u64);
    if publish_decoded_audio(runtime, decoded).is_err() {
        degrade_audio(audio_degraded, runtime);
        return;
    }
    *authenticated_audio_packets = authenticated_audio_packets.saturating_add(1);
}

fn degrade_audio(audio_degraded: &mut bool, runtime: &mut ProtocolRuntime) {
    if *audio_degraded {
        return;
    }
    *audio_degraded = true;
    let _ = runtime.publish_event(SessionEvent::AudioState(AudioState::Failed));
}

fn validate_hpss_audio_flow(audio_flow: AudioMediaFlow) -> Result<()> {
    if audio_flow == AudioMediaFlow::PcToMac {
        bail!(
            "用户名/密码 HPSS 不支持 PC→Mac Audio Chat；stock Apple 实现需要 IDS/Apple ID 邀请状态"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc, Mutex};

    use frd_core::{PixelRect, PixelSize, SessionId};
    use frd_frame::{PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate};
    use frd_media_api::{MediaFrame, MediaPublishError, MediaPublisher};
    use frd_protocol_api::{
        ProtocolError, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionEvent,
        SurfacePublisher,
    };

    use crate::audio_codec::DecodedAudioPacket;
    use crate::media_negotiation::AudioMediaFlow;

    use super::{
        accept_audio_outcome, accept_datagram, publish_decoded_audio, AudioReceiveOutcome,
        MediaDatagram, MediaRole, ViewerMediaState,
    };

    fn accept_state_datagram(
        state: &mut ViewerMediaState,
        runtime: &mut ProtocolRuntime,
        role: MediaRole,
        datagram: MediaDatagram,
    ) -> anyhow::Result<()> {
        accept_datagram(
            &mut state.audio_receiver,
            &mut state.authenticated_audio_packets,
            &mut state.late_audio_packets,
            &mut state.audio_resynchronizations,
            &mut state.non_silent_audio_access_units,
            &mut state.concealed_audio_access_units,
            &mut state.authenticated_video_packets,
            &mut state.video_stream_1,
            &mut state.video_stream_2,
            &mut state.audio_degraded,
            runtime,
            state.transport.generation(),
            role,
            datagram,
        )
    }

    fn establish_desktop_generation(runtime: &mut ProtocolRuntime, session_id: SessionId) {
        runtime
            .begin_generation(
                session_id,
                1,
                PixelSize::new(1, 1).unwrap(),
                PixelFormat::Bgrx8UnormSrgb,
            )
            .unwrap();
    }

    fn assert_desktop_surface_continues(runtime: &mut ProtocolRuntime, session_id: SessionId) {
        runtime
            .publish_surface(SurfaceUpdate::Damage {
                session_id,
                generation: 1,
                revision: 1,
                patches: vec![PixelPatch {
                    rect: PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    stride_bytes: 4,
                    pixels: PixelBuffer::new(vec![1, 2, 3, 0]),
                }],
            })
            .unwrap();
    }

    struct NoopEvents;

    impl RuntimeEventSink for NoopEvents {
        fn publish(&self, _event: SessionEvent) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct RecordingEvents(Arc<Mutex<Vec<SessionEvent>>>);

    impl RuntimeEventSink for RecordingEvents {
        fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
    }

    struct NoopFrames;

    impl SurfacePublisher for NoopFrames {
        fn publish(&self, _update: frd_frame::SurfaceUpdate) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct NoopWake;

    impl RuntimeWake for NoopWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct RecordingMedia(Arc<Mutex<Vec<MediaFrame>>>);

    impl MediaPublisher for RecordingMedia {
        fn publish(&self, frame: MediaFrame) -> Result<(), MediaPublishError> {
            self.0.lock().unwrap().push(frame);
            Ok(())
        }
    }

    struct RejectingMedia {
        failure: MediaPublishError,
        attempts: Arc<Mutex<usize>>,
    }

    impl MediaPublisher for RejectingMedia {
        fn publish(&self, _frame: MediaFrame) -> Result<(), MediaPublishError> {
            *self.attempts.lock().unwrap() += 1;
            Err(self.failure.clone())
        }
    }

    #[test]
    fn debug_media_checkpoint_fires_once_at_each_boundary_and_rearms_after_reset() {
        let mut state =
            ViewerMediaState::new(AudioMediaFlow::MacToPc, 1, "127.0.0.1".parse().unwrap())
                .unwrap();

        for (before_boundary, boundary) in
            [(62, 64), (126, 128), (254, 256), (510, 512), (2046, 2048)]
        {
            state.active_service_ticks = before_boundary;
            assert_eq!(state.debug_advance_active_service_tick(), None);
            let tick = state
                .debug_advance_active_service_tick()
                .unwrap_or_else(|| panic!("tick {boundary} must emit one checkpoint"));
            assert_eq!(tick, boundary);
            let checkpoint = state.debug_service_checkpoint_summary(tick);
            let checkpoint_prefix = format!("[frd-media-checkpoint] tick={boundary}");
            assert!(checkpoint.starts_with(&format!(
                "{checkpoint_prefix} active_service_ticks={boundary} authenticated_video_rtp_stream_1=0 "
            )));
            assert_eq!(
                checkpoint.strip_prefix(&checkpoint_prefix),
                state
                    .debug_close_summary()
                    .strip_prefix("[frd-media-summary]"),
                "checkpoint and close summary must expose the same safe fields"
            );
            assert_eq!(state.debug_advance_active_service_tick(), None);
        }

        state.reset_generation(2).unwrap();
        assert_eq!(state.active_service_ticks, 0);
        state.active_service_ticks = 63;
        assert_eq!(state.debug_advance_active_service_tick(), Some(64));
    }

    #[test]
    fn inactive_media_service_does_not_advance_or_emit_debug_checkpoint() {
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(NoopFrames),
            Some(Box::new(RecordingMedia(Arc::new(Mutex::new(Vec::new()))))),
            Box::new(NoopWake),
        );
        establish_desktop_generation(&mut runtime, session_id);
        let mut state = ViewerMediaState::new_for_session(
            session_id,
            AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();

        assert_eq!(
            state
                .service_active(&mut runtime, 1, std::time::Instant::now())
                .unwrap(),
            0
        );
        assert_eq!(state.active_service_ticks, 0);
    }

    #[test]
    fn decoded_aac_eld_publishes_bounded_48khz_stereo_pcm_without_platform_device() {
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let published = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(NoopFrames),
            Some(Box::new(RecordingMedia(published.clone()))),
            Box::new(NoopWake),
        );
        establish_desktop_generation(&mut runtime, session_id);
        publish_decoded_audio(
            &mut runtime,
            DecodedAudioPacket {
                pcm: vec![1, -2, 3, -4],
                concealed_access_units: 0,
                sequence: 7,
                timestamp: 480,
                ssrc: 0x1234_5678,
            },
        )
        .unwrap();

        let mut published = published.lock().unwrap();
        let MediaFrame::Pcm {
            sample_rate_hz,
            channels,
            samples,
        } = published.pop().expect("one PCM publication")
        else {
            panic!("AAC-ELD decode must publish PCM");
        };
        assert_eq!(sample_rate_hz, 48_000);
        assert_eq!(channels, 2);
        assert_eq!(&*samples, &[1, -2, 3, -4]);
    }

    #[test]
    fn password_hpss_pc_to_mac_fails_before_transport_selection() {
        let selected_transport = Arc::new(Mutex::new(false));
        let observed = selected_transport.clone();
        let result =
            ViewerMediaState::new_with_transport_factory(AudioMediaFlow::PcToMac, move || {
                *observed.lock().unwrap() = true;
                unreachable!("P5 must fail before selecting a network transport")
            });

        assert!(result.is_err());
        assert!(!*selected_transport.lock().unwrap());
    }

    #[test]
    fn authenticated_audio_codec_failure_degrades_once_while_video_and_runtime_continue() {
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let events = Arc::new(Mutex::new(Vec::new()));
        let published = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events.clone())),
            Box::new(NoopFrames),
            Some(Box::new(RecordingMedia(published))),
            Box::new(NoopWake),
        );
        establish_desktop_generation(&mut runtime, session_id);
        let mut state =
            ViewerMediaState::new(AudioMediaFlow::MacToPc, 1, "127.0.0.1".parse().unwrap())
                .unwrap();

        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::Audio,
            MediaDatagram::Rtp(vec![0; 3]),
        )
        .unwrap();
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::Audio,
            MediaDatagram::Rtp(vec![0; 3]),
        )
        .unwrap();
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp(1, 1, false, &startup_parameter_set_ap())),
        )
        .unwrap();

        assert!(state.audio_degraded);
        assert!(!runtime.requires_shutdown());
        assert_eq!(state.authenticated_audio_packets, 0);
        assert_eq!(state.authenticated_video_packets, 1);
        assert_desktop_surface_continues(&mut runtime, session_id);
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| matches!(
                    event,
                    SessionEvent::AudioState(frd_protocol_api::AudioState::Failed)
                ))
                .count(),
            1,
            "degraded audio must not retry the codec or republish failure"
        );
    }

    #[test]
    fn authenticated_udp_video_routes_completed_au_through_the_unified_adapter() {
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let published = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(NoopFrames),
            Some(Box::new(RecordingMedia(published.clone()))),
            Box::new(NoopWake),
        );
        establish_desktop_generation(&mut runtime, session_id);
        let mut state = ViewerMediaState::new_for_session(
            session_id,
            AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();

        let mut aggregation = vec![0x60, 0x01];
        for nal in [
            &[0x40, 0x01, 0xaa][..],
            crate::hevc_sps::CAPTURED_MAIN444_8BIT_SPS,
            &[0x44, 0x01, 0xbb],
            &[0x26, 0x01, 0xcc],
        ] {
            aggregation.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            aggregation.extend_from_slice(nal);
        }
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp(1, 90_000, true, &aggregation)),
        )
        .unwrap();

        let published = published.lock().unwrap();
        assert!(matches!(&published[0], MediaFrame::VideoConfig(config)
            if config.as_input().identity.session_id == session_id
                && config.as_input().identity.stream_id == 1));
        assert!(
            matches!(&published[1], MediaFrame::EncodedVideo(access_unit)
            if access_unit.identity().session_id == session_id
                && access_unit.identity().stream_id == 1)
        );
        assert_eq!(
            state.debug_close_summary(),
            "[frd-media-summary] active_service_ticks=0 authenticated_video_rtp_stream_1=1 authenticated_video_rtp_stream_2=0 discard_unexpected_source=0 discard_empty_datagram=0 discard_truncated_header=0 discard_malformed_packet=0 discard_authentication_failed=0 discard_replay_or_too_old=0 stream_1_complete_configuration_access_units=1 stream_1_reorder_window_exceeded=0 stream_1_recovery_marker_resyncs=0 stream_1_waiting_for_recovery_irap_drops=0 stream_1_completed_access_units=1 stream_2_complete_configuration_access_units=0 stream_2_reorder_window_exceeded=0 stream_2_recovery_marker_resyncs=0 stream_2_waiting_for_recovery_irap_drops=0 stream_2_completed_access_units=0 stream_1_video_config_publications=1 stream_1_encoded_video_publications=1 stream_2_video_config_publications=0 stream_2_encoded_video_publications=0"
        );

        state.reset_generation(2).unwrap();
        assert_eq!(
            state.debug_close_summary(),
            "[frd-media-summary] active_service_ticks=0 authenticated_video_rtp_stream_1=0 authenticated_video_rtp_stream_2=0 discard_unexpected_source=0 discard_empty_datagram=0 discard_truncated_header=0 discard_malformed_packet=0 discard_authentication_failed=0 discard_replay_or_too_old=0 stream_1_complete_configuration_access_units=0 stream_1_reorder_window_exceeded=0 stream_1_recovery_marker_resyncs=0 stream_1_waiting_for_recovery_irap_drops=0 stream_1_completed_access_units=0 stream_2_complete_configuration_access_units=0 stream_2_reorder_window_exceeded=0 stream_2_recovery_marker_resyncs=0 stream_2_waiting_for_recovery_irap_drops=0 stream_2_completed_access_units=0 stream_1_video_config_publications=0 stream_1_encoded_video_publications=0 stream_2_video_config_publications=0 stream_2_encoded_video_publications=0"
        );
    }

    #[test]
    fn apple_startup_configuration_au_is_not_discarded() {
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let published = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(NoopFrames),
            Some(Box::new(RecordingMedia(published.clone()))),
            Box::new(NoopWake),
        );
        establish_desktop_generation(&mut runtime, session_id);
        let mut state = ViewerMediaState::new_for_session(
            session_id,
            AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();

        let aggregation = startup_parameter_set_ap();
        for (sequence, marker, payload) in [
            (10, false, aggregation.as_slice()),
            (11, false, &[0x62, 0x01, 0x93, 0xaa][..]),
            (12, false, &[0x62, 0x01, 0x13, 0xbb][..]),
            (13, true, &[0x62, 0x01, 0x53, 0xcc][..]),
        ] {
            accept_state_datagram(
                &mut state,
                &mut runtime,
                MediaRole::VideoStream1,
                MediaDatagram::Rtp(video_rtp(sequence, 0, marker, payload)),
            )
            .unwrap();
        }

        let published = published.lock().unwrap();
        assert_eq!(published.len(), 2);
        assert!(matches!(&published[0], MediaFrame::VideoConfig(config)
            if config.as_input().identity.session_id == session_id
                && config.as_input().identity.stream_id == 1));
        assert!(
            matches!(&published[1], MediaFrame::EncodedVideo(access_unit)
            if access_unit.identity().session_id == session_id
                && access_unit.identity().stream_id == 1
                && access_unit.timestamp().ticks == 0
                && access_unit.random_access())
        );
    }

    #[test]
    fn configured_packet_loss_requests_one_recovery_and_publishes_only_after_irap() {
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let published = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(NoopFrames),
            Some(Box::new(RecordingMedia(published.clone()))),
            Box::new(NoopWake),
        );
        establish_desktop_generation(&mut runtime, session_id);
        let mut state = ViewerMediaState::new_for_session(
            session_id,
            AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();

        let aggregation = startup_parameter_set_ap();
        for (sequence, marker, payload) in [
            (1, false, aggregation.as_slice()),
            (2, false, &[0x62, 0x01, 0x93, 0xaa][..]),
            (3, true, &[0x62, 0x01, 0x53, 0xbb][..]),
        ] {
            accept_state_datagram(
                &mut state,
                &mut runtime,
                MediaRole::VideoStream1,
                MediaDatagram::Rtp(video_rtp(sequence, 0, marker, payload)),
            )
            .unwrap();
        }

        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp(4, 3_000, false, &[0x02, 0x01, 0xdd])),
        )
        .unwrap();
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp(261, 3_000, true, &[0x02, 0x01, 0xee])),
        )
        .expect("reorder-window loss must drop the damaged AU without failing the session");
        assert_eq!(
            state.video_stream_1.take_recovery_request(),
            Some(super::VideoRecoveryRequest::PictureLoss {
                media_ssrc: 0x1020_3040,
            })
        );
        assert_eq!(state.video_stream_1.take_recovery_request(), None);
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp(262, 6_000, true, &[0x02, 0x01, 0xff])),
        )
        .expect("non-IRAP recovery traffic must be ignored without failing the session");
        assert_eq!(
            published.lock().unwrap().len(),
            2,
            "no frame may be published between reorder loss and the recovery IRAP"
        );
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp(263, 9_000, true, &[0x26, 0x01, 0xab])),
        )
        .expect("the first recovery IRAP must restore publication");

        assert!(!runtime.requires_shutdown());
        let published = published.lock().unwrap();
        assert_eq!(published.len(), 3);
        assert!(
            matches!(&published[2], MediaFrame::EncodedVideo(access_unit)
            if access_unit.identity().session_id == session_id
                && access_unit.generation() == 1
                && access_unit.timestamp().ticks == 9_000
                && access_unit.random_access())
        );
    }

    #[test]
    fn authenticated_video_ssrc_switch_waits_for_new_configuration_irap_without_fatal() {
        const FIRST_SSRC: u32 = 0x1020_3040;
        const SECOND_SSRC: u32 = 0x5060_7080;
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let published = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(NoopFrames),
            Some(Box::new(RecordingMedia(published.clone()))),
            Box::new(NoopWake),
        );
        establish_desktop_generation(&mut runtime, session_id);
        let mut state = ViewerMediaState::new_for_session(
            session_id,
            AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();

        let first_configuration = startup_parameter_set_ap();
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp_for_ssrc(
                FIRST_SSRC,
                1,
                0,
                true,
                &first_configuration,
            )),
        )
        .unwrap();
        assert_eq!(published.lock().unwrap().len(), 2);

        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp_for_ssrc(
                SECOND_SSRC,
                1,
                3_000,
                true,
                &[0x02, 0x01, 0xaa],
            )),
        )
        .expect("authenticated SSRC replacement traffic must not fail the session");
        assert_eq!(published.lock().unwrap().len(), 2);
        assert!(!runtime.requires_shutdown());

        let mut replacement_configuration = startup_parameter_set_ap();
        replacement_configuration.extend_from_slice(&[0, 4, 0x26, 0x01, 0xbb, 0xcc]);
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp_for_ssrc(
                SECOND_SSRC,
                2,
                6_000,
                true,
                &replacement_configuration,
            )),
        )
        .expect("new SSRC configuration plus IRAP must restore publication");

        let published = published.lock().unwrap();
        assert_eq!(published.len(), 4);
        assert!(matches!(&published[2], MediaFrame::VideoConfig(_)));
        assert!(
            matches!(&published[3], MediaFrame::EncodedVideo(access_unit)
            if access_unit.random_access() && access_unit.timestamp().ticks == 6_000)
        );
    }

    #[test]
    fn authenticated_video_ssrc_switch_ignores_late_retired_source_without_flapping() {
        const FIRST_SSRC: u32 = 0x1020_3040;
        const SECOND_SSRC: u32 = 0x5060_7080;
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let published = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(NoopFrames),
            Some(Box::new(RecordingMedia(published.clone()))),
            Box::new(NoopWake),
        );
        establish_desktop_generation(&mut runtime, session_id);
        let mut state = ViewerMediaState::new_for_session(
            session_id,
            AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();

        let mut first_configuration = startup_parameter_set_ap();
        first_configuration.extend_from_slice(&[0, 4, 0x26, 0x01, 0xaa, 0xbb]);
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp_for_ssrc(
                FIRST_SSRC,
                1,
                0,
                true,
                &first_configuration,
            )),
        )
        .unwrap();

        let mut replacement_configuration = startup_parameter_set_ap();
        replacement_configuration.extend_from_slice(&[0, 4, 0x26, 0x01, 0xcc, 0xdd]);
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp_for_ssrc(
                SECOND_SSRC,
                1,
                3_000,
                true,
                &replacement_configuration,
            )),
        )
        .unwrap();
        assert_eq!(published.lock().unwrap().len(), 4);

        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp_for_ssrc(
                FIRST_SSRC,
                2,
                6_000,
                true,
                &[0x02, 0x01, 0xee],
            )),
        )
        .expect("late authenticated packets from the retired SSRC must be ignored");
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp_for_ssrc(
                SECOND_SSRC,
                2,
                9_000,
                true,
                &[0x02, 0x01, 0xff],
            )),
        )
        .expect("the active replacement SSRC must continue without another reset");

        let published = published.lock().unwrap();
        assert_eq!(published.len(), 5);
        assert!(
            matches!(&published[4], MediaFrame::EncodedVideo(access_unit)
            if access_unit.timestamp().ticks == 9_000)
        );
    }

    #[test]
    fn authenticated_video_ssrc_switch_caches_configuration_au_until_later_irap() {
        const FIRST_SSRC: u32 = 0x1020_3040;
        const SECOND_SSRC: u32 = 0x5060_7080;
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let published = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(NoopFrames),
            Some(Box::new(RecordingMedia(published.clone()))),
            Box::new(NoopWake),
        );
        establish_desktop_generation(&mut runtime, session_id);
        let mut state = ViewerMediaState::new_for_session(
            session_id,
            AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();

        let mut first_configuration = startup_parameter_set_ap();
        first_configuration.extend_from_slice(&[0, 4, 0x26, 0x01, 0xaa, 0xbb]);
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp_for_ssrc(
                FIRST_SSRC,
                1,
                0,
                true,
                &first_configuration,
            )),
        )
        .unwrap();

        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp_for_ssrc(
                SECOND_SSRC,
                1,
                3_000,
                true,
                &startup_parameter_set_ap(),
            )),
        )
        .expect("replacement configuration AU must be cached without publication");
        assert_eq!(published.lock().unwrap().len(), 2);

        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp_for_ssrc(
                SECOND_SSRC,
                2,
                6_000,
                true,
                &[0x26, 0x01, 0xcc],
            )),
        )
        .expect("a later replacement IRAP must publish cached configuration first");

        let published = published.lock().unwrap();
        assert_eq!(published.len(), 4);
        assert!(matches!(&published[2], MediaFrame::VideoConfig(_)));
        assert!(
            matches!(&published[3], MediaFrame::EncodedVideo(access_unit)
            if access_unit.random_access() && access_unit.timestamp().ticks == 6_000)
        );
    }

    #[test]
    fn authenticated_video_ssrc_switch_drops_initial_fu_continuation_without_fatal() {
        const FIRST_SSRC: u32 = 0x1020_3040;
        const SECOND_SSRC: u32 = 0x5060_7080;
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let published = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(NoopFrames),
            Some(Box::new(RecordingMedia(published.clone()))),
            Box::new(NoopWake),
        );
        establish_desktop_generation(&mut runtime, session_id);
        let mut state = ViewerMediaState::new_for_session(
            session_id,
            AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();

        let mut first_configuration = startup_parameter_set_ap();
        first_configuration.extend_from_slice(&[0, 4, 0x26, 0x01, 0xaa, 0xbb]);
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp_for_ssrc(
                FIRST_SSRC,
                1,
                0,
                true,
                &first_configuration,
            )),
        )
        .unwrap();

        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp_for_ssrc(
                SECOND_SSRC,
                1,
                3_000,
                false,
                &[0x62, 0x01, 0x20, 0xee],
            )),
        )
        .expect("a replacement stream observed mid-FU must wait instead of failing the session");
        assert_eq!(published.lock().unwrap().len(), 2);

        let mut replacement_configuration = startup_parameter_set_ap();
        replacement_configuration.extend_from_slice(&[0, 4, 0x26, 0x01, 0xcc, 0xdd]);
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp_for_ssrc(
                SECOND_SSRC,
                2,
                6_000,
                true,
                &replacement_configuration,
            )),
        )
        .expect("replacement stream must recover after a complete configuration IRAP");
        assert_eq!(published.lock().unwrap().len(), 4);
    }

    #[test]
    fn startup_loss_waits_for_irap_before_publishing_cached_configuration() {
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let published = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(NoopFrames),
            Some(Box::new(RecordingMedia(published.clone()))),
            Box::new(NoopWake),
        );
        establish_desktop_generation(&mut runtime, session_id);
        let mut state = ViewerMediaState::new_for_session(
            session_id,
            AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();

        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp(1, 0, false, &startup_parameter_set_ap())),
        )
        .unwrap();
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp(258, 0, true, &[0x02, 0x01, 1])),
        )
        .unwrap();

        for (sequence, timestamp, payload) in [
            (259, 3_000, &[0x02, 0x01, 2][..]),
            (260, 6_000, &[0x00, 0x01, 3][..]),
        ] {
            accept_state_datagram(
                &mut state,
                &mut runtime,
                MediaRole::VideoStream1,
                MediaDatagram::Rtp(video_rtp(sequence, timestamp, true, payload)),
            )
            .unwrap();
        }
        assert!(published.lock().unwrap().is_empty());

        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp(261, 9_000, true, &[0x26, 0x01, 4])),
        )
        .unwrap();

        let published = published.lock().unwrap();
        assert_eq!(published.len(), 2);
        assert!(matches!(&published[0], MediaFrame::VideoConfig(_)));
        assert!(
            matches!(&published[1], MediaFrame::EncodedVideo(access_unit)
            if access_unit.random_access() && access_unit.timestamp().ticks == 9_000)
        );
    }

    #[test]
    fn authenticated_video_rtp_stage_precedes_downstream_failure_and_is_once_per_stream() {
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(NoopFrames),
            Some(Box::new(RecordingMedia(Arc::new(Mutex::new(Vec::new()))))),
            Box::new(NoopWake),
        );
        establish_desktop_generation(&mut runtime, session_id);
        let mut state = ViewerMediaState::new_for_session(
            session_id,
            AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();

        for _ in 0..2 {
            assert!(accept_state_datagram(
                &mut state,
                &mut runtime,
                MediaRole::VideoStream1,
                MediaDatagram::Rtp(vec![0x80]),
            )
            .is_err());
        }
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtcp(vec![0x80]),
        )
        .unwrap();
        let _ = accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::Audio,
            MediaDatagram::Rtp(vec![0x80]),
        );

        assert_eq!(state.video_stream_1.stage_trace.observed_stage_count(), 1);
        assert_eq!(state.video_stream_2.stage_trace.observed_stage_count(), 0);
    }

    fn video_rtp(sequence: u16, timestamp: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
        video_rtp_for_ssrc(0x1020_3040, sequence, timestamp, marker, payload)
    }

    fn video_rtp_for_ssrc(
        ssrc: u32,
        sequence: u16,
        timestamp: u32,
        marker: bool,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut packet = vec![0x80, 96 | if marker { 0x80 } else { 0 }];
        packet.extend_from_slice(&sequence.to_be_bytes());
        packet.extend_from_slice(&timestamp.to_be_bytes());
        packet.extend_from_slice(&ssrc.to_be_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    fn startup_parameter_set_ap() -> Vec<u8> {
        let mut aggregation = vec![0x60, 0x01];
        for nal in [
            &[0x40, 0x01, 0xaa][..],
            crate::hevc_sps::CAPTURED_MAIN444_8BIT_SPS,
            &[0x44, 0x01, 0xbb],
        ] {
            aggregation.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            aggregation.extend_from_slice(nal);
        }
        aggregation
    }

    #[test]
    fn pcm_full_or_closed_degrades_audio_once_without_poisoning_desktop_runtime() {
        for failure in [MediaPublishError::Full, MediaPublishError::Closed] {
            let session_id = SessionId::allocate();
            let (_commands, command_rx) = mpsc::channel();
            let events = Arc::new(Mutex::new(Vec::new()));
            let attempts = Arc::new(Mutex::new(0usize));
            let mut runtime = ProtocolRuntime::new(
                session_id,
                command_rx,
                Box::new(RecordingEvents(events.clone())),
                Box::new(NoopFrames),
                Some(Box::new(RejectingMedia {
                    failure,
                    attempts: attempts.clone(),
                })),
                Box::new(NoopWake),
            );
            establish_desktop_generation(&mut runtime, session_id);
            let mut state =
                ViewerMediaState::new(AudioMediaFlow::MacToPc, 1, "127.0.0.1".parse().unwrap())
                    .unwrap();
            let decoded = || crate::audio_codec::DecodedAudioPacket {
                pcm: vec![1, -2, 3, -4],
                concealed_access_units: 0,
                sequence: 7,
                timestamp: 480,
                ssrc: 0x1234_5678,
            };

            accept_audio_outcome(
                &mut state.authenticated_audio_packets,
                &mut state.late_audio_packets,
                &mut state.audio_resynchronizations,
                &mut state.non_silent_audio_access_units,
                &mut state.concealed_audio_access_units,
                &mut state.audio_degraded,
                &mut runtime,
                AudioReceiveOutcome::Decoded(decoded()),
            );
            accept_audio_outcome(
                &mut state.authenticated_audio_packets,
                &mut state.late_audio_packets,
                &mut state.audio_resynchronizations,
                &mut state.non_silent_audio_access_units,
                &mut state.concealed_audio_access_units,
                &mut state.audio_degraded,
                &mut runtime,
                AudioReceiveOutcome::Decoded(decoded()),
            );
            accept_state_datagram(
                &mut state,
                &mut runtime,
                MediaRole::VideoStream2,
                MediaDatagram::Rtp(video_rtp(1, 1, false, &startup_parameter_set_ap())),
            )
            .unwrap();

            assert!(state.audio_degraded);
            assert!(!runtime.requires_shutdown());
            assert_eq!(
                *attempts.lock().unwrap(),
                1,
                "degraded audio must not retry"
            );
            assert_eq!(state.authenticated_video_packets, 1);
            assert_desktop_surface_continues(&mut runtime, session_id);
            assert_eq!(
                events
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|event| matches!(
                        event,
                        SessionEvent::AudioState(frd_protocol_api::AudioState::Failed)
                    ))
                    .count(),
                1
            );
        }
    }
}
