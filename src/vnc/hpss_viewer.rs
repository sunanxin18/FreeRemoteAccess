//! HPSS 实时交互视图：MVS 流接收 + 逐矩形解码 + minifb 渲染 + 键鼠输入。
//!
//! 与 `view` 命令的区别：走 HPSS 路径（虚拟显示器 + MVS 原生编码），
//! 支持高分辨率（1440×2560+），动态分辨率协商。
//!
//! 架构（与 viewer.rs 相同的线程模型）：
//! - 读线程：接收加密帧 → 解析矩形 → MVS 原生事务解码 → 贴到共享帧缓冲 → 请求增量
//! - 主线程：minifb 窗口渲染 + 键鼠事件编码为 RFB 消息 → 加密帧发送

use std::collections::HashSet;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use minifb::{Key, MouseButton, MouseMode, Window, WindowOptions};

use crate::framebuffer::{Framebuffer, PIXEL_BLUE_SHIFT, PIXEL_GREEN_SHIFT, PIXEL_RED_SHIFT};
use crate::keysym;
use crate::pointer_input::{PointerInputState, PointerSample};
use crate::vnc::audio_codec::{
    AacEldEncoder, ArdAudioReceiver, AudioReceiveOutcome, DecodedAudioPacket,
};
use crate::vnc::audio_input::{
    AudioInputPhase, AudioInputRuntime, AudioInputSourceMode, P5_PROBE_FRAME_COUNT,
    P5_PROBE_SAMPLE_RATE_HZ,
};
use crate::vnc::audio_io::AudioPlayback;
use crate::vnc::client::{is_timeout, RfbConn};
use crate::vnc::dynamic_resolution::{
    DisplaySize, DynamicResolutionCapability, DynamicResolutionController, GeometryCommit,
    ResolutionRequest,
};
use crate::vnc::hpss::{self};
use crate::vnc::hpss::{encoding, parse_media, Media};
use crate::vnc::media_negotiation::AudioMediaFlow;
use crate::vnc::media_transport::{
    AudioReceptionEvidence, MediaDatagram, MediaRole, MediaTransport, MediaTransportPhase,
    OutboundAudioSentRange,
};
use crate::vnc::mvs;
use crate::vnc::mvs_stream::{MvsRecord, MvsRecordAssembler, MvsRect};
use crate::vnc::protocol;

const MVS_INCOMPLETE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_AUDIO_ACCESS_UNITS_PER_READER_TICK: usize = 32;
const MAX_PENDING_MEDIA_ANSWER_FRAME_DIAGNOSTICS: usize = 24;
const REVIEWED_AUDIO_INPUT_SOURCE_MODE: AudioInputSourceMode =
    AudioInputSourceMode::DeterministicProbe;

#[derive(Debug, Eq, PartialEq)]
enum AudioOutputPhase {
    ReadyToStart,
    Active,
    Degraded { reason: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MediaAcceptOutcome {
    Applied,
    AuthenticatedNotRendered,
    Discarded,
    AudioDegraded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AudioInputConfirmationDiagnostic {
    packets_sent: u32,
    first_extended_sequence: u32,
    last_extended_sequence: u32,
    ssrc: u32,
    srtcp_extended_highest_sequence: u32,
    srtcp_cumulative_packets_lost: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AudioInputProbeProgressDiagnostic {
    packets_sent: u32,
    first_extended_sequence: u32,
    last_extended_sequence: u32,
    ssrc: u32,
}

fn should_log_audio_resynchronization(count: u64) -> bool {
    count.is_power_of_two()
}

fn should_log_audio_input_probe_progress(count: u64) -> bool {
    count <= u64::from(P5_PROBE_FRAME_COUNT)
        && (count.is_power_of_two() || count == u64::from(P5_PROBE_FRAME_COUNT))
}

fn audio_input_probe_progress_diagnostic(
    sent_audio_access_units: u64,
    range: Option<OutboundAudioSentRange>,
) -> Option<AudioInputProbeProgressDiagnostic> {
    if !should_log_audio_input_probe_progress(sent_audio_access_units) {
        return None;
    }
    let range = range?;
    if u64::from(range.packets_sent) != sent_audio_access_units {
        return None;
    }
    Some(AudioInputProbeProgressDiagnostic {
        packets_sent: range.packets_sent,
        first_extended_sequence: range.first_extended_sequence,
        last_extended_sequence: range.last_extended_sequence,
        ssrc: range.ssrc,
    })
}

struct ViewerMediaState {
    audio_flow: AudioMediaFlow,
    audio_receiver: ArdAudioReceiver,
    audio_playback: Option<AudioPlayback>,
    audio_output_phase: AudioOutputPhase,
    audio_input_runtime: AudioInputRuntime,
    audio_encoder: Option<AacEldEncoder>,
    authenticated_audio_packets: u64,
    late_audio_packets: u64,
    audio_resynchronizations: u64,
    non_silent_audio_access_units: u64,
    concealed_audio_access_units: u64,
    sent_audio_access_units: u64,
    pending_audio_input_confirmation_diagnostic: Option<AudioInputConfirmationDiagnostic>,
    audio_input_confirmation_diagnostic_reported: bool,
    authenticated_video_packets: u64,
}

impl ViewerMediaState {
    fn new(audio_flow: AudioMediaFlow) -> Result<Self> {
        Ok(Self {
            audio_flow,
            audio_receiver: ArdAudioReceiver::new()?,
            audio_playback: None,
            audio_output_phase: AudioOutputPhase::ReadyToStart,
            audio_input_runtime: AudioInputRuntime::new(REVIEWED_AUDIO_INPUT_SOURCE_MODE),
            audio_encoder: None,
            authenticated_audio_packets: 0,
            late_audio_packets: 0,
            audio_resynchronizations: 0,
            non_silent_audio_access_units: 0,
            concealed_audio_access_units: 0,
            sent_audio_access_units: 0,
            pending_audio_input_confirmation_diagnostic: None,
            audio_input_confirmation_diagnostic_reported: false,
            authenticated_video_packets: 0,
        })
    }

    fn audio_input_phase(&self) -> &AudioInputPhase {
        self.audio_input_runtime.phase()
    }

    fn start_audio_input_probe(
        &mut self,
        transport: &MediaTransport,
        generation: u64,
        now: Instant,
    ) {
        if !transport.pc_to_mac_audio_probe_ready(generation)
            || !matches!(self.audio_input_phase(), AudioInputPhase::Disabled)
        {
            return;
        }
        self.pending_audio_input_confirmation_diagnostic = None;
        self.audio_input_confirmation_diagnostic_reported = false;
        self.audio_input_runtime.begin_negotiation(generation);
        match AacEldEncoder::new_for_pc_to_mac() {
            Ok(encoder) => {
                self.audio_encoder = Some(encoder);
                self.audio_input_runtime.mark_transport_active(generation);
                self.audio_input_runtime.start_probe(generation, now);
                eprintln!(
                    "[audio-in] P5 有界确定性探针已启动: phase={:?}, generation={generation}, sample-rate={}, frames={}",
                    self.audio_input_phase(),
                    P5_PROBE_SAMPLE_RATE_HZ,
                    P5_PROBE_FRAME_COUNT
                );
            }
            Err(error) => self.degrade_audio_input(
                generation,
                format!("创建 PC→Mac AAC-ELD 编码器失败: {error:#}"),
            ),
        }
    }

    fn service_audio_input_probe(
        &mut self,
        transport: &mut MediaTransport,
        generation: u64,
        now: Instant,
    ) {
        if self.audio_flow != AudioMediaFlow::PcToMac || self.audio_encoder.is_none() {
            return;
        }
        let frames = self.audio_input_runtime.take_due_probe_frames(
            generation,
            now,
            MAX_AUDIO_ACCESS_UNITS_PER_READER_TICK,
        );
        if let AudioInputPhase::Degraded { reason, .. } = self.audio_input_runtime.phase() {
            let reason = reason.clone();
            self.degrade_audio_input(generation, reason);
            return;
        }
        for frame in frames {
            let sent = self
                .audio_encoder
                .as_ref()
                .context("P5 探针编码器状态缺失")
                .and_then(|encoder| {
                    encoder
                        .encode_pcm_frame(&frame.pcm)
                        .context("编码 P5 探针帧失败")
                })
                .and_then(|access_unit| {
                    transport
                        .send_audio_access_unit(generation, &access_unit)
                        .context("发送 P5 探针 SRTP 帧失败")
                });
            match sent {
                Ok(_) => {}
                Err(error) => {
                    self.degrade_audio_input(generation, format!("{error:#}"));
                    return;
                }
            }
            self.audio_input_runtime
                .record_probe_frame_sent(generation, frame.token, now);
            self.sent_audio_access_units = self.sent_audio_access_units.saturating_add(1);
            if let Some(diagnostic) = audio_input_probe_progress_diagnostic(
                self.sent_audio_access_units,
                transport.outbound_audio_sent_range(),
            ) {
                if diagnostic.packets_sent == u32::from(P5_PROBE_FRAME_COUNT) {
                    eprintln!(
                        "[audio-in] P5 探针发送完成: sent={}, first-extended-sequence={}, last-extended-sequence={}, ssrc={}",
                        diagnostic.packets_sent,
                        diagnostic.first_extended_sequence,
                        diagnostic.last_extended_sequence,
                        diagnostic.ssrc
                    );
                } else {
                    eprintln!(
                        "[audio-in] P5 探针发送进度: sent={}, first-extended-sequence={}, last-extended-sequence={}, ssrc={}",
                        diagnostic.packets_sent,
                        diagnostic.first_extended_sequence,
                        diagnostic.last_extended_sequence,
                        diagnostic.ssrc
                    );
                }
            }
        }
        self.capture_audio_input_confirmation_diagnostic(transport);
    }

    fn observe_audio_input_transport(
        &mut self,
        transport: &MediaTransport,
        generation: u64,
        now: Instant,
    ) {
        if self.audio_flow != AudioMediaFlow::PcToMac {
            return;
        }
        let before = self.audio_input_runtime.phase().clone();
        self.audio_input_runtime
            .observe_transport_evidence(generation, transport.audio_reception_evidence());
        self.audio_input_runtime.poll(generation, now);
        match self.audio_input_runtime.phase() {
            AudioInputPhase::ProbeConfirmed { .. } => {
                self.audio_encoder = None;
            }
            AudioInputPhase::Degraded { reason, .. } => {
                let reason = reason.clone();
                if !matches!(before, AudioInputPhase::Degraded { .. }) {
                    self.degrade_audio_input(generation, reason);
                }
            }
            _ => {}
        }
        self.capture_audio_input_confirmation_diagnostic(transport);
    }

    fn capture_audio_input_confirmation_diagnostic(&mut self, transport: &MediaTransport) {
        if self.audio_input_confirmation_diagnostic_reported
            || !matches!(
                self.audio_input_phase(),
                AudioInputPhase::ProbeConfirmed { .. }
            )
        {
            return;
        }
        let Some(range) = transport.outbound_audio_sent_range() else {
            return;
        };
        let AudioReceptionEvidence::Confirmed {
            extended_highest_sequence,
            cumulative_packets_lost,
        } = transport.audio_reception_evidence()
        else {
            return;
        };
        self.pending_audio_input_confirmation_diagnostic = Some(AudioInputConfirmationDiagnostic {
            packets_sent: range.packets_sent,
            first_extended_sequence: range.first_extended_sequence,
            last_extended_sequence: range.last_extended_sequence,
            ssrc: range.ssrc,
            srtcp_extended_highest_sequence: extended_highest_sequence,
            srtcp_cumulative_packets_lost: cumulative_packets_lost,
        });
        self.audio_input_confirmation_diagnostic_reported = true;
        self.audio_encoder = None;
    }

    fn take_audio_input_confirmation_diagnostic(
        &mut self,
    ) -> Option<AudioInputConfirmationDiagnostic> {
        self.pending_audio_input_confirmation_diagnostic.take()
    }

    fn degrade_audio_input(&mut self, generation: u64, reason: String) {
        self.audio_input_runtime.fail_probe(generation, reason);
        self.audio_encoder = None;
        eprintln!(
            "[audio-in] P5 探针已降级，视频/控制与 Mac→PC 音频继续: phase={:?}",
            self.audio_input_phase()
        );
    }

    fn teardown_audio_input(&mut self) {
        self.audio_encoder = None;
        self.audio_input_runtime.teardown();
        self.pending_audio_input_confirmation_diagnostic = None;
        self.audio_input_confirmation_diagnostic_reported = false;
    }

    fn audio_output_phase(&self) -> &AudioOutputPhase {
        &self.audio_output_phase
    }

    fn degrade_audio(&mut self, error: anyhow::Error) -> MediaAcceptOutcome {
        if matches!(self.audio_output_phase(), AudioOutputPhase::Degraded { .. }) {
            return MediaAcceptOutcome::Discarded;
        }
        let reason = format!("{error:#}");
        self.audio_playback = None;
        self.audio_output_phase = AudioOutputPhase::Degraded {
            reason: reason.clone(),
        };
        eprintln!("[audio-out] Mac→PC 音频已降级，本 generation 不再重试: {reason}");
        MediaAcceptOutcome::AudioDegraded
    }

    fn reset_generation(&mut self) -> Result<()> {
        self.teardown_audio_input();
        let receiver = ArdAudioReceiver::new().context("重建 Mac→PC AAC-ELD 接收器失败")?;
        self.audio_receiver = receiver;
        self.audio_playback = None;
        self.audio_output_phase = AudioOutputPhase::ReadyToStart;
        Ok(())
    }

    fn output_decoded_audio(&mut self, decoded: &DecodedAudioPacket) -> Result<()> {
        if self.audio_playback.is_none() {
            let playback = AudioPlayback::open_default().context("打开 Mac→PC 音频输出设备失败")?;
            eprintln!(
                "[audio-out] 已启用 48 kHz 双声道 AAC-ELD 播放: {}",
                playback.device_description()
            );
            self.audio_playback = Some(playback);
        }
        self.audio_playback
            .as_ref()
            .context("Mac→PC 音频输出状态为 Active 但播放设备缺失")?
            .enqueue_interleaved_stereo(&decoded.pcm)
            .context("Mac→PC 音频 PCM 入队失败")?;
        Ok(())
    }

    fn apply_decoded_audio_with_output<F>(
        &mut self,
        decoded: DecodedAudioPacket,
        output: F,
    ) -> Result<()>
    where
        F: FnOnce(&mut Self, &DecodedAudioPacket) -> Result<()>,
    {
        output(self, &decoded)?;
        self.audio_output_phase = AudioOutputPhase::Active;
        if decoded.pcm.iter().any(|sample| *sample != 0) {
            self.non_silent_audio_access_units =
                self.non_silent_audio_access_units.saturating_add(1);
            if self.non_silent_audio_access_units == 1 {
                eprintln!("[audio-out] 首个非静音 AAC-ELD access unit 已输出到 Windows 音频设备");
            }
        }
        self.authenticated_audio_packets = self.authenticated_audio_packets.saturating_add(1);
        self.concealed_audio_access_units = self
            .concealed_audio_access_units
            .saturating_add(decoded.concealed_access_units as u64);
        if self.authenticated_audio_packets == 1 {
            eprintln!(
                "[audio-out] 首个认证 audio RTP 已解码: sequence={} timestamp={} ssrc=0x{:08x}",
                decoded.sequence, decoded.timestamp, decoded.ssrc
            );
        }
        Ok(())
    }

    fn accept_audio_outcome_with_output<F>(
        &mut self,
        outcome: AudioReceiveOutcome,
        output: F,
    ) -> MediaAcceptOutcome
    where
        F: FnOnce(&mut Self, &DecodedAudioPacket) -> Result<()>,
    {
        let decoded = match outcome {
            AudioReceiveOutcome::DiscardedLate { .. } => {
                self.late_audio_packets = self.late_audio_packets.saturating_add(1);
                return MediaAcceptOutcome::Discarded;
            }
            AudioReceiveOutcome::Decoded(decoded) => decoded,
            AudioReceiveOutcome::Resynchronized {
                decoded,
                skipped_access_units,
            } => {
                self.audio_resynchronizations = self.audio_resynchronizations.saturating_add(1);
                if should_log_audio_resynchronization(self.audio_resynchronizations) {
                    eprintln!(
                        "[audio-out] Mac→PC AAC-ELD 重同步 #{}: 跳过 {} 个 access unit",
                        self.audio_resynchronizations, skipped_access_units
                    );
                }
                decoded
            }
        };
        match self.apply_decoded_audio_with_output(decoded, output) {
            Ok(()) => MediaAcceptOutcome::Applied,
            Err(error) => self.degrade_audio(error),
        }
    }

    fn accept_audio_outcome(&mut self, outcome: AudioReceiveOutcome) -> MediaAcceptOutcome {
        self.accept_audio_outcome_with_output(outcome, |state, decoded| {
            state.output_decoded_audio(decoded)
        })
    }

    fn accept(&mut self, role: MediaRole, datagram: MediaDatagram) -> Result<MediaAcceptOutcome> {
        let outcome = match (role, datagram) {
            (MediaRole::Audio, MediaDatagram::Rtp(packet)) => {
                if matches!(self.audio_output_phase(), AudioOutputPhase::Degraded { .. }) {
                    return Ok(MediaAcceptOutcome::Discarded);
                }
                match self.audio_receiver.decode_rtp_packet(&packet) {
                    Ok(audio_outcome) => self.accept_audio_outcome(audio_outcome),
                    Err(error) => self.degrade_audio(error),
                }
            }
            (MediaRole::Audio, MediaDatagram::Rtcp(_)) => MediaAcceptOutcome::Applied,
            (MediaRole::VideoStream1 | MediaRole::VideoStream2, MediaDatagram::Rtp(_)) => {
                self.authenticated_video_packets =
                    self.authenticated_video_packets.saturating_add(1);
                if self.authenticated_video_packets == 1 {
                    eprintln!("[hpss-view] 已收到首个认证 video RTP 数据报");
                }
                MediaAcceptOutcome::AuthenticatedNotRendered
            }
            (MediaRole::VideoStream1 | MediaRole::VideoStream2, MediaDatagram::Rtcp(_)) => {
                MediaAcceptOutcome::Applied
            }
        };
        Ok(outcome)
    }
}

fn drain_udp_media(
    transport: &mut MediaTransport,
    media_state: &mut ViewerMediaState,
    generation: u64,
) -> Result<usize> {
    if transport.phase() != MediaTransportPhase::Active {
        return Ok(0);
    }
    let summary = transport.drain_receive_round(generation, |role, datagram| {
        media_state.accept(role, datagram).map(|_| ())
    })?;
    Ok(summary.accepted_total)
}

/// 显示 generation 与其像素必须在同一把锁下读取或替换。
struct DisplaySurface {
    generation: u64,
    framebuffer: Framebuffer,
    native_mvs_observability: NativeMvsRenderObservability,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct NativeMvsRenderObservability {
    type_zero_applied_count: u64,
    content_revision: u64,
    first_nonblack_render_revision: Option<u64>,
}

impl DisplaySurface {
    fn new(generation: u64, framebuffer: Framebuffer) -> Self {
        Self {
            generation,
            framebuffer,
            native_mvs_observability: NativeMvsRenderObservability::default(),
        }
    }

    fn record_native_type_zero_applied(&mut self) -> NativeMvsRenderObservability {
        self.native_mvs_observability.type_zero_applied_count = self
            .native_mvs_observability
            .type_zero_applied_count
            .saturating_add(1);
        self.native_mvs_observability.content_revision = self
            .native_mvs_observability
            .content_revision
            .saturating_add(1);
        self.native_mvs_observability
    }

    fn record_native_partial_applied(&mut self) -> NativeMvsRenderObservability {
        self.native_mvs_observability.content_revision = self
            .native_mvs_observability
            .content_revision
            .saturating_add(1);
        self.native_mvs_observability
    }
}

struct DynamicResolutionRuntime {
    controller: DynamicResolutionController,
    pending_since: Option<Instant>,
    initial_size: DisplaySize,
    opt_in: bool,
    evidence: DynamicCapabilityEvidence,
    armed: bool,
}

#[derive(Default)]
struct DynamicCapabilityEvidence {
    controlling_role: bool,
    matching_initial_server_state: bool,
    current_full_media_applied: bool,
    non_paused_media_activity: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TargetDisposition {
    Ready,
    Duplicate,
    Wait,
}

impl DynamicResolutionRuntime {
    fn new(initial_size: DisplaySize, enabled: bool) -> Self {
        // hpssview 是本地明确选择的交互控制角色；其余 Apple capability 谓词
        // 必须由本会话可观察事件逐步满足。MVS 活动只作为本实验的 active-media
        // 证据，不宣称它静态证明 Apple 私有的 isUsingAVCMediaStream 谓词。
        let capability = DynamicResolutionCapability::new(false, false, true, true);
        Self {
            controller: DynamicResolutionController::new(initial_size, enabled, capability),
            pending_since: None,
            initial_size,
            opt_in: enabled,
            evidence: DynamicCapabilityEvidence {
                controlling_role: true,
                ..Default::default()
            },
            armed: false,
        }
    }

    fn maybe_arm(&mut self) -> bool {
        if self.armed
            || !self.evidence.controlling_role
            || !self.evidence.matching_initial_server_state
            || !self.evidence.current_full_media_applied
            || !self.evidence.non_paused_media_activity
        {
            return false;
        }
        self.controller = DynamicResolutionController::new(
            self.initial_size,
            self.opt_in,
            DynamicResolutionCapability::new(
                self.evidence.current_full_media_applied,
                self.evidence.matching_initial_server_state,
                self.evidence.controlling_role,
                !self.evidence.non_paused_media_activity,
            ),
        );
        self.armed = true;
        self.opt_in
    }

    fn observe_initial_server_state(
        &mut self,
        observed: DisplaySize,
        current_surface: DisplaySize,
    ) -> bool {
        if !self.armed && current_surface == self.initial_size && observed == current_surface {
            self.evidence.matching_initial_server_state = true;
        }
        self.maybe_arm()
    }

    fn observe_full_applied(&mut self, generation: u64, current_surface: DisplaySize) -> bool {
        if !self.armed && generation == 0 && current_surface == self.initial_size {
            self.evidence.current_full_media_applied = true;
            self.evidence.non_paused_media_activity = true;
            return self.maybe_arm();
        }
        self.controller.mark_full_frame(generation)
    }

    fn target_disposition(&self, target: DisplaySize) -> TargetDisposition {
        match self.controller.state() {
            crate::vnc::dynamic_resolution::DynamicResolutionState::Stable { size, .. }
                if *size == target =>
            {
                TargetDisposition::Duplicate
            }
            crate::vnc::dynamic_resolution::DynamicResolutionState::Stable { .. } => {
                TargetDisposition::Ready
            }
            _ => TargetDisposition::Wait,
        }
    }

    /// 调用方必须持有 runtime mutex。发送成功前 controller 保持 Stable，
    /// 因而 reader 既无法取得 mutex，也看不到尚未上 wire 的 Pending。
    fn send_target_with<F>(
        &mut self,
        target: DisplaySize,
        send: F,
    ) -> Result<Option<ResolutionRequest>>
    where
        F: FnOnce(DisplaySize) -> Result<Instant>,
    {
        if self.target_disposition(target) != TargetDisposition::Ready {
            return Ok(None);
        }
        let sent_at = send(target)?;
        let request = self
            .controller
            .request_target(target)
            .context("动态分辨率发送成功后无法激活 Pending")?;
        self.pending_since = Some(sent_at);
        Ok(Some(request))
    }

    fn observe_server_state(&mut self, size: DisplaySize) -> Option<GeometryCommit> {
        let commit = self.controller.observe_server_state(size)?;
        self.pending_since = None;
        Some(commit)
    }

    fn timeout_pending(&mut self, now: Instant) -> bool {
        let Some(since) = self.pending_since else {
            return false;
        };
        if now.duration_since(since) < Duration::from_secs(2) {
            return false;
        }
        let timed_out = self.controller.timeout_pending();
        if timed_out {
            self.pending_since = None;
        }
        timed_out
    }
}

#[derive(Default)]
struct ViewportRequestQueue {
    latest: Option<(DisplaySize, Instant)>,
}

impl ViewportRequestQueue {
    fn observe(&mut self, target: DisplaySize, now: Instant) {
        self.latest = Some((target, now));
    }

    fn service<F>(
        &mut self,
        runtime: &mut DynamicResolutionRuntime,
        now: Instant,
        send: F,
    ) -> Result<Option<ResolutionRequest>>
    where
        F: FnOnce(DisplaySize) -> Result<Instant>,
    {
        let Some((target, since)) = self.latest else {
            return Ok(None);
        };
        if now.duration_since(since) < Duration::from_millis(250) {
            return Ok(None);
        }
        match runtime.target_disposition(target) {
            TargetDisposition::Wait => Ok(None),
            TargetDisposition::Duplicate => {
                self.latest = None;
                Ok(None)
            }
            TargetDisposition::Ready => {
                let request = runtime.send_target_with(target, send)?;
                if request.is_some() {
                    self.latest = None;
                }
                Ok(request)
            }
        }
    }

    fn drop_latest(&mut self) {
        self.latest = None;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ContentViewport {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

impl ContentViewport {
    /// 用整数比例把远端 surface 居中放入 drawable；奇数黑边余量留在右/下侧。
    fn fit(source_size: (usize, usize), drawable_size: (usize, usize)) -> Self {
        let source_width = source_size.0.max(1);
        let source_height = source_size.1.max(1);
        let drawable_width = drawable_size.0.max(1);
        let drawable_height = drawable_size.1.max(1);

        let drawable_by_source_height = (drawable_width as u128)
            .checked_mul(source_height as u128)
            .unwrap_or(u128::MAX);
        let drawable_height_by_source_width = (drawable_height as u128)
            .checked_mul(source_width as u128)
            .unwrap_or(u128::MAX);
        let (width, height) = if drawable_by_source_height <= drawable_height_by_source_width {
            let scaled_height = (source_height as u128)
                .checked_mul(drawable_width as u128)
                .and_then(|value| value.checked_div(source_width as u128))
                .unwrap_or(drawable_height as u128)
                .clamp(1, drawable_height as u128);
            (
                drawable_width,
                usize::try_from(scaled_height).unwrap_or(drawable_height),
            )
        } else {
            let scaled_width = (source_width as u128)
                .checked_mul(drawable_height as u128)
                .and_then(|value| value.checked_div(source_height as u128))
                .unwrap_or(drawable_width as u128)
                .clamp(1, drawable_width as u128);
            (
                usize::try_from(scaled_width).unwrap_or(drawable_width),
                drawable_height,
            )
        };

        Self {
            x: (drawable_width - width) / 2,
            y: (drawable_height - height) / 2,
            width,
            height,
        }
    }
}

/// 把当前窗口内容视口坐标映射到当前远端显示坐标；黑边会夹到最近内容边界。
fn map_pointer(
    window_x: f32,
    window_y: f32,
    window_size: (usize, usize),
    display_size: DisplaySize,
) -> (u16, u16) {
    fn map_axis(value: f32, origin: usize, extent: usize, display: u16) -> u16 {
        if extent <= 1 || display <= 1 || !value.is_finite() {
            return 0;
        }
        let content_max = origin.saturating_add(extent - 1) as f64;
        let content_origin = origin as f64;
        let content_extent_max = (extent - 1) as f64;
        let display_max = (display - 1) as f64;
        let clamped = (value as f64).clamp(content_origin, content_max) - content_origin;
        (clamped * display_max / content_extent_max).round() as u16
    }

    if window_size.0 == 0 || window_size.1 == 0 {
        return (0, 0);
    }
    let viewport = ContentViewport::fit(
        (display_size.width as usize, display_size.height as usize),
        window_size,
    );
    (
        map_axis(window_x, viewport.x, viewport.width, display_size.width),
        map_axis(window_y, viewport.y, viewport.height, display_size.height),
    )
}

/// 只有控制器返回精确确认时才提交新 surface，且每个 generation 只提交一次。
fn commit_server_geometry(
    runtime: &mut DynamicResolutionRuntime,
    receiver: &mut MvsReceiveState,
    surface: &mut DisplaySurface,
    media_state: &mut ViewerMediaState,
    observed: DisplaySize,
) -> Option<GeometryCommit> {
    let replacement = Framebuffer::new(observed.width as usize, observed.height as usize).ok()?;
    let commit = runtime.observe_server_state(observed)?;
    if surface.generation.checked_add(1) != Some(commit.generation) {
        return None;
    }
    if let Err(error) = media_state.reset_generation() {
        let _ = media_state.degrade_audio(error);
    }
    receiver.reset(commit.generation);
    *surface = DisplaySurface::new(commit.generation, replacement);
    Some(commit)
}

fn apply_rgb_rect_for_generation(
    surface: &mut DisplaySurface,
    generation: u64,
    rgb: &[u8],
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> Result<bool> {
    if surface.generation != generation {
        return Ok(false);
    }
    apply_rgb_rect(&mut surface.framebuffer, rgb, x, y, width, height)?;
    Ok(true)
}

/// Viewer 读线程持有的、generation 绑定的 MVS 接收状态。
struct MvsReceiveState {
    assembler: MvsRecordAssembler,
    decoder: mvs::MvsDecodeState,
    generation: u64,
    incomplete_since: Option<Instant>,
}

impl MvsReceiveState {
    fn new(generation: u64) -> Self {
        Self {
            assembler: MvsRecordAssembler::default(),
            decoder: mvs::MvsDecodeState::new(generation),
            generation,
            incomplete_since: None,
        }
    }

    fn begin(&mut self, rect: MvsRect, total: u32, first: &[u8]) -> Result<Option<MvsRecord>> {
        self.begin_at(rect, total, first, Instant::now())
    }

    fn begin_at(
        &mut self,
        rect: MvsRect,
        total: u32,
        first: &[u8],
        now: Instant,
    ) -> Result<Option<MvsRecord>> {
        let result = self.assembler.begin(rect, total, first);
        self.incomplete_since = if matches!(result, Ok(None)) && self.assembler.is_pending() {
            Some(now)
        } else {
            None
        };
        result
    }

    fn push_continuation(&mut self, chunk: &[u8]) -> Result<Option<MvsRecord>> {
        let result = self.assembler.push_continuation(chunk);
        if !matches!(result, Ok(None)) {
            self.incomplete_since = None;
        }
        result
    }

    fn is_pending(&self) -> bool {
        self.assembler.is_pending()
    }

    fn reset(&mut self, generation: u64) {
        self.assembler.abort();
        self.incomplete_since = None;
        self.decoder.reset(generation);
        self.generation = generation;
    }

    fn install_tables(&mut self, payload: &[u8]) -> Result<()> {
        self.decoder.install_tables(self.generation, payload)
    }

    fn prepare(
        &mut self,
        payload: &[u8],
        width: u16,
        height: u16,
    ) -> Result<mvs::MvsDecodeDecision> {
        self.decoder
            .prepare(self.generation, payload, width, height)
    }

    fn prepare_rect(
        &mut self,
        payload: &[u8],
        rect: MvsRect,
        display_size: DisplaySize,
    ) -> Result<mvs::MvsDecodeDecision> {
        self.decoder.prepare_rect(
            self.generation,
            payload,
            rect,
            display_size.width,
            display_size.height,
        )
    }

    fn commit(&mut self, prepared: mvs::PreparedGenerationMvs) -> Result<()> {
        self.decoder.commit(prepared).map(|_| ())
    }

    fn commit_opaque(
        &mut self,
        prepared: mvs::PreparedOpaqueMvsState,
    ) -> Result<crate::vnc::mvs_full::PreparedPartialPixels> {
        self.decoder.commit_opaque(prepared)
    }

    fn request_full(&mut self) -> Result<()> {
        self.assembler.abort();
        self.incomplete_since = None;
        self.decoder.request_full(self.generation)
    }

    fn timeout_incomplete(&mut self, now: Instant) -> Result<bool> {
        let Some(since) = self.incomplete_since else {
            return Ok(false);
        };
        if now.checked_duration_since(since).unwrap_or_default() < MVS_INCOMPLETE_TIMEOUT {
            return Ok(false);
        }
        self.request_full()?;
        Ok(true)
    }

    fn awaiting_full(&self) -> bool {
        self.decoder.awaiting_full()
    }

    fn reject_truncated_mvs_envelope(&mut self, msg: &[u8]) -> Result<bool> {
        if hpss::is_truncated_mvs_envelope(msg) {
            self.request_full()?;
            return Ok(true);
        }
        Ok(false)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TableScheduleStatus {
    Scheduled,
    AlreadyScheduled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TableFollowupState {
    None,
    Scheduled { generation: u64, due: Instant },
    Sent { generation: u64 },
}

struct ReaderRequestState {
    generation: u64,
    framebuffer_request_in_flight: bool,
    last_full_request: Option<Instant>,
    table_followup: TableFollowupState,
}

impl ReaderRequestState {
    fn after_startup(sent_at: Instant) -> Self {
        Self {
            generation: 0,
            framebuffer_request_in_flight: true,
            last_full_request: Some(sent_at),
            table_followup: TableFollowupState::None,
        }
    }

    fn framebuffer_request_in_flight(&self) -> bool {
        self.framebuffer_request_in_flight
    }

    fn consume_mvs_response(&mut self) {
        self.framebuffer_request_in_flight = false;
    }

    fn mark_incremental_request_sent(&mut self) {
        self.framebuffer_request_in_flight = true;
    }

    fn mark_full_request_sent(&mut self, sent_at: Instant) {
        self.framebuffer_request_in_flight = true;
        self.last_full_request = Some(sent_at);
    }

    fn reset_generation(&mut self, generation: u64) {
        self.generation = generation;
        self.framebuffer_request_in_flight = false;
        self.table_followup = TableFollowupState::None;
    }

    fn on_valid_table_record(
        &mut self,
        generation: u64,
        arrived_at: Instant,
    ) -> Result<TableScheduleStatus> {
        if generation != self.generation {
            bail!("MVS 表 follow-up 属于过期 generation {generation}");
        }
        match self.table_followup {
            TableFollowupState::None => {
                let rate_due = self
                    .last_full_request
                    .map(|last| last + Duration::from_millis(200))
                    .unwrap_or(arrived_at);
                let due = (arrived_at + Duration::from_millis(200)).max(rate_due);
                self.table_followup = TableFollowupState::Scheduled { generation, due };
                Ok(TableScheduleStatus::Scheduled)
            }
            TableFollowupState::Scheduled {
                generation: scheduled,
                ..
            } if scheduled == generation => Ok(TableScheduleStatus::AlreadyScheduled),
            TableFollowupState::Sent {
                generation: sent, ..
            } if sent == generation => {
                bail!("同一 generation 在 table follow-up 后再次返回 table-only 响应")
            }
            _ => bail!("MVS table follow-up generation 状态不一致"),
        }
    }

    fn table_followup_due(&self) -> Option<Instant> {
        match self.table_followup {
            TableFollowupState::Scheduled { due, .. } => Some(due),
            _ => None,
        }
    }

    fn table_followup_blocks_dynamic(&self) -> bool {
        !matches!(self.table_followup, TableFollowupState::None)
    }

    fn mark_table_followup_sent(&mut self, sent_at: Instant) -> Result<()> {
        let TableFollowupState::Scheduled { generation, .. } = self.table_followup else {
            bail!("MVS table follow-up 未处于 Scheduled")
        };
        self.table_followup = TableFollowupState::Sent { generation };
        self.mark_full_request_sent(sent_at);
        Ok(())
    }

    fn on_full_applied(&mut self, generation: u64) -> Result<()> {
        if generation != self.generation {
            bail!("MVS full boundary 属于过期 generation {generation}");
        }
        self.table_followup = TableFollowupState::None;
        Ok(())
    }

    fn send_rate_limited_full_at<S, W>(&mut self, now: Instant, sleep: S, write: W) -> Result<()>
    where
        S: FnMut(Duration),
        W: FnMut() -> Result<()>,
    {
        request_full_update_at(&mut self.last_full_request, now, sleep, write)?;
        self.framebuffer_request_in_flight = true;
        Ok(())
    }
}

#[derive(Default)]
struct ReaderTickOutcome {
    incomplete_recovered: bool,
    dynamic_timed_out: bool,
    table_followup_sent: bool,
    dynamic_request: Option<ResolutionRequest>,
}

#[allow(
    clippy::too_many_arguments,
    reason = "纯状态机测试需要显式注入四组状态与三条无 I/O 副作用的回调"
)]
fn service_reader_tick_at<S, FW, DW>(
    receiver: &mut MvsReceiveState,
    requests: &mut ReaderRequestState,
    queue: &mut ViewportRequestQueue,
    runtime: &mut DynamicResolutionRuntime,
    now: Instant,
    mut sleep: S,
    mut write_full: FW,
    send_dynamic: DW,
) -> Result<ReaderTickOutcome>
where
    S: FnMut(Duration),
    FW: FnMut() -> Result<()>,
    DW: FnMut(DisplaySize) -> Result<Instant>,
{
    let mut outcome = ReaderTickOutcome::default();

    if receiver.timeout_incomplete(now)? {
        requests.consume_mvs_response();
        requests.send_rate_limited_full_at(now, &mut sleep, &mut write_full)?;
        outcome.incomplete_recovered = true;
    }

    if runtime.timeout_pending(now) {
        queue.drop_latest();
        outcome.dynamic_timed_out = true;
    }

    if !outcome.incomplete_recovered
        && !receiver.is_pending()
        && !requests.framebuffer_request_in_flight()
        && runtime.pending_since.is_none()
        && requests.table_followup_due().is_some_and(|due| now >= due)
    {
        write_full()?;
        requests.mark_table_followup_sent(now)?;
        outcome.table_followup_sent = true;
    }

    if !outcome.incomplete_recovered
        && !outcome.table_followup_sent
        && !receiver.is_pending()
        && !requests.framebuffer_request_in_flight()
        && !requests.table_followup_blocks_dynamic()
    {
        outcome.dynamic_request = queue.service(runtime, now, send_dynamic)?;
    }

    Ok(outcome)
}

struct FullBoundaryOutcome {
    dynamic_request: Option<ResolutionRequest>,
    incremental_sent: bool,
}

fn finish_partial_boundary_at<I>(
    requests: &mut ReaderRequestState,
    mut send_incremental: I,
) -> Result<bool>
where
    I: FnMut() -> Result<()>,
{
    if requests.framebuffer_request_in_flight() {
        return Ok(false);
    }
    send_incremental()?;
    requests.mark_incremental_request_sent();
    Ok(true)
}

#[allow(
    clippy::too_many_arguments,
    reason = "full 边界事务显式携带四组状态及各类写回调以验证严格调用顺序"
)]
fn finish_full_boundary_at<S, FW, DW, IW>(
    receiver: &mut MvsReceiveState,
    requests: &mut ReaderRequestState,
    queue: &mut ViewportRequestQueue,
    runtime: &mut DynamicResolutionRuntime,
    now: Instant,
    sleep: S,
    write_full: FW,
    send_dynamic: DW,
    mut send_incremental: IW,
) -> Result<FullBoundaryOutcome>
where
    S: FnMut(Duration),
    FW: FnMut() -> Result<()>,
    DW: FnMut(DisplaySize) -> Result<Instant>,
    IW: FnMut() -> Result<()>,
{
    requests.on_full_applied(receiver.generation)?;
    let tick = service_reader_tick_at(
        receiver,
        requests,
        queue,
        runtime,
        now,
        sleep,
        write_full,
        send_dynamic,
    )?;
    let incremental_sent =
        if tick.dynamic_request.is_none() && !requests.framebuffer_request_in_flight() {
            send_incremental()?;
            requests.mark_incremental_request_sent();
            true
        } else {
            false
        };
    Ok(FullBoundaryOutcome {
        dynamic_request: tick.dynamic_request,
        incremental_sent,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReaderFrameClass {
    Continuation,
    ServerKeepalive,
    Query,
    ControlOrMedia,
}

fn reader_frame_class(receiver: &MvsReceiveState, msg: &[u8]) -> ReaderFrameClass {
    // Apple session frames preserve message boundaries, and the server may
    // interleave a complete MediaStream control message between MVS chunks.
    // Only a fully validated 0x3f2 message may preempt the opaque continuation
    // rule; heartbeat/query-shaped bytes remain MVS payload while reassembling.
    let interleaved_media_control = matches!(
        parse_media(msg),
        Ok(Media::PortAnnouncement(_) | Media::StreamAnswer(_))
    );
    if receiver.is_pending() && !interleaved_media_control {
        return ReaderFrameClass::Continuation;
    }
    match msg.first().copied() {
        Some(protocol::apple_session::SERVER_KEEPALIVE_MESSAGE_TYPE) => {
            ReaderFrameClass::ServerKeepalive
        }
        Some(hpss::msg::QUERY_08) => ReaderFrameClass::Query,
        _ => ReaderFrameClass::ControlOrMedia,
    }
}

fn describe_media_frame_for_diagnostics(msg: &[u8]) -> String {
    match parse_media(msg) {
        Ok(Media::PortAnnouncement(_)) => "MediaStreamMessage1".to_owned(),
        Ok(Media::StreamAnswer(_)) => "MediaStreamMessage2".to_owned(),
        Ok(Media::Mvs { .. }) => "MVS".to_owned(),
        Ok(Media::Cursor { .. }) => "Cursor".to_owned(),
        Ok(Media::State(encoding)) => format!("state-encoding-0x{encoding:08x}"),
        Err(error) => format!("unparsed-{error}"),
    }
}

fn incremental_request_after_full_apply(
    width: u16,
    height: u16,
) -> [u8; protocol::FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES] {
    protocol::msg_fb_update_request(true, 0, 0, width, height)
}

fn request_full_update(
    write_stream: &Arc<Mutex<std::net::TcpStream>>,
    crypto: &Arc<Mutex<crate::vnc::session::SessionCrypto>>,
    requests: &mut ReaderRequestState,
    width: u16,
    height: u16,
) -> Result<()> {
    let now = Instant::now();
    let req = protocol::msg_fb_update_request(false, 0, 0, width, height);
    requests.send_rate_limited_full_at(now, thread::sleep, || {
        send_encrypted(write_stream, crypto, &req)
    })
}

fn request_full_update_at<S, W>(
    last_full_request: &mut Option<Instant>,
    now: Instant,
    sleep: S,
    write: W,
) -> Result<()>
where
    S: FnOnce(Duration),
    W: FnOnce() -> Result<()>,
{
    let limit = Duration::from_millis(200);
    let delay = last_full_request
        .and_then(|last| (last + limit).checked_duration_since(now))
        .unwrap_or_default();
    if !delay.is_zero() {
        sleep(delay);
    }
    write()?;
    *last_full_request = Some(now + delay);
    Ok(())
}

fn current_surface_size(surface: &Arc<Mutex<DisplaySurface>>) -> DisplaySize {
    let surface = surface.lock().unwrap();
    DisplaySize::new(
        surface.framebuffer.width as u16,
        surface.framebuffer.height as u16,
    )
    .expect("DisplaySurface dimensions are non-zero u16 values")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MvsRecordOutcome {
    TableInstalled,
    FullApplied,
    PartialApplied,
    RecoveryRequested,
    Ignored,
}

fn is_complete_surface_frame(rect: MvsRect, display_size: DisplaySize) -> bool {
    rect.x == 0
        && rect.y == 0
        && rect.width == display_size.width
        && rect.height == display_size.height
}

/// 校验完成记录的矩形；无效远端输入只推进到 fail-closed 全量重同步，不能杀死读线程。
fn mark_recovery_for_invalid_mvs_geometry(
    receiver: &mut MvsReceiveState,
    rect: MvsRect,
    display_size: DisplaySize,
) -> Result<bool> {
    if let Err(error) = crate::vnc::mvs_stream::validate_mvs_rect_against_surface(
        rect,
        display_size.width,
        display_size.height,
    ) {
        eprintln!("[hpss-view] MVS 矩形无效，重同步: {error:#}");
        receiver.request_full()?;
        return Ok(false);
    }
    Ok(true)
}

/// 在私有 staging framebuffer 上应用像素，decoder commit 成功后才发布 surface。
/// `receiver` 只属于 reader 线程；持有 surface 锁期间不进行任何 socket I/O。
fn apply_prepared_mvs_to_surface_with<F>(
    receiver: &mut MvsReceiveState,
    surface: &mut DisplaySurface,
    prepared: mvs::PreparedGenerationMvs,
    rect: MvsRect,
    apply: F,
) -> Result<()>
where
    F: FnOnce(&mut Framebuffer, &[u8], usize, usize, usize, usize) -> Result<()>,
{
    if surface.generation != receiver.generation {
        bail!(
            "MVS prepared frame generation 与当前 surface 不一致: receiver={}, surface={}",
            receiver.generation,
            surface.generation
        );
    }
    let decoded = prepared.decoded();
    if decoded.width != usize::from(rect.width) || decoded.height != usize::from(rect.height) {
        bail!("MVS 原生解码矩形与 wire 矩形不一致");
    }
    let mut staged = Framebuffer::new(surface.framebuffer.width, surface.framebuffer.height)?;
    staged
        .pixels_mut()
        .copy_from_slice(surface.framebuffer.pixels());
    apply(
        &mut staged,
        &decoded.rgb,
        usize::from(rect.x),
        usize::from(rect.y),
        usize::from(rect.width),
        usize::from(rect.height),
    )?;
    #[cfg(test)]
    tests::commit_prepared_mvs(receiver, prepared)?;
    #[cfg(not(test))]
    receiver.commit(prepared)?;
    surface.framebuffer = staged;
    Ok(())
}

fn validate_partial_pixels_for_framebuffer(
    framebuffer: &Framebuffer,
    partial: &crate::vnc::mvs_full::PreparedPartialPixels,
) -> Result<()> {
    use crate::vnc::mvs_full::PartialPixelOperation;

    for operation in &partial.operations {
        match operation {
            PartialPixelOperation::Replace {
                x,
                y,
                width,
                height,
                rgb,
            } => {
                if *width == 0 || *height == 0 || *width > 8 || *height > 8 {
                    bail!("MVS type-1 replace tile 尺寸非法: {width}x{height}");
                }
                let width_u16 = u16::try_from(*width).context("MVS type-1 replace 宽度溢出")?;
                let height_u16 = u16::try_from(*height).context("MVS type-1 replace 高度溢出")?;
                mvs::validate_decoded_rgb_layout(width_u16, height_u16, rgb.len())?;
                if x.checked_add(*width)
                    .is_none_or(|right| right > framebuffer.width)
                    || y.checked_add(*height)
                        .is_none_or(|bottom| bottom > framebuffer.height)
                {
                    bail!("MVS type-1 framebuffer replace 超出 surface");
                }
            }
            PartialPixelOperation::Copy {
                source_x,
                source_y,
                destination_x,
                destination_y,
                width,
                height,
            } => {
                if *width == 0 || *height == 0 || *width > 8 || *height > 8 {
                    bail!("MVS type-1 copy tile 尺寸非法: {width}x{height}");
                }
                if source_x
                    .checked_add(*width)
                    .is_none_or(|right| right > framebuffer.width)
                    || source_y
                        .checked_add(*height)
                        .is_none_or(|bottom| bottom > framebuffer.height)
                    || destination_x
                        .checked_add(*width)
                        .is_none_or(|right| right > framebuffer.width)
                    || destination_y
                        .checked_add(*height)
                        .is_none_or(|bottom| bottom > framebuffer.height)
                {
                    bail!("MVS type-1 framebuffer copy 超出 surface");
                }
            }
        }
    }
    Ok(())
}

/// 只接收已经通过 `validate_partial_pixels_for_framebuffer` 的 decoder 输出。
/// replace 与 copy 都是固定 8x8 上限，因此提交后不再分配内存或返回错误。
fn apply_validated_partial_pixels_to_framebuffer(
    framebuffer: &mut Framebuffer,
    partial: &crate::vnc::mvs_full::PreparedPartialPixels,
) {
    use crate::vnc::mvs_full::PartialPixelOperation;

    for operation in &partial.operations {
        match operation {
            PartialPixelOperation::Replace {
                x,
                y,
                width,
                height,
                rgb,
            } => {
                debug_assert!(*width > 0 && *height > 0 && *width <= 8 && *height <= 8);
                debug_assert_eq!(rgb.len(), width * height * mvs::MVS_RGB_CHANNEL_BYTES);
                debug_assert!(*x + *width <= framebuffer.width);
                debug_assert!(*y + *height <= framebuffer.height);
                for row in 0..*height {
                    for column in 0..*width {
                        let source = (row * *width + column) * mvs::MVS_RGB_CHANNEL_BYTES;
                        let destination = (*y + row) * framebuffer.width + *x + column;
                        let red = u32::from(rgb[source + mvs::MVS_RGB_RED_OFFSET]);
                        let green = u32::from(rgb[source + mvs::MVS_RGB_GREEN_OFFSET]);
                        let blue = u32::from(rgb[source + mvs::MVS_RGB_BLUE_OFFSET]);
                        framebuffer.pixels_mut()[destination] = (red << PIXEL_RED_SHIFT)
                            | (green << PIXEL_GREEN_SHIFT)
                            | (blue << PIXEL_BLUE_SHIFT);
                    }
                }
            }
            PartialPixelOperation::Copy {
                source_x,
                source_y,
                destination_x,
                destination_y,
                width,
                height,
            } => {
                debug_assert!(*width > 0 && *height > 0 && *width <= 8 && *height <= 8);
                debug_assert!(*source_x + *width <= framebuffer.width);
                debug_assert!(*source_y + *height <= framebuffer.height);
                debug_assert!(*destination_x + *width <= framebuffer.width);
                debug_assert!(*destination_y + *height <= framebuffer.height);
                let mut staging = [0u32; 8 * 8];
                for row in 0..*height {
                    let source = (*source_y + row) * framebuffer.width + *source_x;
                    let staging_start = row * *width;
                    staging[staging_start..staging_start + *width]
                        .copy_from_slice(&framebuffer.pixels()[source..source + *width]);
                }
                for row in 0..*height {
                    let staging_start = row * *width;
                    let destination = (*destination_y + row) * framebuffer.width + *destination_x;
                    framebuffer.pixels_mut()[destination..destination + *width]
                        .copy_from_slice(&staging[staging_start..staging_start + *width]);
                }
            }
        }
    }
}

fn apply_prepared_partial_to_surface(
    receiver: &mut MvsReceiveState,
    surface: &mut DisplaySurface,
    prepared: mvs::PreparedOpaqueMvsState,
) -> Result<bool> {
    if surface.generation != receiver.generation {
        bail!(
            "MVS prepared partial generation 与当前 surface 不一致: receiver={}, surface={}",
            receiver.generation,
            surface.generation
        );
    }
    validate_partial_pixels_for_framebuffer(&surface.framebuffer, prepared.partial_pixels())?;
    let has_pixels = !prepared.partial_pixels().operations.is_empty();
    let partial_pixels = receiver.commit_opaque(prepared)?;
    if has_pixels {
        apply_validated_partial_pixels_to_framebuffer(&mut surface.framebuffer, &partial_pixels);
        surface.record_native_partial_applied();
    }
    Ok(has_pixels)
}

/// 已严格分类为像素记录后的原生 prepare/apply/commit 事务。
fn apply_native_mvs_frame_with<F>(
    receiver: &mut MvsReceiveState,
    record: &MvsRecord,
    surface: &Arc<Mutex<DisplaySurface>>,
    dynamic_resolution: &Arc<Mutex<DynamicResolutionRuntime>>,
    apply: F,
) -> Result<MvsRecordOutcome>
where
    F: FnOnce(&mut Framebuffer, &[u8], usize, usize, usize, usize) -> Result<()>,
{
    let (surface_generation, display_size) = {
        let surface = surface.lock().unwrap();
        (
            surface.generation,
            DisplaySize::new(
                surface.framebuffer.width as u16,
                surface.framebuffer.height as u16,
            )
            .expect("DisplaySurface dimensions are non-zero u16 values"),
        )
    };
    if receiver.generation != surface_generation {
        eprintln!("[hpss-view] 忽略过期 generation 的 MVS 记录");
        return Ok(MvsRecordOutcome::Ignored);
    }
    if !mark_recovery_for_invalid_mvs_geometry(receiver, record.rect, display_size)? {
        return Ok(MvsRecordOutcome::RecoveryRequested);
    }
    let complete_surface = is_complete_surface_frame(record.rect, display_size);
    let prepared = match receiver.prepare_rect(&record.payload, record.rect, display_size) {
        Ok(mvs::MvsDecodeDecision::Prepared(prepared)) => prepared,
        Ok(mvs::MvsDecodeDecision::PreparedOpaque(prepared)) => {
            let applied = {
                let mut surface = surface.lock().unwrap();
                if surface.generation != surface_generation {
                    Err(anyhow::anyhow!(
                        "MVS surface generation 在 partial 应用前发生变化"
                    ))
                } else {
                    apply_prepared_partial_to_surface(receiver, &mut surface, prepared)
                }
            };
            match applied {
                Ok(true) => eprintln!("[hpss-view] MVS type-1 增量像素与 codec 状态已提交"),
                Ok(false) => eprintln!("[hpss-view] MVS type-1 no-op/cache 状态已提交"),
                Err(error) => {
                    eprintln!("[hpss-view] MVS type-1 framebuffer 事务失败，重同步: {error:#}");
                    receiver.request_full()?;
                    return Ok(MvsRecordOutcome::RecoveryRequested);
                }
            }
            // Partial pixels are visible, but a type-1 update is never the
            // complete type-0 codec baseline used by the P1 evidence latch.
            return Ok(MvsRecordOutcome::PartialApplied);
        }
        Ok(mvs::MvsDecodeDecision::RequestFull(reason)) => {
            eprintln!("[hpss-view] MVS 原生记录要求全量重同步: {reason:?}");
            receiver.request_full()?;
            return Ok(MvsRecordOutcome::RecoveryRequested);
        }
        Ok(mvs::MvsDecodeDecision::IgnoreStale) => {
            eprintln!("[hpss-view] 忽略过期 generation 的 MVS prepare");
            return Ok(MvsRecordOutcome::Ignored);
        }
        Err(error) => {
            eprintln!("[hpss-view] MVS 原生 prepare 失败，重同步: {error:#}");
            receiver.request_full()?;
            return Ok(MvsRecordOutcome::RecoveryRequested);
        }
    };

    let applied = {
        let mut surface = surface.lock().unwrap();
        if surface.generation != surface_generation {
            Err(anyhow::anyhow!("MVS surface generation 在应用前发生变化"))
        } else {
            apply_prepared_mvs_to_surface_with(receiver, &mut surface, prepared, record.rect, apply)
                .map(|()| surface.record_native_type_zero_applied())
        }
    };
    let observability = match applied {
        Ok(observability) => observability,
        Err(error) => {
            eprintln!("[hpss-view] MVS 原生 framebuffer 事务失败，重同步: {error:#}");
            receiver.request_full()?;
            return Ok(MvsRecordOutcome::RecoveryRequested);
        }
    };

    eprintln!(
        "[hpss-view] native MVS: generation={}, rect=({},{} {}x{}), type0_total={}",
        receiver.generation,
        record.rect.x,
        record.rect.y,
        record.rect.width,
        record.rect.height,
        observability.type_zero_applied_count,
    );
    if complete_surface {
        dynamic_resolution
            .lock()
            .unwrap()
            .observe_full_applied(receiver.generation, display_size);
        eprintln!("[hpss-view] 当前 generation 的完整 surface 证据已确认");
    }
    Ok(MvsRecordOutcome::FullApplied)
}

fn process_complete_mvs_record(
    receiver: &mut MvsReceiveState,
    record: MvsRecord,
    surface: &Arc<Mutex<DisplaySurface>>,
    dynamic_resolution: &Arc<Mutex<DynamicResolutionRuntime>>,
    write_stream: &Arc<Mutex<std::net::TcpStream>>,
    crypto: &Arc<Mutex<crate::vnc::session::SessionCrypto>>,
    requests: &mut ReaderRequestState,
) -> Result<MvsRecordOutcome> {
    let (surface_generation, display_size) = {
        let surface = surface.lock().unwrap();
        (
            surface.generation,
            DisplaySize::new(
                surface.framebuffer.width as u16,
                surface.framebuffer.height as u16,
            )
            .expect("DisplaySurface dimensions are non-zero u16 values"),
        )
    };
    if receiver.generation != surface_generation {
        return Ok(MvsRecordOutcome::Ignored);
    }

    match mvs::classify_mvs_record(record.rect, &record.payload) {
        Ok(mvs::MvsRecordKind::Tables(payload)) => {
            if let Err(e) = receiver.install_tables(payload) {
                eprintln!("[hpss-view] MVS 表初始化无效，重同步: {e:#}");
                receiver.request_full()?;
                return request_full_update(
                    write_stream,
                    crypto,
                    requests,
                    display_size.width,
                    display_size.height,
                )
                .map(|()| MvsRecordOutcome::RecoveryRequested);
            }
            let status = requests.on_valid_table_record(receiver.generation, Instant::now())?;
            eprintln!("[hpss-view] MVS 量化表就绪，full follow-up 已调度: {status:?}");
            return Ok(MvsRecordOutcome::TableInstalled);
        }
        Ok(mvs::MvsRecordKind::Frame(_)) => {}
        Err(error) => {
            eprintln!("[hpss-view] MVS 表初始化候选无效，重同步: {error:#}");
            receiver.request_full()?;
            return request_full_update(
                write_stream,
                crypto,
                requests,
                display_size.width,
                display_size.height,
            )
            .map(|()| MvsRecordOutcome::RecoveryRequested);
        }
    }

    let outcome = apply_native_mvs_frame_with(
        receiver,
        &record,
        surface,
        dynamic_resolution,
        apply_rgb_rect,
    )?;
    if outcome == MvsRecordOutcome::RecoveryRequested {
        return request_full_update(
            write_stream,
            crypto,
            requests,
            display_size.width,
            display_size.height,
        )
        .map(|()| MvsRecordOutcome::RecoveryRequested);
    }
    Ok(outcome)
}

#[allow(
    clippy::too_many_arguments,
    reason = "网络适配层显式传入共享状态，避免隐藏全局状态和锁顺序"
)]
fn service_network_reader_tick(
    receiver: &mut MvsReceiveState,
    requests: &mut ReaderRequestState,
    surface: &Arc<Mutex<DisplaySurface>>,
    viewport_requests: &Arc<Mutex<ViewportRequestQueue>>,
    dynamic_resolution: &Arc<Mutex<DynamicResolutionRuntime>>,
    write_stream: &Arc<Mutex<std::net::TcpStream>>,
    crypto: &Arc<Mutex<crate::vnc::session::SessionCrypto>>,
    now: Instant,
) -> Result<ReaderTickOutcome> {
    let size = current_surface_size(surface);
    let full = protocol::msg_fb_update_request(false, 0, 0, size.width, size.height);
    // 固定锁序：queue → runtime → writer → crypto。surface 已在上方解锁。
    let mut queue = viewport_requests.lock().unwrap();
    let mut runtime = dynamic_resolution.lock().unwrap();
    service_reader_tick_at(
        receiver,
        requests,
        &mut queue,
        &mut runtime,
        now,
        thread::sleep,
        || send_encrypted(write_stream, crypto, &full),
        |target| {
            let query = hpss::build_display_query(target);
            send_encrypted(write_stream, crypto, &query)?;
            Ok(Instant::now())
        },
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "网络 full 边界适配器显式传入共享状态，保持锁和写事务可审计"
)]
fn finish_network_full_boundary(
    receiver: &mut MvsReceiveState,
    requests: &mut ReaderRequestState,
    surface: &Arc<Mutex<DisplaySurface>>,
    viewport_requests: &Arc<Mutex<ViewportRequestQueue>>,
    dynamic_resolution: &Arc<Mutex<DynamicResolutionRuntime>>,
    write_stream: &Arc<Mutex<std::net::TcpStream>>,
    crypto: &Arc<Mutex<crate::vnc::session::SessionCrypto>>,
    now: Instant,
) -> Result<FullBoundaryOutcome> {
    let size = current_surface_size(surface);
    let full = protocol::msg_fb_update_request(false, 0, 0, size.width, size.height);
    let incremental = incremental_request_after_full_apply(size.width, size.height);
    let mut queue = viewport_requests.lock().unwrap();
    let mut runtime = dynamic_resolution.lock().unwrap();
    finish_full_boundary_at(
        receiver,
        requests,
        &mut queue,
        &mut runtime,
        now,
        thread::sleep,
        || send_encrypted(write_stream, crypto, &full),
        |target| {
            let query = hpss::build_display_query(target);
            send_encrypted(write_stream, crypto, &query)?;
            Ok(Instant::now())
        },
        || send_encrypted(write_stream, crypto, &incremental),
    )
}

fn log_reader_tick(outcome: &ReaderTickOutcome) {
    if outcome.incomplete_recovered {
        eprintln!("[hpss-view] MVS 不完整记录超时，已中止并请求全量重同步");
    }
    if outcome.dynamic_timed_out {
        eprintln!("[hpss-view] 动态分辨率确认超时，保留当前显示面并丢弃待处理 viewport");
    }
    if outcome.table_followup_sent {
        eprintln!("[hpss-view] MVS table follow-up full request 已发送");
    }
    if let Some(request) = outcome.dynamic_request {
        eprintln!(
            "[hpss-view] 请求动态分辨率 {}x{} (generation {})",
            request.target.width, request.target.height, request.generation
        );
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "完整 MVS 记录处理需要显式传入 generation 状态与事务写器"
)]
fn handle_complete_mvs_record(
    receiver: &mut MvsReceiveState,
    requests: &mut ReaderRequestState,
    record: MvsRecord,
    surface: &Arc<Mutex<DisplaySurface>>,
    viewport_requests: &Arc<Mutex<ViewportRequestQueue>>,
    dynamic_resolution: &Arc<Mutex<DynamicResolutionRuntime>>,
    write_stream: &Arc<Mutex<std::net::TcpStream>>,
    crypto: &Arc<Mutex<crate::vnc::session::SessionCrypto>>,
) -> Result<()> {
    requests.consume_mvs_response();
    let outcome = process_complete_mvs_record(
        receiver,
        record,
        surface,
        dynamic_resolution,
        write_stream,
        crypto,
        requests,
    )?;
    if outcome == MvsRecordOutcome::FullApplied {
        let boundary = finish_network_full_boundary(
            receiver,
            requests,
            surface,
            viewport_requests,
            dynamic_resolution,
            write_stream,
            crypto,
            Instant::now(),
        )?;
        if let Some(request) = boundary.dynamic_request {
            eprintln!(
                "[hpss-view] full 边界切换为动态分辨率 {}x{} (generation {})",
                request.target.width, request.target.height, request.generation
            );
        } else if boundary.incremental_sent {
            eprintln!("[hpss-view] MVS full 已应用并请求下一增量响应");
        }
    } else if outcome == MvsRecordOutcome::PartialApplied {
        let size = current_surface_size(surface);
        let incremental = incremental_request_after_full_apply(size.width, size.height);
        if finish_partial_boundary_at(requests, || {
            send_encrypted(write_stream, crypto, &incremental)
        })? {
            eprintln!("[hpss-view] MVS type-1 已提交并请求下一增量响应");
        }
    }
    Ok(())
}

fn validate_hpss_audio_flow(
    audio_flow: AudioMediaFlow,
    _source_mode: AudioInputSourceMode,
) -> Result<()> {
    if audio_flow == AudioMediaFlow::PcToMac {
        bail!(
            "用户名/密码 HPSS 不支持 PC→Mac Audio Chat；stock Apple 实现需要 IDS/Apple ID 邀请状态"
        );
    }
    Ok(())
}

fn read_viewer_app_frame_step(conn: &mut RfbConn) -> Result<Option<Vec<u8>>> {
    conn.read_app_frame_step()
}

fn select_initial_display_size(init_w: u16, init_h: u16) -> Option<DisplaySize> {
    DisplaySize::new(init_w, init_h)
}

fn scaled_drawable_size(surface: DisplaySize, scale: f32) -> Result<(usize, usize)> {
    if !scale.is_finite() || !(0.0 < scale && scale <= 1.0) {
        bail!("显示缩放必须是有限数值，且满足 0 < scale <= 1");
    }
    Ok((
        ((surface.width as f32 * scale).ceil() as usize).max(1),
        ((surface.height as f32 * scale).ceil() as usize).max(1),
    ))
}

/// HPSS 实时视图主循环
pub fn run_viewer(
    mut conn: RfbConn,
    display_name: &str,
    init_w: u16,
    init_h: u16,
    scale: f32,
    dynamic_resolution_enabled: bool,
    audio_flow: AudioMediaFlow,
) -> Result<()> {
    validate_hpss_audio_flow(audio_flow, REVIEWED_AUDIO_INPUT_SOURCE_MODE)?;
    let media_server_address = conn.peer_addr()?.ip();
    let media_bind_address = conn.local_addr()?.ip();
    let media_generation = 0u64;
    let mut media_transport = MediaTransport::new(media_generation, media_server_address);
    media_transport.set_audio_flow(audio_flow)?;
    // ── HPSS 协商（与 hpss::run 相同的触发链） ──
    std::thread::sleep(Duration::from_millis(200));
    conn.write_all(&hpss::build_set_display_config(display_name))
        .context("发送 0x1d 失败")?;
    std::thread::sleep(Duration::from_millis(150));

    let initial_size =
        select_initial_display_size(init_w, init_h).context("HPSS 初始显示尺寸无效")?;
    let w = initial_size.width;
    let h = initial_size.height;
    let q09 = hpss::build_display_query(initial_size);
    let fb_req = protocol::msg_fb_update_request(false, 0, 0, w, h);
    conn.write_all(&q09)?;
    std::thread::sleep(Duration::from_millis(120));
    conn.write_all(&fb_req)?;
    let startup_fb_sent_at = Instant::now();
    eprintln!("[hpss-view] 已触发推流（0x1d + 0x09 + 0x03）");
    conn.set_read_timeout(Some(Duration::from_millis(100)))?;

    // ── 共享状态 ──
    let write_stream = Arc::new(Mutex::new(conn.try_clone().context("无法复制 socket")?));
    let crypto = conn.crypto_handle().context("加密未挂载")?;
    let surface = Arc::new(Mutex::new(DisplaySurface::new(
        0,
        Framebuffer::new(w as usize, h as usize)?,
    )));
    let dynamic_resolution = Arc::new(Mutex::new(DynamicResolutionRuntime::new(
        initial_size,
        dynamic_resolution_enabled,
    )));
    let viewport_requests = Arc::new(Mutex::new(ViewportRequestQueue::default()));
    let closing = Arc::new(AtomicBool::new(false));
    let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    // ── 读线程：MVS 流接收 + 解码 ──
    let reader = {
        let surface = surface.clone();
        let dynamic_resolution = dynamic_resolution.clone();
        let viewport_requests = viewport_requests.clone();
        let write_stream = write_stream.clone();
        let crypto = crypto.clone();
        let closing = closing.clone();
        let error_slot = error_slot.clone();
        thread::spawn(move || {
            let mut receiver = MvsReceiveState::new(0);
            let mut requests = ReaderRequestState::after_startup(startup_fb_sent_at);
            let mut pending_media_answer_frame_diagnostics = 0usize;
            let mut viewer_media_state = match ViewerMediaState::new(audio_flow) {
                Ok(state) => state,
                Err(error) => {
                    *error_slot.lock().unwrap() =
                        Some(format!("初始化 UDP 音频状态失败: {error:#}"));
                    return;
                }
            };

            loop {
                if closing.load(Ordering::Relaxed) {
                    break;
                }

                if media_transport.phase() == MediaTransportPhase::Active {
                    if let Err(error) =
                        media_transport.service_control_reports_at(media_generation, Instant::now())
                    {
                        if !closing.load(Ordering::Relaxed) {
                            *error_slot.lock().unwrap() =
                                Some(format!("发送周期 SRTCP 控制报告失败: {error:#}"));
                        }
                        break;
                    }
                    if let Err(error) = drain_udp_media(
                        &mut media_transport,
                        &mut viewer_media_state,
                        media_generation,
                    ) {
                        if !closing.load(Ordering::Relaxed) {
                            *error_slot.lock().unwrap() =
                                Some(format!("接收或解码 UDP 媒体失败: {error:#}"));
                        }
                        break;
                    }
                    let now = Instant::now();
                    viewer_media_state.observe_audio_input_transport(
                        &media_transport,
                        media_generation,
                        now,
                    );
                    viewer_media_state.service_audio_input_probe(
                        &mut media_transport,
                        media_generation,
                        now,
                    );
                    if let Some(diagnostic) =
                        viewer_media_state.take_audio_input_confirmation_diagnostic()
                    {
                        eprintln!(
                            "[audio-in] P5 探针已确认: sent={}, first-extended-sequence={}, last-extended-sequence={}, ssrc={}, srtcp-extended-highest-sequence={}, srtcp-cumulative-packets-lost={}",
                            diagnostic.packets_sent,
                            diagnostic.first_extended_sequence,
                            diagnostic.last_extended_sequence,
                            diagnostic.ssrc,
                            diagnostic.srtcp_extended_highest_sequence,
                            diagnostic.srtcp_cumulative_packets_lost
                        );
                    }
                }

                let tick = service_network_reader_tick(
                    &mut receiver,
                    &mut requests,
                    &surface,
                    &viewport_requests,
                    &dynamic_resolution,
                    &write_stream,
                    &crypto,
                    Instant::now(),
                );
                match tick {
                    Ok(outcome) => log_reader_tick(&outcome),
                    Err(tick_error) => {
                        if !closing.load(Ordering::Relaxed) {
                            *error_slot.lock().unwrap() =
                                Some(format!("reader 请求状态推进失败: {tick_error}"));
                        }
                        break;
                    }
                }

                let msg = match read_viewer_app_frame_step(&mut conn) {
                    Ok(Some(m)) => m,
                    Ok(None) => continue,
                    Err(e) if is_timeout(&e) => continue,
                    Err(e) => {
                        if !closing.load(Ordering::Relaxed) {
                            *error_slot.lock().unwrap() = Some(format!("{e:#}"));
                        }
                        break;
                    }
                };
                if media_transport.phase() == MediaTransportPhase::ConfigSent
                    && pending_media_answer_frame_diagnostics
                        < MAX_PENDING_MEDIA_ANSWER_FRAME_DIAGNOSTICS
                {
                    let class = reader_frame_class(&receiver, &msg);
                    eprintln!(
                        "[hpss-view] 等待 Message2 时收到 TCP 应用帧: {}B, class={class:?}, kind={}, mvs_pending={}",
                        msg.len(),
                        describe_media_frame_for_diagnostics(&msg),
                        receiver.is_pending()
                    );
                    pending_media_answer_frame_diagnostics += 1;
                }
                // continuation 帧没有媒体头；在任何心跳或媒体分类之前原样交给重组器。
                if reader_frame_class(&receiver, &msg) == ReaderFrameClass::Continuation {
                    let record = match receiver.push_continuation(&msg) {
                        Ok(record) => record,
                        Err(e) => {
                            eprintln!("[hpss-view] MVS continuation 结构错误，重同步: {e:#}");
                            let _ = receiver.request_full();
                            requests.consume_mvs_response();
                            let size = current_surface_size(&surface);
                            if let Err(send_error) = request_full_update(
                                &write_stream,
                                &crypto,
                                &mut requests,
                                size.width,
                                size.height,
                            ) {
                                if !closing.load(Ordering::Relaxed) {
                                    *error_slot.lock().unwrap() =
                                        Some(format!("发送全量更新请求失败: {send_error}"));
                                }
                                break;
                            }
                            continue;
                        }
                    };
                    if let Some(record) = record {
                        if let Err(e) = handle_complete_mvs_record(
                            &mut receiver,
                            &mut requests,
                            record,
                            &surface,
                            &viewport_requests,
                            &dynamic_resolution,
                            &write_stream,
                            &crypto,
                        ) {
                            if !closing.load(Ordering::Relaxed) {
                                *error_slot.lock().unwrap() =
                                    Some(format!("处理 MVS continuation 失败: {e:#}"));
                            }
                            break;
                        }
                    }
                    continue;
                }

                match reader_frame_class(&receiver, &msg) {
                    // 0x14 是服务端单向保活通知；客户端不应回写。
                    ReaderFrameClass::ServerKeepalive => continue,
                    ReaderFrameClass::Query => continue, // 服务器自发 0x08 查询
                    ReaderFrameClass::ControlOrMedia => {}
                    ReaderFrameClass::Continuation => unreachable!("continuation 已在上方处理"),
                }

                match parse_media(&msg) {
                    Ok(Media::Mvs {
                        x,
                        y,
                        w: rect_w,
                        h: rect_h,
                        total,
                        body,
                    }) => {
                        let rect = MvsRect {
                            x,
                            y,
                            width: rect_w,
                            height: rect_h,
                        };
                        let record = match receiver.begin(rect, total, &body) {
                            Ok(record) => record,
                            Err(e) => {
                                eprintln!("[hpss-view] MVS 首片结构错误，重同步: {e:#}");
                                let _ = receiver.request_full();
                                requests.consume_mvs_response();
                                let size = current_surface_size(&surface);
                                if let Err(send_error) = request_full_update(
                                    &write_stream,
                                    &crypto,
                                    &mut requests,
                                    size.width,
                                    size.height,
                                ) {
                                    if !closing.load(Ordering::Relaxed) {
                                        *error_slot.lock().unwrap() =
                                            Some(format!("发送全量更新请求失败: {send_error}"));
                                    }
                                    break;
                                }
                                continue;
                            }
                        };
                        if let Some(record) = record {
                            if let Err(e) = handle_complete_mvs_record(
                                &mut receiver,
                                &mut requests,
                                record,
                                &surface,
                                &viewport_requests,
                                &dynamic_resolution,
                                &write_stream,
                                &crypto,
                            ) {
                                if !closing.load(Ordering::Relaxed) {
                                    *error_slot.lock().unwrap() =
                                        Some(format!("处理 MVS 记录失败: {e:#}"));
                                }
                                break;
                            }
                        }
                    }
                    Ok(Media::PortAnnouncement(announcement)) => {
                        eprintln!(
                            "[hpss-view] UDP 端口公告: audio={} announced={} hdr={}, video1={} announced={} hdr={}, video2={} announced={} hdr={}, flags=0x{:08x}",
                            announcement.audio.port,
                            announcement.audio.is_announced(),
                            announcement.audio.flags.hdr(),
                            announcement.video_stream_1.port,
                            announcement.video_stream_1.is_announced(),
                            announcement.video_stream_1.flags.hdr(),
                            announcement.video_stream_2.port,
                            announcement.video_stream_2.is_announced(),
                            announcement.video_stream_2.flags.hdr(),
                            announcement.message_flags,
                        );
                        if media_transport.phase() == MediaTransportPhase::Idle {
                            if let Err(error) = media_transport
                                .accept_port_announcement(media_generation, announcement)
                                .and_then(|()| {
                                    media_transport
                                        .bind_local_sockets(media_generation, media_bind_address)
                                })
                            {
                                if !closing.load(Ordering::Relaxed) {
                                    *error_slot.lock().unwrap() =
                                        Some(format!("准备 UDP 媒体 socket 失败: {error:#}"));
                                }
                                break;
                            }
                            for role in [
                                MediaRole::Audio,
                                MediaRole::VideoStream1,
                                MediaRole::VideoStream2,
                            ] {
                                if let Ok(local) =
                                    media_transport.local_addr(media_generation, role)
                                {
                                    eprintln!(
                                        "[hpss-view] {role:?} 本地 UDP socket 已绑定 {local}"
                                    );
                                }
                            }
                            let configuration =
                                match media_transport.prepare_configuration(media_generation) {
                                    Ok(configuration) => configuration,
                                    Err(error) => {
                                        if !closing.load(Ordering::Relaxed) {
                                            *error_slot.lock().unwrap() =
                                                Some(format!("生成 0x1c 媒体配置失败: {error:#}"));
                                        }
                                        break;
                                    }
                                };
                            #[cfg(debug_assertions)]
                            if let Some((first_slot, second_slot)) =
                                media_transport.diagnostic_audio_material_fingerprints()
                            {
                                eprintln!(
                                    "[hpss-view] Audio SRTP Apple-log-visible 指纹: wire-slot-1={first_slot}, wire-slot-2={second_slot}"
                                );
                            }
                            if let Err(error) =
                                send_encrypted(&write_stream, &crypto, &configuration).and_then(
                                    |()| media_transport.mark_configuration_sent(media_generation),
                                )
                            {
                                if !closing.load(Ordering::Relaxed) {
                                    *error_slot.lock().unwrap() =
                                        Some(format!("发送 0x1c 媒体配置失败: {error:#}"));
                                }
                                break;
                            }
                            eprintln!(
                                "[hpss-view] 已发送经验证的 0x1c 媒体配置（{}B）",
                                configuration.len()
                            );
                        }
                    }
                    Ok(Media::StreamAnswer(answer)) => {
                        if let Err(error) = media_transport
                            .accept_answer(media_generation, answer)
                            .and_then(|()| media_transport.activate(media_generation))
                        {
                            if !closing.load(Ordering::Relaxed) {
                                *error_slot.lock().unwrap() =
                                    Some(format!("接受 Message 2 或激活 SRTP 失败: {error:#}"));
                            }
                            break;
                        }
                        eprintln!(
                            "[hpss-view] 已验证 MediaStream Message 2；SRTP 数据面已激活并发送初始 SRTCP 报告"
                        );
                        viewer_media_state.start_audio_input_probe(
                            &media_transport,
                            media_generation,
                            Instant::now(),
                        );
                    }
                    Ok(Media::Cursor { x, y, w, h, zlib }) => {
                        // 光标：解压 zlib 并叠加（简化：只更新位置提示）
                        let _ = (x, y, w, h, zlib);
                    }
                    Ok(Media::State(encoding::SERVER_STATE)) => {
                        if let Some((nw, nh)) = crate::vnc::hpss::parse_server_state_w_h(&msg) {
                            if let Some(observed) = DisplaySize::new(nw, nh) {
                                // 锁顺序固定为 controller → surface；任何 socket I/O 都在解锁后进行。
                                let commit = {
                                    let mut runtime = dynamic_resolution.lock().unwrap();
                                    let mut surface = surface.lock().unwrap();
                                    let current = DisplaySize::new(
                                        surface.framebuffer.width as u16,
                                        surface.framebuffer.height as u16,
                                    )
                                    .expect("DisplaySurface dimensions are non-zero u16 values");
                                    runtime.observe_initial_server_state(observed, current);
                                    commit_server_geometry(
                                        &mut runtime,
                                        &mut receiver,
                                        &mut surface,
                                        &mut viewer_media_state,
                                        observed,
                                    )
                                };
                                if let Some(commit) = commit {
                                    eprintln!(
                                        "[hpss-view] 分辨率确认并切换 → {}x{} (generation {})",
                                        commit.size.width, commit.size.height, commit.generation
                                    );
                                    requests.reset_generation(commit.generation);
                                    if let Err(send_error) = request_full_update(
                                        &write_stream,
                                        &crypto,
                                        &mut requests,
                                        commit.size.width,
                                        commit.size.height,
                                    ) {
                                        if !closing.load(Ordering::Relaxed) {
                                            *error_slot.lock().unwrap() = Some(format!(
                                                "发送切换后的全量更新请求失败: {send_error}"
                                            ));
                                        }
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    Ok(Media::State(_)) => {}
                    Err(e) => match receiver.reject_truncated_mvs_envelope(&msg) {
                        Ok(true) => {
                            eprintln!("[hpss-view] MVS 信封截断，重同步: {e:#}");
                            requests.consume_mvs_response();
                            let size = current_surface_size(&surface);
                            if let Err(send_error) = request_full_update(
                                &write_stream,
                                &crypto,
                                &mut requests,
                                size.width,
                                size.height,
                            ) {
                                if !closing.load(Ordering::Relaxed) {
                                    *error_slot.lock().unwrap() =
                                        Some(format!("发送全量更新请求失败: {send_error}"));
                                }
                                break;
                            }
                        }
                        Ok(false) => {}
                        Err(state_error) => {
                            if !closing.load(Ordering::Relaxed) {
                                *error_slot.lock().unwrap() =
                                    Some(format!("记录 MVS 全量重同步状态失败: {state_error:#}"));
                            }
                            break;
                        }
                    },
                }
            }
            if viewer_media_state.sent_audio_access_units != 0
                || viewer_media_state.authenticated_audio_packets != 0
                || viewer_media_state.authenticated_video_packets != 0
            {
                eprintln!(
                    "[hpss-view] UDP 媒体统计: audio-in={} audio-sent-range={:?} audio-reception={:?} audio-out={} late={} resync={} non-silent={} concealed={}, video={}",
                    viewer_media_state.sent_audio_access_units,
                    media_transport.outbound_audio_sent_range(),
                    media_transport.audio_reception_evidence(),
                    viewer_media_state.authenticated_audio_packets,
                    viewer_media_state.late_audio_packets,
                    viewer_media_state.audio_resynchronizations,
                    viewer_media_state.non_silent_audio_access_units,
                    viewer_media_state.concealed_audio_access_units,
                    viewer_media_state.authenticated_video_packets
                );
            }
            viewer_media_state.teardown_audio_input();
            let _ = media_transport.close(media_generation);
        })
    };

    // ── 主线程：minifb 窗口 + 输入 ──
    let main_result = (|| -> Result<()> {
        let (vw, vh) = scaled_drawable_size(initial_size, scale)?;
        let mut window = Window::new(
            &format!("FreeRemoteDesk HPSS — [{w}x{h}  Ctrl+Q 退出]"),
            vw,
            vh,
            WindowOptions {
                resize: true,
                ..Default::default()
            },
        )?;
        window.set_target_fps(60);

        let mut scaled: Vec<u32> = Vec::new();
        let mut pressed: HashSet<Key> = HashSet::new();
        let mut pointer_input = PointerInputState::default();
        let mut last_window_size = window.get_size();

        loop {
            let window_size = window.get_size();
            let drawable_size = (window_size.0.max(1), window_size.1.max(1));
            scaled.resize(drawable_size.0.saturating_mul(drawable_size.1), 0);

            render_surface_frame_with(
                &surface,
                drawable_size,
                &mut scaled,
                |pixels, width, height| {
                    window.update_with_buffer(pixels, width, height)?;
                    Ok(())
                },
            )?;

            if dynamic_resolution_enabled && window_size != last_window_size {
                last_window_size = window_size;
                if let Some(target) = DisplaySize::from_viewport(window_size.0, window_size.1) {
                    viewport_requests
                        .lock()
                        .unwrap()
                        .observe(target, Instant::now());
                } else {
                    viewport_requests.lock().unwrap().drop_latest();
                }
            }

            let quit_hotkey = {
                let ctrl = window.is_key_down(Key::LeftCtrl) || window.is_key_down(Key::RightCtrl);
                ctrl && window.is_key_pressed(Key::Q, minifb::KeyRepeat::No)
            };
            if !window.is_open() || quit_hotkey {
                break;
            }
            if let Some(err) = error_slot.lock().unwrap().take() {
                bail!("远程连接中断: {err}");
            }

            // 键盘（差分）
            let now: HashSet<Key> = window.get_keys().into_iter().collect();
            let shift = now.contains(&Key::LeftShift) || now.contains(&Key::RightShift);
            let mut key_msgs = Vec::new();
            for k in &now {
                if !pressed.contains(k) {
                    if let Some(ks) = keysym::to_keysym(*k, shift) {
                        key_msgs.push(protocol::msg_key_event(true, ks));
                    }
                }
            }
            for k in &pressed {
                if !now.contains(k) {
                    if let Some(ks) = keysym::to_keysym(*k, false) {
                        key_msgs.push(protocol::msg_key_event(false, ks));
                    }
                }
            }
            pressed = now;
            for m in &key_msgs {
                send_encrypted(&write_stream, &crypto, m)?;
            }

            // 鼠标：仅窗口激活且指针位于客户区内时采样输入。
            let pointer_position = if window.is_active() {
                window.get_mouse_pos(MouseMode::Discard)
            } else {
                None
            };
            let (sample, local_buttons_down) = if let Some((mx, my)) = pointer_position {
                let mut mask = 0u8;
                if window.get_mouse_down(MouseButton::Left) {
                    mask |= protocol::pointer::PRIMARY;
                }
                if window.get_mouse_down(MouseButton::Middle) {
                    mask |= protocol::pointer::MIDDLE;
                }
                if window.get_mouse_down(MouseButton::Right) {
                    mask |= protocol::pointer::SECONDARY;
                }
                let local_buttons_down = mask != 0;
                if let Some((wx, wy)) = window.get_scroll_wheel() {
                    if wy > 0.0 {
                        mask |= protocol::pointer::WHEEL_UP;
                    } else if wy < 0.0 {
                        mask |= protocol::pointer::WHEEL_DOWN;
                    }
                    if wx > 0.0 {
                        mask |= protocol::pointer::WHEEL_RIGHT;
                    } else if wx < 0.0 {
                        mask |= protocol::pointer::WHEEL_LEFT;
                    }
                }
                let display_size = current_surface_size(&surface);
                let (x, y) = map_pointer(mx, my, window_size, display_size);
                (Some(PointerSample::new(x, y, mask)), local_buttons_down)
            } else {
                (None, false)
            };
            if let Some(event) = pointer_input.next_event(sample, local_buttons_down) {
                let msg = protocol::msg_pointer_event(event.mask, event.x, event.y);
                send_encrypted(&write_stream, &crypto, &msg)?;
            }
        }

        Ok(())
    })();

    let cleanup_result = shutdown_reader(&closing, &write_stream, reader);
    match (main_result, cleanup_result) {
        (Err(main_error), _) => Err(main_error),
        (Ok(()), Err(cleanup_error)) => Err(cleanup_error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// RGB u8 → 帧缓冲像素（0x00RRGGBB）
fn apply_rgb_rect(
    fb: &mut Framebuffer,
    rgb: &[u8],
    x: usize,
    y: usize,
    w: usize,
    h: usize,
) -> Result<()> {
    let width = u16::try_from(w).context("MVS RGB 矩形宽度超出 u16")?;
    let height = u16::try_from(h).context("MVS RGB 矩形高度超出 u16")?;
    mvs::validate_decoded_rgb_layout(width, height, rgb.len())?;
    let right = x.checked_add(w).context("MVS RGB 水平边界溢出")?;
    let bottom = y.checked_add(h).context("MVS RGB 垂直边界溢出")?;
    if right > fb.width || bottom > fb.height {
        bail!("MVS RGB 矩形超出当前帧缓冲");
    }
    for row in 0..h {
        for col in 0..w {
            let src_off = (row * w + col) * mvs::MVS_RGB_CHANNEL_BYTES;
            let dst_off = (y + row) * fb.width + x + col;
            let r = rgb[src_off + mvs::MVS_RGB_RED_OFFSET] as u32;
            let g = rgb[src_off + mvs::MVS_RGB_GREEN_OFFSET] as u32;
            let b = rgb[src_off + mvs::MVS_RGB_BLUE_OFFSET] as u32;
            fb.pixels_mut()[dst_off] =
                (r << PIXEL_RED_SHIFT) | (g << PIXEL_GREEN_SHIFT) | (b << PIXEL_BLUE_SHIFT);
        }
    }
    Ok(())
}

/// 通过加密会话发送（viewer 同款）
fn send_encrypted(
    write_stream: &Arc<Mutex<std::net::TcpStream>>,
    crypto: &Arc<Mutex<crate::vnc::session::SessionCrypto>>,
    msg: &[u8],
) -> Result<()> {
    // 先串行化整个发送事务，再推进加密链；否则两个线程可能按 A/B seal、B/A write
    // 的顺序把链式帧写反。输入等常规路径不持 queue/surface/runtime 锁；reader
    // request-state tick 会按 queue → runtime → writer → crypto 跨单次写入，确保
    // write 成功后才标记 framebuffer in-flight 或动态 Pending。
    let mut writer = write_stream.lock().unwrap();
    let wire = {
        let mut c = crypto.lock().unwrap();
        c.seal(msg)
    }?;
    writer.write_all(&wire).context("写入失败（连接中断？）")
}

fn shutdown_reader(
    closing: &Arc<AtomicBool>,
    write_stream: &Arc<Mutex<std::net::TcpStream>>,
    reader: thread::JoinHandle<()>,
) -> Result<()> {
    closing.store(true, Ordering::Relaxed);
    if let Ok(stream) = write_stream.lock() {
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("HPSS reader 线程异常退出"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RenderSurfaceSnapshot {
    generation: u64,
    source_size: (usize, usize),
    drawable_size: (usize, usize),
    content_viewport: ContentViewport,
    type_zero_applied_count: u64,
    content_revision: u64,
    first_nonblack_render_revision: Option<u64>,
}

fn render_surface_frame_with<F>(
    surface: &Arc<Mutex<DisplaySurface>>,
    drawable_size: (usize, usize),
    render_pixels: &mut [u32],
    update_window: F,
) -> Result<RenderSurfaceSnapshot>
where
    F: FnOnce(&[u32], usize, usize) -> Result<()>,
{
    debug_assert_eq!(
        render_pixels.len(),
        drawable_size.0.saturating_mul(drawable_size.1)
    );
    render_pixels.fill(0);
    let mut snapshot = {
        let surface = surface.lock().unwrap();
        let framebuffer = &surface.framebuffer;
        let content_viewport =
            ContentViewport::fit((framebuffer.width, framebuffer.height), drawable_size);
        let snapshot = RenderSurfaceSnapshot {
            generation: surface.generation,
            source_size: (framebuffer.width, framebuffer.height),
            drawable_size,
            content_viewport,
            type_zero_applied_count: surface.native_mvs_observability.type_zero_applied_count,
            content_revision: surface.native_mvs_observability.content_revision,
            first_nonblack_render_revision: surface
                .native_mvs_observability
                .first_nonblack_render_revision,
        };
        downsample_into_viewport(
            framebuffer.pixels(),
            framebuffer.width,
            framebuffer.height,
            render_pixels,
            drawable_size.0,
            content_viewport,
        );
        snapshot
    };
    let rendered_native_mvs_nonblack = snapshot.type_zero_applied_count > 0
        && viewport_contains_nonblack(render_pixels, drawable_size.0, snapshot.content_viewport);
    update_window(render_pixels, drawable_size.0, drawable_size.1)?;
    let first_nonblack_event = if rendered_native_mvs_nonblack {
        let mut surface = surface.lock().unwrap();
        if surface.generation == snapshot.generation {
            let newly_observed = surface
                .native_mvs_observability
                .first_nonblack_render_revision
                .is_none();
            let first = surface
                .native_mvs_observability
                .first_nonblack_render_revision
                .get_or_insert(snapshot.content_revision);
            snapshot.first_nonblack_render_revision = Some(*first);
            newly_observed
        } else {
            false
        }
    } else {
        false
    };
    if first_nonblack_event {
        eprintln!(
            "[hpss-view] native MVS: generation={}, first_nonblack_render_revision={}",
            snapshot.generation, snapshot.content_revision
        );
    }
    Ok(snapshot)
}

fn downsample_into_viewport(
    src: &[u32],
    source_width: usize,
    source_height: usize,
    dst: &mut [u32],
    drawable_width: usize,
    viewport: ContentViewport,
) {
    for y in 0..viewport.height {
        for x in 0..viewport.width {
            let source_x = x * source_width / viewport.width;
            let source_y = y * source_height / viewport.height;
            dst[(viewport.y + y) * drawable_width + viewport.x + x] =
                src[source_y * source_width + source_x];
        }
    }
}

fn viewport_contains_nonblack(
    pixels: &[u32],
    drawable_width: usize,
    viewport: ContentViewport,
) -> bool {
    (0..viewport.height).any(|y| {
        let row_start = (viewport.y + y) * drawable_width + viewport.x;
        pixels[row_start..row_start + viewport.width]
            .iter()
            .any(|pixel| *pixel != 0)
    })
}

#[cfg(test)]
mod tests {
    use super::{
        apply_native_mvs_frame_with, apply_rgb_rect, apply_rgb_rect_for_generation,
        audio_input_probe_progress_diagnostic, commit_server_geometry, drain_udp_media,
        finish_full_boundary_at, finish_partial_boundary_at, incremental_request_after_full_apply,
        is_complete_surface_frame, map_pointer, mark_recovery_for_invalid_mvs_geometry,
        process_complete_mvs_record, read_viewer_app_frame_step, reader_frame_class,
        render_surface_frame_with, request_full_update_at, scaled_drawable_size,
        select_initial_display_size, service_reader_tick_at, should_log_audio_input_probe_progress,
        should_log_audio_resynchronization, shutdown_reader, validate_hpss_audio_flow,
        AudioOutputPhase, ContentViewport, DisplaySurface, DynamicResolutionRuntime,
        MediaAcceptOutcome, MvsReceiveState, MvsRecordOutcome, NativeMvsRenderObservability,
        ReaderFrameClass, ReaderRequestState, TableFollowupState, TableScheduleStatus,
        ViewerMediaState, ViewportRequestQueue, REVIEWED_AUDIO_INPUT_SOURCE_MODE,
    };
    use crate::framebuffer::Framebuffer;
    use crate::vnc::audio_codec::{
        AudioReceiveOutcome, DecodedAudioPacket, ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT,
    };
    use crate::vnc::audio_input::{
        AudioInputPhase, AudioInputSourceMode, P5_CONFIRMATION_TIMEOUT, P5_PROBE_FRAME_COUNT,
    };
    use crate::vnc::dynamic_resolution::DisplaySize;
    use crate::vnc::hpss::{self, encoding};
    use crate::vnc::media_negotiation::{
        AudioMediaFlow, CompressedProtobufAnswer, MediaStreamAnswer, SrtpMasterMaterial,
        SRTP_AES_256_MASTER_KEY_LEN, SRTP_MASTER_MATERIAL_LEN, SRTP_MASTER_SALT_LEN,
    };
    use crate::vnc::media_protocol::{self, parse_media_stream_port_announcement};
    use crate::vnc::media_transport::{
        MediaDatagram, MediaRole, MediaTransport, OutboundAudioSentRange,
    };
    use crate::vnc::mvs;
    use crate::vnc::mvs_stream::{MvsRecord, MvsRect};
    use crate::vnc::protocol;
    use crate::vnc::srtp::{derive_session_keys, protect_rtp_packet, SrtcpSender, SrtpPacketKind};
    use std::net::{IpAddr, Ipv4Addr, UdpSocket};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    thread_local! {
        static INJECT_NATIVE_MVS_COMMIT_FAILURE: std::cell::Cell<bool> = const {
            std::cell::Cell::new(false)
        };
    }

    pub(super) fn commit_prepared_mvs(
        receiver: &mut MvsReceiveState,
        prepared: mvs::PreparedGenerationMvs,
    ) -> anyhow::Result<()> {
        if INJECT_NATIVE_MVS_COMMIT_FAILURE.with(std::cell::Cell::get) {
            anyhow::bail!("injected native MVS commit failure");
        }
        receiver.commit(prepared).map(|_| ())
    }

    fn with_native_mvs_commit_failure<R>(action: impl FnOnce() -> R) -> R {
        INJECT_NATIVE_MVS_COMMIT_FAILURE.with(|injection| {
            assert!(!injection.replace(true), "commit failure injection nested");
            struct ResetInjection<'a>(&'a std::cell::Cell<bool>);
            impl Drop for ResetInjection<'_> {
                fn drop(&mut self) {
                    self.0.set(false);
                }
            }
            let _reset = ResetInjection(injection);
            action()
        })
    }

    struct DrainUdpMediaLoopback {
        transport: MediaTransport,
        remote: UdpSocket,
        local: std::net::SocketAddr,
        incoming_material: SrtpMasterMaterial,
    }

    impl DrainUdpMediaLoopback {
        const LOCAL_MEDIA_ADDRESS: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);
        const REMOTE_MEDIA_ADDRESS: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);
        const ATTACKER_MEDIA_ADDRESS: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 3);

        fn new(role: MediaRole) -> Self {
            Self::new_with_audio_flow(role, AudioMediaFlow::MacToPc)
        }

        fn new_with_audio_flow(role: MediaRole, audio_flow: AudioMediaFlow) -> Self {
            let remote = UdpSocket::bind((Self::REMOTE_MEDIA_ADDRESS, 0)).unwrap();
            remote
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let remote_port = remote.local_addr().unwrap().port();
            let announcement = parse_media_stream_port_announcement(
                &single_role_port_announcement_fixture(role, remote_port),
            )
            .unwrap();
            let mut transport = MediaTransport::new(0, IpAddr::V4(Self::REMOTE_MEDIA_ADDRESS));
            transport.set_audio_flow(audio_flow).unwrap();
            transport.accept_port_announcement(0, announcement).unwrap();
            transport
                .bind_local_sockets(0, IpAddr::V4(Self::LOCAL_MEDIA_ADDRESS))
                .unwrap();
            let configuration = transport.prepare_configuration(0).unwrap();
            let incoming_material = incoming_material_from_configuration(&configuration, role);
            transport.mark_configuration_sent(0).unwrap();
            transport
                .accept_answer(0, loopback_answer_fixture())
                .unwrap();
            transport.activate(0).unwrap();
            let local = transport.local_addr(0, role).unwrap();
            let mut initial_report = [0u8; 128];
            remote.recv_from(&mut initial_report).unwrap();
            Self {
                transport,
                remote,
                local,
                incoming_material,
            }
        }

        fn send_discarded_traffic(&self) {
            let attacker = UdpSocket::bind((Self::ATTACKER_MEDIA_ADDRESS, 0)).unwrap();
            attacker.send_to(b"unexpected-source", self.local).unwrap();
            self.remote.send_to(&[], self.local).unwrap();
        }

        fn send_rtp(&self, sequence: u16, payload: &[u8]) {
            let plaintext = test_rtp_packet(sequence, payload);
            let keys = derive_session_keys(&self.incoming_material, SrtpPacketKind::Rtp);
            let protected = protect_rtp_packet(&plaintext, &keys, 0).unwrap();
            self.remote.send_to(&protected, self.local).unwrap();
        }

        fn send_rtcp(&self, plaintext: &[u8]) {
            let keys = derive_session_keys(&self.incoming_material, SrtpPacketKind::Rtcp);
            let protected = SrtcpSender::new(keys).protect(plaintext).unwrap();
            self.remote.send_to(&protected, self.local).unwrap();
        }
    }

    fn single_role_port_announcement_fixture(role: MediaRole, port: u16) -> [u8; 54] {
        let mut frame = [0u8; 54];
        frame[0..4].copy_from_slice(&1u32.to_be_bytes());
        frame[12..16].copy_from_slice(&0x03f2i32.to_be_bytes());
        frame[16..18].copy_from_slice(&36u16.to_be_bytes());
        frame[18..20].copy_from_slice(&1u16.to_be_bytes());
        frame[20..22].copy_from_slice(&1u16.to_be_bytes());
        let (port_offset, flags_offset) = match role {
            MediaRole::Audio => (26, 28),
            MediaRole::VideoStream1 => (32, 34),
            MediaRole::VideoStream2 => panic!("测试配置没有第二视频流"),
        };
        frame[port_offset..port_offset + 2].copy_from_slice(&port.to_be_bytes());
        frame[flags_offset..flags_offset + 4].copy_from_slice(&1u32.to_be_bytes());
        frame
    }

    fn loopback_answer_fixture() -> MediaStreamAnswer {
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

    fn incoming_material_from_configuration(
        configuration: &[u8],
        role: MediaRole,
    ) -> SrtpMasterMaterial {
        const CONFIGURATION_ENTRIES_OFFSET: usize = 36;
        const AUDIO_OFFER_LENGTH_OFFSET: usize = 10;
        let audio_offer_len = usize::from(u16::from_be_bytes(
            configuration[AUDIO_OFFER_LENGTH_OFFSET..AUDIO_OFFER_LENGTH_OFFSET + 2]
                .try_into()
                .unwrap(),
        ));
        let entry_offset = match role {
            MediaRole::Audio => CONFIGURATION_ENTRIES_OFFSET,
            MediaRole::VideoStream1 => {
                CONFIGURATION_ENTRIES_OFFSET + SRTP_MASTER_MATERIAL_LEN * 2 + audio_offer_len
            }
            MediaRole::VideoStream2 => panic!("测试配置没有第二视频流"),
        };
        let incoming_offset = entry_offset + SRTP_MASTER_MATERIAL_LEN;
        let key_end = incoming_offset + SRTP_AES_256_MASTER_KEY_LEN;
        let salt_end = key_end + SRTP_MASTER_SALT_LEN;
        SrtpMasterMaterial {
            master_key: configuration[incoming_offset..key_end].try_into().unwrap(),
            master_salt: configuration[key_end..salt_end].try_into().unwrap(),
        }
    }

    fn test_rtp_packet(sequence: u16, payload: &[u8]) -> Vec<u8> {
        const RTP_VERSION_2: u8 = 0x80;
        const SCREEN_SHARING_PAYLOAD_TYPE: u8 = 101;
        let mut packet = vec![RTP_VERSION_2, SCREEN_SHARING_PAYLOAD_TYPE];
        packet.extend_from_slice(&sequence.to_be_bytes());
        packet.extend_from_slice(&960u32.to_be_bytes());
        packet.extend_from_slice(&0x5566_7788u32.to_be_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    fn receiver_report(
        source_ssrc: u32,
        extended_highest_sequence: u32,
        cumulative_packets_lost: i32,
    ) -> Vec<u8> {
        let mut report = vec![0x81, 201, 0, 7];
        report.extend_from_slice(&0xaabb_ccdd_u32.to_be_bytes());
        report.extend_from_slice(&source_ssrc.to_be_bytes());
        report.push(0);
        report.extend_from_slice(&cumulative_packets_lost.to_be_bytes()[1..]);
        report.extend_from_slice(&extended_highest_sequence.to_be_bytes());
        report.extend_from_slice(&0u32.to_be_bytes());
        report.extend_from_slice(&0u32.to_be_bytes());
        report.extend_from_slice(&0u32.to_be_bytes());
        report
    }

    fn send_entire_probe(
        state: &mut ViewerMediaState,
        transport: &mut MediaTransport,
        now: Instant,
    ) {
        while state.sent_audio_access_units < u64::from(P5_PROBE_FRAME_COUNT) {
            state.service_audio_input_probe(transport, 0, now);
            assert!(matches!(
                state.audio_input_phase(),
                AudioInputPhase::ProbeSending { generation: 0 }
                    | AudioInputPhase::ProbeAwaitingReport { generation: 0 }
            ));
        }
        assert_eq!(state.sent_audio_access_units, 500);
        assert!(matches!(
            state.audio_input_phase(),
            AudioInputPhase::ProbeAwaitingReport { generation: 0 }
        ));
    }

    fn assert_video_and_control_remain_serviceable(state: &mut ViewerMediaState, now: Instant) {
        assert_eq!(
            state
                .accept(MediaRole::VideoStream1, MediaDatagram::Rtp(vec![1]))
                .unwrap(),
            MediaAcceptOutcome::AuthenticatedNotRendered
        );
        let initial = DisplaySize::new(1440, 900).unwrap();
        let mut receiver = MvsReceiveState::new(0);
        let mut requests = ReaderRequestState::after_startup(now);
        let mut queue = ViewportRequestQueue::default();
        let mut dynamic = DynamicResolutionRuntime::new(initial, false);
        service_reader_tick_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut dynamic,
            now,
            |_| {},
            || Ok(()),
            |_| anyhow::bail!("disabled dynamic path must not send"),
        )
        .unwrap();
    }

    #[test]
    fn landscape_initial_geometry_keeps_the_trusted_connection_size() {
        let size = select_initial_display_size(1920, 1080).unwrap();

        assert_eq!(size, DisplaySize::new(1920, 1080).unwrap());
    }

    #[test]
    fn scale_changes_only_the_drawable_size() {
        let surface = DisplaySize::new(1920, 1080).unwrap();

        assert_eq!(scaled_drawable_size(surface, 0.25).unwrap(), (480, 270));
        assert_eq!(surface, DisplaySize::new(1920, 1080).unwrap());
    }

    #[test]
    fn drawable_scale_rejects_non_downsample_values() {
        let surface = DisplaySize::new(1920, 1080).unwrap();

        for scale in [f32::NAN, f32::INFINITY, 0.0, -0.25, 1.01] {
            assert!(
                scaled_drawable_size(surface, scale).is_err(),
                "scale {scale:?} 必须在 viewer helper 被拒绝"
            );
        }
    }

    #[test]
    fn drain_udp_media_counts_accepted_video_and_mutates_viewer_state() {
        let mut loopback = DrainUdpMediaLoopback::new(MediaRole::VideoStream1);
        loopback.send_discarded_traffic();
        loopback.send_rtp(1, b"video");
        let mut media_state = ViewerMediaState::new(AudioMediaFlow::MacToPc).unwrap();

        let accepted = drain_udp_media(&mut loopback.transport, &mut media_state, 0).unwrap();

        assert_eq!(accepted, 1);
        assert_eq!(media_state.authenticated_video_packets, 1);
        assert_eq!(media_state.authenticated_audio_packets, 0);
        assert!(media_state.audio_playback.is_none());
        let counters = loopback.transport.discard_counters();
        assert_eq!(counters.unexpected_source, 1);
        assert_eq!(counters.empty_datagram, 1);
        assert_eq!(
            drain_udp_media(&mut loopback.transport, &mut media_state, 0).unwrap(),
            0,
            "active transport Empty must retain the adapter's zero accepted count"
        );
    }

    #[test]
    fn authenticated_udp_video_is_not_reported_as_framebuffer_applied() {
        let mut state = ViewerMediaState::new(AudioMediaFlow::MacToPc).unwrap();

        for role in [MediaRole::VideoStream1, MediaRole::VideoStream2] {
            assert_eq!(
                state.accept(role, MediaDatagram::Rtp(vec![1])).unwrap(),
                MediaAcceptOutcome::AuthenticatedNotRendered,
                "认证的 {role:?} RTP 尚未经过解包、解码或 framebuffer 写入"
            );
        }
        assert_eq!(state.authenticated_video_packets, 2);
    }

    #[test]
    fn drain_udp_media_inactive_transport_remains_a_noop() {
        let mut transport =
            MediaTransport::new(0, IpAddr::V4(DrainUdpMediaLoopback::REMOTE_MEDIA_ADDRESS));
        let mut media_state = ViewerMediaState::new(AudioMediaFlow::MacToPc).unwrap();

        assert_eq!(
            drain_udp_media(&mut transport, &mut media_state, 0).unwrap(),
            0
        );
        assert_eq!(media_state.authenticated_video_packets, 0);
        assert_eq!(media_state.authenticated_audio_packets, 0);
    }

    #[test]
    fn drain_udp_media_degrades_authenticated_audio_handler_failure() {
        let mut loopback = DrainUdpMediaLoopback::new(MediaRole::Audio);
        loopback.send_rtp(1, &[]);
        let mut media_state = ViewerMediaState::new(AudioMediaFlow::MacToPc).unwrap();

        let accepted = drain_udp_media(&mut loopback.transport, &mut media_state, 0).unwrap();

        assert_eq!(accepted, 1);
        assert!(matches!(
            media_state.audio_output_phase(),
            AudioOutputPhase::Degraded { reason } if reason.contains("ARD audio RTP payload 为空")
        ));
        assert!(media_state.audio_playback.is_none());
    }

    #[test]
    fn protected_malformed_audio_rtcp_is_discarded_by_transport() {
        let mut loopback = DrainUdpMediaLoopback::new(MediaRole::Audio);
        let receiver_report_missing_declared_block =
            [0x81, 201, 0x00, 0x01, 0x11, 0x22, 0x33, 0x44];
        loopback.send_rtcp(&receiver_report_missing_declared_block);
        let mut media_state = ViewerMediaState::new(AudioMediaFlow::MacToPc).unwrap();

        assert_eq!(
            drain_udp_media(&mut loopback.transport, &mut media_state, 0).unwrap(),
            0
        );
        assert_eq!(loopback.transport.discard_counters().malformed_packet, 1);
        assert_eq!(
            media_state.audio_output_phase(),
            &AudioOutputPhase::ReadyToStart
        );
    }

    #[test]
    fn protected_valid_audio_rtcp_is_accepted_without_an_outbound_stream() {
        let mut loopback = DrainUdpMediaLoopback::new(MediaRole::Audio);
        let receiver_report_without_blocks = [0x80, 201, 0x00, 0x01, 0x11, 0x22, 0x33, 0x44];
        loopback.send_rtcp(&receiver_report_without_blocks);
        let mut media_state = ViewerMediaState::new(AudioMediaFlow::MacToPc).unwrap();

        assert_eq!(
            drain_udp_media(&mut loopback.transport, &mut media_state, 0).unwrap(),
            1
        );
        assert_eq!(
            media_state.audio_output_phase(),
            &AudioOutputPhase::ReadyToStart
        );
    }

    #[test]
    fn audio_failure_degrades_audio_without_returning_a_viewer_fatal_error() {
        let mut state = ViewerMediaState::new(AudioMediaFlow::MacToPc).unwrap();
        let malformed_authenticated_rtp = vec![0u8; 3];

        let outcome = state
            .accept(
                MediaRole::Audio,
                MediaDatagram::Rtp(malformed_authenticated_rtp),
            )
            .unwrap();

        assert_eq!(outcome, MediaAcceptOutcome::AudioDegraded);
        assert!(matches!(
            state.audio_output_phase(),
            AudioOutputPhase::Degraded { .. }
        ));
        assert_eq!(state.authenticated_video_packets, 0);

        let reason = format!("{:?}", state.audio_output_phase());
        assert_eq!(
            state
                .accept(MediaRole::Audio, MediaDatagram::Rtp(vec![0u8; 3]))
                .unwrap(),
            MediaAcceptOutcome::Discarded,
            "degraded audio must not retry the codec for every packet"
        );
        assert_eq!(format!("{:?}", state.audio_output_phase()), reason);
    }

    #[test]
    fn late_audio_packet_is_counted_without_degrading_output() {
        let mut state = ViewerMediaState::new(AudioMediaFlow::MacToPc).unwrap();
        state.late_audio_packets = u64::MAX;

        let outcome = state.accept_audio_outcome(AudioReceiveOutcome::DiscardedLate {
            sequence: 101,
            last_forward_sequence: 102,
        });

        assert_eq!(outcome, MediaAcceptOutcome::Discarded);
        assert_eq!(state.late_audio_packets, u64::MAX);
        assert_eq!(state.audio_output_phase(), &AudioOutputPhase::ReadyToStart);
    }

    #[test]
    fn resynchronized_audio_uses_normal_apply_path_without_degrading_output() {
        const SKIPPED_ACCESS_UNITS: usize = 117;
        let mut state = ViewerMediaState::new(AudioMediaFlow::MacToPc).unwrap();
        let mut output_calls = 0;
        let decoded = DecodedAudioPacket {
            pcm: vec![1; ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT],
            concealed_access_units: 0,
            sequence: 218,
            timestamp: 9_000_000,
            ssrc: 0x1020_3040,
        };

        let outcome = state.accept_audio_outcome_with_output(
            AudioReceiveOutcome::Resynchronized {
                decoded,
                skipped_access_units: SKIPPED_ACCESS_UNITS,
            },
            |_, decoded| {
                output_calls += 1;
                assert_eq!(decoded.sequence, 218);
                assert_eq!(decoded.pcm.len(), ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT);
                Ok(())
            },
        );

        assert_eq!(outcome, MediaAcceptOutcome::Applied);
        assert_eq!(output_calls, 1);
        assert_eq!(state.audio_resynchronizations, 1);
        assert_eq!(state.authenticated_audio_packets, 1);
        assert_eq!(state.non_silent_audio_access_units, 1);
        assert_eq!(state.concealed_audio_access_units, 0);
        assert_eq!(state.audio_output_phase(), &AudioOutputPhase::Active);
    }

    #[test]
    fn audio_resynchronization_counter_saturates_and_samples_only_powers_of_two() {
        assert_eq!(
            (0..=6)
                .filter(|count| should_log_audio_resynchronization(*count))
                .collect::<Vec<_>>(),
            vec![1, 2, 4]
        );

        let mut state = ViewerMediaState::new(AudioMediaFlow::MacToPc).unwrap();
        state.audio_resynchronizations = u64::MAX;
        let outcome = state.accept_audio_outcome_with_output(
            AudioReceiveOutcome::Resynchronized {
                decoded: DecodedAudioPacket {
                    pcm: vec![0; ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT],
                    concealed_access_units: 0,
                    sequence: 218,
                    timestamp: 9_000_000,
                    ssrc: 0x1020_3040,
                },
                skipped_access_units: 117,
            },
            |_, _| Ok(()),
        );

        assert_eq!(outcome, MediaAcceptOutcome::Applied);
        assert_eq!(state.audio_resynchronizations, u64::MAX);
    }

    #[test]
    fn rgb_channel_hpss_consumer_packs_independent_rgb24_fixture() {
        assert_eq!(
            [
                crate::vnc::mvs::MVS_RGB_RED_OFFSET,
                crate::vnc::mvs::MVS_RGB_GREEN_OFFSET,
                crate::vnc::mvs::MVS_RGB_BLUE_OFFSET,
            ],
            [0, 1, 2]
        );
        let mut framebuffer = Framebuffer::new(1, 1).unwrap();
        apply_rgb_rect(&mut framebuffer, &[0x12, 0x34, 0x56], 0, 0, 1, 1).unwrap();
        assert_eq!(framebuffer.pixels(), &[0x0012_3456]);
    }

    #[test]
    fn render_window_update_does_not_hold_the_display_surface_lock() {
        use std::sync::mpsc;
        use std::sync::{Arc, Mutex};
        use std::thread;

        let mut framebuffer = Framebuffer::new(2, 1).unwrap();
        framebuffer
            .pixels_mut()
            .copy_from_slice(&[0x0012_3456, 0x0078_9abc]);
        let surface = Arc::new(Mutex::new(DisplaySurface::new(7, framebuffer)));
        let (update_entered_tx, update_entered_rx) = mpsc::channel();
        let (release_update_tx, release_update_rx) = mpsc::channel();
        let render_surface = Arc::clone(&surface);
        let render = thread::spawn(move || {
            let mut pixels = vec![0; 2];
            let snapshot = render_surface_frame_with(
                &render_surface,
                (2, 1),
                &mut pixels,
                |pixels, width, height| {
                    assert_eq!((width, height), (2, 1));
                    assert_eq!(pixels, &[0x0012_3456, 0x0078_9abc]);
                    update_entered_tx.send(()).unwrap();
                    release_update_rx
                        .recv_timeout(Duration::from_secs(1))
                        .unwrap();
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(snapshot.generation, 7);
            assert_eq!(snapshot.source_size, (2, 1));
            assert_eq!(snapshot.drawable_size, (2, 1));
        });
        update_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let reader_surface = Arc::clone(&surface);
        let (lock_attempt_tx, lock_attempt_rx) = mpsc::channel();
        let (lock_acquired_tx, lock_acquired_rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            lock_attempt_tx.send(()).unwrap();
            let locked = reader_surface.lock().unwrap();
            lock_acquired_tx
                .send((
                    locked.generation,
                    locked.framebuffer.width,
                    locked.framebuffer.height,
                ))
                .unwrap();
        });
        lock_attempt_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let acquired_before_update_release =
            lock_acquired_rx.recv_timeout(Duration::from_millis(250));
        release_update_tx.send(()).unwrap();
        let acquired = match acquired_before_update_release {
            Ok(acquired) => acquired,
            Err(_) => lock_acquired_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
        };
        render.join().unwrap();
        reader.join().unwrap();

        assert_eq!(acquired, (7, 2, 1));
        assert!(
            acquired_before_update_release.is_ok(),
            "reader surface lock must remain available while window update is withheld"
        );
    }

    #[test]
    fn render_surface_snapshot_preserves_nearest_neighbor_downsample() {
        use std::sync::{Arc, Mutex};

        let mut framebuffer = Framebuffer::new(4, 2).unwrap();
        framebuffer.pixels_mut().copy_from_slice(&[
            0x0000_0001,
            0x0000_0002,
            0x0000_0003,
            0x0000_0004,
            0x0000_0005,
            0x0000_0006,
            0x0000_0007,
            0x0000_0008,
        ]);
        let surface = Arc::new(Mutex::new(DisplaySurface::new(9, framebuffer)));
        let mut pixels = vec![0; 2];

        let snapshot =
            render_surface_frame_with(&surface, (2, 1), &mut pixels, |pixels, width, height| {
                assert_eq!((width, height), (2, 1));
                assert_eq!(pixels, &[0x0000_0001, 0x0000_0003]);
                Ok(())
            })
            .unwrap();

        assert_eq!(snapshot.generation, 9);
        assert_eq!(snapshot.source_size, (4, 2));
        assert_eq!(snapshot.drawable_size, (2, 1));
    }

    #[test]
    fn content_viewport_preserves_aspect_ratio_and_centers_integer_bars() {
        assert_eq!(
            ContentViewport::fit((1440, 2560), (1280, 720)),
            ContentViewport {
                x: 437,
                y: 0,
                width: 405,
                height: 720,
            }
        );
        assert_eq!(
            ContentViewport::fit((2560, 1440), (720, 1280)),
            ContentViewport {
                x: 0,
                y: 437,
                width: 720,
                height: 405,
            }
        );
        assert_eq!(
            ContentViewport::fit((640, 360), (640, 360)),
            ContentViewport {
                x: 0,
                y: 0,
                width: 640,
                height: 360,
            }
        );
    }

    #[test]
    fn portrait_surface_renders_centered_without_stretching_into_landscape_bars() {
        use std::sync::{Arc, Mutex};

        let mut framebuffer = Framebuffer::new(1440, 2560).unwrap();
        framebuffer.pixels_mut().fill(0x0012_3456);
        let mut display_surface = DisplaySurface::new(11, framebuffer);
        display_surface.native_mvs_observability = NativeMvsRenderObservability {
            type_zero_applied_count: 1,
            content_revision: 1,
            first_nonblack_render_revision: None,
        };
        let surface = Arc::new(Mutex::new(display_surface));
        let mut pixels = vec![0x00ff_00ff; 1280 * 720];

        let snapshot = render_surface_frame_with(
            &surface,
            (1280, 720),
            &mut pixels,
            |pixels, width, height| {
                assert_eq!((width, height), (1280, 720));
                for row in pixels.chunks_exact(1280) {
                    assert!(row[..437].iter().all(|pixel| *pixel == 0));
                    assert!(row[437..842].iter().all(|pixel| *pixel == 0x0012_3456));
                    assert!(row[842..].iter().all(|pixel| *pixel == 0));
                }
                assert_eq!(
                    pixels.iter().filter(|pixel| **pixel != 0).count(),
                    405 * 720
                );
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            snapshot.content_viewport,
            ContentViewport {
                x: 437,
                y: 0,
                width: 405,
                height: 720,
            }
        );
        assert_eq!(snapshot.first_nonblack_render_revision, Some(1));
    }

    #[test]
    fn viewer_api_rejects_all_password_hpss_pc_to_mac_audio() {
        assert!(validate_hpss_audio_flow(
            AudioMediaFlow::MacToPc,
            REVIEWED_AUDIO_INPUT_SOURCE_MODE,
        )
        .is_ok());
        assert!(validate_hpss_audio_flow(
            AudioMediaFlow::PcToMac,
            AudioInputSourceMode::DeterministicProbe,
        )
        .is_err());
        assert!(validate_hpss_audio_flow(
            AudioMediaFlow::PcToMac,
            AudioInputSourceMode::WindowsMicrophone,
        )
        .is_err());
    }

    #[test]
    fn evidence_probe_starts_only_after_active_mode_4_transport() {
        let now = Instant::now();
        let inactive = MediaTransport::new(0, IpAddr::V4(Ipv4Addr::LOCALHOST));
        let mut state = ViewerMediaState::new(AudioMediaFlow::PcToMac).unwrap();

        state.start_audio_input_probe(&inactive, 0, now);
        assert!(state.audio_encoder.is_none());
        assert_eq!(state.audio_input_phase(), &AudioInputPhase::Disabled);

        let loopback =
            DrainUdpMediaLoopback::new_with_audio_flow(MediaRole::Audio, AudioMediaFlow::PcToMac);
        state.start_audio_input_probe(&loopback.transport, 0, now);
        assert!(state.audio_encoder.is_some());
        assert_eq!(
            state.audio_input_phase(),
            &AudioInputPhase::ProbeSending { generation: 0 }
        );
    }

    #[test]
    fn deterministic_probe_never_constructs_audio_capture() {
        let now = Instant::now();
        let loopback =
            DrainUdpMediaLoopback::new_with_audio_flow(MediaRole::Audio, AudioMediaFlow::PcToMac);
        let mut state = ViewerMediaState::new(AudioMediaFlow::PcToMac).unwrap();

        assert_eq!(
            state.audio_input_runtime.source_mode(),
            AudioInputSourceMode::DeterministicProbe
        );
        state.start_audio_input_probe(&loopback.transport, 0, now);

        assert!(matches!(
            state.audio_input_phase(),
            AudioInputPhase::ProbeSending { generation: 0 }
        ));
    }

    #[test]
    fn deterministic_probe_sends_at_most_32_access_units_per_reader_tick() {
        let started = Instant::now();
        let mut loopback =
            DrainUdpMediaLoopback::new_with_audio_flow(MediaRole::Audio, AudioMediaFlow::PcToMac);
        let mut state = ViewerMediaState::new(AudioMediaFlow::PcToMac).unwrap();
        state.start_audio_input_probe(&loopback.transport, 0, started);

        state.service_audio_input_probe(
            &mut loopback.transport,
            0,
            started + Duration::from_secs(5),
        );

        assert_eq!(state.sent_audio_access_units, 32);
        assert!(matches!(
            state.audio_input_phase(),
            AudioInputPhase::ProbeSending { generation: 0 }
        ));
    }

    #[test]
    fn evidence_probe_rejects_active_mac_to_pc_transport() {
        let now = Instant::now();
        let loopback =
            DrainUdpMediaLoopback::new_with_audio_flow(MediaRole::Audio, AudioMediaFlow::MacToPc);
        let mut state = ViewerMediaState::new(AudioMediaFlow::PcToMac).unwrap();

        state.start_audio_input_probe(&loopback.transport, 0, now);

        assert!(state.audio_encoder.is_none());
        assert_eq!(state.audio_input_phase(), &AudioInputPhase::Disabled);
    }

    #[test]
    fn evidence_probe_rejects_active_transport_without_audio_role() {
        let now = Instant::now();
        let loopback = DrainUdpMediaLoopback::new_with_audio_flow(
            MediaRole::VideoStream1,
            AudioMediaFlow::PcToMac,
        );
        let mut state = ViewerMediaState::new(AudioMediaFlow::PcToMac).unwrap();

        state.start_audio_input_probe(&loopback.transport, 0, now);

        assert!(state.audio_encoder.is_none());
        assert_eq!(state.audio_input_phase(), &AudioInputPhase::Disabled);
    }

    #[test]
    fn audio_input_probe_progress_diagnostics_are_bounded_and_stop_after_terminal_state() {
        let diagnostic_counts = (0u32..=520)
            .filter(|count| should_log_audio_input_probe_progress(u64::from(*count)))
            .filter_map(|count| {
                audio_input_probe_progress_diagnostic(
                    u64::from(count),
                    Some(OutboundAudioSentRange {
                        ssrc: 0x1122_3344,
                        first_extended_sequence: 10,
                        last_extended_sequence: 10 + count.saturating_sub(1),
                        packets_sent: count,
                    }),
                )
            })
            .map(|diagnostic| diagnostic.packets_sent)
            .collect::<Vec<_>>();
        assert_eq!(
            diagnostic_counts,
            vec![1, 2, 4, 8, 16, 32, 64, 128, 256, 500]
        );

        let started = Instant::now();
        let terminal_at = started + Duration::from_secs(5);
        let mut loopback =
            DrainUdpMediaLoopback::new_with_audio_flow(MediaRole::Audio, AudioMediaFlow::PcToMac);
        let mut state = ViewerMediaState::new(AudioMediaFlow::PcToMac).unwrap();
        state.start_audio_input_probe(&loopback.transport, 0, started);
        send_entire_probe(&mut state, &mut loopback.transport, terminal_at);
        let terminal_range = loopback.transport.outbound_audio_sent_range().unwrap();
        loopback.send_rtcp(&receiver_report(
            terminal_range.ssrc,
            terminal_range.last_extended_sequence,
            0,
        ));
        drain_udp_media(&mut loopback.transport, &mut state, 0).unwrap();
        state.observe_audio_input_transport(&loopback.transport, 0, terminal_at);
        assert!(matches!(
            state.audio_input_phase(),
            AudioInputPhase::ProbeConfirmed { generation: 0 }
        ));

        state.service_audio_input_probe(
            &mut loopback.transport,
            0,
            terminal_at + Duration::from_secs(30),
        );
        assert_eq!(state.sent_audio_access_units, 500);
        assert_eq!(
            loopback.transport.outbound_audio_sent_range(),
            Some(terminal_range)
        );
        assert!(!should_log_audio_input_probe_progress(501));
        assert!(!should_log_audio_input_probe_progress(512));
    }

    #[test]
    fn device_free_probe_sends_500_frames_and_confirms_from_typed_srtcp_evidence() {
        let started = Instant::now();
        let send_at = started + Duration::from_secs(5);
        let mut loopback =
            DrainUdpMediaLoopback::new_with_audio_flow(MediaRole::Audio, AudioMediaFlow::PcToMac);
        let mut state = ViewerMediaState::new(AudioMediaFlow::PcToMac).unwrap();
        state.start_audio_input_probe(&loopback.transport, 0, started);
        send_entire_probe(&mut state, &mut loopback.transport, send_at);

        let sent = loopback.transport.outbound_audio_sent_range().unwrap();
        assert_eq!(sent.packets_sent, 500);
        loopback.send_rtcp(&receiver_report(sent.ssrc, sent.last_extended_sequence, 0));
        drain_udp_media(&mut loopback.transport, &mut state, 0).unwrap();
        state.observe_audio_input_transport(&loopback.transport, 0, send_at);

        assert_eq!(
            state.audio_input_phase(),
            &AudioInputPhase::ProbeConfirmed { generation: 0 }
        );
        let diagnostic = state
            .take_audio_input_confirmation_diagnostic()
            .expect("late typed SRTCP confirmation must expose one diagnostic");
        assert_eq!(diagnostic.packets_sent, sent.packets_sent);
        assert_eq!(diagnostic.ssrc, sent.ssrc);
        assert_eq!(
            diagnostic.srtcp_extended_highest_sequence,
            sent.last_extended_sequence
        );
        state.service_audio_input_probe(
            &mut loopback.transport,
            0,
            send_at + P5_CONFIRMATION_TIMEOUT,
        );
        state.observe_audio_input_transport(&loopback.transport, 0, send_at);
        assert_eq!(state.take_audio_input_confirmation_diagnostic(), None);
        assert_eq!(loopback.transport.outbound_audio_sent_range(), Some(sent));
        assert_eq!(state.sent_audio_access_units, 500);
        assert_video_and_control_remain_serviceable(&mut state, send_at);
    }

    #[test]
    fn deterministic_probe_releases_encoder_after_confirm_or_degrade() {
        let started = Instant::now();
        let send_at = started + Duration::from_secs(5);
        let mut confirmed_loopback =
            DrainUdpMediaLoopback::new_with_audio_flow(MediaRole::Audio, AudioMediaFlow::PcToMac);
        let mut confirmed = ViewerMediaState::new(AudioMediaFlow::PcToMac).unwrap();
        confirmed.start_audio_input_probe(&confirmed_loopback.transport, 0, started);
        send_entire_probe(&mut confirmed, &mut confirmed_loopback.transport, send_at);
        let sent = confirmed_loopback
            .transport
            .outbound_audio_sent_range()
            .unwrap();
        confirmed_loopback.send_rtcp(&receiver_report(sent.ssrc, sent.last_extended_sequence, 0));
        drain_udp_media(&mut confirmed_loopback.transport, &mut confirmed, 0).unwrap();
        confirmed.observe_audio_input_transport(&confirmed_loopback.transport, 0, send_at);
        assert!(confirmed.audio_encoder.is_none());

        let mut degraded_loopback =
            DrainUdpMediaLoopback::new_with_audio_flow(MediaRole::Audio, AudioMediaFlow::PcToMac);
        let mut degraded = ViewerMediaState::new(AudioMediaFlow::PcToMac).unwrap();
        degraded.start_audio_input_probe(&degraded_loopback.transport, 0, started);
        degraded_loopback.transport.close(0).unwrap();
        degraded.service_audio_input_probe(&mut degraded_loopback.transport, 0, started);
        assert!(degraded.audio_encoder.is_none());
    }

    #[test]
    fn early_srtcp_confirmation_emits_exactly_once_at_frame_500() {
        let started = Instant::now();
        let before_final_frame = started + Duration::from_millis(4_980);
        let final_frame_at = started + Duration::from_millis(4_990);
        let mut loopback =
            DrainUdpMediaLoopback::new_with_audio_flow(MediaRole::Audio, AudioMediaFlow::PcToMac);
        let mut state = ViewerMediaState::new(AudioMediaFlow::PcToMac).unwrap();
        state.start_audio_input_probe(&loopback.transport, 0, started);
        while state.sent_audio_access_units < 499 {
            state.service_audio_input_probe(&mut loopback.transport, 0, before_final_frame);
        }
        assert_eq!(state.sent_audio_access_units, 499);

        let before_final_range = loopback.transport.outbound_audio_sent_range().unwrap();
        loopback.send_rtcp(&receiver_report(
            before_final_range.ssrc,
            before_final_range.last_extended_sequence,
            0,
        ));
        drain_udp_media(&mut loopback.transport, &mut state, 0).unwrap();
        state.observe_audio_input_transport(&loopback.transport, 0, before_final_frame);
        assert_eq!(
            state.audio_input_phase(),
            &AudioInputPhase::ProbeSending { generation: 0 }
        );
        assert_eq!(state.take_audio_input_confirmation_diagnostic(), None);

        state.service_audio_input_probe(&mut loopback.transport, 0, final_frame_at);
        let final_range = loopback.transport.outbound_audio_sent_range().unwrap();
        let diagnostic = state
            .take_audio_input_confirmation_diagnostic()
            .expect("frame 500 must make the early typed confirmation diagnostic available");
        assert_eq!(diagnostic.packets_sent, 500);
        assert_eq!(diagnostic.ssrc, final_range.ssrc);
        assert_eq!(
            diagnostic.first_extended_sequence,
            final_range.first_extended_sequence
        );
        assert_eq!(
            diagnostic.last_extended_sequence,
            final_range.last_extended_sequence
        );
        assert_eq!(
            diagnostic.srtcp_extended_highest_sequence,
            before_final_range.last_extended_sequence
        );
        assert_eq!(diagnostic.srtcp_cumulative_packets_lost, 0);

        state.observe_audio_input_transport(&loopback.transport, 0, final_frame_at);
        state.service_audio_input_probe(
            &mut loopback.transport,
            0,
            final_frame_at + P5_CONFIRMATION_TIMEOUT,
        );
        assert_eq!(state.take_audio_input_confirmation_diagnostic(), None);
    }

    #[test]
    fn viewer_iteration_services_p5_before_incomplete_encrypted_frame_completes() {
        use std::io::Write as _;

        let key = [0x51; 16];
        let iv = [0x62; 16];
        let mut sender_crypto = crate::vnc::session::SessionCrypto::from_key_iv(key, iv);
        let wire = sender_crypto.seal(b"viewer-progress").unwrap();
        let (prefix_sent, prefix_ready) = std::sync::mpsc::channel();
        let (release_remainder, remainder_ready) = std::sync::mpsc::channel();
        let (p5_service_progress, p5_service_observed) = std::sync::mpsc::channel();
        let (plaintext_sent, plaintext_ready) = std::sync::mpsc::channel();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&wire[..1]).unwrap();
            prefix_sent.send(()).unwrap();
            remainder_ready.recv().unwrap();
            stream.write_all(&wire[1..]).unwrap();
        });

        let stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut connection = crate::vnc::client::RfbConn::new(stream);
        connection.set_crypto(crate::vnc::session::SessionCrypto::from_key_iv(key, iv));
        prefix_ready.recv().unwrap();

        let viewer = std::thread::spawn(move || {
            let mut service_ticks = 0u8;
            loop {
                service_ticks += 1;
                if service_ticks == 2 {
                    p5_service_progress.send(()).unwrap();
                }
                if let Some(plaintext) = read_viewer_app_frame_step(&mut connection).unwrap() {
                    plaintext_sent.send(plaintext).unwrap();
                    break;
                }
            }
        });

        let progress_before_completion =
            p5_service_observed.recv_timeout(Duration::from_millis(250));
        release_remainder.send(()).unwrap();
        let plaintext = plaintext_ready
            .recv_timeout(Duration::from_secs(2))
            .expect("released encrypted frame must complete");
        viewer.join().unwrap();
        server.join().unwrap();

        assert!(
            progress_before_completion.is_ok(),
            "viewer/P5 service must regain control before the encrypted frame completes"
        );
        assert_eq!(plaintext, b"viewer-progress");
    }

    #[test]
    fn p5_degrade_keeps_video_and_control_serviceable() {
        let started = Instant::now();
        let send_at = started + Duration::from_secs(5);
        let mut loopback =
            DrainUdpMediaLoopback::new_with_audio_flow(MediaRole::Audio, AudioMediaFlow::PcToMac);
        let mut state = ViewerMediaState::new(AudioMediaFlow::PcToMac).unwrap();
        state.start_audio_input_probe(&loopback.transport, 0, started);
        send_entire_probe(&mut state, &mut loopback.transport, send_at);

        state.observe_audio_input_transport(
            &loopback.transport,
            0,
            send_at + P5_CONFIRMATION_TIMEOUT,
        );

        assert!(matches!(
            state.audio_input_phase(),
            AudioInputPhase::Degraded { generation: 0, .. }
        ));
        assert!(state.audio_encoder.is_none());
        assert_video_and_control_remain_serviceable(&mut state, send_at + P5_CONFIRMATION_TIMEOUT);
    }

    #[test]
    fn generation_mismatch_degrades_only_audio_input_and_releases_encoder() {
        let now = Instant::now();
        let mut loopback =
            DrainUdpMediaLoopback::new_with_audio_flow(MediaRole::Audio, AudioMediaFlow::PcToMac);
        let mut state = ViewerMediaState::new(AudioMediaFlow::PcToMac).unwrap();
        state.start_audio_input_probe(&loopback.transport, 0, now);

        state.service_audio_input_probe(&mut loopback.transport, 1, now);

        assert!(matches!(
            state.audio_input_phase(),
            AudioInputPhase::Degraded { .. }
        ));
        assert!(state.audio_encoder.is_none());
        assert_video_and_control_remain_serviceable(&mut state, now);
    }

    #[test]
    fn transport_send_failure_degrades_only_audio_input() {
        let now = Instant::now();
        let mut loopback =
            DrainUdpMediaLoopback::new_with_audio_flow(MediaRole::Audio, AudioMediaFlow::PcToMac);
        let mut state = ViewerMediaState::new(AudioMediaFlow::PcToMac).unwrap();
        state.start_audio_input_probe(&loopback.transport, 0, now);
        loopback.transport.close(0).unwrap();

        state.service_audio_input_probe(&mut loopback.transport, 0, now);

        assert!(matches!(
            state.audio_input_phase(),
            AudioInputPhase::Degraded { generation: 0, .. }
        ));
        assert!(state.audio_encoder.is_none());
        assert_video_and_control_remain_serviceable(&mut state, now);
    }

    #[test]
    fn malformed_and_out_of_range_reports_never_confirm_the_probe() {
        for report_kind in ["malformed", "out-of-range"] {
            let started = Instant::now();
            let send_at = started + Duration::from_secs(5);
            let mut loopback = DrainUdpMediaLoopback::new_with_audio_flow(
                MediaRole::Audio,
                AudioMediaFlow::PcToMac,
            );
            let mut state = ViewerMediaState::new(AudioMediaFlow::PcToMac).unwrap();
            state.start_audio_input_probe(&loopback.transport, 0, started);
            send_entire_probe(&mut state, &mut loopback.transport, send_at);
            let sent = loopback.transport.outbound_audio_sent_range().unwrap();
            if report_kind == "malformed" {
                loopback.send_rtcp(&[0x81, 201, 0, 1, 0, 0, 0, 0]);
            } else {
                loopback.send_rtcp(&receiver_report(
                    sent.ssrc,
                    sent.last_extended_sequence.saturating_add(1),
                    0,
                ));
            }
            drain_udp_media(&mut loopback.transport, &mut state, 0).unwrap();
            state.observe_audio_input_transport(
                &loopback.transport,
                0,
                send_at + P5_CONFIRMATION_TIMEOUT,
            );

            assert!(matches!(
                state.audio_input_phase(),
                AudioInputPhase::Degraded { generation: 0, .. }
            ));
            assert!(state.audio_encoder.is_none());
            assert_video_and_control_remain_serviceable(
                &mut state,
                send_at + P5_CONFIRMATION_TIMEOUT,
            );
        }
    }

    fn media_stream_answer_fixture() -> Vec<u8> {
        const MESSAGE_TWO_VERSION: u16 = 2;
        const MESSAGE_TWO_KIND: u16 = 2;
        const STREAM_ONE_SUPPORTS_60_FPS: u32 = 1;
        fn captured_answer_container() -> Vec<u8> {
            let fixture = crate::vnc::read_private_fixture_text(
                "ard_re/fixtures/avc_mode_4_answer.bplist.hex",
            );
            let compact = fixture.trim();
            compact
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect()
        }

        let audio = captured_answer_container();
        let video = captured_answer_container();
        let mut body = Vec::new();
        body.extend_from_slice(&MESSAGE_TWO_VERSION.to_be_bytes());
        body.extend_from_slice(&MESSAGE_TWO_KIND.to_be_bytes());
        body.extend_from_slice(&STREAM_ONE_SUPPORTS_60_FPS.to_be_bytes());
        body.extend_from_slice(&(audio.len() as u16).to_be_bytes());
        body.extend_from_slice(&(video.len() as u16).to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&[0; 4]);
        body.extend_from_slice(&audio);
        body.extend_from_slice(&video);

        let mut frame = Vec::new();
        frame.extend_from_slice(&media_protocol::PRIMARY_MEDIA_STREAM_ID.to_be_bytes());
        frame.extend_from_slice(&[0; 8]);
        frame.extend_from_slice(&media_protocol::MEDIA_STREAM_CONTROL_ENCODING.to_be_bytes());
        frame.extend_from_slice(&(body.len() as u16).to_be_bytes());
        frame.extend_from_slice(&body);
        frame
    }

    fn type_two_tables_fixture() -> [u8; 129] {
        let mut payload = [0u8; 129];
        payload[0] = 2;
        payload
    }

    struct TestBitWriter {
        bytes: Vec<u8>,
        current: u8,
        used: u8,
    }

    impl TestBitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                used: 0,
            }
        }

        fn write_bits(&mut self, value: u32, count: u8) {
            for shift in (0..count).rev() {
                self.current = (self.current << 1) | (((value >> shift) & 1) as u8);
                self.used += 1;
                if self.used == 8 {
                    self.bytes.push(self.current);
                    self.current = 0;
                    self.used = 0;
                }
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.used != 0 {
                self.current <<= 8 - self.used;
                self.bytes.push(self.current);
            }
            self.bytes
        }
    }

    fn native_mode_zero_payload() -> Vec<u8> {
        let mut mode = TestBitWriter::new();
        mode.write_bits(1, 1);
        mode.write_bits(0, 3);
        mode.write_bits(0, 1);
        mode.write_bits(0x6d, 8);
        let mode = mode.finish();

        let mut data = TestBitWriter::new();
        data.write_bits(0x6d, 8);
        let data = data.finish();
        let data_offset = 6 + mode.len();
        let mut payload = vec![
            0,
            0,
            0,
            u8::try_from(data_offset >> 16).unwrap(),
            u8::try_from((data_offset >> 8) & 0xff).unwrap(),
            u8::try_from(data_offset & 0xff).unwrap(),
        ];
        payload.extend_from_slice(&mode);
        payload.extend_from_slice(&data);
        payload
    }

    fn native_opcode_zero_partial_payload() -> Vec<u8> {
        let mut bits = TestBitWriter::new();
        bits.write_bits(0, 2);
        bits.write_bits(0x6d, 8);
        bits.write_bits(0x76, 8);
        bits.write_bits(0x73, 8);
        let mut payload = vec![1, 0, 0];
        payload.extend(bits.finish());
        payload
    }

    fn native_mode_five_seed_payload() -> Vec<u8> {
        let mut mode = TestBitWriter::new();
        mode.write_bits(1, 1);
        mode.write_bits(5, 3);
        mode.write_bits(0, 1);
        mode.write_bits(0x6d, 8);
        let mode = mode.finish();

        let mut data = TestBitWriter::new();
        data.write_bits(0, 3);
        data.write_bits(0, 2);
        data.write_bits(0, 2);
        data.write_bits(0, 2);
        data.write_bits(0b0010, 4);
        data.write_bits(0x6d, 8);
        let data = data.finish();
        let data_offset = 6 + mode.len();
        let mut payload = vec![
            0,
            0,
            0,
            u8::try_from(data_offset >> 16).unwrap(),
            u8::try_from((data_offset >> 8) & 0xff).unwrap(),
            u8::try_from(data_offset & 0xff).unwrap(),
        ];
        payload.extend_from_slice(&mode);
        payload.extend_from_slice(&data);
        payload
    }

    fn native_opcode_one_partial_payload() -> Vec<u8> {
        let mut bits = TestBitWriter::new();
        bits.write_bits(1, 2);
        bits.write_bits(0, 6);
        bits.write_bits(0, 1);
        bits.write_bits(0b1010, 4);
        bits.write_bits(0, 1);
        bits.write_bits(0b1010, 4);
        bits.write_bits(0x6d, 8);
        bits.write_bits(0x76, 8);
        bits.write_bits(0x73, 8);
        let mut payload = vec![1, 1, 1];
        payload.extend(bits.finish());
        payload
    }

    fn native_record(rect: MvsRect) -> MvsRecord {
        MvsRecord {
            rect,
            payload: native_mode_zero_payload(),
        }
    }

    fn commit_native_mode_zero(receiver: &mut MvsReceiveState) {
        let decision = receiver.prepare(&native_mode_zero_payload(), 8, 8).unwrap();
        let mvs::MvsDecodeDecision::Prepared(prepared) = decision else {
            panic!("expected native preparation");
        };
        receiver.commit(prepared).unwrap();
    }

    fn native_surface(width: usize, height: usize) -> Arc<Mutex<DisplaySurface>> {
        Arc::new(Mutex::new(DisplaySurface::new(
            0,
            Framebuffer::new(width, height).unwrap(),
        )))
    }

    fn native_runtime(width: u16, height: u16) -> Arc<Mutex<DynamicResolutionRuntime>> {
        Arc::new(Mutex::new(DynamicResolutionRuntime::new(
            DisplaySize::new(width, height).unwrap(),
            true,
        )))
    }

    #[test]
    fn native_subrectangle_applies_without_complete_surface_evidence() {
        let surface = native_surface(16, 8);
        let dynamic = native_runtime(16, 8);
        let mut receiver = MvsReceiveState::new(0);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let record = native_record(MvsRect {
            x: 8,
            y: 0,
            width: 8,
            height: 8,
        });

        assert_eq!(
            apply_native_mvs_frame_with(
                &mut receiver,
                &record,
                &surface,
                &dynamic,
                apply_rgb_rect,
            )
            .unwrap(),
            MvsRecordOutcome::FullApplied
        );
        let surface = surface.lock().unwrap();
        assert!(surface.framebuffer.pixels()[..8]
            .iter()
            .all(|pixel| *pixel == 0));
        assert!(surface.framebuffer.pixels()[8..16]
            .iter()
            .all(|pixel| *pixel == 0x00ff_ffff));
        drop(surface);
        assert!(!dynamic.lock().unwrap().evidence.current_full_media_applied);
        assert!(!receiver.awaiting_full());
    }

    #[test]
    fn native_type_zero_reaches_render_and_malformed_type_one_preserves_visible_pixels() {
        let surface = native_surface(16, 8);
        let dynamic = native_runtime(16, 8);
        let mut receiver = MvsReceiveState::new(0);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let type_zero = native_record(MvsRect {
            x: 8,
            y: 0,
            width: 8,
            height: 8,
        });

        assert_eq!(
            apply_native_mvs_frame_with(
                &mut receiver,
                &type_zero,
                &surface,
                &dynamic,
                apply_rgb_rect,
            )
            .unwrap(),
            MvsRecordOutcome::FullApplied
        );

        let mut first_render = vec![0; 16 * 8];
        let first_snapshot =
            render_surface_frame_with(&surface, (16, 8), &mut first_render, |pixels, _, _| {
                assert!(pixels.iter().any(|pixel| *pixel != 0));
                Ok(())
            })
            .unwrap();
        assert_eq!(first_snapshot.type_zero_applied_count, 1);
        assert_eq!(first_snapshot.content_revision, 1);
        assert_eq!(first_snapshot.first_nonblack_render_revision, Some(1));

        let malformed_type_one = MvsRecord {
            rect: MvsRect {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            },
            payload: vec![1, 0x6d, 0x76, 0x73],
        };
        assert_eq!(
            apply_native_mvs_frame_with(
                &mut receiver,
                &malformed_type_one,
                &surface,
                &dynamic,
                apply_rgb_rect,
            )
            .unwrap(),
            MvsRecordOutcome::RecoveryRequested
        );

        let mut second_render = vec![0; 16 * 8];
        let second_snapshot =
            render_surface_frame_with(&surface, (16, 8), &mut second_render, |pixels, _, _| {
                assert!(pixels.iter().any(|pixel| *pixel != 0));
                Ok(())
            })
            .unwrap();
        assert_eq!(second_render, first_render);
        assert_eq!(second_snapshot.type_zero_applied_count, 1);
        assert_eq!(second_snapshot.content_revision, 1);
        assert_eq!(second_snapshot.first_nonblack_render_revision, Some(1));
    }

    #[test]
    fn slice_d_opaque_record_commits_through_orchestrator_without_surface_or_p1_publication() {
        let surface = native_surface(8, 8);
        surface
            .lock()
            .unwrap()
            .framebuffer
            .pixels_mut()
            .fill(0x0012_3456);
        let before = surface.lock().unwrap().framebuffer.pixels().to_vec();
        let dynamic = native_runtime(8, 8);
        let mut receiver = MvsReceiveState::new(0);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        commit_native_mode_zero(&mut receiver);
        assert!(!receiver.awaiting_full());
        let record = MvsRecord {
            rect: MvsRect {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            },
            payload: native_opcode_zero_partial_payload(),
        };

        assert_eq!(
            apply_native_mvs_frame_with(
                &mut receiver,
                &record,
                &surface,
                &dynamic,
                |_, _, _, _, _, _| panic!("opaque type-1 reached framebuffer apply"),
            )
            .unwrap(),
            MvsRecordOutcome::PartialApplied
        );
        assert_eq!(surface.lock().unwrap().framebuffer.pixels(), before);
        assert!(!receiver.awaiting_full());
        let runtime = dynamic.lock().unwrap();
        assert!(!runtime.evidence.current_full_media_applied);
        assert!(!runtime.evidence.non_paused_media_activity);
        assert!(!runtime.armed);
    }

    #[test]
    fn type_one_opcode_one_replaces_visible_pixels_transactionally() {
        let surface = native_surface(8, 8);
        let dynamic = native_runtime(8, 8);
        let mut receiver = MvsReceiveState::new(0);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let seed = receiver
            .prepare(&native_mode_five_seed_payload(), 8, 8)
            .unwrap();
        let mvs::MvsDecodeDecision::Prepared(seed) = seed else {
            panic!("expected mode-five seed preparation");
        };
        receiver.commit(seed).unwrap();
        surface
            .lock()
            .unwrap()
            .framebuffer
            .pixels_mut()
            .fill(0x0012_3456);
        let before = surface.lock().unwrap().framebuffer.pixels().to_vec();
        let record = MvsRecord {
            rect: MvsRect {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            },
            payload: native_opcode_one_partial_payload(),
        };

        assert_eq!(
            apply_native_mvs_frame_with(
                &mut receiver,
                &record,
                &surface,
                &dynamic,
                apply_rgb_rect,
            )
            .unwrap(),
            MvsRecordOutcome::PartialApplied
        );
        let surface = surface.lock().unwrap();
        assert_ne!(surface.framebuffer.pixels(), before);
        assert_eq!(surface.native_mvs_observability.type_zero_applied_count, 0);
        assert_eq!(surface.native_mvs_observability.content_revision, 1);
        drop(surface);
        assert!(!dynamic.lock().unwrap().evidence.current_full_media_applied);
    }

    #[test]
    fn slice_d_opaque_response_boundary_sends_one_normal_incremental_only() {
        let mut requests = ReaderRequestState::after_startup(Instant::now());
        requests.consume_mvs_response();
        let mut writes = 0usize;

        assert!(finish_partial_boundary_at(&mut requests, || {
            writes += 1;
            Ok(())
        })
        .unwrap());
        assert_eq!(writes, 1);
        assert!(requests.framebuffer_request_in_flight());
        assert!(!finish_partial_boundary_at(&mut requests, || {
            writes += 1;
            Ok(())
        })
        .unwrap());
        assert_eq!(writes, 1);
    }

    #[test]
    fn invalid_geometry_is_rejected_before_native_prepare_or_surface_mutation() {
        let surface = native_surface(8, 8);
        let dynamic = native_runtime(8, 8);
        let mut receiver = MvsReceiveState::new(0);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let initial = native_record(MvsRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        });
        apply_native_mvs_frame_with(&mut receiver, &initial, &surface, &dynamic, apply_rgb_rect)
            .unwrap();
        assert!(!receiver.awaiting_full());
        let before = surface.lock().unwrap().framebuffer.pixels().to_vec();
        let invalid = native_record(MvsRect {
            x: 1,
            y: 0,
            width: 8,
            height: 8,
        });

        assert_eq!(
            apply_native_mvs_frame_with(
                &mut receiver,
                &invalid,
                &surface,
                &dynamic,
                |_, _, _, _, _, _| panic!("invalid geometry reached framebuffer apply"),
            )
            .unwrap(),
            MvsRecordOutcome::RecoveryRequested
        );
        assert_eq!(surface.lock().unwrap().framebuffer.pixels(), before);
        assert!(receiver.awaiting_full());
    }

    #[test]
    fn failed_surface_apply_drops_preparation_and_retry_decodes_identically() {
        let surface = native_surface(8, 8);
        let dynamic = native_runtime(8, 8);
        let mut receiver = MvsReceiveState::new(0);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let record = native_record(MvsRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        });
        let before = surface.lock().unwrap().framebuffer.pixels().to_vec();

        assert_eq!(
            apply_native_mvs_frame_with(
                &mut receiver,
                &record,
                &surface,
                &dynamic,
                |_, _, _, _, _, _| anyhow::bail!("injected framebuffer failure"),
            )
            .unwrap(),
            MvsRecordOutcome::RecoveryRequested
        );
        assert_eq!(surface.lock().unwrap().framebuffer.pixels(), before);
        assert!(receiver.awaiting_full());
        {
            let runtime = dynamic.lock().unwrap();
            assert!(!runtime.evidence.current_full_media_applied);
            assert!(!runtime.armed);
        }

        assert_eq!(
            apply_native_mvs_frame_with(
                &mut receiver,
                &record,
                &surface,
                &dynamic,
                apply_rgb_rect,
            )
            .unwrap(),
            MvsRecordOutcome::FullApplied
        );
        assert!(surface
            .lock()
            .unwrap()
            .framebuffer
            .pixels()
            .iter()
            .all(|pixel| *pixel == 0x00ff_ffff));
    }

    #[test]
    fn type_one_and_decode_failure_preserve_visible_surface_and_request_resync() {
        let surface = native_surface(8, 8);
        surface
            .lock()
            .unwrap()
            .framebuffer
            .pixels_mut()
            .fill(0x0012_3456);
        let dynamic = native_runtime(8, 8);
        let mut receiver = MvsReceiveState::new(0);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let before = surface.lock().unwrap().framebuffer.pixels().to_vec();

        for payload in [vec![1, 0x6d, 0x76, 0x73], vec![0]] {
            let record = MvsRecord {
                rect: MvsRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                },
                payload,
            };
            assert_eq!(
                apply_native_mvs_frame_with(
                    &mut receiver,
                    &record,
                    &surface,
                    &dynamic,
                    apply_rgb_rect,
                )
                .unwrap(),
                MvsRecordOutcome::RecoveryRequested
            );
            assert_eq!(surface.lock().unwrap().framebuffer.pixels(), before);
            assert!(receiver.awaiting_full());
        }
    }

    #[test]
    fn exact_native_surface_commits_before_arming_p1_evidence() {
        let surface = native_surface(8, 8);
        let dynamic = native_runtime(8, 8);
        let initial = DisplaySize::new(8, 8).unwrap();
        assert!(!dynamic
            .lock()
            .unwrap()
            .observe_initial_server_state(initial, initial));
        let mut receiver = MvsReceiveState::new(0);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let record = native_record(MvsRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        });

        assert_eq!(
            apply_native_mvs_frame_with(
                &mut receiver,
                &record,
                &surface,
                &dynamic,
                apply_rgb_rect,
            )
            .unwrap(),
            MvsRecordOutcome::FullApplied
        );
        assert!(!receiver.awaiting_full());
        let runtime = dynamic.lock().unwrap();
        assert!(runtime.evidence.current_full_media_applied);
        assert!(runtime.armed);
    }

    #[test]
    fn exact_full_commit_failure_through_orchestrator_never_publishes_surface_or_p1_evidence() {
        let dynamic = native_runtime(8, 8);
        let initial = DisplaySize::new(8, 8).unwrap();
        assert!(!dynamic
            .lock()
            .unwrap()
            .observe_initial_server_state(initial, initial));
        let mut receiver = MvsReceiveState::new(0);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let surface = native_surface(8, 8);
        surface
            .lock()
            .unwrap()
            .framebuffer
            .pixels_mut()
            .fill(0x0065_4321);
        let before = surface.lock().unwrap().framebuffer.pixels().to_vec();
        let record = native_record(MvsRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        });

        assert_eq!(
            with_native_mvs_commit_failure(|| {
                apply_native_mvs_frame_with(
                    &mut receiver,
                    &record,
                    &surface,
                    &dynamic,
                    apply_rgb_rect,
                )
            })
            .unwrap(),
            MvsRecordOutcome::RecoveryRequested
        );
        assert_eq!(surface.lock().unwrap().framebuffer.pixels(), before);
        assert!(receiver.awaiting_full());
        let runtime = dynamic.lock().unwrap();
        assert!(!runtime.evidence.current_full_media_applied);
        assert!(!runtime.evidence.non_paused_media_activity);
        assert!(!runtime.armed);
        assert_eq!(
            runtime.controller.state(),
            &crate::vnc::dynamic_resolution::DynamicResolutionState::Unavailable
        );
    }

    fn arm_runtime(runtime: &mut DynamicResolutionRuntime, initial: DisplaySize, full_first: bool) {
        if full_first {
            assert!(!runtime.observe_full_applied(0, initial));
            assert!(runtime.observe_initial_server_state(initial, initial));
        } else {
            assert!(!runtime.observe_initial_server_state(initial, initial));
            assert!(runtime.observe_full_applied(0, initial));
        }
    }

    #[test]
    fn capability_arms_only_after_matching_state_and_current_full_in_either_order() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();

        for full_first in [false, true] {
            let mut runtime = DynamicResolutionRuntime::new(initial, true);
            assert!(runtime
                .send_target_with(target, |_| Ok(Instant::now()))
                .unwrap()
                .is_none());
            arm_runtime(&mut runtime, initial, full_first);
            assert!(runtime
                .send_target_with(target, |_| Ok(Instant::now()))
                .unwrap()
                .is_some());
        }
    }

    #[test]
    fn capability_mismatch_and_default_off_never_arm_dynamic_requests() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let mismatch = DisplaySize::new(1024, 768).unwrap();

        let mut mismatch_runtime = DynamicResolutionRuntime::new(initial, true);
        assert!(!mismatch_runtime.observe_initial_server_state(mismatch, initial));
        assert!(!mismatch_runtime.observe_full_applied(0, initial));
        assert!(mismatch_runtime
            .send_target_with(target, |_| Ok(Instant::now()))
            .unwrap()
            .is_none());

        let mut disabled = DynamicResolutionRuntime::new(initial, false);
        assert!(!disabled.observe_initial_server_state(initial, initial));
        assert!(!disabled.observe_full_applied(0, initial));
        assert!(disabled
            .send_target_with(target, |_| Ok(Instant::now()))
            .unwrap()
            .is_none());
    }

    #[test]
    fn resize_pending_becomes_visible_only_after_successful_send() {
        use std::sync::mpsc;
        use std::sync::{Arc, Mutex};
        use std::thread;

        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let sent_at = Instant::now();
        let mut initial_runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut initial_runtime, initial, false);
        let runtime = Arc::new(Mutex::new(initial_runtime));
        let reader_runtime = runtime.clone();
        let (start_tx, start_rx) = mpsc::channel();
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let (commit_tx, commit_rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            start_rx.recv().unwrap();
            attempt_tx.send(()).unwrap();
            let committed = reader_runtime
                .lock()
                .unwrap()
                .observe_server_state(target)
                .is_some();
            commit_tx.send(committed).unwrap();
        });

        let request = {
            let mut runtime = runtime.lock().unwrap();
            runtime
                .send_target_with(target, |_| {
                    start_tx.send(()).unwrap();
                    attempt_rx.recv().unwrap();
                    assert!(commit_rx.try_recv().is_err());
                    Ok(sent_at)
                })
                .unwrap()
                .unwrap()
        };

        assert!(commit_rx.recv().unwrap());
        reader.join().unwrap();
        assert_eq!(request.target, target);
        assert_eq!(runtime.lock().unwrap().pending_since, None);
    }

    #[test]
    fn failed_resize_send_leaves_controller_stable_without_timestamp() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);

        assert!(runtime
            .send_target_with(target, |_| anyhow::bail!("injected send failure"))
            .is_err());
        assert!(runtime.pending_since.is_none());
        assert!(matches!(
            runtime.controller.state(),
            crate::vnc::dynamic_resolution::DynamicResolutionState::Stable { size, .. }
                if *size == initial
        ));
    }

    #[test]
    fn latest_debounced_viewport_waits_for_inflight_then_sends_once() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let first = DisplaySize::new(1280, 720).unwrap();
        let latest = DisplaySize::new(1024, 768).unwrap();
        let started = Instant::now();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        let mut queue = ViewportRequestQueue::default();
        let mut sent = Vec::new();

        queue.observe(first, started);
        queue
            .service(&mut runtime, started + Duration::from_millis(250), |size| {
                sent.push(size);
                Ok(started + Duration::from_millis(250))
            })
            .unwrap();
        queue.observe(latest, started + Duration::from_millis(300));
        queue
            .service(&mut runtime, started + Duration::from_millis(550), |size| {
                sent.push(size);
                Ok(started + Duration::from_millis(550))
            })
            .unwrap();
        assert_eq!(sent, vec![first]);
        assert!(queue.latest.is_some());

        assert!(runtime.observe_server_state(first).is_some());
        assert!(runtime.observe_full_applied(1, first));
        queue
            .service(&mut runtime, started + Duration::from_millis(551), |size| {
                sent.push(size);
                Ok(started + Duration::from_millis(551))
            })
            .unwrap();
        queue
            .service(&mut runtime, started + Duration::from_millis(800), |size| {
                sent.push(size);
                Ok(started + Duration::from_millis(800))
            })
            .unwrap();

        assert_eq!(sent, vec![first, latest]);
        assert!(queue.latest.is_none());
    }

    #[test]
    fn old_incremental_inflight_defers_dynamic_until_fragmented_response_boundary() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let started = Instant::now();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        let mut receiver = MvsReceiveState::new(0);
        let mut requests = ReaderRequestState::after_startup(started);
        let mut queue = ViewportRequestQueue::default();
        queue.observe(target, started);
        let mut dynamic_sends = Vec::new();
        let mut full_sends = 0;

        let before_response = service_reader_tick_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            started + Duration::from_millis(250),
            |_| {},
            || {
                full_sends += 1;
                Ok(())
            },
            |size| {
                dynamic_sends.push(size);
                Ok(started + Duration::from_millis(250))
            },
        )
        .unwrap();
        assert!(before_response.dynamic_request.is_none());
        assert!(requests.framebuffer_request_in_flight());

        let rect = MvsRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        assert!(receiver
            .begin_at(rect, 2, &[0], started + Duration::from_millis(300))
            .unwrap()
            .is_none());
        let while_fragmented = service_reader_tick_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            started + Duration::from_millis(350),
            |_| {},
            || {
                full_sends += 1;
                Ok(())
            },
            |size| {
                dynamic_sends.push(size);
                Ok(started + Duration::from_millis(350))
            },
        )
        .unwrap();
        assert!(while_fragmented.dynamic_request.is_none());
        assert!(receiver.push_continuation(&[1]).unwrap().is_some());
        requests.consume_mvs_response();

        let mut incrementals = 0;
        let boundary = finish_full_boundary_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            started + Duration::from_millis(351),
            |_| {},
            || {
                full_sends += 1;
                Ok(())
            },
            |size| {
                dynamic_sends.push(size);
                Ok(started + Duration::from_millis(351))
            },
            || {
                incrementals += 1;
                Ok(())
            },
        )
        .unwrap();

        assert!(boundary.dynamic_request.is_some());
        assert!(!boundary.incremental_sent);
        assert_eq!(dynamic_sends, vec![target]);
        assert_eq!(incrementals, 0);
        assert_eq!(full_sends, 0);
        let server_state = [
            0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0x04, 0x51, 0, 0x4c, 0, 5, 0x05, 0xa0, 0x0a, 0,
        ];
        assert_eq!(
            reader_frame_class(&receiver, &server_state),
            ReaderFrameClass::ControlOrMedia
        );
        assert!(runtime.observe_server_state(target).is_some());
    }

    #[test]
    fn full_boundary_without_mature_candidate_sends_one_incremental() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let started = Instant::now();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        let mut receiver = MvsReceiveState::new(0);
        let mut requests = ReaderRequestState::after_startup(started);
        let mut queue = ViewportRequestQueue::default();
        requests.consume_mvs_response();
        let mut incrementals = 0;

        let boundary = finish_full_boundary_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            started + Duration::from_millis(10),
            |_| {},
            || Ok(()),
            |_| anyhow::bail!("dynamic send should not run"),
            || {
                incrementals += 1;
                Ok(())
            },
        )
        .unwrap();

        assert!(boundary.dynamic_request.is_none());
        assert!(boundary.incremental_sent);
        assert_eq!(incrementals, 1);
        assert!(requests.framebuffer_request_in_flight());
    }

    #[test]
    fn table_followup_is_one_shot_and_full_before_due_cancels_it() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let started = Instant::now();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        let mut receiver = MvsReceiveState::new(0);
        let mut requests = ReaderRequestState::after_startup(started);
        let mut queue = ViewportRequestQueue::default();
        requests.consume_mvs_response();
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        assert_eq!(
            requests.on_valid_table_record(0, started).unwrap(),
            TableScheduleStatus::Scheduled
        );
        let due = requests.table_followup_due().unwrap();
        assert_eq!(
            requests
                .on_valid_table_record(0, started + Duration::from_millis(50))
                .unwrap(),
            TableScheduleStatus::AlreadyScheduled
        );
        assert_eq!(requests.table_followup_due(), Some(due));

        let mut table_full_writes = 0;
        service_reader_tick_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            due - Duration::from_millis(1),
            |_| {},
            || {
                table_full_writes += 1;
                Ok(())
            },
            |_| anyhow::bail!("dynamic send should not run"),
        )
        .unwrap();
        assert_eq!(table_full_writes, 0);

        requests.consume_mvs_response();
        commit_native_mode_zero(&mut receiver);
        let mut incrementals = 0;
        finish_full_boundary_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            due - Duration::from_millis(1),
            |_| {},
            || {
                table_full_writes += 1;
                Ok(())
            },
            |_| anyhow::bail!("dynamic send should not run"),
            || {
                incrementals += 1;
                Ok(())
            },
        )
        .unwrap();
        service_reader_tick_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            due + Duration::from_secs(1),
            |_| {},
            || {
                table_full_writes += 1;
                Ok(())
            },
            |_| anyhow::bail!("dynamic send should not run"),
        )
        .unwrap();

        assert_eq!(table_full_writes, 0);
        assert_eq!(incrementals, 1);
        assert!(requests.table_followup_due().is_none());
        assert!(!receiver.awaiting_full());
    }

    #[test]
    fn table_only_followup_sends_exactly_once_and_duplicate_after_sent_is_error() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let started = Instant::now();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        let mut receiver = MvsReceiveState::new(0);
        let mut requests = ReaderRequestState::after_startup(started);
        let mut queue = ViewportRequestQueue::default();
        requests.consume_mvs_response();
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        assert_eq!(
            requests.on_valid_table_record(0, started).unwrap(),
            TableScheduleStatus::Scheduled
        );
        let due = requests.table_followup_due().unwrap();
        let mut writes = 0;

        let tick = service_reader_tick_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            due,
            |_| {},
            || {
                writes += 1;
                Ok(())
            },
            |_| anyhow::bail!("dynamic send should not run"),
        )
        .unwrap();
        assert!(tick.table_followup_sent);
        assert_eq!(writes, 1);
        requests.consume_mvs_response();
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        assert!(requests
            .on_valid_table_record(0, due + Duration::from_millis(1))
            .is_err());
        service_reader_tick_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            due + Duration::from_secs(1),
            |_| {},
            || {
                writes += 1;
                Ok(())
            },
            |_| anyhow::bail!("dynamic send should not run"),
        )
        .unwrap();
        assert_eq!(writes, 1);
    }

    #[test]
    fn loop_tick_expires_continuous_continuations_and_independently_times_out_dynamic() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let latest = DisplaySize::new(1024, 768).unwrap();
        let started = Instant::now();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        runtime.send_target_with(target, |_| Ok(started)).unwrap();
        let mut receiver = MvsReceiveState::new(0);
        let rect = MvsRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        assert!(receiver
            .begin_at(rect, 100, &[0], started)
            .unwrap()
            .is_none());
        let mut requests = ReaderRequestState::after_startup(started);
        let mut queue = ViewportRequestQueue::default();
        queue.observe(latest, started + Duration::from_millis(10));
        let mut full_writes = 0;
        let mut final_tick = None;

        for step in 1..=40 {
            let now = started + Duration::from_millis(step * 50);
            let tick = service_reader_tick_at(
                &mut receiver,
                &mut requests,
                &mut queue,
                &mut runtime,
                now,
                |_| {},
                || {
                    full_writes += 1;
                    Ok(())
                },
                |_| anyhow::bail!("dynamic send should not run"),
            )
            .unwrap();
            if step < 40 {
                assert!(!tick.incomplete_recovered);
                assert!(receiver.push_continuation(&[]).unwrap().is_none());
            } else {
                final_tick = Some(tick);
            }
        }

        let final_tick = final_tick.unwrap();
        assert!(final_tick.incomplete_recovered);
        assert!(final_tick.dynamic_timed_out);
        assert_eq!(full_writes, 1);
        assert!(!receiver.is_pending());
        assert!(queue.latest.is_none());
        assert_eq!(
            reader_frame_class(&receiver, &[0x14]),
            ReaderFrameClass::ServerKeepalive
        );
        let server_state = [
            0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0x04, 0x51, 0, 0x4c, 0, 5, 0x05, 0xa0, 0x0a, 0,
        ];
        assert_eq!(
            reader_frame_class(&receiver, &server_state),
            ReaderFrameClass::ControlOrMedia
        );
    }

    #[test]
    fn full_resync_rate_limit_waits_then_writes_exactly_once() {
        let now = Instant::now();
        let mut last_request = Some(now);
        let mut delays = Vec::new();
        let mut writes = 0;

        request_full_update_at(
            &mut last_request,
            now,
            |delay| delays.push(delay),
            || {
                writes += 1;
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(delays, vec![Duration::from_millis(200)]);
        assert_eq!(writes, 1);
        assert_eq!(last_request, Some(now + Duration::from_millis(200)));
    }

    #[test]
    fn shutdown_reader_sets_closing_wakes_socket_and_joins() {
        use std::io::Read as _;
        use std::net::{TcpListener, TcpStream};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let closing = Arc::new(AtomicBool::new(false));
        let reader_done = Arc::new(AtomicBool::new(false));
        let done = reader_done.clone();
        let handle = thread::spawn(move || {
            let mut byte = [0u8; 1];
            let _ = server.read(&mut byte);
            done.store(true, Ordering::Relaxed);
        });
        let writer = Arc::new(Mutex::new(client));

        shutdown_reader(&closing, &writer, handle).unwrap();

        assert!(closing.load(Ordering::Relaxed));
        assert!(reader_done.load(Ordering::Relaxed));
    }

    #[test]
    fn pending_record_treats_heartbeat_shaped_frame_as_opaque_continuation() {
        let mut receiver = MvsReceiveState::new(0);
        let rect = MvsRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };

        assert!(receiver.begin(rect, 2, &[0x00]).unwrap().is_none());
        let record = receiver.push_continuation(&[0x14]).unwrap().unwrap();

        assert_eq!(record.rect, rect);
        assert_eq!(record.payload, vec![0x00, 0x14]);
    }

    #[test]
    fn pending_record_treats_query_shaped_frame_as_opaque_continuation() {
        let mut receiver = MvsReceiveState::new(0);
        let rect = MvsRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };

        assert!(receiver.begin(rect, 2, &[0x00]).unwrap().is_none());
        let record = receiver.push_continuation(&[0x08]).unwrap().unwrap();

        assert_eq!(record.payload, vec![0x00, 0x08]);
    }

    #[test]
    #[ignore = "需要未纳入公开仓库的本地授权 AVConference fixture"]
    fn pending_record_does_not_swallow_complete_media_stream_answer() {
        let mut receiver = MvsReceiveState::new(0);
        let rect = MvsRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        assert!(receiver.begin(rect, 1024, &[0]).unwrap().is_none());

        assert_eq!(
            reader_frame_class(&receiver, &media_stream_answer_fixture()),
            ReaderFrameClass::ControlOrMedia
        );
        assert!(receiver.is_pending());
    }

    #[test]
    #[ignore = "需要未纳入公开仓库的本地授权 AVConference fixture"]
    fn pending_record_keeps_malformed_media_control_lookalike_as_continuation() {
        let mut receiver = MvsReceiveState::new(0);
        let rect = MvsRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        assert!(receiver.begin(rect, 1024, &[0]).unwrap().is_none());
        let mut truncated = media_stream_answer_fixture();
        truncated.pop();

        assert_eq!(
            reader_frame_class(&receiver, &truncated),
            ReaderFrameClass::Continuation
        );
    }

    #[test]
    fn truncated_mvs_envelope_requests_resynchronization() {
        let mut receiver = MvsReceiveState::new(0);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        commit_native_mode_zero(&mut receiver);
        let mut truncated = vec![0; 16];
        truncated[12..16].copy_from_slice(&encoding::MVS.to_be_bytes());

        assert!(receiver.reject_truncated_mvs_envelope(&truncated).unwrap());
        assert!(receiver.awaiting_full());
        assert!(hpss::is_truncated_mvs_envelope(&truncated));
    }

    #[test]
    fn full_apply_always_queues_next_incremental_request() {
        assert_eq!(
            incremental_request_after_full_apply(1440, 2560),
            protocol::msg_fb_update_request(true, 0, 0, 1440, 2560)
        );
    }

    #[test]
    fn map_pointer_bottom_right_uses_current_display_bottom_right() {
        assert_eq!(
            map_pointer(
                639.0,
                359.0,
                (640, 360),
                DisplaySize::new(1280, 720).unwrap()
            ),
            (1279, 719)
        );
    }

    #[test]
    fn map_pointer_uses_fitted_content_and_clamps_landscape_bars() {
        let display = DisplaySize::new(1440, 2560).unwrap();

        assert_eq!(map_pointer(437.0, 0.0, (1280, 720), display), (0, 0));
        assert_eq!(
            map_pointer(841.0, 719.0, (1280, 720), display),
            (1439, 2559)
        );
        assert_eq!(map_pointer(0.0, 0.0, (1280, 720), display), (0, 0));
        assert_eq!(
            map_pointer(1279.0, 719.0, (1280, 720), display),
            (1439, 2559)
        );
    }

    #[test]
    fn map_pointer_uses_fitted_content_and_clamps_portrait_bars() {
        let display = DisplaySize::new(2560, 1440).unwrap();

        assert_eq!(map_pointer(0.0, 437.0, (720, 1280), display), (0, 0));
        assert_eq!(
            map_pointer(719.0, 841.0, (720, 1280), display),
            (2559, 1439)
        );
        assert_eq!(map_pointer(0.0, 0.0, (720, 1280), display), (0, 0));
        assert_eq!(
            map_pointer(719.0, 1279.0, (720, 1280), display),
            (2559, 1439)
        );
    }

    #[test]
    fn map_pointer_clamps_and_handles_zero_window_axes() {
        let display = DisplaySize::new(1280, 720).unwrap();
        assert_eq!(map_pointer(-5.0, 900.0, (640, 360), display), (0, 719));
        assert_eq!(map_pointer(100.0, 100.0, (0, 0), display), (0, 0));
    }

    #[test]
    fn exact_server_ack_replaces_surface_once_but_mismatch_does_not() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        let mut surface = DisplaySurface::new(0, Framebuffer::new(1440, 900).unwrap());
        surface.native_mvs_observability = NativeMvsRenderObservability {
            type_zero_applied_count: 4,
            content_revision: 4,
            first_nonblack_render_revision: Some(3),
        };
        let mut receiver = MvsReceiveState::new(0);
        let mut media_state = ViewerMediaState::new(AudioMediaFlow::MacToPc).unwrap();
        assert_eq!(
            media_state
                .accept(MediaRole::Audio, MediaDatagram::Rtp(vec![0u8; 3]))
                .unwrap(),
            MediaAcceptOutcome::AudioDegraded
        );
        media_state.late_audio_packets = 7;
        media_state.audio_resynchronizations = 11;
        arm_runtime(&mut runtime, initial, false);
        runtime
            .send_target_with(target, |_| Ok(Instant::now()))
            .unwrap()
            .unwrap();

        assert!(commit_server_geometry(
            &mut runtime,
            &mut receiver,
            &mut surface,
            &mut media_state,
            DisplaySize::new(1024, 768).unwrap()
        )
        .is_none());
        assert_eq!(
            (surface.framebuffer.width, surface.framebuffer.height),
            (1440, 900)
        );
        assert_eq!(surface.native_mvs_observability.type_zero_applied_count, 4);
        assert!(matches!(
            media_state.audio_output_phase(),
            AudioOutputPhase::Degraded { .. }
        ));

        let commit = commit_server_geometry(
            &mut runtime,
            &mut receiver,
            &mut surface,
            &mut media_state,
            target,
        )
        .unwrap();
        assert_eq!(commit.generation, 1);
        assert_eq!(surface.generation, 1);
        assert_eq!(
            (surface.framebuffer.width, surface.framebuffer.height),
            (1280, 720)
        );
        assert_eq!(
            surface.native_mvs_observability,
            NativeMvsRenderObservability::default()
        );
        assert_eq!(
            media_state.audio_output_phase(),
            &AudioOutputPhase::ReadyToStart
        );
        assert_eq!(media_state.late_audio_packets, 7);
        assert_eq!(media_state.audio_resynchronizations, 11);
        assert!(commit_server_geometry(
            &mut runtime,
            &mut receiver,
            &mut surface,
            &mut media_state,
            target
        )
        .is_none());
        assert_eq!(surface.generation, 1);
    }

    #[test]
    fn pending_timeout_preserves_surface_geometry_and_generation() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let started = Instant::now();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        let surface = DisplaySurface::new(0, Framebuffer::new(1440, 900).unwrap());
        arm_runtime(&mut runtime, initial, false);
        runtime
            .send_target_with(target, |_| Ok(started))
            .unwrap()
            .unwrap();

        assert!(!runtime.timeout_pending(started + Duration::from_millis(1999)));
        assert!(runtime.timeout_pending(started + Duration::from_secs(2)));
        assert_eq!(surface.generation, 0);
        assert_eq!(
            (surface.framebuffer.width, surface.framebuffer.height),
            (1440, 900)
        );
    }

    #[test]
    fn dynamic_resolution_full_evidence_requires_the_complete_surface() {
        let surface = DisplaySize::new(1440, 900).unwrap();
        assert!(is_complete_surface_frame(
            MvsRect {
                x: 0,
                y: 0,
                width: 1440,
                height: 900,
            },
            surface,
        ));
        assert!(!is_complete_surface_frame(
            MvsRect {
                x: 0,
                y: 0,
                width: 720,
                height: 900,
            },
            surface,
        ));
    }

    #[test]
    fn stale_generation_pixels_cannot_mutate_current_surface() {
        let mut surface = DisplaySurface::new(2, Framebuffer::new(2, 1).unwrap());
        let rgb = [0xff, 0, 0];

        assert!(!apply_rgb_rect_for_generation(&mut surface, 1, &rgb, 0, 0, 1, 1).unwrap());
        assert_eq!(surface.framebuffer.pixels(), &[0, 0]);
        assert!(apply_rgb_rect_for_generation(&mut surface, 2, &rgb, 0, 0, 1, 1).unwrap());
        assert_eq!(surface.framebuffer.pixels(), &[0x00ff0000, 0]);
    }

    #[test]
    fn out_of_bounds_mvs_geometry_marks_recovery_instead_of_failing_the_reader() {
        let mut receiver = MvsReceiveState::new(0);
        let display = DisplaySize::new(640, 480).unwrap();
        let invalid = MvsRect {
            x: 630,
            y: 470,
            width: 20,
            height: 20,
        };

        assert!(!mark_recovery_for_invalid_mvs_geometry(&mut receiver, invalid, display).unwrap());
        assert!(receiver.awaiting_full());
    }

    #[test]
    fn mvs_table_record_requires_a_completely_zero_rectangle() {
        use std::net::{TcpListener, TcpStream};
        use std::sync::{Arc, Mutex};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (_server, _) = listener.accept().unwrap();
        let write_stream = Arc::new(Mutex::new(client));
        let crypto = Arc::new(Mutex::new(crate::vnc::session::SessionCrypto::from_key_iv(
            [1; 16], [2; 16],
        )));
        let display_size = DisplaySize::new(2, 2).unwrap();
        let surface = Arc::new(Mutex::new(DisplaySurface::new(
            0,
            Framebuffer::new(2, 2).unwrap(),
        )));
        let dynamic_resolution = Arc::new(Mutex::new(DynamicResolutionRuntime::new(
            display_size,
            false,
        )));

        for rect in [
            MvsRect {
                x: 1,
                y: 0,
                width: 0,
                height: 0,
            },
            MvsRect {
                x: 0,
                y: 1,
                width: 0,
                height: 0,
            },
            MvsRect {
                x: 0,
                y: 0,
                width: 1,
                height: 0,
            },
            MvsRect {
                x: 0,
                y: 0,
                width: 0,
                height: 1,
            },
        ] {
            let mut receiver = MvsReceiveState::new(0);
            let mut requests = ReaderRequestState {
                generation: 0,
                framebuffer_request_in_flight: false,
                last_full_request: None,
                table_followup: TableFollowupState::None,
            };
            let record = MvsRecord {
                rect,
                payload: vec![0; 129],
            };

            assert_eq!(
                process_complete_mvs_record(
                    &mut receiver,
                    record,
                    &surface,
                    &dynamic_resolution,
                    &write_stream,
                    &crypto,
                    &mut requests,
                )
                .unwrap(),
                MvsRecordOutcome::RecoveryRequested,
            );
            assert!(receiver.awaiting_full());
        }
    }
}
