use std::io::ErrorKind;
use std::net::Shutdown;
use std::time::Duration;

use frd_core::SessionId;
use frd_protocol_api::{ProtocolError, ProtocolExit, ProtocolRuntime, SessionCommand};
use ironrdp::graphics::image_processing::PixelFormat as IronPixelFormat;
use ironrdp::pdu::geometry::InclusiveRectangle;
use ironrdp::session::image::DecodedImage;
use ironrdp::session::{ActiveStageBuilder, ActiveStageOutput};

use crate::baseline::RdpBaseline;
use crate::connector::ActivatedRdpSession;
use crate::error::{rdp_error, RDP_ACTIVATION_FAILED};
use crate::surface::validate_negotiated_size;

const ACTIVE_SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(10);

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
    let negotiated_size = validate_negotiated_size(
        connection.desktop_size.width,
        connection.desktop_size.height,
    )
    .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?;
    let mut baseline = RdpBaseline::begin(runtime, session_id, negotiated_size)?;
    let mut image = DecodedImage::new(
        IronPixelFormat::RgbA32,
        connection.desktop_size.width,
        connection.desktop_size.height,
    );
    let mut active_stage = ActiveStageBuilder {
        static_channels: connection.static_channels,
        user_channel_id: connection.user_channel_id,
        io_channel_id: connection.io_channel_id,
        message_channel_id: connection.message_channel_id,
        share_id: connection.share_id,
        compression_type: connection.compression_type,
        enable_server_pointer: connection.enable_server_pointer,
        pointer_software_rendering: connection.pointer_software_rendering,
    }
    .build();
    let (mut framed, _server_public_key) = transport.into_parts();
    framed
        .get_inner()
        .0
        .sock
        .set_read_timeout(Some(ACTIVE_SOCKET_POLL_INTERVAL))
        .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?;

    loop {
        while let Some(command) = runtime.try_next_command() {
            if matches!(command, SessionCommand::Disconnect) {
                shutdown(&framed);
                return Ok(());
            }
        }
        if runtime.requires_shutdown() {
            shutdown(&framed);
            return Err(ProtocolError::Terminal);
        }

        let (action, payload) = match framed.read_pdu() {
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
                shutdown(&framed);
                return Ok(());
            }
            Err(_) => {
                shutdown(&framed);
                return Err(rdp_error(RDP_ACTIVATION_FAILED));
            }
        };
        let outputs = active_stage
            .process(&mut image, action, &payload)
            .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?;
        for output in outputs {
            match output {
                ActiveStageOutput::ResponseFrame(frame) => framed
                    .write_all(&frame)
                    .map_err(|_| rdp_error(RDP_ACTIVATION_FAILED))?,
                ActiveStageOutput::GraphicsUpdate(region) => {
                    publish_graphics_update(runtime, &mut baseline, &image, 1, region)?
                }
                ActiveStageOutput::Terminate(_) => {
                    shutdown(&framed);
                    return Ok(());
                }
                ActiveStageOutput::DeactivateAll => {
                    shutdown(&framed);
                    return Err(rdp_error(RDP_ACTIVATION_FAILED));
                }
                ActiveStageOutput::PointerDefault
                | ActiveStageOutput::PointerHidden
                | ActiveStageOutput::PointerPosition { .. }
                | ActiveStageOutput::PointerBitmap(_)
                | ActiveStageOutput::MultitransportRequest(_)
                | ActiveStageOutput::AutoDetect(_) => {}
            }
        }
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

fn shutdown(framed: &ironrdp_blocking::Framed<crate::tls::TlsStream>) {
    let _ = framed.get_inner().0.sock.shutdown(Shutdown::Both);
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc, Mutex};

    use frd_core::{PixelSize, SessionId};
    use frd_frame::{FrameCompleteness, SurfaceUpdate};
    use frd_protocol_api::{
        ProtocolError, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionEvent,
        SurfacePublisher,
    };
    use ironrdp::graphics::image_processing::PixelFormat as IronPixelFormat;
    use ironrdp::pdu::geometry::InclusiveRectangle;
    use ironrdp::session::image::DecodedImage;

    use crate::baseline::RdpBaseline;

    use super::publish_graphics_update;

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
