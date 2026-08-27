//! Authenticated Apple HPSS/MVS session loop.
//!
//! The loop runs on the protocol coordinator's worker. `AppleWriterHandle` is
//! the sole outbound crypto owner; this module only serializes protocol-neutral
//! commands into that writer.

use std::error;
use std::fmt;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use frd_core::{
    ButtonState, InputEvent, PixelPoint, PointerButton, PointerButtons, PointerSample, SessionId,
    WheelDelta,
};
use frd_protocol_api::{
    AudioState, ConnectionStage, ProtocolError, ProtocolExit, ProtocolRuntime, SessionCapabilities,
    SessionCommand, SessionEvent,
};

use crate::connection::{is_peer_closed, is_timeout, AppleWriterHandle};
use crate::dynamic_resolution::DisplaySize;
use crate::factory::EstablishedAppleSession;
use crate::hpss::{self, Media};
use crate::media_negotiation::AudioMediaFlow;
use crate::media_runtime::ViewerMediaState;
use crate::network_reader::{NetworkFrameOutcome, NetworkReaderRuntime};
use crate::protocol;

const APPLE_RUNTIME_FAILED: &str = "apple_runtime_failed";
const APPLE_KEYBOARD_INPUT_UNSUPPORTED: &str = "apple_keyboard_input_unsupported_task_10";
const APPLE_RUNTIME_READ_POLL: Duration = Duration::from_millis(100);

fn adapter_error() -> ProtocolError {
    ProtocolError::adapter(frd_core::ProtocolId::apple_hpss_mvs(), APPLE_RUNTIME_FAILED)
}

#[derive(Debug)]
struct UnsupportedKeyboardInput;

impl fmt::Display for UnsupportedKeyboardInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Apple 迁移阶段仅支持鼠标；统一键盘输入由 Task 10 恢复")
    }
}

impl error::Error for UnsupportedKeyboardInput {}

fn is_unsupported_keyboard(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<UnsupportedKeyboardInput>())
}

fn protocol_exit_for_runtime_error(error: anyhow::Error) -> ProtocolExit {
    if is_peer_closed(&error) {
        ProtocolExit::Closed
    } else if is_unsupported_keyboard(&error) {
        ProtocolExit::Failed(ProtocolError::adapter(
            frd_core::ProtocolId::apple_hpss_mvs(),
            APPLE_KEYBOARD_INPUT_UNSUPPORTED,
        ))
    } else {
        ProtocolExit::Failed(adapter_error())
    }
}

#[derive(Default)]
pub(crate) struct PointerWireState {
    point: Option<PixelPoint>,
    buttons: u8,
}

impl PointerWireState {
    pub(crate) fn handle(&mut self, event: InputEvent, writer: &AppleWriterHandle) -> Result<()> {
        match event {
            InputEvent::PointerSample(sample) => self.handle_sample(sample, writer)?,
            InputEvent::PointerMove { remote } => {
                self.point = Some(remote);
                self.send(self.buttons, writer)?;
            }
            InputEvent::PointerButton { button, state } => {
                let bit = match button {
                    PointerButton::Primary => protocol::pointer::PRIMARY,
                    PointerButton::Middle => protocol::pointer::MIDDLE,
                    PointerButton::Secondary => protocol::pointer::SECONDARY,
                    // The previous HPSS viewer did not assign Apple/RFB wire
                    // bits to these protocol-neutral buttons. Task 10 owns
                    // any wider platform-normalization contract.
                    PointerButton::Back | PointerButton::Forward => return Ok(()),
                };
                match state {
                    ButtonState::Pressed => self.buttons |= bit,
                    ButtonState::Released => self.buttons &= !bit,
                }
                self.send(self.buttons, writer)?;
            }
            InputEvent::Wheel { delta_x, delta_y } => {
                let mut mask = self.buttons;
                if delta_y > 0.0 {
                    mask |= protocol::pointer::WHEEL_UP;
                } else if delta_y < 0.0 {
                    mask |= protocol::pointer::WHEEL_DOWN;
                }
                if delta_x > 0.0 {
                    mask |= protocol::pointer::WHEEL_RIGHT;
                } else if delta_x < 0.0 {
                    mask |= protocol::pointer::WHEEL_LEFT;
                }
                self.send(mask, writer)?;
            }
            InputEvent::ReleaseAll => {
                self.release_all(writer)?;
            }
            // PhysicalKeyCode intentionally has no cross-platform keysym
            // meaning yet. Do not guess one in the Apple adapter or silently
            // claim the input was handled.
            InputEvent::PhysicalKey { .. } | InputEvent::Text { .. } => {
                return Err(UnsupportedKeyboardInput.into());
            }
        }
        Ok(())
    }

    fn handle_sample(&mut self, sample: PointerSample, writer: &AppleWriterHandle) -> Result<()> {
        self.point = Some(sample.remote);
        self.buttons = pointer_button_mask(sample.buttons);
        self.send(self.buttons | wheel_mask(sample.wheel), writer)
    }

    pub(crate) fn release_all(&mut self, writer: &AppleWriterHandle) -> Result<()> {
        let point = self.point.unwrap_or(PixelPoint { x: 0, y: 0 });
        writer.send_private_message(&protocol::msg_pointer_event(
            0,
            u16::try_from(point.x).context("指针 x 超出 Apple RFB u16 范围")?,
            u16::try_from(point.y).context("指针 y 超出 Apple RFB u16 范围")?,
        ))?;
        self.point = None;
        self.buttons = 0;
        Ok(())
    }

    fn send(&self, mask: u8, writer: &AppleWriterHandle) -> Result<()> {
        let Some(point) = self.point else {
            return Ok(());
        };
        let x = u16::try_from(point.x).context("指针 x 超出 Apple RFB u16 范围")?;
        let y = u16::try_from(point.y).context("指针 y 超出 Apple RFB u16 范围")?;
        writer.send_private_message(&protocol::msg_pointer_event(mask, x, y))
    }
}

fn pointer_button_mask(buttons: PointerButtons) -> u8 {
    (if buttons.primary {
        protocol::pointer::PRIMARY
    } else {
        0
    }) | (if buttons.middle {
        protocol::pointer::MIDDLE
    } else {
        0
    }) | (if buttons.secondary {
        protocol::pointer::SECONDARY
    } else {
        0
    })
}

fn wheel_mask(wheel: WheelDelta) -> u8 {
    (if wheel.vertical > 0 {
        protocol::pointer::WHEEL_UP
    } else if wheel.vertical < 0 {
        protocol::pointer::WHEEL_DOWN
    } else {
        0
    }) | (if wheel.horizontal > 0 {
        protocol::pointer::WHEEL_RIGHT
    } else if wheel.horizontal < 0 {
        protocol::pointer::WHEEL_LEFT
    } else {
        0
    })
}

pub(crate) fn run_authenticated_session(
    established: EstablishedAppleSession,
    runtime: ProtocolRuntime,
    session_id: SessionId,
) -> ProtocolExit {
    let EstablishedAppleSession {
        connection,
        metadata,
    } = established;
    run_established_hpss_session(
        connection,
        metadata.name,
        metadata.size,
        runtime,
        session_id,
        false,
        AudioMediaFlow::MacToPc,
    )
}

/// Compatibility entry for the root protocol lab, which already owns an
/// authenticated Apple connection. Product composition uses the factory.
pub fn run_established_hpss_session(
    connection: crate::AppleConnection,
    display_name: String,
    initial_pixel_size: frd_core::PixelSize,
    runtime: ProtocolRuntime,
    session_id: SessionId,
    dynamic_resolution_enabled: bool,
    audio_flow: AudioMediaFlow,
) -> ProtocolExit {
    match run_authenticated_session_inner(
        connection,
        display_name,
        initial_pixel_size,
        runtime,
        session_id,
        dynamic_resolution_enabled,
        audio_flow,
    ) {
        Ok(()) => ProtocolExit::Closed,
        Err(error) => protocol_exit_for_runtime_error(error),
    }
}

fn run_authenticated_session_inner(
    mut connection: crate::AppleConnection,
    display_name: String,
    initial_pixel_size: frd_core::PixelSize,
    mut runtime: ProtocolRuntime,
    session_id: SessionId,
    dynamic_resolution_enabled: bool,
    audio_flow: AudioMediaFlow,
) -> Result<()> {
    let initial_size = DisplaySize::new(
        u16::try_from(initial_pixel_size.width).context("Apple 初始宽度超出 u16")?,
        u16::try_from(initial_pixel_size.height).context("Apple 初始高度超出 u16")?,
    )
    .context("Apple 初始显示尺寸无效")?;
    let media_server_address = connection.peer_addr()?.ip();
    let media_bind_address = connection.local_addr()?.ip();

    runtime
        .publish_event(SessionEvent::StageChanged(ConnectionStage::TransportReady))
        .map_err(|error| anyhow::anyhow!(error.code()))?;
    runtime
        .publish_event(SessionEvent::CapabilitiesChanged(SessionCapabilities {
            dynamic_resolution: dynamic_resolution_enabled,
            remote_audio: true,
            ..SessionCapabilities::default()
        }))
        .map_err(|error| anyhow::anyhow!(error.code()))?;
    runtime
        .publish_event(SessionEvent::AudioState(AudioState::Starting))
        .map_err(|error| anyhow::anyhow!(error.code()))?;

    // Same verified startup ordering and timing as the legacy HPSS viewer.
    std::thread::sleep(Duration::from_millis(200));
    connection
        .write_all(&hpss::build_set_display_config(&display_name))
        .context("发送 Apple SetDisplayConfiguration 失败")?;
    std::thread::sleep(Duration::from_millis(150));
    connection.write_all(&hpss::build_display_query(initial_size))?;
    std::thread::sleep(Duration::from_millis(120));
    connection.write_all(&protocol::msg_fb_update_request(
        false,
        0,
        0,
        initial_size.width,
        initial_size.height,
    )?)?;
    let startup_fb_sent_at = Instant::now();
    connection.set_read_timeout(Some(APPLE_RUNTIME_READ_POLL))?;
    if !connection.is_encrypted() {
        anyhow::bail!("Apple HPSS runtime requires the authenticated encrypted session");
    }
    let writer = connection.writer_handle()?;

    let mut media = ViewerMediaState::new(audio_flow, 1, media_server_address)?;
    let mut reader = NetworkReaderRuntime::new(
        &mut runtime,
        session_id,
        initial_size,
        dynamic_resolution_enabled,
        startup_fb_sent_at,
    )
    .map_err(|error| anyhow::anyhow!(error.code()))?;
    let mut pointer = PointerWireState::default();
    let mut disconnect_requested = false;

    let loop_result = (|| -> Result<()> {
        while !disconnect_requested {
            while let Some(command) = runtime.try_next_command() {
                match command {
                    SessionCommand::Disconnect => {
                        disconnect_requested = true;
                        break;
                    }
                    SessionCommand::ViewportChanged { viewport, .. } => {
                        reader.observe_viewport(viewport, Instant::now());
                    }
                    SessionCommand::Input(input) => pointer.handle(input.event, &writer)?,
                    SessionCommand::ResolveServerIdentity { .. }
                    | SessionCommand::ClipboardWrite(_) => {}
                }
            }
            if disconnect_requested || runtime.requires_shutdown() {
                break;
            }

            let now = Instant::now();
            media.service_active(&mut runtime, reader.generation(), now)?;
            reader.service_tick(&writer, now)?;

            let message = match connection.read_app_frame_step() {
                Ok(Some(message)) => message,
                Ok(None) => continue,
                Err(error) if is_timeout(&error) => continue,
                Err(error) => return Err(error),
            };
            let mut before_generation_commit = || pointer.release_all(&writer);
            match reader.handle_frame(
                message,
                &writer,
                &mut media,
                &mut runtime,
                &mut before_generation_commit,
            )? {
                NetworkFrameOutcome::Consumed => {}
                NetworkFrameOutcome::Media(Media::PortAnnouncement(announcement)) => {
                    media.handle_port_announcement(
                        reader.generation(),
                        announcement,
                        media_bind_address,
                        &writer,
                    )?;
                }
                NetworkFrameOutcome::Media(Media::StreamAnswer(answer)) => {
                    media.handle_answer(reader.generation(), answer)?;
                    runtime
                        .publish_event(SessionEvent::AudioState(AudioState::Playing))
                        .map_err(|error| anyhow::anyhow!(error.code()))?;
                }
                NetworkFrameOutcome::Media(_) => unreachable!("reader only returns media control"),
            }
        }
        Ok(())
    })();

    let _ = runtime.publish_event(SessionEvent::StageChanged(ConnectionStage::Disconnecting));
    let _ = runtime.publish_event(SessionEvent::AudioState(AudioState::Stopped));
    let _ = media.close(reader.generation());
    let _ = connection.shutdown();
    let _ = writer.shutdown();
    loop_result
}

#[cfg(test)]
mod tests {
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use frd_protocol_api::{
        ProtocolError, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionCommand,
        SessionEvent, SurfacePublisher,
    };

    struct NoopEvents;

    impl RuntimeEventSink for NoopEvents {
        fn publish(&self, _event: SessionEvent) -> Result<(), ProtocolError> {
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

    #[test]
    fn production_writer_timeout_unblocks_runtime_to_drain_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        socket2::SockRef::from(&listener)
            .set_recv_buffer_size(1024)
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (release_peer_tx, release_peer_rx) = mpsc::channel();
        let peer = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            socket2::SockRef::from(&stream)
                .set_recv_buffer_size(1024)
                .unwrap();
            release_peer_rx.recv().unwrap();
            drop(stream);
        });
        let client = TcpStream::connect(address).unwrap();
        socket2::SockRef::from(&client)
            .set_send_buffer_size(1024)
            .unwrap();
        let mut connection = crate::AppleConnection::new(client);
        let writer = connection.writer_handle().unwrap();
        let session_id = frd_core::SessionId::allocate();
        let (commands, command_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(NoopFrames),
            None,
            Box::new(NoopWake),
        );
        commands.send(SessionCommand::Disconnect).unwrap();
        let active_writer = writer.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let session = thread::spawn(move || {
            let payload = vec![0x5a; 1024 * 1024];
            let write: anyhow::Result<()> = loop {
                if let Err(error) = active_writer.send_private_message(&payload) {
                    break Err(error);
                }
            };
            let command = runtime.try_next_command();
            done_tx.send((write, command)).unwrap();
        });

        let completed = done_rx.recv_timeout(Duration::from_secs(3));
        let completed_within_bound = completed.is_ok();
        release_peer_tx.send(()).unwrap();
        peer.join().unwrap();
        let (write, command) = completed.unwrap_or_else(|_| {
            done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("peer close must release the blocked production writer")
        });
        session.join().unwrap();
        writer.shutdown().unwrap();

        assert!(write.is_err());
        assert!(matches!(command, Some(SessionCommand::Disconnect)));
        assert!(
            completed_within_bound,
            "production write must fail before the 3s session bound"
        );
    }

    #[test]
    fn unsupported_keyboard_input_returns_stable_fail_closed_exit() {
        let exit = super::protocol_exit_for_runtime_error(super::UnsupportedKeyboardInput.into());

        assert!(matches!(
            exit,
            frd_protocol_api::ProtocolExit::Failed(error)
                if error.code() == "apple_keyboard_input_unsupported_task_10"
        ));
    }
}
