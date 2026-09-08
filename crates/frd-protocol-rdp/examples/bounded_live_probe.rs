//! 有界、无 GUI 的 RDP 互操作验证；仅从管道 stdin 接受凭据。
//! stdin 三行依次是主机、用户名、密码。首次自动保存指纹，变化时拒绝。
//! FRD_RDP_PROBE_PIN_DIR 指定探针专用指纹目录；不使用或替代产品平台存储。
//! 仅统计解码发布，不证明窗口呈现、输入、剪贴板或音频播放。

use sha2::{Digest, Sha256};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use frd_core::{PixelSize, SecretBuffer, SessionId};
use frd_frame::{FrameCompleteness, SurfaceUpdate};
use frd_media_api::{
    ChromaFormat, VideoCapabilityProvider, VideoCodec, VideoDecodeQuery, VideoDecoderFactory,
    VideoPixelFormat, VideoProfile,
};
use frd_protocol_api::{
    ConnectRequest, Credentials, Endpoint, ProtocolError, ProtocolExit, ProtocolFactory,
    ProtocolId, ProtocolRuntime, RuntimeEventSink, RuntimeWake, ServerIdentityDecision,
    SessionCommand, SessionEvent, SurfacePublisher,
};
use frd_protocol_rdp::{
    Avc420DecoderProvider, Avc444DecoderProvider, RdpClientPlatformIdentity,
    RdpGraphicsCapabilities, RdpGraphicsObserver, RdpProtocolFactory,
};
use frd_video_ffmpeg::FfmpegBackend;

const EGFX_AVC420_OPT_IN_ENV: &str = "FRD_RDP_PROBE_EGFX_AVC420";
const EGFX_AVC444_OPT_IN_ENV: &str = "FRD_RDP_PROBE_EGFX_AVC444";
const EGFX_BUNDLE_ENV: &str = "FRD_RDP_PROBE_MACOS_APP";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProbeEgfxMode {
    Avc420,
    Avc444,
}

#[derive(Default)]
struct Counts {
    resets: u64,
    frames: u64,
    baselines: u64,
    patches: u64,
    pixel_bytes: u64,
    first_frame: Option<Instant>,
    last_frame: Option<Instant>,
}

struct Frames(Arc<Mutex<Counts>>);

impl SurfacePublisher for Frames {
    fn publish(&self, update: SurfaceUpdate) -> Result<(), ProtocolError> {
        let mut counts = self
            .0
            .lock()
            .map_err(|_| ProtocolError::FramePortRejected)?;
        match update {
            SurfaceUpdate::Reset {
                generation, size, ..
            } => {
                counts.resets += 1;
                println!(
                    "画面重置 generation={generation} width={} height={}",
                    size.width, size.height
                );
            }
            SurfaceUpdate::Damage { patches, .. } => {
                counts.patches += patches.len() as u64;
                counts.pixel_bytes += patches
                    .iter()
                    .map(|patch| patch.pixels.len() as u64)
                    .sum::<u64>();
            }
            SurfaceUpdate::FrameBoundary { completeness, .. } => {
                counts.frames += 1;
                counts.baselines += u64::from(completeness == FrameCompleteness::FullBaseline);
                if counts.first_frame.is_none() {
                    counts.first_frame = Some(Instant::now());
                    println!("首帧已解码 completeness={completeness:?}");
                }
                counts.last_frame = Some(Instant::now());
            }
        }
        Ok(())
    }
}

struct Events(mpsc::Sender<SessionEvent>);

impl RuntimeEventSink for Events {
    fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError> {
        self.0
            .send(event)
            .map_err(|_| ProtocolError::EventPortClosed)
    }
}

struct Wake;

impl RuntimeWake for Wake {
    fn wake(&self) -> Result<(), ProtocolError> {
        Ok(())
    }
}

fn read_line(reader: &mut impl BufRead) -> Result<String, &'static str> {
    let mut line = String::new();
    if reader
        .read_line(&mut line)
        .map_err(|_| "probe_stdin_failed")?
        == 0
    {
        return Err("probe_stdin_closed");
    }
    if line.ends_with('\n') {
        line.pop();
    }
    if line.ends_with('\r') {
        line.pop();
    }
    if line.is_empty() {
        return Err("probe_empty_input");
    }
    Ok(line)
}

fn exit_code(exit: &ProtocolExit) -> &'static str {
    match exit {
        ProtocolExit::Closed => "closed",
        ProtocolExit::Failed(error) => error.code(),
    }
}

fn load_pin(path: &Path) -> Result<Option<[u8; 32]>, &'static str> {
    match std::fs::read(path) {
        Ok(bytes) => bytes.try_into().map(Some).map_err(|_| "probe_pin_invalid"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("probe_pin_read_failed"),
    }
}

fn remember_pin(path: &Path, pin: [u8; 32]) -> Result<(), &'static str> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            file.write_all(&pin).map_err(|_| "probe_pin_write_failed")?;
            file.sync_all().map_err(|_| "probe_pin_write_failed")
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if load_pin(path)? == Some(pin) {
                Ok(())
            } else {
                Err("probe_pin_changed")
            }
        }
        Err(_) => Err("probe_pin_write_failed"),
    }
}

fn parse_egfx_opt_in(value: Option<&str>) -> Result<bool, &'static str> {
    match value {
        Some("1") => Ok(true),
        Some("") | None => Ok(false),
        Some(_) => Err("probe_egfx_opt_in_invalid"),
    }
}

fn egfx_opt_in(name: &str) -> Result<bool, &'static str> {
    match std::env::var(name) {
        Ok(value) => parse_egfx_opt_in(Some(&value)),
        Err(std::env::VarError::NotPresent) => parse_egfx_opt_in(None),
        Err(std::env::VarError::NotUnicode(_)) => Err("probe_egfx_opt_in_invalid"),
    }
}

fn probe_egfx_mode() -> Result<Option<ProbeEgfxMode>, &'static str> {
    let avc420 = egfx_opt_in(EGFX_AVC420_OPT_IN_ENV)?;
    let avc444 = egfx_opt_in(EGFX_AVC444_OPT_IN_ENV)?;
    probe_egfx_mode_from_flags(avc420, avc444)
}

fn probe_egfx_mode_from_flags(
    avc420: bool,
    avc444: bool,
) -> Result<Option<ProbeEgfxMode>, &'static str> {
    match (avc420, avc444) {
        (true, true) => Err("probe_egfx_multiple_codecs"),
        (true, false) => Ok(Some(ProbeEgfxMode::Avc420)),
        (false, true) => Ok(Some(ProbeEgfxMode::Avc444)),
        (false, false) => Ok(None),
    }
}

/// Load the exact software backend only for an explicit macOS probe opt-in.
///
/// The normal example path returns `None`, preserving the legacy-only probe.  The returned
/// factory is already checked against the exact H.264 profile selected by the opt-in; this local
/// capability line must not be confused with server confirmation or live decoder evidence.
fn load_probe_egfx_factory(
) -> Result<Option<(ProbeEgfxMode, Arc<dyn VideoDecoderFactory>)>, &'static str> {
    let Some(mode) = probe_egfx_mode()? else {
        println!("EGFX opt_in=false local_capability=disabled server_confirmation=not_requested");
        return Ok(None);
    };
    let mode_name = match mode {
        ProbeEgfxMode::Avc420 => "AVC420",
        ProbeEgfxMode::Avc444 => "AVC444",
    };

    #[cfg(not(target_os = "macos"))]
    {
        println!("EGFX {mode_name} opt_in=true local_capability=unavailable reason=macos_bundle_required");
        return Err("probe_egfx_macos_only");
    }

    #[cfg(target_os = "macos")]
    {
        let bundle = std::env::var_os(EGFX_BUNDLE_ENV).ok_or("probe_egfx_bundle_required")?;
        let backend = FfmpegBackend::load_from_signed_application_bundle(bundle)
            .map_err(|_| "probe_egfx_backend_unavailable")?;
        let query = VideoDecodeQuery {
            codec: VideoCodec::H264,
            profile: match mode {
                ProbeEgfxMode::Avc420 => VideoProfile::H264Avc420,
                ProbeEgfxMode::Avc444 => VideoProfile::H264Avc444,
            },
            chroma: match mode {
                ProbeEgfxMode::Avc420 => ChromaFormat::Yuv420,
                ProbeEgfxMode::Avc444 => ChromaFormat::Yuv444,
            },
            bit_depth: 8,
            coded_size: PixelSize::new(1, 1).ok_or("probe_egfx_query_invalid")?,
            frame_rate: None,
            preferred_outputs: vec![match mode {
                ProbeEgfxMode::Avc420 => VideoPixelFormat::Yuv420P8,
                ProbeEgfxMode::Avc444 => VideoPixelFormat::Yuv444P8,
            }]
            .into_boxed_slice(),
        };
        let support = backend.query(&query);
        let support_name = match &support {
            frd_media_api::VideoDecodeSupport::SoftwareExact(_) => "software_exact",
            frd_media_api::VideoDecodeSupport::HardwareExact(_) => "hardware_exact",
            frd_media_api::VideoDecodeSupport::Unsupported(_) => "unsupported",
        };
        let provider_state = if support.is_exact() {
            "attached"
        } else {
            "not_attached"
        };
        println!(
            "EGFX {mode_name} opt_in=true backend={} local_capability={} provider={} server_confirmation={}",
            backend.backend_id().as_str(),
            support_name,
            provider_state,
            if support.is_exact() {
                "pending"
            } else {
                "not_requested"
            }
        );
        if !support.is_exact() {
            return Err(match mode {
                ProbeEgfxMode::Avc420 => "probe_egfx_avc420_capability_unavailable",
                ProbeEgfxMode::Avc444 => "probe_egfx_avc444_capability_unavailable",
            });
        }
        if mode == ProbeEgfxMode::Avc444 && !backend.supports_avc420() {
            return Err("probe_egfx_avc444_requires_avc420_decoder");
        }
        Ok(Some((
            mode,
            Arc::new(backend) as Arc<dyn VideoDecoderFactory>,
        )))
    }
}

fn run() -> Result<(), &'static str> {
    // 拒绝直接终端输入，避免终端驱动回显密码；由可信父进程写入匿名管道。
    if io::stdin().is_terminal() {
        return Err("probe_requires_non_echoing_stdin_pipe");
    }
    let egfx_mode = probe_egfx_mode()?;
    let egfx_factory = load_probe_egfx_factory()?;
    let mut reader = io::BufReader::new(io::stdin());
    let host = read_line(&mut reader)?;
    let username = read_line(&mut reader)?;
    let mut password = SecretBuffer::from_text(read_line(&mut reader)?);
    let pin_root =
        std::env::var_os("FRD_RDP_PROBE_PIN_DIR").ok_or("probe_pin_directory_required")?;
    let pin_root = std::path::PathBuf::from(pin_root);
    std::fs::create_dir_all(&pin_root).map_err(|_| "probe_pin_directory_failed")?;
    let key = Sha256::digest(format!("rdp\0{host}\03389").as_bytes());
    let name = key
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let pin_path = pin_root.join(format!("{name}.pin"));
    let saved_pin = load_pin(&pin_path)?;
    let session_id = SessionId::allocate();
    let request = ConnectRequest {
        session_id,
        endpoint: Endpoint::new(host, 3389).ok_or("probe_invalid_endpoint")?,
        protocol_id: ProtocolId::rdp(),
        credentials: Some(Credentials {
            username,
            password: password.take(),
        }),
        saved_server_pin: saved_pin,
        display_intent: frd_core::DisplayIntent::default(),
    };
    let (command_tx, command_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let counts = Arc::new(Mutex::new(Counts::default()));
    let runtime = ProtocolRuntime::new(
        session_id,
        command_rx,
        Box::new(Events(event_tx)),
        Box::new(Frames(counts.clone())),
        None,
        Box::new(Wake),
    );
    // 高频编码计数最多每五秒输出一次；能力、编码类型和失败状态变化立即输出。
    let last_graphics_log = Mutex::new(None::<(RdpGraphicsCapabilities, Instant)>);
    let final_graphics = Arc::new(Mutex::new(None::<RdpGraphicsCapabilities>));
    let observer_graphics = final_graphics.clone();
    let graphics_observer: Arc<dyn RdpGraphicsObserver> = Arc::new(
        move |capabilities: RdpGraphicsCapabilities| {
            *observer_graphics
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(capabilities);
            let mut summary = capabilities;
            summary.egfx_diagnostics.unhandled_codec_count = 0;
            summary.egfx_diagnostics.avc420_decoded_pictures_total = 0;
            summary.egfx_diagnostics.avc444_decoded_updates_total = 0;
            summary.egfx_diagnostics.clearcodec_decoded_bitmaps_total = 0;
            summary.egfx_diagnostics.progressive_decoded_updates_total = 0;
            summary.egfx_diagnostics.frames_queued_total = 0;
            summary.egfx_diagnostics.frames_runtime_accepted_total = 0;
            let mut last = last_graphics_log
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if last.as_ref().is_some_and(|(previous, time)| {
                *previous == summary && time.elapsed() < Duration::from_secs(5)
            }) {
                return;
            }
            *last = Some((summary, Instant::now()));
            drop(last);
            println!(
                "RDP 图形能力 legacy_bitmap={} remotefx={} egfx_advertised={} egfx_confirmed={} egfx_frame_confirmed={} avc420={} avc444={}",
                capabilities.legacy_bitmap,
                capabilities.remotefx,
                capabilities.egfx_advertised,
                capabilities.egfx_confirmed,
                capabilities.egfx_frame_confirmed,
                capabilities.avc420,
                capabilities.avc444,
            );
            println!("RDP EGFX 阶段 start_calls={} capability_messages_queued={} typed_confirmation_ever={} failure_count={} first_failure={:?} progressive_failure_detail={:?} confirmed_version={:x?} confirmed_flags={:x?} unhandled_codec_count={} last_unhandled_codec={:x?}",
                capabilities.egfx_diagnostics.start_calls,
                capabilities.egfx_diagnostics.capability_messages_queued,
                capabilities.egfx_diagnostics.typed_confirmation_ever,
                capabilities.egfx_diagnostics.failure_count,
                capabilities.egfx_diagnostics.first_failure,
                capabilities.egfx_diagnostics.progressive_failure_detail,
                capabilities.egfx_diagnostics.confirmed_version,
                capabilities.egfx_diagnostics.confirmed_flags,
                capabilities.egfx_diagnostics.unhandled_codec_count,
                capabilities.egfx_diagnostics.last_unhandled_codec);
            println!("RDP EGFX 实际计数 avc420_decoded_pictures_total={} avc444_decoded_updates_total={} clearcodec_decoded_bitmaps_total={} progressive_decoded_updates_total={} frames_queued_total={} frames_runtime_accepted_total={}",
                capabilities.egfx_diagnostics.avc420_decoded_pictures_total,
                capabilities.egfx_diagnostics.avc444_decoded_updates_total,
                capabilities.egfx_diagnostics.clearcodec_decoded_bitmaps_total,
                capabilities.egfx_diagnostics.progressive_decoded_updates_total,
                capabilities.egfx_diagnostics.frames_queued_total,
                capabilities.egfx_diagnostics.frames_runtime_accepted_total);
        },
    );
    let factory = if let Some((mode, egfx_factory)) = egfx_factory {
        let provider: Arc<dyn frd_protocol_rdp::EgfxDecoderProvider> = match mode {
            ProbeEgfxMode::Avc420 => Arc::new(Avc420DecoderProvider::from_factory(egfx_factory)),
            ProbeEgfxMode::Avc444 => Arc::new(Avc444DecoderProvider::new(egfx_factory)),
        };
        RdpProtocolFactory::with_egfx_decoder_provider_and_gate(
            RdpClientPlatformIdentity::Macintosh,
            provider,
            frd_protocol_rdp::RdpGraphicsAdvertisementGate::LiveInteroperable,
        )
    } else {
        RdpProtocolFactory::new(RdpClientPlatformIdentity::Macintosh)
    }
    .with_graphics_observer(graphics_observer);
    let session = factory
        .create(request, runtime)
        .map_err(|error| error.code())?;
    let worker = thread::spawn(move || session.run());
    let start = Instant::now();
    let mut disconnect_at = None;
    let mut last_report = Instant::now();
    println!(
        "验证启动 client=macOS protocol=IronRDP input=disabled active_seconds=20 total_seconds=60"
    );
    loop {
        match event_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(SessionEvent::StageChanged(stage)) => println!("连接阶段 {stage:?}"),
            Ok(SessionEvent::ServerIdentityChallenge(challenge)) => {
                let fingerprint = challenge
                    .sha256_fingerprint
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                println!(
                    "身份确认 sha256={fingerprint} validation={:?}",
                    challenge.validation.kind()
                );
                if let Some(failure) = challenge.validation.failure() {
                    println!(
                        "身份原因 code={} reason={}",
                        failure.code(),
                        failure.reason()
                    );
                }
                let decision = if challenge.validation.is_pin_mismatch() {
                    println!("服务器证书指纹已变化，停止自动连接；保留原指纹");
                    ServerIdentityDecision::Reject
                } else if remember_pin(&pin_path, challenge.sha256_fingerprint).is_ok() {
                    println!("服务器证书身份已保存/匹配，继续连接");
                    if saved_pin.is_none() {
                        ServerIdentityDecision::TrustAndRemember
                    } else {
                        ServerIdentityDecision::TrustOnce
                    }
                } else {
                    println!("指纹持久化失败，停止连接");
                    ServerIdentityDecision::Reject
                };
                let _ = command_tx.send(SessionCommand::ResolveServerIdentity {
                    session_id,
                    challenge_id: challenge.challenge_id,
                    decision,
                });
            }
            Ok(SessionEvent::SurfaceGenerationChanged {
                generation, size, ..
            }) => {
                println!(
                    "激活 generation={generation} width={} height={}",
                    size.width, size.height
                );
            }
            Ok(SessionEvent::CapabilitiesChanged(caps)) => println!("能力声明 {caps:?}"),
            Ok(SessionEvent::AudioState(state)) => println!("音频状态 {state:?}"),
            Ok(SessionEvent::Error(error)) => println!("会话错误 code={}", error.code()),
            Ok(SessionEvent::Closed(exit)) => println!("关闭事件 code={}", exit_code(&exit)),
            // 不输出远程剪贴板或底层诊断。
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }
        let counts_guard = counts.lock().map_err(|_| "probe_counts_failed")?;
        let active_elapsed = counts_guard.first_frame.map(|time| time.elapsed());
        if last_report.elapsed() >= Duration::from_secs(5) {
            println!(
                "统计 elapsed_seconds={} resets={} frames={} full_baselines={} patches={} decoded_bytes={}",
                start.elapsed().as_secs(),
                counts_guard.resets,
                counts_guard.frames,
                counts_guard.baselines,
                counts_guard.patches,
                counts_guard.pixel_bytes
            );
            last_report = Instant::now();
        }
        drop(counts_guard);
        if worker.is_finished() {
            break;
        }
        if disconnect_at.is_none()
            && (start.elapsed() >= Duration::from_secs(60)
                || active_elapsed.is_some_and(|elapsed| elapsed >= Duration::from_secs(20)))
        {
            println!("有界观察结束，发送 Disconnect");
            let _ = command_tx.send(SessionCommand::Disconnect);
            disconnect_at = Some(Instant::now());
        }
        if disconnect_at.is_some_and(|time| time.elapsed() > Duration::from_secs(15)) {
            return Err("probe_cleanup_timeout");
        }
    }
    let exit = worker.join().map_err(|_| "probe_worker_panicked")?;
    let counts = counts.lock().map_err(|_| "probe_counts_failed")?;
    let first_frame_ms = counts
        .first_frame
        .map(|time| time.duration_since(start).as_millis());
    let sustained_refresh_ms = counts
        .first_frame
        .zip(counts.last_frame)
        .map(|(first, last)| last.duration_since(first).as_millis());
    println!(
        "验证结果 exit={} resets={} frames={} full_baselines={} patches={} decoded_bytes={} first_frame_ms={first_frame_ms:?} sustained_refresh_ms={sustained_refresh_ms:?} cleanup=joined",
        exit_code(&exit),
        counts.resets,
        counts.frames,
        counts.baselines,
        counts.patches,
        counts.pixel_bytes
    );
    match exit {
        ProtocolExit::Failed(error) => Err(error.code()),
        ProtocolExit::Closed if counts.frames == 0 => Err("probe_no_decoded_frames"),
        ProtocolExit::Closed => {
            let latest = final_graphics.lock().map_err(|_| "probe_graphics_failed")?;
            validate_egfx_result(egfx_mode, latest.as_ref().map(|c| &c.egfx_diagnostics))
        }
    }
}

// AVC opt-in 必须验证真实 codec 使用，不能以 ClearCodec 首帧替代 AVC 验收。
fn validate_egfx_result(
    mode: Option<ProbeEgfxMode>,
    diagnostics: Option<&frd_protocol_rdp::RdpEgfxDiagnostics>,
) -> Result<(), &'static str> {
    let Some(mode) = mode else {
        return Ok(());
    };
    let evidence = diagnostics.ok_or("probe_egfx_evidence_missing")?;
    if evidence.failure_count != 0 {
        return Err("probe_egfx_pipeline_failed");
    }
    let decoded = match mode {
        ProbeEgfxMode::Avc420 => evidence.avc420_decoded_pictures_total,
        ProbeEgfxMode::Avc444 => evidence.avc444_decoded_updates_total,
    };
    if decoded == 0 {
        return Err("probe_requested_avc_not_decoded");
    }
    if evidence.frames_runtime_accepted_total == 0 {
        return Err("probe_egfx_no_runtime_frames");
    }
    Ok(())
}

fn main() {
    if let Err(code) = run() {
        eprintln!("验证未通过 code={code}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_egfx_opt_in, probe_egfx_mode_from_flags, ProbeEgfxMode};

    #[test]
    fn avc_probe_rejects_clearcodec_only_and_failure_after_first_frame() {
        let mut evidence = frd_protocol_rdp::RdpEgfxDiagnostics {
            clearcodec_decoded_bitmaps_total: 3,
            frames_runtime_accepted_total: 2,
            ..Default::default()
        };
        assert_eq!(
            super::validate_egfx_result(Some(ProbeEgfxMode::Avc444), Some(&evidence)),
            Err("probe_requested_avc_not_decoded")
        );
        evidence.avc444_decoded_updates_total = 1;
        assert_eq!(
            super::validate_egfx_result(Some(ProbeEgfxMode::Avc444), Some(&evidence)),
            Ok(())
        );
        evidence.failure_count = 1;
        assert_eq!(
            super::validate_egfx_result(Some(ProbeEgfxMode::Avc444), Some(&evidence)),
            Err("probe_egfx_pipeline_failed")
        );
        assert_eq!(super::validate_egfx_result(None, None), Ok(()));
    }

    #[test]
    fn egfx_opt_in_accepts_only_one() {
        assert_eq!(parse_egfx_opt_in(Some("1")), Ok(true));
        assert_eq!(parse_egfx_opt_in(Some("")), Ok(false));
        assert_eq!(parse_egfx_opt_in(None), Ok(false));
        assert_eq!(
            parse_egfx_opt_in(Some("true")),
            Err("probe_egfx_opt_in_invalid")
        );
    }

    #[test]
    fn egfx_probe_mode_rejects_ambiguous_codec_opt_in() {
        assert_eq!(probe_egfx_mode_from_flags(false, false), Ok(None));
        assert_eq!(
            probe_egfx_mode_from_flags(true, false),
            Ok(Some(ProbeEgfxMode::Avc420))
        );
        assert_eq!(
            probe_egfx_mode_from_flags(false, true),
            Ok(Some(ProbeEgfxMode::Avc444))
        );
        assert_eq!(
            probe_egfx_mode_from_flags(true, true),
            Err("probe_egfx_multiple_codecs")
        );
    }
}
