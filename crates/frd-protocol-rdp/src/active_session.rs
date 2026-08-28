use std::io::ErrorKind;
use std::time::Duration;

use frd_core::SessionId;
use frd_protocol_api::{ProtocolError, ProtocolExit, ProtocolRuntime};
use ironrdp::connector::connection_activation::{
    ConnectionActivationFactory, ConnectionActivationState,
};
use ironrdp::connector::{ConnectionResult, DesktopSize, Sequence as _};
use ironrdp::core::WriteBuf;
use ironrdp::graphics::image_processing::PixelFormat as IronPixelFormat;
use ironrdp::pdu::geometry::InclusiveRectangle;
use ironrdp::session::image::DecodedImage;
use ironrdp::session::{fast_path, ActiveStage, ActiveStageBuilder, ActiveStageOutput};

use crate::baseline::RdpBaseline;
use crate::connector::ActivatedRdpSession;
use crate::error::{rdp_error, RDP_ACTIVATION_FAILED};
use crate::input::RdpInputState;
use crate::runtime::{
    drain_active_commands, drain_reactivation_commands, ActiveCommandBatch, ActiveCommandDrain,
    ReactivationCommand,
};
use crate::surface::validate_negotiated_size;
use crate::tls::TlsStream;
use crate::writer::OrderedRdpWriter;

const ACTIVE_SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_FAST_PATH_INPUT_EVENTS: usize = 255;

pub(crate) fn run_active_session(
    session: ActivatedRdpSession,
    session_id: SessionId,
    runtime: &mut ProtocolRuntime,
) -> ProtocolExit {
    match run_active_session_inner(session, session_id, runtime) {
        Ok(()) => ProtocolExit::Closed,
        Err(error) => ProtocolExit::Failed(error),
    }
}

fn run_active_session_inner(
    session: ActivatedRdpSession,
    session_id: SessionId,
    runtime: &mut ProtocolRuntime,
) -> Result<(), ProtocolError> {
    let ActivatedRdpSession {
        connection,
        transport,
    } = session;
    let (framed, _server_public_key) = transport.into_parts();
    let mut writer = OrderedRdpWriter::new(framed);
    if writer
        .set_read_timeout(ACTIVE_SOCKET_POLL_INTERVAL)
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
    let mut active_stage = ActiveStageBuilder {
        static_channels,
        user_channel_id,
        io_channel_id,
        message_channel_id,
        share_id,
        compression_type,
        enable_server_pointer,
        pointer_software_rendering,
    }
    .build();
    let mut input = RdpInputState::new();
    let mut generation = 1;

    let result = run_active_loop(
        runtime,
        &mut writer,
        &mut active_stage,
        &mut image,
        &mut baseline,
        &mut input,
        &mut generation,
        &activation_factory,
    );
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
    result
}

#[allow(clippy::too_many_arguments)]
fn run_active_loop(
    runtime: &mut ProtocolRuntime,
    writer: &mut OrderedRdpWriter<TlsStream>,
    active_stage: &mut ActiveStage,
    image: &mut DecodedImage,
    baseline: &mut RdpBaseline,
    input: &mut RdpInputState,
    generation: &mut u64,
    activation_factory: &ConnectionActivationFactory,
) -> Result<(), ProtocolError> {
    let mut command_drain = ActiveCommandDrain::new();
    loop {
        match drain_active_commands(runtime, input, &mut command_drain)
            .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?
        {
            ActiveCommandBatch::Continue { events, pending } => {
                if route_input_events(
                    active_stage,
                    image,
                    writer,
                    runtime,
                    baseline,
                    *generation,
                    &events,
                )? != ActiveOutputControl::Continue
                {
                    return Err(rdp_error(RDP_ACTIVATION_FAILED));
                }
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
                );
                return Err(ProtocolError::Terminal);
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
        let outputs = active_stage
            .process(image, action, payload.as_ref())
            .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?;
        match route_active_outputs(outputs, writer, runtime, baseline, image, *generation)? {
            ActiveOutputControl::Continue => {}
            ActiveOutputControl::Terminate => return Ok(()),
            ActiveOutputControl::Deactivate => {
                let releases = input.stop();
                if route_input_events(
                    active_stage,
                    image,
                    writer,
                    runtime,
                    baseline,
                    *generation,
                    &releases,
                )? != ActiveOutputControl::Continue
                {
                    return Err(rdp_error(RDP_ACTIVATION_FAILED));
                }
                match drive_reactivation(
                    runtime,
                    writer,
                    active_stage,
                    image,
                    baseline,
                    generation,
                    input,
                    activation_factory,
                )? {
                    ReactivationOutcome::Continue => {}
                    ReactivationOutcome::Disconnect => return Ok(()),
                    ReactivationOutcome::Terminal => return Err(ProtocolError::Terminal),
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveOutputControl {
    Continue,
    Terminate,
    Deactivate,
}

fn route_active_outputs<S: std::io::Write>(
    outputs: Vec<ActiveStageOutput>,
    writer: &mut OrderedRdpWriter<S>,
    runtime: &mut ProtocolRuntime,
    baseline: &mut RdpBaseline,
    image: &DecodedImage,
    generation: u64,
) -> Result<ActiveOutputControl, ProtocolError> {
    let mut control = ActiveOutputControl::Continue;
    for output in outputs {
        match output {
            ActiveStageOutput::ResponseFrame(frame) => writer
                .write_frame(&frame)
                .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?,
            ActiveStageOutput::GraphicsUpdate(region) => {
                publish_graphics_update(runtime, baseline, image, generation, region)?
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

#[allow(clippy::too_many_arguments)]
fn route_input_events(
    active_stage: &mut ActiveStage,
    image: &mut DecodedImage,
    writer: &mut OrderedRdpWriter<TlsStream>,
    runtime: &mut ProtocolRuntime,
    baseline: &mut RdpBaseline,
    generation: u64,
    events: &[ironrdp::pdu::input::fast_path::FastPathInputEvent],
) -> Result<ActiveOutputControl, ProtocolError> {
    if events.is_empty() || !writer.is_writable() {
        return Ok(ActiveOutputControl::Continue);
    }
    let mut control = ActiveOutputControl::Continue;
    for batch in fast_path_input_batches(events) {
        let outputs = active_stage
            .process_fastpath_input(image, batch)
            .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?;
        match route_active_outputs(outputs, writer, runtime, baseline, image, generation)? {
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
    Disconnect,
    Terminal,
}

#[allow(clippy::too_many_arguments)]
fn drive_reactivation(
    runtime: &mut ProtocolRuntime,
    writer: &mut OrderedRdpWriter<TlsStream>,
    active_stage: &mut ActiveStage,
    image: &mut DecodedImage,
    baseline: &mut RdpBaseline,
    generation: &mut u64,
    input: &mut RdpInputState,
    activation_factory: &ConnectionActivationFactory,
) -> Result<ReactivationOutcome, ProtocolError> {
    let mut activation = activation_factory.create();
    let mut buffer = WriteBuf::new();
    loop {
        match drain_reactivation_commands(runtime) {
            ReactivationCommand::Continue => {}
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
            *generation =
                commit_reactivated_surface(runtime, baseline, image, *generation, desktop_size)?;
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
            input.resume();
            return Ok(ReactivationOutcome::Continue);
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
        );
    }
}

fn publish_graphics_update(
    runtime: &mut ProtocolRuntime,
    baseline: &mut RdpBaseline,
    image: &DecodedImage,
    generation: u64,
    region: InclusiveRectangle,
) -> Result<(), ProtocolError> {
    baseline.publish(runtime, image, generation, region)
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
    use std::io::{self, Read, Write};
    use std::sync::{mpsc, Arc, Mutex};

    use frd_core::{InputEvent, KeyState, Modifiers, PhysicalKeyCode, PixelSize, SessionId};
    use frd_frame::{FrameCompleteness, SurfaceUpdate};
    use frd_protocol_api::{
        ProtocolError, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionEvent,
        SurfacePublisher,
    };
    use ironrdp::connector::DesktopSize;
    use ironrdp::graphics::image_processing::PixelFormat as IronPixelFormat;
    use ironrdp::pdu::geometry::InclusiveRectangle;
    use ironrdp::session::image::DecodedImage;
    use ironrdp::session::ActiveStageOutput;
    use ironrdp_blocking::Framed;

    use crate::baseline::RdpBaseline;
    use crate::input::RdpInputState;
    use crate::writer::OrderedRdpWriter;

    use super::{
        commit_reactivated_surface, fast_path_input_batches, publish_graphics_update,
        route_active_outputs, ActiveOutputControl,
    };

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
        )
        .expect("partial reactivated update publishes");

        assert_eq!(generation, 2);
        assert_eq!((image.width(), image.height()), (3, 2));
        let updates = updates.lock().expect("frame log");
        assert!(matches!(
            &updates[1],
            SurfaceUpdate::Reset { generation: 2, .. }
        ));
        assert!(matches!(
            updates.last(),
            Some(SurfaceUpdate::FrameBoundary {
                generation: 2,
                completeness: FrameCompleteness::Incremental,
                ..
            })
        ));
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
        let image = DecodedImage::new(IronPixelFormat::RgbA32, 2, 2);
        let mut writer = OrderedRdpWriter::new(Framed::new(RecordingIo::default()));

        let control = route_active_outputs(
            vec![
                ActiveStageOutput::ResponseFrame(b"before".to_vec()),
                ActiveStageOutput::DeactivateAll,
                ActiveStageOutput::ResponseFrame(b"after".to_vec()),
            ],
            &mut writer,
            &mut runtime,
            &mut baseline,
            &image,
            1,
        )
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
    fn lifecycle_large_release_is_split_after_database_state_is_cleared() {
        let mut input = RdpInputState::new();
        for code in 1..=255 {
            input
                .translate(InputEvent::PhysicalKey {
                    code: PhysicalKeyCode(code),
                    state: KeyState::Pressed,
                    modifiers: Modifiers::default(),
                })
                .expect("normal scan code is valid");
        }
        input
            .translate(InputEvent::PhysicalKey {
                code: PhysicalKeyCode(0xe001),
                state: KeyState::Pressed,
                modifiers: Modifiers::default(),
            })
            .expect("extended scan code is valid");

        let releases = input.stop();
        let batches = fast_path_input_batches(&releases).collect::<Vec<_>>();

        assert_eq!(releases.len(), 256);
        assert!(input.stop().is_empty(), "database state is cleared once");
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
