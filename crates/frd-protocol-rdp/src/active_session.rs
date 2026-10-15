use std::io::ErrorKind;
use std::sync::Arc;
use std::time::Duration;

use frd_core::{PixelSize, SessionId};
use frd_frame::SurfaceUpdate;
use frd_protocol_api::{
    ProtocolError, ProtocolExit, ProtocolRuntime, SessionCapabilities, SessionEvent,
};
use ironrdp::connector::connection_activation::{
    ConnectionActivationFactory, ConnectionActivationState,
};
use ironrdp::connector::{ConnectionResult, DesktopSize, Sequence as _};
use ironrdp::core::WriteBuf;
use ironrdp::displaycontrol::client::DisplayControlClient;
use ironrdp::graphics::image_processing::PixelFormat as IronPixelFormat;
#[cfg(test)]
use ironrdp::pdu::geometry::InclusiveRectangle;
use ironrdp::rdpsnd::client::Rdpsnd;
use ironrdp::session::image::{ActiveStageProcessResult, DecodedImage};
use ironrdp::session::{fast_path, ActiveStage, ActiveStageBuilder, ActiveStageOutput};

use crate::audio::{self, RdpAudioAdapter};
use crate::baseline::{RdpBaseline, MAX_DECODED_COVERAGE_ENTRIES};
use crate::clipboard::{self, ClipboardServiceAction};
use crate::connector::{ActivatedRdpSession, RdpGraphicsCapability};
use crate::display::{DisplayControlAdapter, DisplayControlCapabilityState, ResizeConfirmation};
use crate::egfx::EgfxAdapter;
use crate::error::{rdp_error, RDP_ACTIVATION_FAILED, RDP_EGFX_FAILED};
use crate::factory::{RdpEgfxFailure, RdpGraphicsCapabilities, RdpGraphicsObserver};
use crate::input::RdpInputState;
use crate::legacy_decode::ResolvedLegacyDecode;
use crate::runtime::{
    drain_active_commands, drain_reactivation_commands, ActiveCommandBatch, ActiveCommandDrain,
    ActiveOptionalCommand, ReactivationCommand,
};
use crate::surface::validate_negotiated_size;
use crate::tls::TlsStream;
use crate::writer::OrderedRdpWriter;

const ACTIVE_SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(10);
const ACTIVE_SOCKET_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_FAST_PATH_INPUT_EVENTS: usize = 255;

pub(crate) fn run_active_session(
    session: ActivatedRdpSession,
    session_id: SessionId,
    runtime: &mut ProtocolRuntime,
    legacy_decode: ResolvedLegacyDecode,
    graphics_observer: Option<Arc<dyn RdpGraphicsObserver>>,
) -> ProtocolExit {
    match run_active_session_inner(
        session,
        session_id,
        runtime,
        legacy_decode,
        graphics_observer,
    ) {
        Ok(()) => ProtocolExit::Closed,
        Err(error) => ProtocolExit::Failed(error),
    }
}

fn run_active_session_inner(
    session: ActivatedRdpSession,
    session_id: SessionId,
    runtime: &mut ProtocolRuntime,
    legacy_decode: ResolvedLegacyDecode,
    graphics_observer: Option<Arc<dyn RdpGraphicsObserver>>,
) -> Result<(), ProtocolError> {
    let ActivatedRdpSession {
        connection,
        transport,
        audio,
        display_capabilities,
        mut graphics_capability,
    } = session;
    debug_assert!(
        graphics_capability.legacy_bitmap && graphics_capability.remotefx,
        "RDP activation must retain the legacy fallback"
    );
    if let Some(observer) = graphics_observer.as_deref() {
        observer.observe(graphics_capability.snapshot());
    }
    let (framed, _server_public_key) = transport.into_parts();
    let mut writer = OrderedRdpWriter::new(framed);
    if writer
        .set_read_timeout(ACTIVE_SOCKET_POLL_INTERVAL)
        .is_err()
        || writer
            .set_write_timeout(ACTIVE_SOCKET_WRITE_TIMEOUT)
            .is_err()
    {
        writer.shutdown();
        return Err(rdp_error(RDP_ACTIVATION_FAILED));
    }

    let ConnectionResult {
        io_channel_id,
        user_channel_id,
        message_channel_id,
        share_id,
        static_channels,
        desktop_size,
        enable_server_pointer,
        pointer_software_rendering,
        activation_factory,
        compression_type,
    } = connection;
    let negotiated_size = match validate_negotiated_size(desktop_size.width, desktop_size.height) {
        Ok(size) => size,
        Err(_) => {
            writer.shutdown();
            return Err(rdp_error(RDP_ACTIVATION_FAILED));
        }
    };
    let mut baseline = match RdpBaseline::begin(runtime, session_id, negotiated_size) {
        Ok(baseline) => baseline,
        Err(error) => {
            writer.shutdown();
            return Err(error);
        }
    };
    let mut image = DecodedImage::new(
        IronPixelFormat::RgbA32,
        desktop_size.width,
        desktop_size.height,
    );
    let mut active_stage = build_active_stage(
        ActiveStageBuilder {
            static_channels,
            user_channel_id,
            io_channel_id,
            message_channel_id,
            share_id,
            compression_type,
            enable_server_pointer,
            pointer_software_rendering,
        },
        legacy_decode,
    );
    let mut input = RdpInputState::new();
    if let Err(error) = publish_active_input_capabilities(runtime) {
        writer.shutdown();
        return Err(error);
    }
    let mut generation = 1;
    let mut display = DisplayControlAdapter::new(negotiated_size);
    let mut published_capabilities = SessionCapabilities {
        physical_key_input: true,
        text_input: true,
        ..SessionCapabilities::default()
    };

    let result = run_active_loop(
        session_id,
        runtime,
        &mut writer,
        &mut active_stage,
        &mut image,
        &mut baseline,
        &mut input,
        &mut generation,
        &mut display,
        &display_capabilities,
        &audio,
        &mut graphics_capability,
        graphics_observer.as_deref(),
        &mut published_capabilities,
        &activation_factory,
    );
    let audio_cleanup = stop_and_drain_audio(runtime, &audio);
    if let Some(cliprdr) = active_stage.get_svc_processor_mut::<ironrdp::cliprdr::CliprdrClient>() {
        clipboard::reset(cliprdr);
    }
    best_effort_release_held_input(
        runtime,
        &mut writer,
        &mut active_stage,
        &mut image,
        &mut baseline,
        &mut input,
        generation,
    );
    writer.shutdown();
    result.and(audio_cleanup)
}

/// 初始生产 Stage 和编码流测试共用此构造路径；处理任何输入前完成绑定。
pub(crate) fn build_active_stage(
    builder: ActiveStageBuilder,
    legacy_decode: ResolvedLegacyDecode,
) -> ActiveStage {
    let mut stage = builder.build();
    if let ResolvedLegacyDecode::CheckedNative(kernels) = legacy_decode {
        stage.set_native_legacy_kernels(kernels);
    }
    stage
}

#[allow(clippy::too_many_arguments)]
fn run_active_loop(
    session_id: SessionId,
    runtime: &mut ProtocolRuntime,
    writer: &mut OrderedRdpWriter<TlsStream>,
    active_stage: &mut ActiveStage,
    image: &mut DecodedImage,
    baseline: &mut RdpBaseline,
    input: &mut RdpInputState,
    generation: &mut u64,
    display: &mut DisplayControlAdapter,
    display_capabilities: &DisplayControlCapabilityState,
    audio: &RdpAudioAdapter,
    graphics_capability: &mut RdpGraphicsCapability,
    graphics_observer: Option<&dyn RdpGraphicsObserver>,
    published_capabilities: &mut SessionCapabilities,
    activation_factory: &ConnectionActivationFactory,
) -> Result<(), ProtocolError> {
    let mut command_drain = ActiveCommandDrain::new();
    let mut display_retry = DisplayRetryState::default();
    loop {
        refresh_optional_capabilities(
            active_stage,
            display,
            display_capabilities,
            audio,
            runtime,
            published_capabilities,
            !display_retry.pending(),
        )?;
        if display_retry.pending() {
            match drain_reactivation_commands(runtime, session_id, *generation) {
                ReactivationCommand::Continue { latest_viewport } => {
                    if let Some(viewport) = latest_viewport {
                        display.observe_viewport(viewport);
                    }
                }
                ReactivationCommand::Disconnect => return Ok(()),
                ReactivationCommand::Terminal => return Err(ProtocolError::Terminal),
            }
        } else {
            match drain_active_commands(runtime, input, &mut command_drain)
                .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?
            {
                ActiveCommandBatch::Continue {
                    events,
                    optional_commands,
                    pending,
                } => {
                    if route_input_events(
                        active_stage,
                        image,
                        writer,
                        runtime,
                        baseline,
                        *generation,
                        &events,
                        display_retry.publish_graphics(),
                    )? != ActiveOutputControl::Continue
                    {
                        return Err(rdp_error(RDP_ACTIVATION_FAILED));
                    }
                    service_optional_channels(
                        active_stage,
                        writer,
                        runtime,
                        display,
                        audio,
                        optional_commands,
                    )?;
                    refresh_optional_capabilities(
                        active_stage,
                        display,
                        display_capabilities,
                        audio,
                        runtime,
                        published_capabilities,
                        true,
                    )?;
                    if pending {
                        continue;
                    }
                }
                ActiveCommandBatch::Disconnect(events) => {
                    let _ = route_input_events(
                        active_stage,
                        image,
                        writer,
                        runtime,
                        baseline,
                        *generation,
                        &events,
                        display_retry.publish_graphics(),
                    );
                    return Ok(());
                }
                ActiveCommandBatch::Terminal(events) => {
                    let _ = route_input_events(
                        active_stage,
                        image,
                        writer,
                        runtime,
                        baseline,
                        *generation,
                        &events,
                        display_retry.publish_graphics(),
                    );
                    return Err(ProtocolError::Terminal);
                }
            }
        }

        let (action, payload) = match writer.read_pdu() {
            Ok(frame) => frame,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                continue;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::UnexpectedEof
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::ConnectionReset
                ) =>
            {
                writer.stop_writes();
                return Ok(());
            }
            Err(_) => return Err(rdp_error(RDP_ACTIVATION_FAILED)),
        };
        let batch =
            process_server_batch(active_stage, image, *generation, action, payload.as_ref())?;
        match route_active_outputs(
            batch,
            writer,
            runtime,
            baseline,
            display_retry.publish_graphics(),
        )? {
            ActiveOutputControl::Continue => {}
            ActiveOutputControl::Terminate => return Ok(()),
            ActiveOutputControl::Deactivate => {
                stop_and_drain_audio(runtime, audio)?;
                let releases = suspend_active_input_capabilities(runtime, input)?;
                *published_capabilities = SessionCapabilities::default();
                if route_input_events(
                    active_stage,
                    image,
                    writer,
                    runtime,
                    baseline,
                    *generation,
                    &releases,
                    display_retry.publish_graphics(),
                )? != ActiveOutputControl::Continue
                {
                    return Err(rdp_error(RDP_ACTIVATION_FAILED));
                }
                let outcome = drive_reactivation(
                    session_id,
                    runtime,
                    writer,
                    active_stage,
                    image,
                    baseline,
                    generation,
                    display,
                    display_capabilities,
                    activation_factory,
                )?;
                match apply_reactivation_outcome(
                    outcome,
                    &mut display_retry,
                    runtime,
                    input,
                    published_capabilities,
                )? {
                    ReactivationOutcome::Continue | ReactivationOutcome::RetryDisplay => {}
                    ReactivationOutcome::Disconnect => return Ok(()),
                    ReactivationOutcome::Terminal => return Err(ProtocolError::Terminal),
                }
            }
        }
        let egfx_updates = drain_egfx_surface_updates(active_stage);
        let egfx_frame_confirmed = egfx_updates
            .iter()
            .any(|update| matches!(update, SurfaceUpdate::FrameBoundary { .. }));
        let accepted =
            publish_egfx_surface_updates(runtime, baseline, session_id, generation, egfx_updates)?;
        let previous_graphics = graphics_capability.snapshot();
        observe_egfx_confirmation(active_stage, graphics_capability, egfx_frame_confirmed);
        graphics_capability
            .egfx_diagnostics
            .frames_runtime_accepted_total = graphics_capability
            .egfx_diagnostics
            .frames_runtime_accepted_total
            .saturating_add(accepted);
        let current_graphics = graphics_capability.snapshot();
        publish_graphics_status(previous_graphics, current_graphics, graphics_observer)?;
        service_optional_channels(active_stage, writer, runtime, display, audio, Vec::new())?;
    }
}

/// 先发布固定诊断，再终止已失败的图形会话；正常重激活停用不构成失败。
fn publish_graphics_status(
    previous: RdpGraphicsCapabilities,
    current: RdpGraphicsCapabilities,
    observer: Option<&dyn RdpGraphicsObserver>,
) -> Result<(), ProtocolError> {
    if previous != current {
        if let Some(observer) = observer {
            observer.observe(current);
        }
    }
    let diagnostics = current.egfx_diagnostics;
    if diagnostics.first_failure != Some(RdpEgfxFailure::Reactivation)
        && (diagnostics.failure_count > 0 || diagnostics.first_failure.is_some())
    {
        return Err(rdp_error(RDP_EGFX_FAILED));
    }
    Ok(())
}

fn observe_egfx_confirmation(
    active_stage: &mut ActiveStage,
    graphics_capability: &mut RdpGraphicsCapability,
    egfx_frame_confirmed: bool,
) {
    let Some(adapter) = active_stage
        .get_dvc::<EgfxAdapter>()
        .and_then(|channel| channel.channel_processor_downcast_ref::<EgfxAdapter>())
    else {
        return;
    };

    let accepted = graphics_capability
        .egfx_diagnostics
        .frames_runtime_accepted_total;
    graphics_capability.egfx_diagnostics = adapter.diagnostics();
    graphics_capability
        .egfx_diagnostics
        .frames_runtime_accepted_total = accepted;
    apply_egfx_confirmation(
        graphics_capability,
        adapter.is_active(),
        adapter.avc420_confirmed(),
        adapter.avc444_confirmed(),
        egfx_frame_confirmed,
    );
}

fn apply_egfx_confirmation(
    graphics_capability: &mut RdpGraphicsCapability,
    egfx_confirmed: bool,
    avc420_confirmed: bool,
    avc444_confirmed: bool,
    egfx_frame_confirmed: bool,
) {
    graphics_capability.egfx_confirmed = egfx_confirmed;
    graphics_capability.egfx_frame_confirmed |= egfx_frame_confirmed;
    graphics_capability.avc420 =
        graphics_capability.egfx_advertised && egfx_confirmed && avc420_confirmed;
    graphics_capability.avc444 =
        graphics_capability.egfx_advertised && egfx_confirmed && avc444_confirmed;
}

fn refresh_optional_capabilities(
    active_stage: &mut ActiveStage,
    display: &mut DisplayControlAdapter,
    display_capabilities: &DisplayControlCapabilityState,
    audio_adapter: &RdpAudioAdapter,
    runtime: &mut ProtocolRuntime,
    published: &mut SessionCapabilities,
    keyboard_input_ready: bool,
) -> Result<(), ProtocolError> {
    let display_max_monitor_area =
        current_display_max_monitor_area(active_stage, display_capabilities);
    display.set_negotiated(display_max_monitor_area);

    if let Some(rdpsnd) = active_stage.get_svc_processor::<Rdpsnd>() {
        audio::observe_negotiation(rdpsnd, audio_adapter);
    }
    let clipboard_capabilities = active_stage
        .get_svc_processor::<ironrdp::cliprdr::CliprdrClient>()
        .map(clipboard::capabilities)
        .unwrap_or_default();
    let audio_capabilities = audio::capabilities(audio_adapter);
    let desired = SessionCapabilities {
        dynamic_resolution: display.dynamic_resolution(),
        clipboard_read: clipboard_capabilities.clipboard_read,
        clipboard_write: clipboard_capabilities.clipboard_write,
        remote_audio: audio_capabilities.remote_audio,
        physical_key_input: keyboard_input_ready,
        text_input: keyboard_input_ready,
    };
    if desired == *published {
        return Ok(());
    }
    let audio_started = !published.remote_audio && desired.remote_audio;
    runtime.publish_event(SessionEvent::CapabilitiesChanged(desired))?;
    *published = desired;
    if audio_started {
        runtime.publish_event(SessionEvent::AudioState(
            frd_protocol_api::AudioState::Starting,
        ))?;
    }
    Ok(())
}

fn current_display_max_monitor_area(
    active_stage: &mut ActiveStage,
    display_capabilities: &DisplayControlCapabilityState,
) -> Option<u64> {
    active_stage
        .get_dvc::<DisplayControlClient>()
        .and_then(|channel| channel.channel_processor_downcast_ref::<DisplayControlClient>())
        .filter(|display_control| display_control.ready())
        .and_then(|_| display_capabilities.max_monitor_area())
}

fn service_optional_channels(
    active_stage: &mut ActiveStage,
    writer: &mut OrderedRdpWriter<TlsStream>,
    runtime: &mut ProtocolRuntime,
    display: &mut DisplayControlAdapter,
    audio_adapter: &RdpAudioAdapter,
    commands: Vec<ActiveOptionalCommand>,
) -> Result<(), ProtocolError> {
    for command in commands {
        match command {
            ActiveOptionalCommand::ViewportChanged(viewport) => {
                display.observe_viewport(viewport);
            }
            ActiveOptionalCommand::ClipboardWrite(payload) => {
                let messages = active_stage
                    .get_svc_processor_mut::<ironrdp::cliprdr::CliprdrClient>()
                    .map(|cliprdr| clipboard::write_text(cliprdr, payload))
                    .transpose()
                    .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?
                    .flatten();
                if let Some(messages) = messages {
                    let frame = active_stage
                        .process_svc_processor_messages(messages)
                        .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?;
                    writer
                        .write_frame(&frame)
                        .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?;
                }
            }
        }
    }

    loop {
        let action = active_stage
            .get_svc_processor_mut::<ironrdp::cliprdr::CliprdrClient>()
            .map(clipboard::next_service_action)
            .transpose()
            .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?
            .flatten();
        match action {
            Some(ClipboardServiceAction::Wire(messages)) => {
                let frame = active_stage
                    .process_svc_processor_messages(messages)
                    .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?;
                writer
                    .write_frame(&frame)
                    .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?;
            }
            Some(ClipboardServiceAction::Publish(payload)) => {
                runtime.publish_event(SessionEvent::Clipboard(payload))?;
            }
            None => break,
        }
    }

    send_pending_resize(display, writer, |target| {
        active_stage
            .encode_resize(target.width, target.height, None, None)
            .ok_or_else(|| rdp_error(RDP_ACTIVATION_FAILED))?
            .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))
    })?;

    audio_adapter.drain_to_runtime(runtime)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveOutputControl {
    Continue,
    Terminate,
    Deactivate,
}

/// 同步消费的借用信封：只有执行解码的 helper 能绑定图像，借用期间不能替换它。
struct ProcessedImageBatch<'image> {
    image: &'image DecodedImage,
    source_generation: u64,
    result: ActiveStageProcessResult,
}

fn process_server_batch<'image>(
    stage: &mut ActiveStage,
    image: &'image mut DecodedImage,
    generation: u64,
    action: ironrdp::pdu::Action,
    payload: &[u8],
) -> Result<ProcessedImageBatch<'image>, ProtocolError> {
    let source_generation = generation;
    let result = stage
        .process_with_decoded_coverage(image, action, payload)
        .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?;
    Ok(ProcessedImageBatch {
        image,
        source_generation,
        result,
    })
}

fn process_input_batch<'image>(
    stage: &mut ActiveStage,
    image: &'image mut DecodedImage,
    generation: u64,
    events: &[ironrdp::pdu::input::fast_path::FastPathInputEvent],
) -> Result<ProcessedImageBatch<'image>, ProtocolError> {
    let source_generation = generation;
    let result = stage
        .process_fastpath_input_with_decoded_coverage(image, events)
        .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?;
    Ok(ProcessedImageBatch {
        image,
        source_generation,
        result,
    })
}

/// 全批次先校验，再按原输出次序写响应、呈现并收集控制状态。
fn validate_processed_batch(
    batch: &mut ProcessedImageBatch<'_>,
    baseline: &RdpBaseline,
    publish_graphics: bool,
) -> Result<(), ProtocolError> {
    let receipts = &mut batch.result.decoded_coverage;
    if receipts.len() > MAX_DECODED_COVERAGE_ENTRIES {
        return Err(ProtocolError::FramePortRejected);
    }
    receipts.sort_unstable_by_key(|receipt| receipt.graphics_output_index);
    let mut receipt_iter = receipts.iter();
    let mut count = 0usize;
    for (index, output) in batch.result.outputs.iter().enumerate() {
        if let ActiveStageOutput::GraphicsUpdate(damage) = output {
            let receipt = receipt_iter
                .next()
                .ok_or(ProtocolError::FramePortRejected)?;
            if receipt.graphics_output_index != index {
                return Err(ProtocolError::FramePortRejected);
            }
            count = count
                .checked_add(receipt.rectangles.len())
                .filter(|&n| n <= MAX_DECODED_COVERAGE_ENTRIES)
                .ok_or(ProtocolError::FramePortRejected)?;
            if publish_graphics {
                baseline.validate_publication(
                    batch.image,
                    batch.source_generation,
                    damage,
                    &receipt.rectangles,
                )?;
            } else {
                // 过时显示仍允许响应/控制前进，但内容收据也必须一起丢弃。
                RdpBaseline::validate_regions(batch.image, damage, &receipt.rectangles)?;
            }
        }
    }
    if receipt_iter.next().is_some() {
        return Err(ProtocolError::FramePortRejected);
    }
    Ok(())
}

fn route_active_outputs<S: std::io::Write>(
    mut batch: ProcessedImageBatch<'_>,
    writer: &mut OrderedRdpWriter<S>,
    runtime: &mut ProtocolRuntime,
    baseline: &mut RdpBaseline,
    publish_graphics: bool,
) -> Result<ActiveOutputControl, ProtocolError> {
    validate_processed_batch(&mut batch, baseline, publish_graphics)?;
    let mut receipts = batch.result.decoded_coverage.into_iter();
    let mut control = ActiveOutputControl::Continue;
    for output in batch.result.outputs {
        match output {
            ActiveStageOutput::ResponseFrame(frame) => writer
                .write_frame(&frame)
                .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?,
            ActiveStageOutput::GraphicsUpdate(region) => {
                let receipt = receipts.next().expect("完整批次已校验图形与收据一一对应");
                if publish_graphics {
                    baseline.publish(
                        runtime,
                        batch.image,
                        batch.source_generation,
                        region,
                        &receipt.rectangles,
                    )?;
                }
            }
            ActiveStageOutput::Terminate(_) => control = ActiveOutputControl::Terminate,
            ActiveStageOutput::DeactivateAll if control != ActiveOutputControl::Terminate => {
                control = ActiveOutputControl::Deactivate;
            }
            ActiveStageOutput::DeactivateAll => {}
            ActiveStageOutput::PointerDefault
            | ActiveStageOutput::PointerHidden
            | ActiveStageOutput::PointerPosition { .. }
            | ActiveStageOutput::PointerBitmap(_)
            | ActiveStageOutput::MultitransportRequest(_)
            | ActiveStageOutput::AutoDetect(_) => {}
        }
    }
    Ok(control)
}

fn drain_egfx_surface_updates(active_stage: &mut ActiveStage) -> Vec<SurfaceUpdate> {
    active_stage
        .get_dvc::<EgfxAdapter>()
        .and_then(|channel| channel.channel_processor_downcast_ref::<EgfxAdapter>())
        .map(EgfxAdapter::drain_surface_updates)
        .unwrap_or_default()
}

fn disable_egfx_for_reactivation(active_stage: &mut ActiveStage) {
    if let Some(adapter) = active_stage
        .get_dvc::<EgfxAdapter>()
        .and_then(|channel| channel.channel_processor_downcast_ref::<EgfxAdapter>())
    {
        adapter.disable_for_reactivation();
    }
}

/// 当前 producer 在同一锁下发布并 drain 完整 Damage/Boundary 对。
/// 先校验整个列表，避免半帧或坏尾部在共享邮箱中留下有效前缀。
fn validate_egfx_publication_units(
    session_id: SessionId,
    generation: u64,
    updates: &[SurfaceUpdate],
) -> Result<(), ProtocolError> {
    let mut expected_generation = generation;
    let mut units = updates.iter();
    while let Some(update) = units.next() {
        match update {
            SurfaceUpdate::Reset {
                session_id: owner,
                generation: next,
                format,
                ..
            } => {
                if *owner != session_id {
                    return Err(ProtocolError::StaleSession);
                }
                if *next <= expected_generation {
                    return Err(ProtocolError::InvalidGeneration);
                }
                if *format != frd_frame::PixelFormat::Bgrx8UnormSrgb {
                    return Err(ProtocolError::FramePortRejected);
                }
                expected_generation = *next;
            }
            SurfaceUpdate::Damage {
                session_id: owner,
                generation: current,
                revision,
                ..
            } => {
                if *owner != session_id || *current != expected_generation {
                    return Err(ProtocolError::StaleSurface);
                }
                let Some(SurfaceUpdate::FrameBoundary {
                    session_id: boundary_owner,
                    generation: boundary_generation,
                    revision: boundary_revision,
                    ..
                }) = units.next()
                else {
                    return Err(ProtocolError::FramePortRejected);
                };
                if *boundary_owner != session_id || *boundary_generation != expected_generation {
                    return Err(ProtocolError::StaleSurface);
                }
                if boundary_revision != revision {
                    return Err(ProtocolError::FramePortRejected);
                }
            }
            SurfaceUpdate::FrameBoundary {
                session_id: owner,
                generation: current,
                ..
            } => {
                if *owner != session_id || *current != expected_generation {
                    return Err(ProtocolError::StaleSurface);
                }
                return Err(ProtocolError::FramePortRejected);
            }
        }
    }
    Ok(())
}

fn publish_egfx_surface_updates(
    runtime: &mut ProtocolRuntime,
    baseline: &mut RdpBaseline,
    session_id: SessionId,
    generation: &mut u64,
    updates: Vec<SurfaceUpdate>,
) -> Result<u64, ProtocolError> {
    validate_egfx_publication_units(session_id, *generation, &updates)?;
    let mut accepted = 0u64;
    for update in updates {
        match update {
            SurfaceUpdate::Reset {
                session_id: update_session,
                generation: update_generation,
                size,
                format,
            } => {
                if update_session != session_id {
                    return Err(ProtocolError::StaleSession);
                }
                if update_generation <= *generation {
                    return Err(ProtocolError::InvalidGeneration);
                }
                if format != frd_frame::PixelFormat::Bgrx8UnormSrgb {
                    return Err(ProtocolError::FramePortRejected);
                }
                // EGFX and legacy Bitmap/RemoteFX share the runtime surface.
                // Rebind the legacy coverage tracker at the same generation
                // boundary so a later legacy update remains a valid fallback.
                // `begin_next_generation` allocates the replacement before
                // committing the runtime reset, preserving fail-closed state
                // if the replacement cannot be created.
                baseline.begin_next_generation(runtime, update_generation, size)?;
                *generation = update_generation;
            }
            update => {
                let frame = matches!(update, SurfaceUpdate::FrameBoundary { .. });
                let (external_generation, external_revision) = match &update {
                    SurfaceUpdate::Damage {
                        generation,
                        revision,
                        ..
                    }
                    | SurfaceUpdate::FrameBoundary {
                        generation,
                        revision,
                        ..
                    } => (*generation, *revision),
                    SurfaceUpdate::Reset { .. } => unreachable!("Reset 在上面的分支处理"),
                };
                runtime.publish_surface(update)?;
                // 已接受的外部呈现不证明 legacy image 内容完整；下一次 legacy
                // 必须用较新的完整快照接管，不能混入旧呈现的增量。
                baseline.observe_external_revision(external_generation, external_revision)?;
                if frame {
                    accepted = accepted.saturating_add(1);
                }
            }
        }
    }
    Ok(accepted)
}

#[allow(clippy::too_many_arguments)]
fn route_input_events(
    active_stage: &mut ActiveStage,
    image: &mut DecodedImage,
    writer: &mut OrderedRdpWriter<TlsStream>,
    runtime: &mut ProtocolRuntime,
    baseline: &mut RdpBaseline,
    generation: u64,
    events: &[ironrdp::pdu::input::fast_path::FastPathInputEvent],
    publish_graphics: bool,
) -> Result<ActiveOutputControl, ProtocolError> {
    if events.is_empty() || !writer.is_writable() {
        return Ok(ActiveOutputControl::Continue);
    }
    let mut control = ActiveOutputControl::Continue;
    for batch in fast_path_input_batches(events) {
        let processed = process_input_batch(active_stage, image, generation, batch)?;
        match route_active_outputs(processed, writer, runtime, baseline, publish_graphics)? {
            ActiveOutputControl::Continue => {}
            ActiveOutputControl::Deactivate if control != ActiveOutputControl::Terminate => {
                control = ActiveOutputControl::Deactivate;
            }
            ActiveOutputControl::Deactivate => {}
            ActiveOutputControl::Terminate => control = ActiveOutputControl::Terminate,
        }
    }
    Ok(control)
}

fn fast_path_input_batches(
    events: &[ironrdp::pdu::input::fast_path::FastPathInputEvent],
) -> std::slice::Chunks<'_, ironrdp::pdu::input::fast_path::FastPathInputEvent> {
    events.chunks(MAX_FAST_PATH_INPUT_EVENTS)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReactivationOutcome {
    Continue,
    RetryDisplay,
    Disconnect,
    Terminal,
}

#[derive(Default)]
struct DisplayRetryState {
    pending: bool,
}

impl DisplayRetryState {
    fn pending(&self) -> bool {
        self.pending
    }

    fn publish_graphics(&self) -> bool {
        !self.pending
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReactivationSurfaceDisposition {
    Committed,
    StaleCapability,
}

#[allow(clippy::too_many_arguments)]
fn drive_reactivation(
    session_id: SessionId,
    runtime: &mut ProtocolRuntime,
    writer: &mut OrderedRdpWriter<TlsStream>,
    active_stage: &mut ActiveStage,
    image: &mut DecodedImage,
    baseline: &mut RdpBaseline,
    generation: &mut u64,
    display: &mut DisplayControlAdapter,
    display_capabilities: &DisplayControlCapabilityState,
    activation_factory: &ConnectionActivationFactory,
) -> Result<ReactivationOutcome, ProtocolError> {
    let mut activation = activation_factory.create();
    let mut buffer = WriteBuf::new();
    loop {
        match drain_reactivation_commands(runtime, session_id, *generation) {
            ReactivationCommand::Continue { latest_viewport } => {
                if let Some(viewport) = latest_viewport {
                    display.observe_viewport(viewport);
                }
            }
            ReactivationCommand::Disconnect => return Ok(ReactivationOutcome::Disconnect),
            ReactivationCommand::Terminal => return Ok(ReactivationOutcome::Terminal),
        }

        buffer.clear();
        let written = if let Some(hint) = activation.next_pdu_hint() {
            let payload = match writer.read_by_hint(hint) {
                Ok(payload) => payload,
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    continue;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::UnexpectedEof
                            | ErrorKind::ConnectionAborted
                            | ErrorKind::ConnectionReset
                    ) =>
                {
                    writer.stop_writes();
                    return Ok(ReactivationOutcome::Disconnect);
                }
                Err(_) => return Err(rdp_error(RDP_ACTIVATION_FAILED)),
            };
            activation
                .step(payload.as_ref(), &mut buffer)
                .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?
        } else {
            activation
                .step_no_input(&mut buffer)
                .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?
        };
        if written.size().is_some() {
            writer
                .write_frame(buffer.filled())
                .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?;
        }

        if let ConnectionActivationState::Finalized {
            desktop_size,
            share_id,
            enable_server_pointer,
            pointer_software_rendering,
        } = activation.connection_activation_state()
        {
            let current_max_monitor_area =
                current_display_max_monitor_area(active_stage, display_capabilities);
            let disposition = finalize_reactivated_surface(
                runtime,
                baseline,
                image,
                generation,
                display,
                desktop_size,
                current_max_monitor_area,
            )?;
            if disposition == ReactivationSurfaceDisposition::Committed {
                disable_egfx_for_reactivation(active_stage);
            }
            active_stage.set_fastpath_processor(
                fast_path::ProcessorBuilder {
                    io_channel_id: activation.io_channel_id(),
                    user_channel_id: activation.user_channel_id(),
                    share_id,
                    enable_server_pointer,
                    pointer_software_rendering,
                    bulk_decompressor: None,
                }
                .build(),
            );
            active_stage.set_share_id(share_id);
            active_stage.set_enable_server_pointer(enable_server_pointer);
            return match disposition {
                ReactivationSurfaceDisposition::Committed => Ok(ReactivationOutcome::Continue),
                ReactivationSurfaceDisposition::StaleCapability => {
                    Ok(ReactivationOutcome::RetryDisplay)
                }
            };
        }
    }
}

fn apply_reactivation_outcome(
    outcome: ReactivationOutcome,
    retry: &mut DisplayRetryState,
    runtime: &mut ProtocolRuntime,
    input: &mut RdpInputState,
    published_capabilities: &mut SessionCapabilities,
) -> Result<ReactivationOutcome, ProtocolError> {
    match outcome {
        ReactivationOutcome::Continue => {
            retry.pending = false;
            resume_active_input_capabilities(runtime, input)?;
            *published_capabilities = SessionCapabilities {
                physical_key_input: true,
                text_input: true,
                ..SessionCapabilities::default()
            };
        }
        ReactivationOutcome::RetryDisplay => retry.pending = true,
        ReactivationOutcome::Disconnect | ReactivationOutcome::Terminal => {}
    }
    Ok(outcome)
}

fn send_pending_resize<S, E>(
    display: &mut DisplayControlAdapter,
    writer: &mut OrderedRdpWriter<S>,
    encode: E,
) -> Result<(), ProtocolError>
where
    S: std::io::Write,
    E: FnOnce(PixelSize) -> Result<Vec<u8>, ProtocolError>,
{
    let Some(target) = display.take_resize_request() else {
        return Ok(());
    };
    let frame = encode(target)?;
    writer
        .write_frame(&frame)
        .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))
}

#[allow(clippy::too_many_arguments)]
fn finalize_reactivated_surface(
    runtime: &mut ProtocolRuntime,
    baseline: &mut RdpBaseline,
    image: &mut DecodedImage,
    generation: &mut u64,
    display: &mut DisplayControlAdapter,
    desktop_size: DesktopSize,
    current_max_monitor_area: Option<u64>,
) -> Result<ReactivationSurfaceDisposition, ProtocolError> {
    let observed_size = validate_negotiated_size(desktop_size.width, desktop_size.height)
        .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?;
    match display.confirm_reactivation(observed_size, current_max_monitor_area) {
        ResizeConfirmation::Mismatch => Err(rdp_error(RDP_ACTIVATION_FAILED)),
        ResizeConfirmation::StaleCapability => {
            *image = DecodedImage::new(
                IronPixelFormat::RgbA32,
                desktop_size.width,
                desktop_size.height,
            );
            Ok(ReactivationSurfaceDisposition::StaleCapability)
        }
        ResizeConfirmation::NoRequest | ResizeConfirmation::Confirmed => {
            *generation =
                commit_reactivated_surface(runtime, baseline, image, *generation, desktop_size)?;
            Ok(ReactivationSurfaceDisposition::Committed)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn best_effort_release_held_input(
    runtime: &mut ProtocolRuntime,
    writer: &mut OrderedRdpWriter<TlsStream>,
    active_stage: &mut ActiveStage,
    image: &mut DecodedImage,
    baseline: &mut RdpBaseline,
    input: &mut RdpInputState,
    generation: u64,
) {
    let releases = input.stop();
    if writer.is_writable() {
        let _ = route_input_events(
            active_stage,
            image,
            writer,
            runtime,
            baseline,
            generation,
            &releases,
            false,
        );
    }
}

#[cfg(test)]
fn publish_graphics_update(
    runtime: &mut ProtocolRuntime,
    baseline: &mut RdpBaseline,
    image: &DecodedImage,
    generation: u64,
    region: InclusiveRectangle,
    content: &[InclusiveRectangle],
) -> Result<(), ProtocolError> {
    baseline.publish(runtime, image, generation, region, content)
}

fn publish_active_input_capabilities(runtime: &mut ProtocolRuntime) -> Result<(), ProtocolError> {
    runtime.publish_event(SessionEvent::CapabilitiesChanged(SessionCapabilities {
        physical_key_input: true,
        text_input: true,
        ..SessionCapabilities::default()
    }))
}

fn stop_and_drain_audio(
    runtime: &mut ProtocolRuntime,
    audio: &RdpAudioAdapter,
) -> Result<(), ProtocolError> {
    audio.stop();
    audio.drain_to_runtime(runtime)
}

fn suspend_active_input_capabilities(
    runtime: &mut ProtocolRuntime,
    input: &mut RdpInputState,
) -> Result<Vec<ironrdp::pdu::input::fast_path::FastPathInputEvent>, ProtocolError> {
    let releases = input.stop();
    runtime.publish_event(SessionEvent::CapabilitiesChanged(
        SessionCapabilities::default(),
    ))?;
    Ok(releases)
}

fn resume_active_input_capabilities(
    runtime: &mut ProtocolRuntime,
    input: &mut RdpInputState,
) -> Result<(), ProtocolError> {
    input.resume();
    publish_active_input_capabilities(runtime)
}

fn commit_reactivated_surface(
    runtime: &mut ProtocolRuntime,
    baseline: &mut RdpBaseline,
    image: &mut DecodedImage,
    generation: u64,
    desktop_size: DesktopSize,
) -> Result<u64, ProtocolError> {
    let size = validate_negotiated_size(desktop_size.width, desktop_size.height)
        .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?;
    let next_generation = generation
        .checked_add(1)
        .ok_or(ProtocolError::InvalidGeneration)?;
    let replacement = DecodedImage::new(
        IronPixelFormat::RgbA32,
        desktop_size.width,
        desktop_size.height,
    );
    baseline.begin_next_generation(runtime, next_generation, size)?;
    *image = replacement;
    Ok(next_generation)
}

#[cfg(test)]
mod tests {
    #[allow(unexpected_cfgs)]
    mod legacy_pipeline {
        include!("legacy_pipeline.rs");
    }
    use std::io::{self, Read, Write};
    use std::sync::{mpsc, Arc, Mutex};

    use frd_core::{
        InputEvent, KeyState, Modifiers, PhysicalKeyCode, PhysicalViewport, PixelRect, PixelSize,
        SessionId,
    };
    use frd_frame::{FrameCompleteness, PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate};
    use frd_media_api::{MediaFrame, MediaPublishError, MediaPublisher};
    use frd_protocol_api::{
        ProtocolError, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionCapabilities,
        SessionEvent, SurfacePublisher,
    };
    use ironrdp::connector::DesktopSize;
    use ironrdp::graphics::image_processing::PixelFormat as IronPixelFormat;
    use ironrdp::pdu::geometry::InclusiveRectangle;
    use ironrdp::session::image::DecodedImage;
    use ironrdp::session::ActiveStageOutput;
    use ironrdp_blocking::Framed;

    use crate::audio::RdpAudioAdapter;
    use crate::baseline::RdpBaseline;
    use crate::connector::{baseline_graphics_capabilities, RdpGraphicsCapability};
    use crate::display::DisplayControlAdapter;
    use crate::input::RdpInputState;
    use crate::writer::OrderedRdpWriter;

    use super::{
        apply_egfx_confirmation, apply_reactivation_outcome, commit_reactivated_surface,
        fast_path_input_batches, finalize_reactivated_surface, publish_active_input_capabilities,
        publish_egfx_surface_updates, publish_graphics_update, resume_active_input_capabilities,
        route_active_outputs, send_pending_resize, stop_and_drain_audio,
        suspend_active_input_capabilities, ActiveOutputControl, DisplayRetryState,
        ReactivationOutcome, ReactivationSurfaceDisposition,
    };

    fn rect(left: u16, top: u16, right: u16, bottom: u16) -> InclusiveRectangle {
        InclusiveRectangle {
            left,
            top,
            right,
            bottom,
        }
    }

    fn stage(native: bool, pointer: bool) -> ironrdp::session::ActiveStage {
        let selection = crate::factory::tests::resolved_session_selection(if native {
            crate::RdpLegacyDecodePolicy::RequiredNative
        } else {
            crate::RdpLegacyDecodePolicy::Compatibility
        });
        super::build_active_stage(
            ironrdp::session::ActiveStageBuilder {
                static_channels: ironrdp::svc::StaticChannelSet::new(),
                user_channel_id: 1001,
                io_channel_id: 1003,
                message_channel_id: None,
                share_id: 1,
                compression_type: None,
                enable_server_pointer: pointer,
                pointer_software_rendering: pointer,
            },
            selection,
        )
    }

    fn bitmap(
        wire: &[u8],
        bpp: u16,
        width: u16,
        height: u16,
        rectangle: InclusiveRectangle,
        compressed: bool,
    ) -> ironrdp::pdu::bitmap::BitmapData<'_> {
        use ironrdp::pdu::bitmap::{BitmapData, Compression};
        BitmapData {
            rectangle,
            width,
            height,
            bits_per_pixel: bpp,
            compression_flags: if compressed {
                Compression::BITMAP_COMPRESSION | Compression::NO_BITMAP_COMPRESSION_HDR
            } else {
                Compression::empty()
            },
            compressed_data_header: None,
            bitmap_data: wire,
        }
    }

    fn fast_packet(updates: &[(ironrdp::pdu::fast_path::UpdateCode, Vec<u8>)]) -> Vec<u8> {
        use ironrdp::core::encode_vec;
        use ironrdp::pdu::fast_path::{
            EncryptionFlags, FastPathHeader, FastPathUpdatePdu, Fragmentation,
        };
        let mut payload = Vec::new();
        for (update_code, data) in updates {
            payload.extend(
                encode_vec(&FastPathUpdatePdu {
                    fragmentation: Fragmentation::Single,
                    update_code: *update_code,
                    compression_flags: None,
                    compression_type: None,
                    data,
                })
                .unwrap(),
            );
        }
        let mut frame = encode_vec(&FastPathHeader::new(
            EncryptionFlags::empty(),
            payload.len(),
        ))
        .unwrap();
        frame.extend(payload);
        frame
    }

    fn bitmap_packet(rectangles: Vec<ironrdp::pdu::bitmap::BitmapData<'_>>, slow: bool) -> Vec<u8> {
        use ironrdp::core::encode_vec;
        use ironrdp::pdu::bitmap::BitmapUpdateData;
        let data = encode_vec(&BitmapUpdateData { rectangles }).unwrap();
        if !slow {
            return fast_packet(&[(ironrdp::pdu::fast_path::UpdateCode::Bitmap, data)]);
        }
        // Update 不提供 encoder；Share header 后使用真实 MCS/X224 编码与 Stage 解析。
        let mut user_data = Vec::new();
        user_data.extend(u16::try_from(18 + data.len()).unwrap().to_le_bytes());
        user_data.extend(0x17u16.to_le_bytes());
        user_data.extend(1001u16.to_le_bytes());
        user_data.extend(1u32.to_le_bytes());
        user_data.extend([0, 2]);
        user_data.extend(u16::try_from(data.len()).unwrap().to_le_bytes());
        user_data.extend([2, 0, 0, 0]);
        user_data.extend(data);
        encode_vec(&ironrdp::pdu::x224::X224(
            ironrdp::pdu::mcs::SendDataIndication {
                initiator_id: 1001,
                channel_id: 1003,
                user_data: user_data.into(),
            },
        ))
        .unwrap()
    }

    struct CoverageHarness {
        session_id: SessionId,
        runtime: ProtocolRuntime,
        baseline: RdpBaseline,
        image: DecodedImage,
        stage: ironrdp::session::ActiveStage,
        writer: OrderedRdpWriter<RecordingIo>,
        mailbox: Arc<Mutex<frd_frame::FrameMailbox>>,
        compiler: frd_frame::FrameTransactionCompiler,
    }

    impl CoverageHarness {
        fn new(width: u16, height: u16, native: bool, pointer: bool) -> Self {
            Self::with_entry_limit(width, height, native, pointer, 3)
        }

        fn with_entry_limit(
            width: u16,
            height: u16,
            native: bool,
            pointer: bool,
            limit: usize,
        ) -> Self {
            let session_id = SessionId::allocate();
            let mailbox = Arc::new(Mutex::new(frd_frame::FrameMailbox::new(
                limit,
                usize::from(width) * usize::from(height) * 4,
            )));
            let (_, rx) = mpsc::channel();
            let mut runtime = ProtocolRuntime::new(
                session_id,
                rx,
                Box::new(NoopEvents),
                Box::new(frd_protocol_api::MailboxSurfacePublisher::new(
                    mailbox.clone(),
                )),
                None,
                Box::new(NoopWake),
            );
            let baseline = RdpBaseline::begin(
                &mut runtime,
                session_id,
                PixelSize {
                    width: u32::from(width),
                    height: u32::from(height),
                },
            )
            .unwrap();
            Self {
                session_id,
                runtime,
                baseline,
                image: DecodedImage::new(IronPixelFormat::RgbA32, width, height),
                stage: stage(native, pointer),
                writer: OrderedRdpWriter::new(Framed::new(RecordingIo::default())),
                mailbox,
                compiler: frd_frame::FrameTransactionCompiler::new(session_id),
            }
        }

        fn route(
            &mut self,
            packet: &[u8],
            slow: bool,
            generation: u64,
            publish: bool,
        ) -> Result<ActiveOutputControl, ProtocolError> {
            let batch = super::process_server_batch(
                &mut self.stage,
                &mut self.image,
                generation,
                if slow {
                    ironrdp::pdu::Action::X224
                } else {
                    ironrdp::pdu::Action::FastPath
                },
                packet,
            )?;
            super::route_active_outputs(
                batch,
                &mut self.writer,
                &mut self.runtime,
                &mut self.baseline,
                publish,
            )
        }

        fn compile(&mut self) -> Vec<frd_frame::FrameTransaction> {
            self.compiler
                .compile(self.mailbox.lock().unwrap().drain_committed_updates())
                .unwrap()
        }
    }

    fn transaction_pixels(
        transaction: &frd_frame::FrameTransaction,
        width: usize,
        height: usize,
    ) -> Vec<u8> {
        let revision = match transaction {
            frd_frame::FrameTransaction::Startup { revision, .. }
            | frd_frame::FrameTransaction::Revision { revision, .. } => revision,
        };
        let mut pixels = vec![0; width * height * 4];
        for patch in &revision.patches {
            for y in 0..patch.rect.height as usize {
                let destination = ((patch.rect.y as usize + y) * width + patch.rect.x as usize) * 4;
                let source = y * patch.stride_bytes as usize;
                let bytes = patch.rect.width as usize * 4;
                pixels[destination..destination + bytes]
                    .copy_from_slice(&patch.pixels.as_bytes()[source..source + bytes]);
            }
        }
        pixels
    }

    #[test]
    fn legacy_native_factory_stage_binding_preserves_pending_pointer_on_real_failure() {
        use ironrdp::core::encode_vec;
        use ironrdp::pdu::fast_path::UpdateCode;
        use ironrdp::pdu::pointer::{ColorPointerAttribute, Point16, PointerAttribute};
        for native in [false, true] {
            for replacement in [false, true] {
                for invalid_header in [false, true] {
                    let mut h = CoverageHarness::new(4, 2, native, true);
                    if replacement {
                        // 与真实再激活路径相同的 replacement setter；新的 processor 默认 None。
                        h.stage.set_fastpath_processor(
                            ironrdp::session::fast_path::ProcessorBuilder {
                                io_channel_id: 1003,
                                user_channel_id: 1001,
                                share_id: 2,
                                enable_server_pointer: true,
                                pointer_software_rendering: true,
                                bulk_decompressor: None,
                            }
                            .build(),
                        );
                    }
                    let shape = encode_vec(&PointerAttribute {
                        xor_bpp: 32,
                        color_pointer: ColorPointerAttribute {
                            cache_index: 1,
                            hot_spot: Point16 { x: 0, y: 0 },
                            width: 1,
                            height: 1,
                            xor_mask: &[255, 255, 255, 255],
                            and_mask: &[0, 0],
                        },
                    })
                    .unwrap();
                    h.route(
                        &fast_packet(&[(UpdateCode::NewPointer, shape)]),
                        false,
                        1,
                        true,
                    )
                    .unwrap();
                    h.route(
                        &fast_packet(&[(UpdateCode::CachedPointer, 1u16.to_le_bytes().to_vec())]),
                        false,
                        1,
                        true,
                    )
                    .unwrap();
                    let before = h.image.data().to_vec();
                    assert!(
                        before[..3].iter().any(|&value| value != 0),
                        "先证明实际软件指针可见"
                    );
                    h.stage.update_mouse_pos(2, 0);
                    let failure = if invalid_header {
                        vec![0]
                    } else {
                        bitmap_packet(vec![bitmap(&[], 24, 2, 1, rect(0, 0, 1, 0), true)], false)
                    };
                    assert!(h.route(&failure, false, 1, true).is_err());
                    assert_eq!(h.baseline.known_pixels(), 0);
                    assert!(h.compile().is_empty());
                    if native {
                        assert_eq!(
                            h.image.data(),
                            before,
                            "native 失败前不得消费 pending pointer"
                        );
                    } else {
                        assert_ne!(
                            h.image.data(),
                            before,
                            "默认路径应保留原有先移动 pointer 的行为"
                        );
                    }
                    // 无 scalar 重试：成功空包才消费 native 保留的 pending pointer。
                    let batch = super::process_server_batch(
                        &mut h.stage,
                        &mut h.image,
                        1,
                        ironrdp::pdu::Action::FastPath,
                        &fast_packet(&[]),
                    )
                    .unwrap();
                    assert_eq!(batch.result.decoded_coverage.len(), usize::from(native));
                    assert!(batch
                        .result
                        .decoded_coverage
                        .iter()
                        .all(|r| r.rectangles.is_empty()));
                }
            }
        }
    }

    #[test]
    fn legacy_native_factory_reactivation_starts_empty_and_requires_real_full_content() {
        let mut h = CoverageHarness::new(3, 1, true, false);
        let endpoints = bitmap_packet(
            vec![
                bitmap(&[0x61, 1, 2, 3], 24, 1, 1, rect(0, 0, 0, 0), true),
                bitmap(&[0x61, 4, 5, 6], 24, 1, 1, rect(2, 0, 2, 0), true),
            ],
            false,
        );
        let middle = bitmap_packet(
            vec![bitmap(&[0x61, 7, 8, 9], 24, 1, 1, rect(1, 0, 1, 0), true)],
            false,
        );
        for generation in [1, 2] {
            if generation == 2 {
                assert_eq!(
                    commit_reactivated_surface(
                        &mut h.runtime,
                        &mut h.baseline,
                        &mut h.image,
                        1,
                        DesktopSize {
                            width: 3,
                            height: 1
                        }
                    )
                    .unwrap(),
                    2
                );
                h.stage.set_fastpath_processor(
                    ironrdp::session::fast_path::ProcessorBuilder {
                        io_channel_id: 1003,
                        user_channel_id: 1001,
                        share_id: 2,
                        enable_server_pointer: false,
                        pointer_software_rendering: false,
                        bulk_decompressor: None,
                    }
                    .build(),
                );
                assert_eq!(h.image.data(), [0; 12]);
                assert_eq!(h.baseline.known_pixels(), 0);
                assert!(!h.baseline.baseline_established());
            }
            h.route(&endpoints, false, generation, true).unwrap();
            assert_eq!(h.baseline.known_pixels(), 2);
            assert!(!h.baseline.baseline_established());
            assert!(h.compile().is_empty());
            h.route(&middle, false, generation, true).unwrap();
            let transactions = h.compile();
            assert!(matches!(
                &transactions[..],
                [frd_frame::FrameTransaction::Startup { .. }]
            ));
            assert_eq!(
                transaction_pixels(&transactions[0], 3, 1),
                [1, 2, 3, 255, 7, 8, 9, 255, 4, 5, 6, 255]
            );
        }
    }

    #[test]
    fn decoded_coverage_encoded_fast_slow_endpoints_need_middle_before_mailbox_startup() {
        for native in [false, true] {
            for slow in [false, true] {
                for compressed in [false, true] {
                    for bpp in [24, 32] {
                        let a = match (compressed, bpp) {
                            (true, 24) => vec![0x61, 1, 2, 3],
                            (true, 32) => vec![0x20, 3, 2, 1, 0],
                            _ => vec![1, 2, 3, 0],
                        };
                        let b = match (compressed, bpp) {
                            (true, 24) => vec![0x61, 4, 5, 6],
                            (true, 32) => vec![0x20, 6, 5, 4, 0],
                            _ => vec![4, 5, 6, 0],
                        };
                        let mut h = CoverageHarness::new(3, 1, native, false);
                        let packet = bitmap_packet(
                            vec![
                                bitmap(&a, bpp, 1, 1, rect(0, 0, 0, 0), compressed),
                                bitmap(&b, bpp, 1, 1, rect(2, 0, 2, 0), compressed),
                            ],
                            slow,
                        );
                        let batch = super::process_server_batch(
                            &mut h.stage,
                            &mut h.image,
                            1,
                            if slow {
                                ironrdp::pdu::Action::X224
                            } else {
                                ironrdp::pdu::Action::FastPath
                            },
                            &packet,
                        )
                        .unwrap();
                        assert!(batch.result.outputs.iter().any(|o| matches!(o,
                            ActiveStageOutput::GraphicsUpdate(r) if *r == rect(0, 0, 2, 0))));
                        assert_eq!(
                            batch.result.decoded_coverage[0].rectangles,
                            [rect(0, 0, 0, 0), rect(2, 0, 2, 0)]
                        );
                        assert_eq!(batch.image.data(), [3, 2, 1, 255, 0, 0, 0, 0, 6, 5, 4, 255]);
                        super::route_active_outputs(
                            batch,
                            &mut h.writer,
                            &mut h.runtime,
                            &mut h.baseline,
                            true,
                        )
                        .unwrap();
                        assert_eq!(h.baseline.known_pixels(), 2);
                        assert!(!h.baseline.baseline_established());
                        assert!(h.compile().is_empty(), "全损伤框中的空洞必须阻止 Startup");
                        h.route(
                            &bitmap_packet(
                                vec![bitmap(&a, bpp, 1, 1, rect(1, 0, 1, 0), compressed)],
                                slow,
                            ),
                            slow,
                            1,
                            true,
                        )
                        .unwrap();
                        let transactions = h.compile();
                        assert_eq!(transactions.len(), 1);
                        let frd_frame::FrameTransaction::Startup { revision, .. } =
                            &transactions[0]
                        else {
                            panic!("必须发布完整 Startup");
                        };
                        assert_eq!(revision.completeness, FrameCompleteness::FullBaseline);
                        assert_eq!(
                            transaction_pixels(&transactions[0], 3, 1),
                            [1, 2, 3, 255, 1, 2, 3, 255, 4, 5, 6, 255]
                        );
                        assert!(h.baseline.baseline_established());
                        // 后续合法内容是增量，不能再伪造第二个 FullBaseline。
                        h.route(
                            &bitmap_packet(
                                vec![bitmap(&b, bpp, 1, 1, rect(1, 0, 1, 0), compressed)],
                                false,
                            ),
                            false,
                            1,
                            true,
                        )
                        .unwrap();
                        assert!(
                            matches!(&h.compile()[0], frd_frame::FrameTransaction::Revision { revision, .. }
                            if revision.completeness == FrameCompleteness::Incremental)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn decoded_coverage_complete_raw_packed_rdp6_crop_reaches_real_mailbox() {
        // 独立 source oracle 解码完整 3x3；实际被测 Stage 应取 source 上方 2x2。
        for native in [false, true] {
            for bpp in [8, 15, 16, 24, 32] {
                for compressed in [false, true] {
                    let bytes = usize::from(bpp).div_ceil(8);
                    let tight = 3 * bytes;
                    let stride = (tight + 3) & !3;
                    let mut raw = Vec::new();
                    for y in 0..3u8 {
                        for x in 0..3u8 {
                            for channel in 0..bytes {
                                raw.push(
                                    (7 + usize::from(y) * 43 + usize::from(x) * 11 + channel * 19)
                                        as u8,
                                );
                            }
                        }
                        raw.extend(vec![0xeb; stride - tight]);
                    }
                    let wire = if !compressed {
                        raw
                    } else if bpp != 32 {
                        let mut data = vec![0x89];
                        for row in raw.chunks_exact(stride) {
                            data.extend_from_slice(&row[..tight]);
                        }
                        data
                    } else {
                        let mut data = vec![0x20];
                        for channel in [2usize, 1, 0] {
                            for row in raw.chunks_exact(stride) {
                                for x in 0..3 {
                                    data.push(row[x * 4 + channel]);
                                }
                            }
                        }
                        data.push(0);
                        data
                    };
                    let mut oracle = DecodedImage::new(IronPixelFormat::RgbA32, 3, 3);
                    ironrdp::session::fast_path::ProcessorBuilder {
                        io_channel_id: 1003,
                        user_channel_id: 1001,
                        share_id: 1,
                        enable_server_pointer: false,
                        pointer_software_rendering: false,
                        bulk_decompressor: None,
                    }
                    .build()
                    .process_bitmap_update(
                        &mut oracle,
                        ironrdp::pdu::bitmap::BitmapUpdateData {
                            rectangles: vec![bitmap(
                                &wire,
                                bpp,
                                3,
                                3,
                                rect(0, 0, 2, 2),
                                compressed,
                            )],
                        },
                    )
                    .unwrap();
                    let mut expected = Vec::new();
                    for y in 0..2 {
                        for x in 0..2 {
                            let pixel = &oracle.data()[y * 12 + x * 4..][..4];
                            expected.extend([pixel[2], pixel[1], pixel[0], 255]);
                        }
                    }
                    let mut h = CoverageHarness::new(2, 2, native, false);
                    h.route(
                        &bitmap_packet(
                            vec![bitmap(&wire, bpp, 3, 3, rect(0, 0, 1, 1), compressed)],
                            false,
                        ),
                        false,
                        1,
                        true,
                    )
                    .unwrap();
                    assert_eq!(h.baseline.known_pixels(), 4);
                    let transactions = h.compile();
                    assert_eq!(transactions.len(), 1);
                    assert_eq!(transaction_pixels(&transactions[0], 2, 2), expected,
                        "source stride/height crop native={native} bpp={bpp} compressed={compressed}");
                }
            }
        }
    }

    #[test]
    fn decoded_coverage_batch_rejects_bad_associations_and_late_regions_before_effects() {
        use ironrdp::core::encode_vec;
        use ironrdp::pdu::fast_path::UpdateCode;
        use ironrdp::pdu::surface_commands::{FrameAction, FrameMarkerPdu, SurfaceCommand};
        let bitmap_data = encode_vec(&ironrdp::pdu::bitmap::BitmapUpdateData {
            rectangles: vec![bitmap(&[1, 2, 3, 0], 24, 1, 1, rect(0, 0, 0, 0), false)],
        })
        .unwrap();
        let middle = encode_vec(&ironrdp::pdu::bitmap::BitmapUpdateData {
            rectangles: vec![bitmap(&[4, 5, 6, 0], 24, 1, 1, rect(1, 0, 1, 0), false)],
        })
        .unwrap();
        let marker = encode_vec(&SurfaceCommand::FrameMarker(FrameMarkerPdu {
            frame_action: FrameAction::End,
            frame_id: Some(37),
        }))
        .unwrap();
        let packet = fast_packet(&[
            (UpdateCode::Bitmap, bitmap_data),
            (UpdateCode::SurfaceCommands, marker),
            (UpdateCode::Bitmap, middle),
        ]);
        for fault in [
            "missing",
            "duplicate",
            "wrong_type",
            "orphan",
            "index_range",
            "late_region",
            "reverse",
            "outside_damage",
            "budget",
            "generation",
            "image",
        ] {
            let mut h = CoverageHarness::new(3, 1, false, false);
            if fault == "image" {
                h.image = DecodedImage::new(IronPixelFormat::RgbA32, 4, 1);
            }
            let mut batch = super::process_server_batch(
                &mut h.stage,
                &mut h.image,
                1,
                ironrdp::pdu::Action::FastPath,
                &packet,
            )
            .unwrap();
            assert!(matches!(
                batch.result.outputs[0],
                ActiveStageOutput::ResponseFrame(_)
            ));
            match fault {
                "missing" => {
                    batch.result.decoded_coverage.pop();
                }
                "duplicate" => {
                    batch
                        .result
                        .decoded_coverage
                        .push(batch.result.decoded_coverage[0].clone());
                }
                "wrong_type" => batch.result.decoded_coverage[0].graphics_output_index = 0,
                "orphan" => {
                    batch.result.outputs.pop();
                }
                "index_range" => {
                    batch.result.decoded_coverage[0].graphics_output_index = usize::MAX
                }
                "late_region" => batch
                    .result
                    .decoded_coverage
                    .last_mut()
                    .unwrap()
                    .rectangles
                    .push(rect(2, 0, 3, 0)),
                "reverse" => batch
                    .result
                    .decoded_coverage
                    .last_mut()
                    .unwrap()
                    .rectangles
                    .push(rect(2, 0, 1, 0)),
                "outside_damage" => batch.result.decoded_coverage[0]
                    .rectangles
                    .push(rect(2, 0, 2, 0)),
                "budget" => {
                    batch.result.decoded_coverage[0].rectangles =
                        vec![rect(0, 0, 0, 0); super::MAX_DECODED_COVERAGE_ENTRIES + 1]
                }
                "generation" => batch.source_generation = 2,
                "image" => {}
                _ => unreachable!(),
            }
            assert!(
                super::route_active_outputs(
                    batch,
                    &mut h.writer,
                    &mut h.runtime,
                    &mut h.baseline,
                    true
                )
                .is_err(),
                "{fault}"
            );
            assert_eq!(h.baseline.known_pixels(), 0, "无有效前缀记账：{fault}");
            assert!(!h.baseline.baseline_established());
            assert_eq!(h.mailbox.lock().unwrap().len(), 1);
            assert!(
                h.writer
                    .into_framed()
                    .into_inner_no_leftover()
                    .written
                    .is_empty(),
                "校验必须早于响应：{fault}"
            );
        }
        // receipt 可乱序，但显式索引必须将实际内容映射到对应 GraphicsUpdate。
        let mut h = CoverageHarness::new(3, 1, false, false);
        let mut batch = super::process_server_batch(
            &mut h.stage,
            &mut h.image,
            1,
            ironrdp::pdu::Action::FastPath,
            &packet,
        )
        .unwrap();
        batch.result.decoded_coverage.reverse();
        let expected_response: Vec<_> = batch
            .result
            .outputs
            .iter()
            .filter_map(|o| {
                if let ActiveStageOutput::ResponseFrame(frame) = o {
                    Some(frame.as_slice())
                } else {
                    None
                }
            })
            .flatten()
            .copied()
            .collect();
        super::route_active_outputs(batch, &mut h.writer, &mut h.runtime, &mut h.baseline, true)
            .unwrap();
        assert_eq!(h.baseline.known_pixels(), 2);
        assert_eq!(
            h.writer.into_framed().into_inner_no_leftover().written,
            expected_response
        );
    }

    #[test]
    fn decoded_coverage_failed_and_underfilled_updates_do_not_overwrite_known_pixels() {
        for native in [false, true] {
            let mut h = CoverageHarness::new(3, 1, native, false);
            h.route(
                &bitmap_packet(
                    vec![bitmap(&[1, 2, 3, 0], 24, 1, 1, rect(0, 0, 0, 0), false)],
                    false,
                ),
                false,
                1,
                true,
            )
            .unwrap();
            let original = h.image.data().to_vec();
            for wire in [&[][..], &[0x61, 9, 8, 7][..]] {
                assert!(h
                    .route(
                        &bitmap_packet(vec![bitmap(wire, 24, 2, 1, rect(0, 0, 1, 0), true)], false),
                        false,
                        1,
                        true
                    )
                    .is_err());
                assert_eq!(h.image.data(), original);
                assert_eq!(h.baseline.known_pixels(), 1);
                assert!(!h.baseline.baseline_established());
            }
        }
    }

    #[test]
    fn decoded_coverage_stale_generation_and_same_size_reactivation_reset_knowledge() {
        let mut h = CoverageHarness::new(3, 1, false, false);
        h.route(
            &bitmap_packet(
                vec![bitmap(&[1, 2, 3, 0], 24, 1, 1, rect(0, 0, 0, 0), false)],
                false,
            ),
            false,
            1,
            true,
        )
        .unwrap();
        assert_eq!(h.baseline.known_pixels(), 1);
        let generation = commit_reactivated_surface(
            &mut h.runtime,
            &mut h.baseline,
            &mut h.image,
            1,
            DesktopSize {
                width: 3,
                height: 1,
            },
        )
        .unwrap();
        assert_eq!(generation, 2);
        assert_eq!(h.baseline.known_pixels(), 0);
        assert_eq!(h.image.data(), [0; 12]);
        let packet = bitmap_packet(
            vec![bitmap(&[1, 2, 3, 0], 24, 1, 1, rect(0, 0, 0, 0), false)],
            false,
        );
        assert_eq!(
            h.route(&packet, false, 1, true),
            Err(ProtocolError::StaleSurface)
        );
        assert_eq!(h.baseline.known_pixels(), 0);
        h.route(&packet, false, 2, true).unwrap();
        assert_eq!(h.baseline.known_pixels(), 1);
        assert!(!h.baseline.baseline_established());
        assert!(h.compile().is_empty());
    }

    #[test]
    fn decoded_coverage_stale_display_discards_real_content_and_preserves_controls() {
        let mut h = CoverageHarness::new(3, 1, false, false);
        // same-generation 临时 replacement 的完整内容不得累计到最终 surface。
        h.image = DecodedImage::new(IronPixelFormat::RgbA32, 4, 1);
        let wire = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0, 10, 11, 12, 0];
        let packet = bitmap_packet(
            vec![bitmap(&wire, 32, 4, 1, rect(0, 0, 3, 0), false)],
            false,
        );
        let mut batch = super::process_server_batch(
            &mut h.stage,
            &mut h.image,
            1,
            ironrdp::pdu::Action::FastPath,
            &packet,
        )
        .unwrap();
        batch
            .result
            .outputs
            .insert(0, ActiveStageOutput::ResponseFrame(b"before".to_vec()));
        for receipt in &mut batch.result.decoded_coverage {
            receipt.graphics_output_index += 1;
        }
        batch.result.outputs.push(ActiveStageOutput::DeactivateAll);
        batch
            .result
            .outputs
            .push(ActiveStageOutput::ResponseFrame(b"after".to_vec()));
        assert_eq!(
            super::route_active_outputs(
                batch,
                &mut h.writer,
                &mut h.runtime,
                &mut h.baseline,
                false
            )
            .unwrap(),
            ActiveOutputControl::Deactivate
        );
        assert_eq!(h.baseline.known_pixels(), 0);
        assert!(!h.baseline.baseline_established());
        assert_eq!(h.mailbox.lock().unwrap().len(), 1);
        assert_eq!(
            h.writer.into_framed().into_inner_no_leftover().written,
            b"beforeafter"
        );
    }

    #[test]
    fn decoded_coverage_cursor_and_local_input_damage_never_create_content() {
        use ironrdp::core::encode_vec;
        use ironrdp::pdu::fast_path::UpdateCode;
        use ironrdp::pdu::input::{
            fast_path::FastPathInputEvent,
            mouse::{MousePdu, PointerFlags},
        };
        use ironrdp::pdu::pointer::{ColorPointerAttribute, Point16, PointerAttribute};
        for native in [false, true] {
            let mut h = CoverageHarness::new(4, 2, native, true);
            let shape = encode_vec(&PointerAttribute {
                xor_bpp: 32,
                color_pointer: ColorPointerAttribute {
                    cache_index: 1,
                    hot_spot: Point16 { x: 0, y: 0 },
                    width: 1,
                    height: 1,
                    xor_mask: &[3, 2, 1, 255],
                    and_mask: &[0, 0],
                },
            })
            .unwrap();
            h.route(
                &fast_packet(&[(UpdateCode::NewPointer, shape)]),
                false,
                1,
                true,
            )
            .unwrap();
            let mouse = |x, y| {
                [FastPathInputEvent::MouseEvent(MousePdu {
                    flags: PointerFlags::MOVE,
                    number_of_wheel_rotation_units: 0,
                    x_position: x,
                    y_position: y,
                })]
            };
            let batch =
                super::process_input_batch(&mut h.stage, &mut h.image, 1, &mouse(1, 0)).unwrap();
            assert!(batch
                .result
                .decoded_coverage
                .iter()
                .any(|r| r.rectangles.is_empty()));
            let expected_response: Vec<_> = batch
                .result
                .outputs
                .iter()
                .filter_map(|o| {
                    if let ActiveStageOutput::ResponseFrame(data) = o {
                        Some(data.as_slice())
                    } else {
                        None
                    }
                })
                .flatten()
                .copied()
                .collect();
            super::route_active_outputs(
                batch,
                &mut h.writer,
                &mut h.runtime,
                &mut h.baseline,
                true,
            )
            .unwrap();
            assert_eq!(h.baseline.known_pixels(), 0);
            assert!(h.compile().is_empty());
            let packet = bitmap_packet(
                vec![bitmap(&[1, 2, 3, 0], 24, 1, 1, rect(0, 0, 0, 0), false)],
                false,
            );
            h.stage.update_mouse_pos(2, 0);
            let batch = super::process_server_batch(
                &mut h.stage,
                &mut h.image,
                1,
                ironrdp::pdu::Action::FastPath,
                &packet,
            )
            .unwrap();
            assert!(batch.result.outputs.iter().any(
                |o| matches!(o, ActiveStageOutput::GraphicsUpdate(r) if *r != rect(0, 0, 0, 0))
            ));
            super::route_active_outputs(
                batch,
                &mut h.writer,
                &mut h.runtime,
                &mut h.baseline,
                true,
            )
            .unwrap();
            assert_eq!(h.baseline.known_pixels(), 1, "光标扩大 damage 不贡献内容");
            assert!(h.compile().is_empty());
            let wire = vec![11u8; 32];
            h.route(
                &bitmap_packet(
                    vec![bitmap(&wire, 32, 4, 2, rect(0, 0, 3, 1), false)],
                    false,
                ),
                false,
                1,
                true,
            )
            .unwrap();
            assert_eq!(h.compile().len(), 1);
            let batch =
                super::process_input_batch(&mut h.stage, &mut h.image, 1, &mouse(3, 1)).unwrap();
            assert!(batch
                .result
                .decoded_coverage
                .iter()
                .all(|r| r.rectangles.is_empty()));
            super::route_active_outputs(
                batch,
                &mut h.writer,
                &mut h.runtime,
                &mut h.baseline,
                true,
            )
            .unwrap();
            let transactions = h.compile();
            assert!(
                matches!(&transactions[0], frd_frame::FrameTransaction::Revision { revision, .. }
                if revision.completeness == FrameCompleteness::Incremental)
            );
            assert_eq!(h.baseline.known_pixels(), 8);
            assert!(h
                .writer
                .into_framed()
                .into_inner_no_leftover()
                .written
                .starts_with(&expected_response));
        }
    }

    #[test]
    fn decoded_coverage_noop_sentinel_is_empty_but_actual_origin_pixel_is_content() {
        use ironrdp::core::encode_vec;
        use ironrdp::pdu::surface_commands::{FrameAction, FrameMarkerPdu, SurfaceCommand};
        let marker = fast_packet(&[(
            ironrdp::pdu::fast_path::UpdateCode::SurfaceCommands,
            encode_vec(&SurfaceCommand::FrameMarker(FrameMarkerPdu {
                frame_action: FrameAction::End,
                frame_id: Some(7),
            }))
            .unwrap(),
        )]);
        let mut h = CoverageHarness::new(1, 1, false, false);
        let batch = super::process_server_batch(
            &mut h.stage,
            &mut h.image,
            1,
            ironrdp::pdu::Action::FastPath,
            &marker,
        )
        .unwrap();
        assert!(batch
            .result
            .outputs
            .iter()
            .any(|o| matches!(o, ActiveStageOutput::GraphicsUpdate(r) if *r == rect(0, 0, 0, 0))));
        assert!(batch
            .result
            .decoded_coverage
            .iter()
            .all(|r| r.rectangles.is_empty()));
        super::route_active_outputs(batch, &mut h.writer, &mut h.runtime, &mut h.baseline, true)
            .unwrap();
        assert_eq!(h.baseline.known_pixels(), 0);
        assert!(h.compile().is_empty());
        h.route(
            &bitmap_packet(
                vec![bitmap(&[9, 8, 7, 0], 24, 1, 1, rect(0, 0, 0, 0), false)],
                false,
            ),
            false,
            1,
            true,
        )
        .unwrap();
        let transaction = h.compile();
        assert_eq!(h.baseline.known_pixels(), 1);
        assert_eq!(transaction_pixels(&transaction[0], 1, 1), [9, 8, 7, 255]);
        h.route(&marker, false, 1, true).unwrap();
        assert!(
            matches!(&h.compile()[0], frd_frame::FrameTransaction::Revision { revision, .. }
            if revision.completeness == FrameCompleteness::Incremental)
        );
        assert_eq!(h.baseline.known_pixels(), 1);
    }

    #[test]
    fn decoded_coverage_real_queue_retry_failure_keeps_knowledge_then_compiles_full_retry() {
        let mut h = CoverageHarness::with_entry_limit(3, 1, false, false, 2);
        let wire = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0];
        let packet = bitmap_packet(
            vec![bitmap(&wire, 32, 3, 1, rect(0, 0, 2, 0), false)],
            false,
        );
        assert_eq!(
            h.route(&packet, false, 1, true),
            Err(ProtocolError::NeedsFullSnapshot)
        );
        assert_eq!(
            h.baseline.known_pixels(),
            3,
            "只丢失呈现队列，内容知识仍然完整"
        );
        assert!(!h.baseline.baseline_established());
        assert_eq!(
            h.mailbox.lock().unwrap().len(),
            1,
            "两次快照均在 boundary 溢出并删除前缀"
        );
        // 再次输入仍然只能尝试完整恢复，不能从丢失的 presentation 基线发送增量。
        assert_eq!(
            h.route(
                &bitmap_packet(
                    vec![bitmap(&[9, 8, 7, 0], 24, 1, 1, rect(1, 0, 1, 0), false)],
                    false
                ),
                false,
                1,
                true
            ),
            Err(ProtocolError::NeedsFullSnapshot)
        );
        assert!(!h.baseline.baseline_established());
        assert!(
            h.compile().is_empty(),
            "先让真实 compiler 保留 Reset 并腾出队列空间"
        );
        h.route(
            &fast_packet(&[(
                ironrdp::pdu::fast_path::UpdateCode::SurfaceCommands,
                ironrdp::core::encode_vec(
                    &ironrdp::pdu::surface_commands::SurfaceCommand::FrameMarker(
                        ironrdp::pdu::surface_commands::FrameMarkerPdu {
                            frame_action: ironrdp::pdu::surface_commands::FrameAction::End,
                            frame_id: Some(8),
                        },
                    ),
                )
                .unwrap(),
            )]),
            false,
            1,
            true,
        )
        .unwrap();
        let transactions = h.compile();
        assert_eq!(transactions.len(), 1);
        assert!(
            matches!(&transactions[0], frd_frame::FrameTransaction::Startup { revision, .. }
            if revision.completeness == FrameCompleteness::FullBaseline && revision.revision == 5)
        );
        assert_eq!(
            transaction_pixels(&transactions[0], 3, 1),
            [1, 2, 3, 255, 9, 8, 7, 255, 7, 8, 9, 255]
        );
        assert!(h.baseline.baseline_established());
    }

    #[test]
    fn decoded_coverage_complete_image_survives_real_incremental_queue_overflow() {
        let mut h = CoverageHarness::new(3, 1, false, false);
        let wire = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0];
        h.route(
            &bitmap_packet(
                vec![bitmap(&wire, 32, 3, 1, rect(0, 0, 2, 0), false)],
                false,
            ),
            false,
            1,
            true,
        )
        .unwrap();
        assert_eq!(h.mailbox.lock().unwrap().len(), 3);
        // Startup 未消费时再来一次 increment：真正的 FrameMailbox 清空旧事务并要求快照。
        h.route(
            &bitmap_packet(
                vec![bitmap(&[9, 8, 7, 0], 24, 1, 1, rect(1, 0, 1, 0), false)],
                false,
            ),
            false,
            1,
            true,
        )
        .unwrap();
        let transactions = h.compile();
        assert_eq!(transactions.len(), 1);
        assert!(
            matches!(&transactions[0], frd_frame::FrameTransaction::Startup { revision, .. }
            if revision.completeness == FrameCompleteness::FullBaseline && revision.revision == 3)
        );
        assert_eq!(
            transaction_pixels(&transactions[0], 3, 1),
            [1, 2, 3, 255, 9, 8, 7, 255, 7, 8, 9, 255]
        );
        assert_eq!(h.baseline.known_pixels(), 3);
    }

    #[test]
    fn decoded_coverage_egfx_reset_and_same_generation_frames_cannot_seed_legacy_image() {
        let mut h = CoverageHarness::new(3, 1, false, false);
        let mut generation = 1;
        h.route(
            &bitmap_packet(
                vec![bitmap(&[1, 2, 3, 0], 24, 1, 1, rect(0, 0, 0, 0), false)],
                false,
            ),
            false,
            1,
            true,
        )
        .unwrap();
        let session_id = h.session_id;
        let size = PixelSize {
            width: 3,
            height: 1,
        };
        let egfx_frame = |generation| SurfaceUpdate::Damage {
            session_id,
            generation,
            revision: 1,
            patches: vec![PixelPatch {
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 3,
                    height: 1,
                },
                stride_bytes: 12,
                pixels: PixelBuffer::new(vec![17; 12]),
            }],
        };
        publish_egfx_surface_updates(
            &mut h.runtime,
            &mut h.baseline,
            session_id,
            &mut generation,
            vec![
                egfx_frame(1),
                SurfaceUpdate::FrameBoundary {
                    session_id,
                    generation: 1,
                    revision: 1,
                    completeness: FrameCompleteness::FullBaseline,
                },
            ],
        )
        .unwrap();
        assert_eq!(
            h.baseline.known_pixels(),
            1,
            "同世代 EGFX 不能填补 legacy image"
        );
        assert!(!h.baseline.baseline_established());
        h.compile();
        publish_egfx_surface_updates(
            &mut h.runtime,
            &mut h.baseline,
            session_id,
            &mut generation,
            vec![
                SurfaceUpdate::Reset {
                    session_id,
                    generation: 2,
                    size,
                    format: PixelFormat::Bgrx8UnormSrgb,
                },
                egfx_frame(2),
                SurfaceUpdate::FrameBoundary {
                    session_id,
                    generation: 2,
                    revision: 1,
                    completeness: FrameCompleteness::FullBaseline,
                },
            ],
        )
        .unwrap();
        assert_eq!(generation, 2);
        assert_eq!(h.baseline.known_pixels(), 0);
        assert!(!h.baseline.baseline_established());
        h.compile();
        h.route(
            &bitmap_packet(
                vec![bitmap(&[4, 5, 6, 0], 24, 1, 1, rect(1, 0, 1, 0), false)],
                false,
            ),
            false,
            2,
            true,
        )
        .unwrap();
        assert_eq!(h.baseline.known_pixels(), 1);
        assert!(h.compile().is_empty());
        h.route(
            &bitmap_packet(
                vec![
                    bitmap(&[1, 2, 3, 0], 24, 1, 1, rect(0, 0, 0, 0), false),
                    bitmap(&[7, 8, 9, 0], 24, 1, 1, rect(2, 0, 2, 0), false),
                ],
                false,
            ),
            false,
            2,
            true,
        )
        .unwrap();
        let transactions = h.compile();
        assert_eq!(transactions.len(), 1);
        assert!(
            matches!(&transactions[0], frd_frame::FrameTransaction::Revision { revision, .. }
            if revision.generation == 2 && revision.revision == 2
                && revision.completeness == FrameCompleteness::FullBaseline)
        );
        assert_eq!(
            transaction_pixels(&transactions[0], 3, 1),
            [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255]
        );
        assert!(h.baseline.baseline_established());
    }

    fn disjoint_rfx_packet() -> Vec<u8> {
        use ironrdp::core::{decode_cursor, encode_vec, ReadCursor};
        use ironrdp::pdu::codecs::rfx::{Block, CodecChannel, RfxRectangle};
        let frozen = include_bytes!("../../../third_party/ironrdp-session-0.11.0/tests/fixtures/rfx-original-supplemental.bin");
        let mut records = &frozen[12..];
        let mut source = None;
        while !records.is_empty() {
            let fields: [&[u8]; 6] = std::array::from_fn(|_| {
                let count = u32::from_le_bytes(records[..4].try_into().unwrap()) as usize;
                let field = &records[4..4 + count];
                records = &records[4 + count..];
                field
            });
            if fields[0] == b"session-1-two-success" {
                source = Some(fields[2]);
                break;
            }
        }
        let mut input = ReadCursor::new(source.expect("独立原始 RFX 双 tile fixture"));
        let mut wire = Vec::new();
        while !input.is_empty() {
            let mut block: Block<'_> = decode_cursor(&mut input).unwrap();
            match &mut block {
                Block::CodecChannel(CodecChannel::Region(region)) => {
                    region.rectangles = vec![
                        RfxRectangle {
                            x: 0,
                            y: 0,
                            width: 1,
                            height: 64,
                        },
                        RfxRectangle {
                            x: 127,
                            y: 0,
                            width: 1,
                            height: 64,
                        },
                    ];
                }
                Block::CodecChannel(CodecChannel::TileSet(tiles)) => {
                    for (i, tile) in tiles.tiles.iter_mut().enumerate() {
                        tile.x = i as u16;
                        tile.y = 0;
                    }
                }
                Block::Channels(channels) => {
                    channels.0[0].width = 128;
                    channels.0[0].height = 64;
                }
                _ => {}
            }
            wire.extend(encode_vec(&block).unwrap());
        }
        let bits = ironrdp::pdu::surface_commands::SurfaceBitsPdu {
            destination: ironrdp::pdu::geometry::ExclusiveRectangle {
                left: 0,
                top: 0,
                right: 128,
                bottom: 64,
            },
            extended_bitmap_data: ironrdp::pdu::surface_commands::ExtendedBitmapDataPdu {
                bpp: 32,
                codec_id: 3,
                width: 128,
                height: 64,
                header: None,
                data: &wire,
            },
        };
        fast_packet(&[(
            ironrdp::pdu::fast_path::UpdateCode::SurfaceCommands,
            encode_vec(&ironrdp::pdu::surface_commands::SurfaceCommand::SetSurfaceBits(bits))
                .unwrap(),
        )])
    }

    #[test]
    fn decoded_coverage_real_disjoint_rfx_bbox_cannot_establish_before_middle_content() {
        for native in [false, true] {
            let mut h = CoverageHarness::new(128, 64, native, false);
            let batch = super::process_server_batch(
                &mut h.stage,
                &mut h.image,
                1,
                ironrdp::pdu::Action::FastPath,
                &disjoint_rfx_packet(),
            )
            .unwrap();
            assert!(batch
                .result
                .outputs
                .iter()
                .any(|o| matches!(o, ActiveStageOutput::GraphicsUpdate(r)
                if *r == rect(0, 0, 127, 63))));
            for y in 0..64 {
                for x in 0..128 {
                    let pixel = &batch.image.data()[(y * 128 + x) * 4..][..4];
                    assert_eq!(
                        pixel != [0; 4],
                        x == 0 || x == 127,
                        "实际 RFX writer 只能写到已到达 clip x={x} y={y}"
                    );
                }
            }
            super::route_active_outputs(
                batch,
                &mut h.writer,
                &mut h.runtime,
                &mut h.baseline,
                true,
            )
            .unwrap();
            assert_eq!(h.baseline.known_pixels(), 128);
            assert!(!h.baseline.baseline_established());
            assert!(h.compile().is_empty());
            let before = h.image.data().to_vec();
            let middle = vec![53; 126 * 64 * 4];
            h.route(
                &bitmap_packet(
                    vec![bitmap(&middle, 32, 126, 64, rect(1, 0, 126, 63), false)],
                    false,
                ),
                false,
                1,
                true,
            )
            .unwrap();
            let transactions = h.compile();
            assert_eq!(transactions.len(), 1);
            let actual = transaction_pixels(&transactions[0], 128, 64);
            for y in 0..64 {
                for x in 0..128 {
                    let offset = (y * 128 + x) * 4;
                    let expected = if x == 0 || x == 127 {
                        vec![before[offset + 2], before[offset + 1], before[offset], 255]
                    } else {
                        vec![53, 53, 53, 255]
                    };
                    assert_eq!(&actual[offset..offset + 4], expected);
                }
            }
        }
    }

    #[test]
    fn decoded_coverage_captured_generation_is_checked_when_tracker_changes_during_borrow() {
        let mut h = CoverageHarness::new(3, 1, false, false);
        let image_identity = &h.image as *const DecodedImage;
        let packet = bitmap_packet(
            vec![bitmap(&[1, 2, 3, 0], 24, 1, 1, rect(0, 0, 0, 0), false)],
            false,
        );
        let batch = super::process_server_batch(
            &mut h.stage,
            &mut h.image,
            1,
            ironrdp::pdu::Action::FastPath,
            &packet,
        )
        .unwrap();
        assert!(
            std::ptr::eq(batch.image, image_identity),
            "borrow 必须绑定实际被解码图像"
        );
        h.baseline
            .begin_next_generation(
                &mut h.runtime,
                2,
                PixelSize {
                    width: 3,
                    height: 1,
                },
            )
            .unwrap();
        assert_eq!(
            super::route_active_outputs(
                batch,
                &mut h.writer,
                &mut h.runtime,
                &mut h.baseline,
                true
            ),
            Err(ProtocolError::StaleSurface)
        );
        assert_eq!(h.baseline.known_pixels(), 0);
        assert!(!h.baseline.baseline_established());
    }

    #[test]
    fn decoded_coverage_stage_error_retains_prior_image_writes_without_publishing_metadata() {
        let mut h = CoverageHarness::new(3, 1, false, false);
        let packet = bitmap_packet(
            vec![
                bitmap(&[1, 2, 3, 0], 24, 1, 1, rect(0, 0, 0, 0), false),
                bitmap(&[0x61, 9, 8, 7], 24, 2, 1, rect(1, 0, 2, 0), true),
            ],
            false,
        );
        assert!(h.route(&packet, false, 1, true).is_err());
        assert_eq!(&h.image.data()[..4], [3, 2, 1, 255], "不声称整个 PDU 回滚");
        assert_eq!(&h.image.data()[4..], [0; 8]);
        assert_eq!(h.baseline.known_pixels(), 0);
        assert_eq!(h.mailbox.lock().unwrap().len(), 1);
    }

    #[test]
    fn decoded_coverage_same_size_stale_display_content_cannot_seed_final_image() {
        let mut h = CoverageHarness::new(3, 1, false, false);
        h.image = DecodedImage::new(IronPixelFormat::RgbA32, 3, 1);
        let wire = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0];
        h.route(
            &bitmap_packet(
                vec![bitmap(&wire, 32, 3, 1, rect(0, 0, 2, 0), false)],
                false,
            ),
            false,
            1,
            false,
        )
        .unwrap();
        assert_eq!(h.baseline.known_pixels(), 0);
        assert!(!h.baseline.baseline_established());
        assert!(h.compile().is_empty());
        let generation = commit_reactivated_surface(
            &mut h.runtime,
            &mut h.baseline,
            &mut h.image,
            1,
            DesktopSize {
                width: 3,
                height: 1,
            },
        )
        .unwrap();
        h.route(
            &bitmap_packet(
                vec![bitmap(&[9, 8, 7, 0], 24, 1, 1, rect(1, 0, 1, 0), false)],
                false,
            ),
            false,
            generation,
            true,
        )
        .unwrap();
        assert_eq!(h.baseline.known_pixels(), 1);
        assert!(!h.baseline.baseline_established());
        assert!(h.compile().is_empty());
    }

    #[test]
    fn decoded_coverage_accepted_external_revision_requires_new_full_legacy_presentation() {
        let mut h = CoverageHarness::new(3, 1, false, false);
        let wire = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0];
        h.route(
            &bitmap_packet(
                vec![bitmap(&wire, 32, 3, 1, rect(0, 0, 2, 0), false)],
                false,
            ),
            false,
            1,
            true,
        )
        .unwrap();
        h.compile();
        assert!(h.baseline.baseline_established());
        let mut generation = 1;
        let session_id = h.session_id;
        publish_egfx_surface_updates(
            &mut h.runtime,
            &mut h.baseline,
            session_id,
            &mut generation,
            vec![
                SurfaceUpdate::Damage {
                    session_id,
                    generation: 1,
                    revision: 5,
                    patches: vec![PixelPatch {
                        rect: PixelRect {
                            x: 0,
                            y: 0,
                            width: 3,
                            height: 1,
                        },
                        stride_bytes: 12,
                        pixels: PixelBuffer::new(vec![17; 12]),
                    }],
                },
                SurfaceUpdate::FrameBoundary {
                    session_id,
                    generation: 1,
                    revision: 5,
                    completeness: FrameCompleteness::FullBaseline,
                },
            ],
        )
        .unwrap();
        assert!(!h.baseline.baseline_established());
        assert_eq!(h.baseline.known_pixels(), 3);
        assert_eq!(h.baseline.presentation_revision(), 5);
        h.compile();
        h.route(
            &bitmap_packet(
                vec![bitmap(&[9, 8, 7, 0], 24, 1, 1, rect(1, 0, 1, 0), false)],
                false,
            ),
            false,
            1,
            true,
        )
        .unwrap();
        let transactions = h.compile();
        assert_eq!(transactions.len(), 1);
        assert!(
            matches!(&transactions[0], frd_frame::FrameTransaction::Revision { revision, .. }
            if revision.revision == 6 && revision.completeness == FrameCompleteness::FullBaseline)
        );
        assert_eq!(
            transaction_pixels(&transactions[0], 3, 1),
            [1, 2, 3, 255, 9, 8, 7, 255, 7, 8, 9, 255]
        );
        assert!(h.baseline.baseline_established());
        assert_eq!(h.baseline.known_pixels(), 3);
    }

    #[test]
    fn decoded_coverage_failed_or_stale_external_publication_does_not_change_baseline() {
        for fault in [
            "duplicate_revision",
            "stale_generation",
            "stale_session",
            "bad_patch",
        ] {
            let mut h = CoverageHarness::new(3, 1, false, false);
            let wire = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0];
            h.route(
                &bitmap_packet(
                    vec![bitmap(&wire, 32, 3, 1, rect(0, 0, 2, 0), false)],
                    false,
                ),
                false,
                1,
                true,
            )
            .unwrap();
            h.compile();
            let mut generation = 1;
            let session_id = if fault == "stale_session" {
                SessionId::allocate()
            } else {
                h.session_id
            };
            let update = SurfaceUpdate::Damage {
                session_id,
                generation: if fault == "stale_generation" { 2 } else { 1 },
                revision: if fault == "duplicate_revision" { 1 } else { 9 },
                patches: vec![PixelPatch {
                    rect: PixelRect {
                        x: if fault == "bad_patch" { 3 } else { 0 },
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    stride_bytes: 4,
                    pixels: PixelBuffer::new(vec![17; 4]),
                }],
            };
            assert!(
                publish_egfx_surface_updates(
                    &mut h.runtime,
                    &mut h.baseline,
                    h.session_id,
                    &mut generation,
                    vec![
                        update,
                        SurfaceUpdate::FrameBoundary {
                            session_id,
                            generation: if fault == "stale_generation" { 2 } else { 1 },
                            revision: if fault == "duplicate_revision" { 1 } else { 9 },
                            completeness: FrameCompleteness::FullBaseline,
                        }
                    ]
                )
                .is_err(),
                "{fault}"
            );
            assert_eq!(h.baseline.known_pixels(), 3);
            assert_eq!(
                h.baseline.presentation_revision(),
                1,
                "observer 只记录已接受的 revision：{fault}"
            );
            assert!(
                h.baseline.baseline_established(),
                "失败的 foreign 呈现未接管：{fault}"
            );
        }
    }

    #[test]
    fn decoded_coverage_external_batch_requires_closed_pairs_before_any_publication() {
        for fault in [
            "unclosed",
            "orphan_boundary",
            "reset_inside",
            "second_damage",
            "wrong_revision",
            "valid_prefix_bad_tail",
            "reset_bad_tail",
            "wrong_session",
            "wrong_generation",
        ] {
            let mut h = CoverageHarness::new(3, 1, false, false);
            {
                let mut mailbox = h.mailbox.lock().unwrap();
                *mailbox = frd_frame::FrameMailbox::new(8, 64);
                assert_eq!(
                    mailbox.push(SurfaceUpdate::Reset {
                        session_id: h.session_id,
                        generation: 1,
                        size: PixelSize {
                            width: 3,
                            height: 1
                        },
                        format: PixelFormat::Bgrx8UnormSrgb
                    }),
                    frd_frame::PushOutcome::Queued
                );
            }
            let wire = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0];
            h.route(
                &bitmap_packet(
                    vec![bitmap(&wire, 32, 3, 1, rect(0, 0, 2, 0), false)],
                    false,
                ),
                false,
                1,
                true,
            )
            .unwrap();
            h.compile();
            let session_id = h.session_id;
            let damage = |generation, revision| SurfaceUpdate::Damage {
                session_id,
                generation,
                revision,
                patches: vec![PixelPatch {
                    rect: PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    stride_bytes: 4,
                    pixels: PixelBuffer::new(vec![17; 4]),
                }],
            };
            let boundary = |session_id, generation, revision| SurfaceUpdate::FrameBoundary {
                session_id,
                generation,
                revision,
                completeness: FrameCompleteness::FullBaseline,
            };
            let reset = || SurfaceUpdate::Reset {
                session_id,
                generation: 2,
                size: PixelSize {
                    width: 3,
                    height: 1,
                },
                format: PixelFormat::Bgrx8UnormSrgb,
            };
            let updates = match fault {
                "unclosed" => vec![damage(1, 5)],
                "orphan_boundary" => vec![boundary(session_id, 1, 5)],
                "reset_inside" => vec![damage(1, 5), reset()],
                "second_damage" => vec![damage(1, 5), damage(1, 6)],
                "wrong_revision" => vec![damage(1, 5), boundary(session_id, 1, 6)],
                "valid_prefix_bad_tail" => {
                    vec![damage(1, 5), boundary(session_id, 1, 5), damage(1, 6)]
                }
                "reset_bad_tail" => vec![reset(), damage(2, 5)],
                "wrong_session" => vec![damage(1, 5), boundary(SessionId::allocate(), 1, 5)],
                "wrong_generation" => vec![damage(1, 5), boundary(session_id, 2, 5)],
                _ => unreachable!(),
            };
            let mut generation = 1;
            assert_eq!(
                publish_egfx_surface_updates(
                    &mut h.runtime,
                    &mut h.baseline,
                    session_id,
                    &mut generation,
                    updates
                ),
                Err(if fault == "wrong_session" || fault == "wrong_generation" {
                    ProtocolError::StaleSurface
                } else {
                    ProtocolError::FramePortRejected
                }),
                "{fault}"
            );
            assert_eq!(generation, 1, "未闭合批次的 Reset 也不能提交：{fault}");
            assert_eq!(h.baseline.known_pixels(), 3);
            assert!(
                h.baseline.baseline_established(),
                "未经接受的外部 damage 不接管呈现：{fault}"
            );
            assert_eq!(h.baseline.presentation_revision(), 1);
            assert!(
                h.mailbox.lock().unwrap().is_empty(),
                "有效前缀也不得发布：{fault}"
            );
            h.route(
                &bitmap_packet(
                    vec![bitmap(&[9, 8, 7, 0], 24, 1, 1, rect(1, 0, 1, 0), false)],
                    false,
                ),
                false,
                1,
                true,
            )
            .unwrap();
            let transactions = h.compile();
            assert!(
                matches!(&transactions[0], frd_frame::FrameTransaction::Revision { revision, .. }
                if revision.revision == 2 && revision.completeness == FrameCompleteness::Incremental)
            );
        }
    }

    #[test]
    fn decoded_coverage_valid_external_pair_port_failure_retains_only_accepted_effects() {
        struct RejectBoundary(Arc<Mutex<frd_frame::FrameMailbox>>);
        impl SurfacePublisher for RejectBoundary {
            fn publish(&self, update: SurfaceUpdate) -> Result<(), ProtocolError> {
                if matches!(update, SurfaceUpdate::FrameBoundary { .. }) {
                    return Err(ProtocolError::FramePortRejected);
                }
                match self.0.lock().unwrap().push(update) {
                    frd_frame::PushOutcome::Queued => Ok(()),
                    frd_frame::PushOutcome::NeedsFullSnapshot => {
                        Err(ProtocolError::NeedsFullSnapshot)
                    }
                    frd_frame::PushOutcome::Rejected => Err(ProtocolError::FramePortRejected),
                }
            }
        }
        let session_id = SessionId::allocate();
        let mailbox = Arc::new(Mutex::new(frd_frame::FrameMailbox::new(3, 12)));
        let (_, rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            rx,
            Box::new(NoopEvents),
            Box::new(RejectBoundary(mailbox.clone())),
            None,
            Box::new(NoopWake),
        );
        let mut baseline = RdpBaseline::begin(
            &mut runtime,
            session_id,
            PixelSize {
                width: 3,
                height: 1,
            },
        )
        .unwrap();
        let mut generation = 1;
        let result = publish_egfx_surface_updates(
            &mut runtime,
            &mut baseline,
            session_id,
            &mut generation,
            vec![
                SurfaceUpdate::Damage {
                    session_id,
                    generation: 1,
                    revision: 7,
                    patches: vec![PixelPatch {
                        rect: PixelRect {
                            x: 0,
                            y: 0,
                            width: 3,
                            height: 1,
                        },
                        stride_bytes: 12,
                        pixels: PixelBuffer::new(vec![17; 12]),
                    }],
                },
                SurfaceUpdate::FrameBoundary {
                    session_id,
                    generation: 1,
                    revision: 7,
                    completeness: FrameCompleteness::FullBaseline,
                },
            ],
        );
        assert_eq!(result, Err(ProtocolError::FramePortRejected));
        assert!(runtime.requires_shutdown());
        assert_eq!(
            baseline.presentation_revision(),
            7,
            "已接受 Damage 的高水位保留"
        );
        assert_eq!(baseline.known_pixels(), 0);
        assert!(!baseline.baseline_established());
        assert_eq!(
            mailbox.lock().unwrap().len(),
            2,
            "不声称 runtime port 的整事务回滚"
        );
        let mut compiler = frd_frame::FrameTransactionCompiler::new(session_id);
        assert!(compiler
            .compile(mailbox.lock().unwrap().drain_committed_updates())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn decoded_coverage_external_high_water_overflow_fails_before_legacy_publication() {
        let mut h = CoverageHarness::new(3, 1, false, false);
        let wire = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0];
        h.route(
            &bitmap_packet(
                vec![bitmap(&wire, 32, 3, 1, rect(0, 0, 2, 0), false)],
                false,
            ),
            false,
            1,
            true,
        )
        .unwrap();
        h.compile();
        let session_id = h.session_id;
        let mut generation = 1;
        publish_egfx_surface_updates(
            &mut h.runtime,
            &mut h.baseline,
            session_id,
            &mut generation,
            vec![
                SurfaceUpdate::Damage {
                    session_id,
                    generation: 1,
                    revision: u64::MAX,
                    patches: vec![PixelPatch {
                        rect: PixelRect {
                            x: 0,
                            y: 0,
                            width: 3,
                            height: 1,
                        },
                        stride_bytes: 12,
                        pixels: PixelBuffer::new(vec![17; 12]),
                    }],
                },
                SurfaceUpdate::FrameBoundary {
                    session_id,
                    generation: 1,
                    revision: u64::MAX,
                    completeness: FrameCompleteness::FullBaseline,
                },
            ],
        )
        .unwrap();
        h.compile();
        assert_eq!(
            h.route(
                &bitmap_packet(
                    vec![bitmap(&[9, 8, 7, 0], 24, 1, 1, rect(1, 0, 1, 0), false)],
                    false
                ),
                false,
                1,
                true
            ),
            Err(ProtocolError::FramePortRejected)
        );
        assert_eq!(h.baseline.presentation_revision(), u64::MAX);
        assert_eq!(h.baseline.known_pixels(), 3);
        assert!(!h.baseline.baseline_established());
        assert!(h.mailbox.lock().unwrap().is_empty());
    }

    #[test]
    fn fatal_egfx_status_is_observed_before_sanitized_session_failure() {
        use crate::factory::RdpEgfxFailure;
        let previous = RdpGraphicsCapability::default().snapshot();
        for (count, reason) in [
            (1, None),
            (0, Some(RdpEgfxFailure::ProgressiveState)),
            (1, Some(RdpEgfxFailure::Publisher)),
        ] {
            let mut current = previous;
            current.egfx_diagnostics.failure_count = count;
            current.egfx_diagnostics.first_failure = reason;
            current.egfx_diagnostics.progressive_failure_detail =
                Some("region lacks current frame tiles");
            let seen = Mutex::new(Vec::new());
            let observer = |status| seen.lock().unwrap().push(status);
            let error =
                super::publish_graphics_status(previous, current, Some(&observer)).unwrap_err();
            assert_eq!(error.code(), "rdp_egfx_failed");
            assert_eq!(seen.lock().unwrap().as_slice(), &[current]);
            assert!(super::publish_graphics_status(current, current, None).is_err());
        }
    }

    #[test]
    fn absent_unconfirmed_and_intentionally_disabled_egfx_remain_nonfatal() {
        let previous = RdpGraphicsCapability::default().snapshot();
        assert!(super::publish_graphics_status(previous, previous, None).is_ok());
        let mut current = previous;
        current.egfx_advertised = true;
        current.egfx_confirmed = false;
        assert!(super::publish_graphics_status(previous, current, None).is_ok());
        let adapter = crate::egfx::EgfxAdapter::with_surface_publisher(
            None,
            crate::egfx::EgfxSurfacePublisher::try_new(SessionId::allocate(), 1).unwrap(),
        );
        adapter.disable_for_reactivation();
        assert!(adapter.is_failed());
        current.egfx_diagnostics = adapter.diagnostics();
        assert_eq!(current.egfx_diagnostics.failure_count, 1);
        assert_eq!(
            current.egfx_diagnostics.first_failure,
            Some(crate::factory::RdpEgfxFailure::Reactivation)
        );
        assert!(super::publish_graphics_status(previous, current, None).is_ok());
    }

    #[test]
    fn active_rdp_input_publishes_physical_and_text_capabilities() {
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let (events, event_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(RecordingFrames(Arc::new(Mutex::new(Vec::new())))),
            None,
            Box::new(NoopWake),
        );

        publish_active_input_capabilities(&mut runtime).expect("active input capabilities publish");

        assert_eq!(
            event_rx.recv().expect("one active capability event"),
            SessionEvent::CapabilitiesChanged(SessionCapabilities {
                dynamic_resolution: false,
                clipboard_read: false,
                clipboard_write: false,
                remote_audio: false,
                physical_key_input: true,
                text_input: true,
            })
        );
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn active_session_graphics_diagnostic_stays_legacy_only() {
        let capabilities = baseline_graphics_capabilities();
        assert!(capabilities.legacy_bitmap);
        assert!(capabilities.remotefx);
        assert!(!capabilities.egfx_advertised);
        assert!(!capabilities.egfx_confirmed);
        assert!(!capabilities.egfx_frame_confirmed);
        assert!(!capabilities.avc420);
        assert!(!capabilities.avc444);
    }

    #[test]
    fn graphics_diagnostic_requires_server_confirmation_before_avc420() {
        let mut capabilities = RdpGraphicsCapability {
            egfx_advertised: true,
            ..baseline_graphics_capabilities()
        };

        apply_egfx_confirmation(&mut capabilities, false, true, true, false);
        assert!(!capabilities.egfx_confirmed);
        assert!(!capabilities.egfx_frame_confirmed);
        assert!(!capabilities.avc420);
        assert!(!capabilities.avc444);

        apply_egfx_confirmation(&mut capabilities, true, true, true, false);
        assert!(capabilities.egfx_confirmed);
        assert!(!capabilities.egfx_frame_confirmed);
        assert!(capabilities.avc420);
        assert!(capabilities.avc444);
    }

    #[test]
    fn graphics_diagnostic_marks_egfx_only_after_a_published_frame_boundary() {
        let mut capabilities = RdpGraphicsCapability {
            egfx_advertised: true,
            ..baseline_graphics_capabilities()
        };

        apply_egfx_confirmation(&mut capabilities, true, true, false, false);
        assert!(capabilities.egfx_confirmed);
        assert!(!capabilities.egfx_frame_confirmed);

        apply_egfx_confirmation(&mut capabilities, true, true, false, true);
        assert!(capabilities.egfx_frame_confirmed);

        // The observation is monotonic for the session: a later capability
        // poll without a frame cannot erase already published evidence.
        apply_egfx_confirmation(&mut capabilities, true, true, false, false);
        assert!(capabilities.egfx_frame_confirmed);
    }

    #[test]
    fn deactivate_all_clears_input_capabilities_before_successful_reactivation() {
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let (events, event_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(RecordingFrames(Arc::new(Mutex::new(Vec::new())))),
            None,
            Box::new(NoopWake),
        );
        let mut input = RdpInputState::new();

        publish_active_input_capabilities(&mut runtime)
            .expect("initial input capabilities publish");
        let _releases = suspend_active_input_capabilities(&mut runtime, &mut input)
            .expect("deactivation clears input capabilities");
        assert_eq!(
            input.translate(InputEvent::Text {
                utf8: "a".to_owned(),
            }),
            Err(crate::input::RdpInputError::Stopped)
        );
        resume_active_input_capabilities(&mut runtime, &mut input)
            .expect("successful reactivation resumes input capabilities");
        assert!(input
            .translate(InputEvent::Text {
                utf8: "a".to_owned(),
            })
            .is_ok());

        assert_eq!(
            event_rx.try_iter().collect::<Vec<_>>(),
            vec![
                SessionEvent::CapabilitiesChanged(SessionCapabilities {
                    physical_key_input: true,
                    text_input: true,
                    ..SessionCapabilities::default()
                }),
                SessionEvent::CapabilitiesChanged(SessionCapabilities::default()),
                SessionEvent::CapabilitiesChanged(SessionCapabilities {
                    physical_key_input: true,
                    text_input: true,
                    ..SessionCapabilities::default()
                }),
            ]
        );
    }

    #[test]
    fn failed_or_disconnected_reactivation_never_republishes_input_capabilities() {
        for terminal_outcome in ["failure", "disconnect"] {
            let session_id = SessionId::allocate();
            let (_commands, command_rx) = mpsc::channel();
            let (events, event_rx) = mpsc::channel();
            let mut runtime = ProtocolRuntime::new(
                session_id,
                command_rx,
                Box::new(RecordingEvents(events)),
                Box::new(RecordingFrames(Arc::new(Mutex::new(Vec::new())))),
                None,
                Box::new(NoopWake),
            );
            let mut input = RdpInputState::new();

            publish_active_input_capabilities(&mut runtime)
                .expect("initial input capabilities publish");
            let _releases = suspend_active_input_capabilities(&mut runtime, &mut input)
                .expect("deactivation clears input capabilities");

            assert_eq!(
                event_rx.try_iter().collect::<Vec<_>>(),
                vec![
                    SessionEvent::CapabilitiesChanged(SessionCapabilities {
                        physical_key_input: true,
                        text_input: true,
                        ..SessionCapabilities::default()
                    }),
                    SessionEvent::CapabilitiesChanged(SessionCapabilities::default()),
                ],
                "{terminal_outcome} reactivation must not restore input capabilities"
            );
        }
    }

    #[test]
    fn lifecycle_audio_stop_clears_queued_pcm_and_publishes_stopped_before_reactivation() {
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let (events, event_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(RecordingFrames(Arc::new(Mutex::new(Vec::new())))),
            Some(Box::new(AcceptingMedia)),
            Box::new(NoopWake),
        );
        let audio = RdpAudioAdapter::new();
        let epoch = audio
            .begin_server_offer()
            .expect("server offer begins an epoch");
        assert!(audio.observe_server_offer(epoch, Some(0), 48_000, 2, 16));
        audio.accept_wave(0, &[0x01, 0x00, 0x02, 0x00]);

        stop_and_drain_audio(&mut runtime, &audio).expect("audio lifecycle stop publishes");

        assert!(!audio.remote_audio());
        assert!(audio.take_frame().is_none());
        assert!(!runtime.requires_shutdown());
        assert_eq!(
            event_rx.try_iter().collect::<Vec<_>>(),
            vec![SessionEvent::AudioState(
                frd_protocol_api::AudioState::Stopped
            )]
        );
    }

    #[test]
    fn active_session_graphics_output_publishes_one_damage_and_one_boundary() {
        let session_id = SessionId::allocate();
        let updates = Arc::new(Mutex::new(Vec::new()));
        let (_commands, command_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(RecordingFrames(updates.clone())),
            None,
            Box::new(NoopWake),
        );
        let mut baseline = RdpBaseline::begin(
            &mut runtime,
            session_id,
            PixelSize {
                width: 2,
                height: 2,
            },
        )
        .expect("generation begins");
        let image = DecodedImage::new(IronPixelFormat::RgbA32, 2, 2);

        publish_graphics_update(
            &mut runtime,
            &mut baseline,
            &image,
            1,
            InclusiveRectangle {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            },
            &[InclusiveRectangle {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            }],
        )
        .expect("graphics update publishes");

        let mut updates = updates.lock().expect("frame log");
        let updates = std::mem::take(&mut *updates);
        assert_eq!(updates.len(), 3);
        assert!(matches!(updates[0], SurfaceUpdate::Reset { .. }));
        assert!(matches!(
            updates[1],
            SurfaceUpdate::Damage {
                generation: 1,
                revision: 1,
                ..
            }
        ));
        assert!(matches!(
            updates[2],
            SurfaceUpdate::FrameBoundary {
                generation: 1,
                revision: 1,
                completeness: FrameCompleteness::FullBaseline,
                ..
            }
        ));
    }

    #[test]
    fn egfx_surface_reset_is_admitted_before_current_generation_damage() {
        let session_id = SessionId::allocate();
        let updates = Arc::new(Mutex::new(Vec::new()));
        let (_commands, command_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(RecordingFrames(updates.clone())),
            None,
            Box::new(NoopWake),
        );
        let mut baseline = RdpBaseline::begin(
            &mut runtime,
            session_id,
            PixelSize {
                width: 2,
                height: 2,
            },
        )
        .expect("baseline generation begins");
        let mut generation = 1;
        let patch = PixelPatch {
            rect: PixelRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
            stride_bytes: 8,
            pixels: PixelBuffer::from_boxed_slice(vec![0; 16].into_boxed_slice()),
        };

        let accepted = publish_egfx_surface_updates(
            &mut runtime,
            &mut baseline,
            session_id,
            &mut generation,
            vec![
                SurfaceUpdate::Reset {
                    session_id,
                    generation: 2,
                    size: PixelSize {
                        width: 2,
                        height: 2,
                    },
                    format: PixelFormat::Bgrx8UnormSrgb,
                },
                SurfaceUpdate::Damage {
                    session_id,
                    generation: 2,
                    revision: 1,
                    patches: vec![patch],
                },
                SurfaceUpdate::FrameBoundary {
                    session_id,
                    generation: 2,
                    revision: 1,
                    completeness: FrameCompleteness::FullBaseline,
                },
            ],
        )
        .expect("EGFX reset and current-generation frame publish");
        assert_eq!(accepted, 1);

        assert_eq!(generation, 2);
        {
            let updates = updates.lock().expect("frame log");
            assert_eq!(
                updates.len(),
                4,
                "old reset plus new reset, damage and boundary"
            );
            assert!(matches!(
                updates[1],
                SurfaceUpdate::Reset { generation: 2, .. }
            ));
            assert!(matches!(
                updates[2],
                SurfaceUpdate::Damage { generation: 2, .. }
            ));
            assert!(matches!(
                updates[3],
                SurfaceUpdate::FrameBoundary { generation: 2, .. }
            ));
        }

        // The legacy baseline must be rebound by the same EGFX reset. A
        // subsequent Bitmap/RemoteFX update in generation 2 remains a valid
        // fallback instead of being rejected as stale.
        let image = DecodedImage::new(IronPixelFormat::RgbA32, 2, 2);
        publish_graphics_update(
            &mut runtime,
            &mut baseline,
            &image,
            generation,
            InclusiveRectangle {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            },
            &[InclusiveRectangle {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            }],
        )
        .expect("legacy fallback remains valid after EGFX reset");
        let updates = updates.lock().expect("frame log");
        assert!(matches!(
            updates.last(),
            Some(SurfaceUpdate::FrameBoundary { generation: 2, .. })
        ));
    }

    #[test]
    fn egfx_surface_update_rejects_a_stale_session_before_runtime_publication() {
        let session_id = SessionId::allocate();
        let stale_session = SessionId::allocate();
        let updates = Arc::new(Mutex::new(Vec::new()));
        let (_commands, command_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(RecordingFrames(updates.clone())),
            None,
            Box::new(NoopWake),
        );
        let mut baseline = RdpBaseline::begin(
            &mut runtime,
            session_id,
            PixelSize {
                width: 2,
                height: 2,
            },
        )
        .expect("baseline generation begins");
        updates.lock().expect("frame log").clear();
        let mut generation = 1;

        let error = publish_egfx_surface_updates(
            &mut runtime,
            &mut baseline,
            session_id,
            &mut generation,
            vec![SurfaceUpdate::Reset {
                session_id: stale_session,
                generation: 2,
                size: PixelSize {
                    width: 2,
                    height: 2,
                },
                format: PixelFormat::Bgrx8UnormSrgb,
            }],
        )
        .expect_err("stale EGFX session must fail closed");

        assert_eq!(error, ProtocolError::StaleSession);
        assert_eq!(generation, 1);
        assert!(updates.lock().expect("frame log").is_empty());
    }

    #[test]
    fn egfx_surface_reset_rejects_a_non_advancing_generation_before_reset_publication() {
        let session_id = SessionId::allocate();
        let updates = Arc::new(Mutex::new(Vec::new()));
        let (_commands, command_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(RecordingFrames(updates.clone())),
            None,
            Box::new(NoopWake),
        );
        let mut baseline = RdpBaseline::begin(
            &mut runtime,
            session_id,
            PixelSize {
                width: 2,
                height: 2,
            },
        )
        .expect("baseline generation begins");
        updates.lock().expect("frame log").clear();
        let mut generation = 1;

        let error = publish_egfx_surface_updates(
            &mut runtime,
            &mut baseline,
            session_id,
            &mut generation,
            vec![SurfaceUpdate::Reset {
                session_id,
                generation: 1,
                size: PixelSize {
                    width: 2,
                    height: 2,
                },
                format: PixelFormat::Bgrx8UnormSrgb,
            }],
        )
        .expect_err("EGFX reset must advance the active generation");

        assert_eq!(error, ProtocolError::InvalidGeneration);
        assert_eq!(generation, 1);
        assert!(updates.lock().expect("frame log").is_empty());
    }

    #[test]
    fn lifecycle_reactivation_starts_a_new_empty_surface_generation() {
        let session_id = SessionId::allocate();
        let updates = Arc::new(Mutex::new(Vec::new()));
        let (_commands, command_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(RecordingFrames(updates.clone())),
            None,
            Box::new(NoopWake),
        );
        let mut baseline = RdpBaseline::begin(
            &mut runtime,
            session_id,
            PixelSize {
                width: 2,
                height: 2,
            },
        )
        .expect("first generation begins");
        let mut image = DecodedImage::new(IronPixelFormat::RgbA32, 2, 2);

        let generation = commit_reactivated_surface(
            &mut runtime,
            &mut baseline,
            &mut image,
            1,
            DesktopSize {
                width: 3,
                height: 2,
            },
        )
        .expect("reactivated surface commits");
        publish_graphics_update(
            &mut runtime,
            &mut baseline,
            &image,
            generation,
            InclusiveRectangle {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            &[InclusiveRectangle {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            }],
        )
        .expect("partial reactivated update publishes");

        assert_eq!(generation, 2);
        assert_eq!((image.width(), image.height()), (3, 2));
        let update_log = updates.lock().expect("frame log");
        assert!(matches!(
            &update_log[1],
            SurfaceUpdate::Reset { generation: 2, .. }
        ));
        // Reactivation starts with an empty surface. A partial bitmap must be
        // buffered until the decoder has exact coverage; publishing an
        // incremental boundary here would recreate the black-screen startup
        // failure seen with real RDP bitmap orderings.
        assert_eq!(update_log.len(), 2);
        drop(update_log);

        publish_graphics_update(
            &mut runtime,
            &mut baseline,
            &image,
            generation,
            InclusiveRectangle {
                left: 0,
                top: 0,
                right: 2,
                bottom: 1,
            },
            &[InclusiveRectangle {
                left: 0,
                top: 0,
                right: 2,
                bottom: 1,
            }],
        )
        .expect("full reactivated update publishes");
        let update_log = updates.lock().expect("frame log");
        assert!(matches!(
            update_log.last(),
            Some(SurfaceUpdate::FrameBoundary {
                generation: 2,
                completeness: FrameCompleteness::FullBaseline,
                ..
            })
        ));
    }

    #[test]
    fn lifecycle_stale_display_reactivation_keeps_latest_valid_viewport_in_same_session() {
        let session_id = SessionId::allocate();
        let updates = Arc::new(Mutex::new(Vec::new()));
        let (_commands, command_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(RecordingFrames(updates.clone())),
            None,
            Box::new(NoopWake),
        );
        let initial = PixelSize {
            width: 800,
            height: 600,
        };
        let mut baseline =
            RdpBaseline::begin(&mut runtime, session_id, initial).expect("generation begins");
        let mut image = DecodedImage::new(IronPixelFormat::RgbA32, 800, 600);
        let mut generation = 1;
        let mut input = RdpInputState::new();
        let _ = suspend_active_input_capabilities(&mut runtime, &mut input)
            .expect("input suspends before reactivation");
        let mut published_capabilities = SessionCapabilities::default();
        let mut retry_state = DisplayRetryState::default();
        let mut display = DisplayControlAdapter::new(initial);
        display.set_negotiated(Some(1600 * 900));
        display.observe_viewport(viewport(1600, 900));
        assert_eq!(
            display.take_resize_request(),
            Some(PixelSize {
                width: 1600,
                height: 900,
            })
        );
        display.observe_viewport(viewport(1280, 720));

        let disposition = finalize_reactivated_surface(
            &mut runtime,
            &mut baseline,
            &mut image,
            &mut generation,
            &mut display,
            DesktopSize {
                width: 1600,
                height: 900,
            },
            Some(1280 * 720),
        )
        .expect("stale capability is recoverable");

        assert_eq!(disposition, ReactivationSurfaceDisposition::StaleCapability);
        assert_eq!(
            apply_reactivation_outcome(
                ReactivationOutcome::RetryDisplay,
                &mut retry_state,
                &mut runtime,
                &mut input,
                &mut published_capabilities,
            )
            .expect("stale retry transition is recoverable"),
            ReactivationOutcome::RetryDisplay
        );
        assert!(retry_state.pending());
        assert_eq!(
            input.translate(InputEvent::Text {
                utf8: "x".to_owned(),
            }),
            Err(crate::input::RdpInputError::Stopped),
            "stale retry does not resume input"
        );
        assert_eq!(generation, 1, "stale generation is not committed");
        assert_eq!((image.width(), image.height()), (1600, 900));
        assert_eq!(updates.lock().expect("frame log").len(), 1);

        let mut writer = OrderedRdpWriter::new(Framed::new(RecordingIo::default()));
        let mut batch = super::process_server_batch(
            &mut stage(false, false),
            &mut image,
            generation,
            ironrdp::pdu::Action::FastPath,
            &bitmap_packet(
                vec![bitmap(&[1, 2, 3, 0], 24, 1, 1, rect(0, 0, 0, 0), false)],
                false,
            ),
        )
        .unwrap();
        batch
            .result
            .outputs
            .insert(0, ActiveStageOutput::ResponseFrame(b"progress".to_vec()));
        for receipt in &mut batch.result.decoded_coverage {
            receipt.graphics_output_index += 1;
        }
        let control = route_active_outputs(
            batch,
            &mut writer,
            &mut runtime,
            &mut baseline,
            retry_state.publish_graphics(),
        )
        .expect("stale graphics remain quarantined while wire progress continues");
        assert_eq!(control, ActiveOutputControl::Continue);
        let mut encoded_target = None;
        send_pending_resize(&mut display, &mut writer, |target| {
            encoded_target = Some(target);
            Ok(b"resize".to_vec())
        })
        .expect("retained resize is encoded and sent");
        assert_eq!(
            encoded_target,
            Some(PixelSize {
                width: 1280,
                height: 720,
            })
        );
        let mut batch =
            super::process_input_batch(&mut stage(false, false), &mut image, generation, &[])
                .unwrap();
        batch.result.outputs.push(ActiveStageOutput::DeactivateAll);
        let control = route_active_outputs(
            batch,
            &mut writer,
            &mut runtime,
            &mut baseline,
            retry_state.publish_graphics(),
        )
        .expect("next deactivate remains observable while quarantined");
        assert_eq!(control, ActiveOutputControl::Deactivate);
        assert_eq!(
            writer.into_framed().into_inner_no_leftover().written,
            b"progressresize"
        );
        assert_eq!(updates.lock().expect("frame log").len(), 1);

        let mut mismatch = DisplayControlAdapter::new(initial);
        mismatch.set_negotiated(Some(1600 * 900));
        mismatch.observe_viewport(viewport(1600, 900));
        assert!(mismatch.take_resize_request().is_some());
        assert!(finalize_reactivated_surface(
            &mut runtime,
            &mut baseline,
            &mut image,
            &mut generation,
            &mut mismatch,
            DesktopSize {
                width: 1598,
                height: 900,
            },
            Some(1600 * 900),
        )
        .is_err());
        assert_eq!(generation, 1, "true mismatch remains fatal before commit");

        let disposition = finalize_reactivated_surface(
            &mut runtime,
            &mut baseline,
            &mut image,
            &mut generation,
            &mut display,
            DesktopSize {
                width: 1280,
                height: 720,
            },
            Some(1280 * 720),
        )
        .expect("retained viewport can complete");
        assert_eq!(disposition, ReactivationSurfaceDisposition::Committed);
        assert_eq!(
            apply_reactivation_outcome(
                ReactivationOutcome::Continue,
                &mut retry_state,
                &mut runtime,
                &mut input,
                &mut published_capabilities,
            )
            .expect("current exact reactivation restores the active state"),
            ReactivationOutcome::Continue
        );
        assert!(!retry_state.pending());
        assert!(input
            .translate(InputEvent::Text {
                utf8: "x".to_owned(),
            })
            .is_ok());
        assert_eq!(generation, 2);
        assert_eq!((image.width(), image.height()), (1280, 720));
    }

    fn viewport(width: u32, height: u32) -> PhysicalViewport {
        let size = PixelSize { width, height };
        PhysicalViewport::new(
            size,
            PixelRect {
                x: 0,
                y: 0,
                width,
                height,
            },
            size,
        )
        .expect("valid viewport")
    }

    #[test]
    fn lifecycle_output_router_writes_every_response_before_changing_state() {
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(RecordingFrames(Arc::new(Mutex::new(Vec::new())))),
            None,
            Box::new(NoopWake),
        );
        let mut baseline = RdpBaseline::begin(
            &mut runtime,
            session_id,
            PixelSize {
                width: 2,
                height: 2,
            },
        )
        .expect("generation begins");
        let mut image = DecodedImage::new(IronPixelFormat::RgbA32, 2, 2);
        let mut writer = OrderedRdpWriter::new(Framed::new(RecordingIo::default()));
        let mut batch =
            super::process_input_batch(&mut stage(false, false), &mut image, 1, &[]).unwrap();
        batch.result.outputs = vec![
            ActiveStageOutput::ResponseFrame(b"before".to_vec()),
            ActiveStageOutput::DeactivateAll,
            ActiveStageOutput::ResponseFrame(b"after".to_vec()),
        ];
        let control = route_active_outputs(batch, &mut writer, &mut runtime, &mut baseline, true)
            .expect("outputs route");

        assert_eq!(control, ActiveOutputControl::Deactivate);
        assert_eq!(
            writer.into_framed().into_inner_no_leftover().written,
            b"beforeafter"
        );
    }

    #[test]
    fn input_large_text_is_split_into_valid_ordered_fast_path_batches() {
        let mut input = RdpInputState::new();
        let events = input
            .translate(InputEvent::Text {
                utf8: "a".repeat(128),
            })
            .expect("large text remains valid input");

        let batches = fast_path_input_batches(&events).collect::<Vec<_>>();

        assert_eq!(events.len(), 256);
        assert_eq!(
            batches.iter().map(|batch| batch.len()).collect::<Vec<_>>(),
            [255, 1]
        );
        assert_eq!(
            batches.into_iter().flatten().collect::<Vec<_>>(),
            events.iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn lifecycle_release_all_clears_database_state_once() {
        let mut input = RdpInputState::new();
        for usage in [0x04, 0xe4] {
            input
                .translate(InputEvent::PhysicalKey {
                    code: PhysicalKeyCode::from_usb_hid_usage(usage),
                    state: KeyState::Pressed,
                    modifiers: Modifiers::default(),
                })
                .expect("HID key is supported");
        }

        let releases = input.stop();

        assert_eq!(releases.len(), 2);
        assert!(input.stop().is_empty(), "database state is cleared once");
    }

    #[test]
    fn input_large_fast_path_batch_is_split_at_255_events() {
        use ironrdp::pdu::input::fast_path::{FastPathInputEvent, KeyboardFlags};

        let releases = vec![FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x1e); 256];
        let batches = fast_path_input_batches(&releases).collect::<Vec<_>>();

        assert_eq!(
            batches.iter().map(|batch| batch.len()).collect::<Vec<_>>(),
            [255, 1]
        );
        assert_eq!(
            batches.into_iter().flatten().collect::<Vec<_>>(),
            releases.iter().collect::<Vec<_>>()
        );
    }

    #[derive(Default)]
    struct RecordingIo {
        written: Vec<u8>,
    }

    impl Read for RecordingIo {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for RecordingIo {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct RecordingEvents(mpsc::Sender<SessionEvent>);

    impl RuntimeEventSink for RecordingEvents {
        fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError> {
            self.0
                .send(event)
                .map_err(|_| ProtocolError::EventPortClosed)
        }
    }

    struct NoopEvents;

    impl RuntimeEventSink for NoopEvents {
        fn publish(&self, _: SessionEvent) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct NoopWake;

    impl RuntimeWake for NoopWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct AcceptingMedia;

    impl MediaPublisher for AcceptingMedia {
        fn publish(&self, _: MediaFrame) -> Result<(), MediaPublishError> {
            Ok(())
        }
    }

    struct RecordingFrames(Arc<Mutex<Vec<SurfaceUpdate>>>);

    impl SurfacePublisher for RecordingFrames {
        fn publish(&self, update: SurfaceUpdate) -> Result<(), ProtocolError> {
            self.0
                .lock()
                .map_err(|_| ProtocolError::FramePortRejected)?
                .push(update);
            Ok(())
        }
    }
}
