//! Authenticated Apple HPSS/MVS session loop.
//!
//! The loop runs on the protocol coordinator's worker. `AppleWriterHandle` is
//! the sole outbound crypto owner; this module only serializes protocol-neutral
//! commands into that writer.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use frd_core::{
    ButtonState, InputEvent, KeyState, Modifiers, PhysicalKeyCode, PixelPoint, PointerButton,
    PointerButtons, PointerSample, SessionId, WheelDelta,
};
use frd_protocol_api::{
    AudioState, ConnectionStage, ProtocolError, ProtocolExit, ProtocolRuntime, SessionCapabilities,
    SessionCommand, SessionEvent,
};

use crate::connection::{is_peer_closed, is_timeout, AppleWriterHandle};
use crate::dynamic_resolution::DisplaySize;
use crate::factory::EstablishedAppleSession;
use crate::high_performance::{HighPerformanceUnavailable, APPLE_HIGH_PERFORMANCE_UNAVAILABLE};
use crate::hpss::{self, Media};
use crate::media_negotiation::AudioMediaFlow;
use crate::media_runtime::ViewerMediaState;
use crate::network_reader::{NetworkFrameOutcome, NetworkReaderRuntime};
use crate::protocol;

const APPLE_RUNTIME_FAILED: &str = "apple_runtime_failed";
const APPLE_RUNTIME_READ_POLL: Duration = Duration::from_millis(100);
fn startup_display_size(server_init: DisplaySize) -> DisplaySize {
    server_init
}

fn adapter_error() -> ProtocolError {
    ProtocolError::adapter(frd_core::ProtocolId::apple_hpss_mvs(), APPLE_RUNTIME_FAILED)
}

fn high_performance_unavailable_error() -> ProtocolError {
    ProtocolError::adapter(
        frd_core::ProtocolId::apple_hpss_mvs(),
        APPLE_HIGH_PERFORMANCE_UNAVAILABLE,
    )
}

fn protocol_exit_for_runtime_error(error: anyhow::Error) -> ProtocolExit {
    if error
        .chain()
        .any(|cause| cause.is::<HighPerformanceUnavailable>())
    {
        ProtocolExit::Failed(high_performance_unavailable_error())
    } else if is_peer_closed(&error) {
        ProtocolExit::Closed
    } else {
        ProtocolExit::Failed(adapter_error())
    }
}

fn is_startup_transport_close(error: &anyhow::Error) -> bool {
    is_peer_closed(error)
        || error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
            .any(|io_error| {
                matches!(
                    io_error.kind(),
                    std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::NotConnected
                        | std::io::ErrorKind::UnexpectedEof
                )
            })
}

fn preserve_startup_transport_error<T>(result: Result<T>) -> Result<T> {
    result.map_err(|error| {
        if is_startup_transport_close(&error) {
            HighPerformanceUnavailable.into()
        } else {
            error
        }
    })
}

pub(crate) fn preserve_pending_confirmation_result<T>(
    was_pending: bool,
    result: Result<T>,
) -> Result<T> {
    if was_pending {
        preserve_startup_transport_error(result)
    } else {
        result
    }
}

#[derive(Default)]
pub(crate) struct PointerWireState {
    point: Option<PixelPoint>,
    buttons: u8,
    pressed_keys: BTreeMap<PhysicalKeyCode, u32>,
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
            InputEvent::PhysicalKey {
                code,
                state,
                modifiers,
            } => {
                self.handle_key(code, state, modifiers, writer)?;
            }
            // The verified ARD 3.10 path uses RFB/X11 physical keysyms. No
            // separate committed-text wire path has been established yet.
            InputEvent::Text { .. } => {}
        }
        Ok(())
    }

    fn handle_sample(&mut self, sample: PointerSample, writer: &AppleWriterHandle) -> Result<()> {
        self.point = Some(sample.remote);
        self.buttons = pointer_button_mask(sample.buttons);
        self.send(self.buttons | wheel_mask(sample.wheel), writer)
    }

    pub(crate) fn release_all(&mut self, writer: &AppleWriterHandle) -> Result<()> {
        for keysym in std::mem::take(&mut self.pressed_keys).into_values() {
            writer.send_private_message(&protocol::msg_key_event(false, keysym))?;
        }
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

    fn handle_key(
        &mut self,
        code: PhysicalKeyCode,
        state: KeyState,
        modifiers: Modifiers,
        writer: &AppleWriterHandle,
    ) -> Result<()> {
        match state {
            KeyState::Pressed => {
                let Some(keysym) = self
                    .pressed_keys
                    .get(&code)
                    .copied()
                    .or_else(|| apple_keysym(code, modifiers))
                else {
                    return Ok(());
                };
                self.pressed_keys.insert(code, keysym);
                writer.send_private_message(&protocol::msg_key_event(true, keysym))
            }
            KeyState::Released => {
                let Some(keysym) = self.pressed_keys.remove(&code) else {
                    return Ok(());
                };
                writer.send_private_message(&protocol::msg_key_event(false, keysym))
            }
        }
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

fn apple_keysym(code: PhysicalKeyCode, modifiers: Modifiers) -> Option<u32> {
    let usage = code.usb_hid_usage();
    let shifted = modifiers.shift;
    let keysym = match usage {
        0x04..=0x1d => {
            let lower = u32::from(b'a') + u32::from(usage - 0x04);
            if shifted {
                lower - 0x20
            } else {
                lower
            }
        }
        0x1e..=0x27 => {
            const PLAIN: [u32; 10] = [
                b'1' as u32,
                b'2' as u32,
                b'3' as u32,
                b'4' as u32,
                b'5' as u32,
                b'6' as u32,
                b'7' as u32,
                b'8' as u32,
                b'9' as u32,
                b'0' as u32,
            ];
            const SHIFTED: [u32; 10] = [
                b'!' as u32,
                b'@' as u32,
                b'#' as u32,
                b'$' as u32,
                b'%' as u32,
                b'^' as u32,
                b'&' as u32,
                b'*' as u32,
                b'(' as u32,
                b')' as u32,
            ];
            let index = usize::from(usage - 0x1e);
            if shifted {
                SHIFTED[index]
            } else {
                PLAIN[index]
            }
        }
        0x28 => 0xff0d,
        0x29 => 0xff1b,
        0x2a => 0xff08,
        0x2b => 0xff09,
        0x2c => 0x20,
        0x2d => shifted_ascii(shifted, b'-', b'_'),
        0x2e => shifted_ascii(shifted, b'=', b'+'),
        0x2f => shifted_ascii(shifted, b'[', b'{'),
        0x30 => shifted_ascii(shifted, b']', b'}'),
        0x31 | 0x64 => shifted_ascii(shifted, b'\\', b'|'),
        0x33 => shifted_ascii(shifted, b';', b':'),
        0x34 => shifted_ascii(shifted, b'\'', b'"'),
        0x35 => shifted_ascii(shifted, b'`', b'~'),
        0x36 => shifted_ascii(shifted, b',', b'<'),
        0x37 => shifted_ascii(shifted, b'.', b'>'),
        0x38 => shifted_ascii(shifted, b'/', b'?'),
        0x39 => 0xffe5,
        0x3a..=0x45 => 0xffbe + u32::from(usage - 0x3a),
        0x46 => 0xff61,
        0x47 => 0xff14,
        0x48 => 0xff13,
        0x49 => 0xff63,
        0x4a => 0xff50,
        0x4b => 0xff55,
        0x4c => 0xffff,
        0x4d => 0xff57,
        0x4e => 0xff56,
        0x4f => 0xff53,
        0x50 => 0xff51,
        0x51 => 0xff54,
        0x52 => 0xff52,
        0x53 => 0xff7f,
        0x54 => 0xffaf,
        0x55 => 0xffaa,
        0x56 => 0xffad,
        0x57 => 0xffab,
        0x58 => 0xff8d,
        0x59..=0x61 => 0xffb1 + u32::from(usage - 0x59),
        0x62 => 0xffb0,
        0x63 => 0xffae,
        0x65 => 0xff67,
        0xe0 => 0xffe3,
        0xe1 => 0xffe1,
        0xe2 => 0xffe9,
        0xe3 => 0xffe7,
        0xe4 => 0xffe4,
        0xe5 => 0xffe2,
        0xe6 => 0xffea,
        0xe7 => 0xffe8,
        _ => return None,
    };
    Some(keysym)
}

const fn shifted_ascii(shifted: bool, plain: u8, shifted_value: u8) -> u32 {
    if shifted {
        shifted_value as u32
    } else {
        plain as u32
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
    run_authenticated_session_with_media(
        connection,
        metadata.name,
        metadata.size,
        runtime,
        session_id,
        false,
        AudioMediaFlow::MacToPc,
        metadata.encoding_profile == crate::session::SessionEncodingProfile::AppleUdpMedia,
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
    run_authenticated_session_with_media(
        connection,
        display_name,
        initial_pixel_size,
        runtime,
        session_id,
        dynamic_resolution_enabled,
        audio_flow,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_authenticated_session_with_media(
    connection: crate::AppleConnection,
    display_name: String,
    initial_pixel_size: frd_core::PixelSize,
    runtime: ProtocolRuntime,
    session_id: SessionId,
    dynamic_resolution_enabled: bool,
    audio_flow: AudioMediaFlow,
    udp_media_enabled: bool,
) -> ProtocolExit {
    match run_authenticated_session_inner(
        connection,
        display_name,
        initial_pixel_size,
        runtime,
        session_id,
        dynamic_resolution_enabled,
        audio_flow,
        udp_media_enabled,
    ) {
        Ok(()) => ProtocolExit::Closed,
        Err(error) => protocol_exit_for_runtime_error(error),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "媒体控制必须显式携带 generation、地址、writer 与发布门禁"
)]
fn handle_media_control(
    control: Media,
    media: &mut ViewerMediaState,
    runtime: &mut ProtocolRuntime,
    generation: u64,
    media_bind_address: std::net::IpAddr,
    writer: &AppleWriterHandle,
    audio_started: bool,
) -> Result<()> {
    match control {
        Media::PortAnnouncement(announcement) => {
            media.handle_port_announcement(generation, announcement, media_bind_address, writer)
        }
        Media::StreamAnswer(answer) => {
            media.handle_answer(generation, answer)?;
            if audio_started {
                runtime
                    .publish_event(SessionEvent::AudioState(AudioState::Playing))
                    .map_err(|error| anyhow::anyhow!(error.code()))?;
            }
            Ok(())
        }
        Media::Mvs { .. } | Media::Cursor { .. } | Media::State(_) => {
            unreachable!("reader 只返回媒体控制消息")
        }
    }
}

// Keep the verified session wiring explicit; aggregating these arguments would
// only move the boundary and risk coupling transport/media state construction.
#[allow(clippy::too_many_arguments)]
fn run_authenticated_session_inner(
    mut connection: crate::AppleConnection,
    display_name: String,
    initial_pixel_size: frd_core::PixelSize,
    mut runtime: ProtocolRuntime,
    session_id: SessionId,
    dynamic_resolution_enabled: bool,
    audio_flow: AudioMediaFlow,
    udp_media_enabled: bool,
) -> Result<()> {
    let server_init_size = DisplaySize::new(
        u16::try_from(initial_pixel_size.width).context("Apple 初始宽度超出 u16")?,
        u16::try_from(initial_pixel_size.height).context("Apple 初始高度超出 u16")?,
    )
    .context("Apple 初始显示尺寸无效")?;
    let initial_size = startup_display_size(server_init_size);
    let media_server_address = connection.peer_addr()?.ip();
    let media_bind_address = connection.local_addr()?.ip();
    if !connection.is_encrypted() {
        return Err(HighPerformanceUnavailable.into());
    }
    let initial_admission =
        NetworkReaderRuntime::admit_initial_generation(&mut runtime, session_id, initial_size)
            .map_err(|error| anyhow::anyhow!(error.code()))?;
    // Same verified startup ordering and timing as the legacy HPSS viewer.
    std::thread::sleep(Duration::from_millis(200));
    preserve_startup_transport_error(
        connection
            .write_all(&hpss::build_set_display_config(&display_name))
            .context("发送 Apple SetDisplayConfiguration 失败"),
    )?;
    let startup_gate_origin = Instant::now();
    std::thread::sleep(Duration::from_millis(150));
    preserve_startup_transport_error(
        connection.write_all(&hpss::build_display_query(initial_size)),
    )?;
    std::thread::sleep(Duration::from_millis(120));
    preserve_startup_transport_error(connection.write_all(&protocol::msg_fb_update_request(
        false,
        0,
        0,
        initial_size.width,
        initial_size.height,
    )?))?;
    let initial_full_sent_at = Instant::now();
    connection.set_read_timeout(Some(APPLE_RUNTIME_READ_POLL))?;
    let writer = connection.writer_handle()?;

    let mut media = ViewerMediaState::new(audio_flow, 1, media_server_address)?;
    let mut reader = NetworkReaderRuntime::new_admitted(
        session_id,
        initial_size,
        dynamic_resolution_enabled,
        startup_gate_origin,
        initial_full_sent_at,
        initial_admission,
    )
    .map_err(|error| anyhow::anyhow!(error.code()))?;
    let mut pointer = PointerWireState::default();
    let mut disconnect_requested = false;
    let mut readiness_published = false;
    let mut audio_started = false;
    #[cfg(test)]
    connection
        .notify_session_runtime_test_event(crate::connection::SessionRuntimeTestEvent::LoopEntered);

    let loop_result = (|| -> Result<()> {
        while !disconnect_requested {
            while let Some(command) = runtime.try_next_command() {
                match command {
                    SessionCommand::Disconnect => {
                        #[cfg(test)]
                        connection.notify_session_runtime_test_event(
                            crate::connection::SessionRuntimeTestEvent::DisconnectDequeued,
                        );
                        disconnect_requested = true;
                        break;
                    }
                    SessionCommand::ViewportChanged { viewport, .. } => {
                        if reader.is_high_performance_confirmed() {
                            reader.observe_viewport(viewport, Instant::now());
                        }
                    }
                    SessionCommand::Input(input) => {
                        if reader.is_high_performance_confirmed() {
                            pointer.handle(input.event, &writer)?;
                        }
                    }
                    SessionCommand::ResolveServerIdentity { .. }
                    | SessionCommand::ClipboardWrite(_) => {}
                }
            }
            if disconnect_requested || runtime.requires_shutdown() {
                break;
            }

            let now = Instant::now();
            if reader.is_high_performance_confirmed() {
                media.service_active(&mut runtime, reader.generation(), now)?;
            }
            reader.service_tick(&writer, &mut runtime, now)?;

            let message = match connection.read_app_frame_step() {
                Ok(Some(message)) => message,
                Ok(None) => continue,
                Err(error) if is_timeout(&error) => continue,
                Err(error)
                    if !reader.is_high_performance_confirmed()
                        && is_startup_transport_close(&error) =>
                {
                    return Err(HighPerformanceUnavailable.into());
                }
                Err(error) => return Err(error),
            };
            let mut before_generation_commit = || pointer.release_all(&writer);
            let was_pending = !reader.is_high_performance_confirmed();
            let outcome = preserve_pending_confirmation_result(
                was_pending,
                reader.handle_frame(
                    message,
                    &writer,
                    &mut media,
                    &mut runtime,
                    &mut before_generation_commit,
                ),
            )?;
            match outcome {
                NetworkFrameOutcome::Consumed => {}
                NetworkFrameOutcome::HighPerformanceConfirmed { size } => {
                    eprintln!(
                        "[apple] High Performance 虚拟显示器已确认: {}x{}",
                        size.width, size.height
                    );
                    if readiness_published {
                        anyhow::bail!("Apple High Performance readiness 重复发布");
                    }
                    runtime
                        .publish_event(SessionEvent::StageChanged(ConnectionStage::TransportReady))
                        .map_err(|error| anyhow::anyhow!(error.code()))?;
                    runtime
                        .publish_event(SessionEvent::CapabilitiesChanged(SessionCapabilities {
                            dynamic_resolution: dynamic_resolution_enabled,
                            remote_audio: udp_media_enabled,
                            text_input: true,
                            ..SessionCapabilities::default()
                        }))
                        .map_err(|error| anyhow::anyhow!(error.code()))?;
                    if udp_media_enabled {
                        runtime
                            .publish_event(SessionEvent::AudioState(AudioState::Starting))
                            .map_err(|error| anyhow::anyhow!(error.code()))?;
                        audio_started = true;
                    }
                    readiness_published = true;
                    while let Some(control) = reader.take_buffered_media_control() {
                        handle_media_control(
                            control,
                            &mut media,
                            &mut runtime,
                            reader.generation(),
                            media_bind_address,
                            &writer,
                            audio_started,
                        )?;
                    }
                }
                NetworkFrameOutcome::Media(control) => {
                    if !readiness_published {
                        return Err(HighPerformanceUnavailable.into());
                    }
                    handle_media_control(
                        control,
                        &mut media,
                        &mut runtime,
                        reader.generation(),
                        media_bind_address,
                        &writer,
                        audio_started,
                    )?;
                }
            }
        }
        Ok(())
    })();

    let _ = runtime.publish_event(SessionEvent::StageChanged(ConnectionStage::Disconnecting));
    if audio_started {
        let _ = runtime.publish_event(SessionEvent::AudioState(AudioState::Stopped));
    }
    let _ = media.close(reader.generation());
    #[cfg(test)]
    connection
        .notify_session_runtime_test_event(crate::connection::SessionRuntimeTestEvent::MediaClosed);
    let _ = connection.shutdown();
    #[cfg(test)]
    connection.notify_session_runtime_test_event(
        crate::connection::SessionRuntimeTestEvent::ConnectionShutdown,
    );
    let _ = writer.shutdown();
    #[cfg(test)]
    connection.notify_session_runtime_test_event(
        crate::connection::SessionRuntimeTestEvent::WriterShutdown,
    );
    loop_result
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    };
    use std::thread;
    use std::time::{Duration, Instant};

    use frd_protocol_api::{
        MailboxSurfacePublisher, ProtocolError, ProtocolRuntime, RuntimeEventSink, RuntimeWake,
        SessionCommand, SessionEvent, SurfacePublisher,
    };

    struct NoopWake;

    enum RuntimeTrace {
        Event(SessionEvent),
        Surface(frd_frame::SurfaceUpdate),
        Wake,
    }

    struct TracingEvents(mpsc::Sender<RuntimeTrace>);

    impl RuntimeEventSink for TracingEvents {
        fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError> {
            self.0
                .send(RuntimeTrace::Event(event))
                .map_err(|_| ProtocolError::EventPortClosed)
        }
    }

    struct BlockingReadinessEvents {
        trace: mpsc::Sender<RuntimeTrace>,
        blocked: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        blocked_once: AtomicBool,
    }

    impl RuntimeEventSink for BlockingReadinessEvents {
        fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError> {
            if matches!(
                event,
                SessionEvent::StageChanged(frd_protocol_api::ConnectionStage::TransportReady)
            ) && !self.blocked_once.swap(true, Ordering::SeqCst)
            {
                self.blocked
                    .send(())
                    .map_err(|_| ProtocolError::EventPortClosed)?;
                self.release
                    .lock()
                    .unwrap()
                    .recv()
                    .map_err(|_| ProtocolError::EventPortClosed)?;
            }
            self.trace
                .send(RuntimeTrace::Event(event))
                .map_err(|_| ProtocolError::EventPortClosed)
        }
    }

    struct TracingFrames(mpsc::Sender<RuntimeTrace>);

    impl SurfacePublisher for TracingFrames {
        fn publish(&self, update: frd_frame::SurfaceUpdate) -> Result<(), ProtocolError> {
            self.0
                .send(RuntimeTrace::Surface(update))
                .map_err(|_| ProtocolError::FramePortRejected)
        }
    }

    struct TracingWake(mpsc::Sender<RuntimeTrace>);

    impl RuntimeWake for TracingWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            self.0
                .send(RuntimeTrace::Wake)
                .map_err(|_| ProtocolError::WakeFailed)
        }
    }

    struct ProductionHarness {
        peer: TcpStream,
        peer_crypto: crate::session::SessionCrypto,
        commands: mpsc::Sender<SessionCommand>,
        trace: mpsc::Receiver<RuntimeTrace>,
        exit: mpsc::Receiver<frd_protocol_api::ProtocolExit>,
        worker: thread::JoinHandle<()>,
        session_id: frd_core::SessionId,
    }

    fn read_encrypted_test_message(
        peer: &mut TcpStream,
        crypto: &mut crate::session::SessionCrypto,
    ) -> Vec<u8> {
        let mut prefix = [0u8; 2];
        peer.read_exact(&mut prefix).unwrap();
        let length = usize::from(u16::from_be_bytes(prefix));
        let mut ciphertext = vec![0u8; length];
        peer.read_exact(&mut ciphertext).unwrap();
        crypto.open(&ciphertext).unwrap()
    }

    fn write_encrypted_test_message(
        peer: &mut TcpStream,
        crypto: &mut crate::session::SessionCrypto,
        message: &[u8],
    ) {
        let wire = crypto.seal(message).unwrap();
        peer.write_all(&wire).unwrap();
    }

    impl ProductionHarness {
        fn read_message(&mut self) -> Vec<u8> {
            read_encrypted_test_message(&mut self.peer, &mut self.peer_crypto)
        }

        fn send_message(&mut self, message: &[u8]) {
            write_encrypted_test_message(&mut self.peer, &mut self.peer_crypto, message);
        }

        fn read_verified_startup(&mut self) {
            let set_display = self.read_message();
            assert_eq!(set_display.first(), Some(&0x1d));
            assert_eq!(set_display.len(), 308);
            let display_query = self.read_message();
            assert_eq!(display_query.first(), Some(&0x09));
            assert_eq!(self.read_message(), [3, 0, 0, 0, 0, 0, 0, 8, 0, 8]);
        }
    }

    fn server_state_message(width: u16, height: u16) -> Vec<u8> {
        let mut server_state = vec![0u8; 94];
        server_state[0..4].copy_from_slice(&1u32.to_be_bytes());
        server_state[12..16].copy_from_slice(&crate::hpss::encoding::SERVER_STATE.to_be_bytes());
        server_state[16..18].copy_from_slice(&76u16.to_be_bytes());
        server_state[18..20].copy_from_slice(&5u16.to_be_bytes());
        server_state[20..22].copy_from_slice(&width.to_be_bytes());
        server_state[22..24].copy_from_slice(&height.to_be_bytes());
        server_state[24..26].copy_from_slice(&width.to_be_bytes());
        server_state[26..28].copy_from_slice(&height.to_be_bytes());
        server_state[36..38].copy_from_slice(&1u16.to_be_bytes());
        server_state
    }

    fn port_announcement_message(audio_port: u16) -> Vec<u8> {
        let mut message = vec![
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x03, 0xf2, 0x00, 0x24, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x45, 0x67,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        message[26..28].copy_from_slice(&audio_port.to_be_bytes());
        message
    }

    fn start_production_harness_with_events(
        udp_media_enabled: bool,
        make_events: impl FnOnce(mpsc::Sender<RuntimeTrace>) -> Box<dyn RuntimeEventSink>,
    ) -> ProductionHarness {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let key = [0x31; 16];
        let iv = [0x42; 16];
        let mut connection = crate::AppleConnection::new(client);
        connection
            .set_crypto(crate::session::SessionCrypto::from_key_iv(key, iv))
            .unwrap();
        let session_id = frd_core::SessionId::allocate();
        let (commands, command_rx) = mpsc::channel();
        let (trace_tx, trace) = mpsc::channel();
        let events = make_events(trace_tx.clone());
        let runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            events,
            Box::new(TracingFrames(trace_tx.clone())),
            None,
            Box::new(TracingWake(trace_tx)),
        );
        let (exit_tx, exit) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = super::run_authenticated_session_with_media(
                connection,
                "production-session-test".to_owned(),
                frd_core::PixelSize::new(8, 8).unwrap(),
                runtime,
                session_id,
                false,
                crate::media_negotiation::AudioMediaFlow::MacToPc,
                udp_media_enabled,
            );
            exit_tx.send(result).unwrap();
        });
        ProductionHarness {
            peer,
            peer_crypto: crate::session::SessionCrypto::from_key_iv(key, iv),
            commands,
            trace,
            exit,
            worker,
            session_id,
        }
    }

    fn start_production_harness(udp_media_enabled: bool) -> ProductionHarness {
        start_production_harness_with_events(udp_media_enabled, |trace| {
            Box::new(TracingEvents(trace))
        })
    }

    #[test]
    fn startup_geometry_preserves_the_authenticated_server_init() {
        let landscape = crate::dynamic_resolution::DisplaySize::new(1920, 1080).unwrap();
        let portrait = crate::dynamic_resolution::DisplaySize::new(1440, 2560).unwrap();

        assert_eq!(super::startup_display_size(landscape), landscape);
        assert_eq!(super::startup_display_size(portrait), portrait);
    }

    #[test]
    fn normalized_usb_hid_keys_follow_the_verified_legacy_keysym_mapping() {
        let plain = frd_core::Modifiers::default();
        let shifted = frd_core::Modifiers {
            shift: true,
            ..Default::default()
        };

        assert_eq!(
            super::apple_keysym(frd_core::PhysicalKeyCode::from_usb_hid_usage(0x04), plain),
            Some(0x61)
        );
        assert_eq!(
            super::apple_keysym(frd_core::PhysicalKeyCode::from_usb_hid_usage(0x04), shifted),
            Some(0x41)
        );
        assert_eq!(
            super::apple_keysym(frd_core::PhysicalKeyCode::from_usb_hid_usage(0x1e), shifted),
            Some(0x21)
        );
        assert_eq!(
            super::apple_keysym(frd_core::PhysicalKeyCode::from_usb_hid_usage(0x28), plain),
            Some(0xff0d)
        );
    }

    #[test]
    fn production_session_defers_generation_and_readiness_until_strict_server_state() {
        let mut harness = start_production_harness(false);
        harness.read_verified_startup();
        assert!(matches!(
            harness.trace.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        harness
            .commands
            .send(SessionCommand::Input(frd_core::SessionInput {
                session_id: harness.session_id,
                generation: 1,
                event: frd_core::InputEvent::ReleaseAll,
            }))
            .unwrap();
        let viewport = frd_core::PhysicalViewport::new(
            frd_core::PixelSize::new(32, 16).unwrap(),
            frd_core::PixelRect {
                x: 0,
                y: 0,
                width: 32,
                height: 16,
            },
            frd_core::PixelSize::new(8, 8).unwrap(),
        )
        .unwrap();
        harness
            .commands
            .send(SessionCommand::ViewportChanged {
                session_id: harness.session_id,
                generation: 1,
                viewport,
            })
            .unwrap();

        harness
            .peer
            .set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        let mut pending_byte = [0u8; 1];
        assert!(matches!(
            harness.peer.read(&mut pending_byte),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                )
        ));
        harness
            .peer
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        harness.send_message(&server_state_message(16, 8));
        assert_eq!(harness.read_message(), [3, 0, 0, 0, 0, 0, 0, 16, 0, 8]);

        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Event(SessionEvent::SurfaceGenerationChanged {
                generation: 1,
                size,
                ..
            }) if size == frd_core::PixelSize::new(16, 8).unwrap()
        ));
        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Surface(frd_frame::SurfaceUpdate::Reset {
                generation: 1,
                size,
                ..
            }) if size == frd_core::PixelSize::new(16, 8).unwrap()
        ));
        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Wake
        ));
        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Event(SessionEvent::StageChanged(
                frd_protocol_api::ConnectionStage::TransportReady
            ))
        ));
        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Wake
        ));
        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Event(SessionEvent::CapabilitiesChanged(capabilities))
                if !capabilities.remote_audio && !capabilities.dynamic_resolution
        ));
        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Wake
        ));

        harness
            .peer
            .set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        let mut byte = [0u8; 1];
        assert!(matches!(
            harness.peer.read(&mut byte),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                )
        ));
        harness.peer.shutdown(std::net::Shutdown::Both).unwrap();
        assert_eq!(
            harness.exit.recv_timeout(Duration::from_secs(2)).unwrap(),
            frd_protocol_api::ProtocolExit::Closed
        );
        harness.worker.join().unwrap();
    }

    #[test]
    fn production_session_buffers_media_until_readiness_then_handles_it_before_next_read() {
        let (blocked_tx, blocked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut harness = start_production_harness_with_events(true, move |trace| {
            Box::new(BlockingReadinessEvents {
                trace,
                blocked: blocked_tx,
                release: Mutex::new(release_rx),
                blocked_once: AtomicBool::new(false),
            })
        });
        harness.read_verified_startup();

        harness.send_message(&port_announcement_message(17_767));
        harness
            .peer
            .set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        let mut pending_byte = [0u8; 1];
        assert!(matches!(
            harness.peer.read(&mut pending_byte),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                )
        ));
        assert!(matches!(
            harness.trace.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        harness
            .peer
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        harness.send_message(&server_state_message(16, 8));
        assert_eq!(harness.read_message(), [3, 0, 0, 0, 0, 0, 0, 16, 0, 8]);
        blocked_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        harness.send_message(&server_state_message(24, 8));
        harness
            .peer
            .set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        let mut byte = [0u8; 1];
        assert!(matches!(
            harness.peer.read(&mut byte),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                )
        ));
        release_tx.send(()).unwrap();
        harness
            .peer
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let media_configuration = harness.read_message();
        assert_eq!(media_configuration.first(), Some(&0x1c));
        let mut saw_sentinel_full = false;
        for _ in 0..3 {
            if harness.read_message() == [3, 0, 0, 0, 0, 0, 0, 24, 0, 8] {
                saw_sentinel_full = true;
                break;
            }
        }
        assert!(
            saw_sentinel_full,
            "缓存媒体必须在读取下一 ServerState 前排空"
        );

        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Event(SessionEvent::SurfaceGenerationChanged { generation: 1, .. })
        ));
        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Surface(frd_frame::SurfaceUpdate::Reset { generation: 1, .. })
        ));
        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Wake
        ));
        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Event(SessionEvent::StageChanged(
                frd_protocol_api::ConnectionStage::TransportReady
            ))
        ));
        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Wake
        ));
        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Event(SessionEvent::CapabilitiesChanged(capabilities))
                if capabilities.remote_audio
        ));
        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Wake
        ));
        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Event(SessionEvent::AudioState(
                frd_protocol_api::AudioState::Starting
            ))
        ));
        assert!(matches!(
            harness.trace.recv_timeout(Duration::from_secs(1)).unwrap(),
            RuntimeTrace::Wake
        ));

        harness.peer.shutdown(std::net::Shutdown::Both).unwrap();
        assert_eq!(
            harness.exit.recv_timeout(Duration::from_secs(2)).unwrap(),
            frd_protocol_api::ProtocolExit::Closed
        );
        harness.worker.join().unwrap();
    }

    #[test]
    fn production_session_pending_peer_close_is_high_performance_unavailable() {
        let mut harness = start_production_harness(true);
        harness.read_verified_startup();
        assert!(matches!(
            harness.trace.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        harness.peer.shutdown(std::net::Shutdown::Both).unwrap();
        let exit = harness.exit.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            exit,
            frd_protocol_api::ProtocolExit::Failed(ref error)
                if error.code()
                    == crate::high_performance::APPLE_HIGH_PERFORMANCE_UNAVAILABLE
        ));
        harness.worker.join().unwrap();
        while let Ok(trace) = harness.trace.try_recv() {
            assert!(!matches!(
                trace,
                RuntimeTrace::Event(SessionEvent::AudioState(_))
            ));
        }
    }

    #[test]
    fn production_session_unencrypted_startup_fails_before_any_hpss_write_or_event() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let connection = crate::AppleConnection::new(client);
        let session_id = frd_core::SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let (trace_tx, trace_rx) = mpsc::channel();
        let runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(TracingEvents(trace_tx.clone())),
            Box::new(TracingFrames(trace_tx.clone())),
            None,
            Box::new(TracingWake(trace_tx)),
        );

        let exit = super::run_authenticated_session_with_media(
            connection,
            "unencrypted-test".to_owned(),
            frd_core::PixelSize::new(8, 8).unwrap(),
            runtime,
            session_id,
            false,
            crate::media_negotiation::AudioMediaFlow::MacToPc,
            false,
        );

        assert!(matches!(
            exit,
            frd_protocol_api::ProtocolExit::Failed(ref error)
                if error.code()
                    == crate::high_performance::APPLE_HIGH_PERFORMANCE_UNAVAILABLE
        ));
        assert!(matches!(
            trace_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected)
        ));
        let mut byte = [0u8; 1];
        match peer.read(&mut byte) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            other => panic!("未加密启动不得发送 HPSS 字节: {other:?}"),
        }
    }

    #[test]
    fn production_session_startup_transport_close_classifier_and_typed_exit_are_stable() {
        for kind in [
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::ConnectionAborted,
            std::io::ErrorKind::NotConnected,
            std::io::ErrorKind::UnexpectedEof,
        ] {
            let error = anyhow::Error::new(std::io::Error::new(kind, "synthetic startup close"))
                .context("startup write wrapper");
            assert!(super::is_startup_transport_close(&error));

            let pending_error = super::preserve_pending_confirmation_result::<()>(
                true,
                Err(anyhow::Error::new(std::io::Error::new(
                    kind,
                    "synthetic confirmed-size full write close",
                ))
                .context("handle_frame confirmation write")),
            )
            .unwrap_err();
            assert_eq!(
                pending_error.downcast_ref::<crate::high_performance::HighPerformanceUnavailable>(),
                Some(&crate::high_performance::HighPerformanceUnavailable)
            );

            let confirmed_error = super::preserve_pending_confirmation_result::<()>(
                false,
                Err(anyhow::Error::new(std::io::Error::new(
                    kind,
                    "synthetic active reader close",
                ))),
            )
            .unwrap_err();
            assert!(confirmed_error
                .chain()
                .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
                .any(|error| error.kind() == kind));
            assert!(confirmed_error
                .downcast_ref::<crate::high_performance::HighPerformanceUnavailable>()
                .is_none());
        }
        let unrelated = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "synthetic unrelated error",
        ));
        assert!(!super::is_startup_transport_close(&unrelated));

        assert!(matches!(
            super::protocol_exit_for_runtime_error(
                crate::high_performance::HighPerformanceUnavailable.into()
            ),
            frd_protocol_api::ProtocolExit::Failed(ref error)
                if error.code()
                    == crate::high_performance::APPLE_HIGH_PERFORMANCE_UNAVAILABLE
        ));
    }

    impl RuntimeWake for NoopWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct RecordingEvents(mpsc::Sender<SessionEvent>);

    impl RuntimeEventSink for RecordingEvents {
        fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError> {
            self.0.send(event).map_err(|_| ProtocolError::Terminal)
        }
    }

    struct RecordingFrames(mpsc::Sender<frd_frame::SurfaceUpdate>);

    impl SurfacePublisher for RecordingFrames {
        fn publish(&self, update: frd_frame::SurfaceUpdate) -> Result<(), ProtocolError> {
            self.0.send(update).map_err(|_| ProtocolError::Terminal)
        }
    }

    #[test]
    fn production_startup_rejects_actual_surface_capacity_before_any_wire_write() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut connection = crate::AppleConnection::new(client);
        connection
            .set_crypto(crate::session::SessionCrypto::from_key_iv(
                [0x31; 16], [0x42; 16],
            ))
            .unwrap();

        let session_id = frd_core::SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::channel();
        let mailbox = Arc::new(Mutex::new(frd_frame::FrameMailbox::new(8, 12)));
        let runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events_tx)),
            Box::new(MailboxSurfacePublisher::new(mailbox.clone())),
            None,
            Box::new(NoopWake),
        );

        let exit = super::run_established_hpss_session(
            connection,
            "surface-capacity-test".to_owned(),
            frd_core::PixelSize::new(2, 2).unwrap(),
            runtime,
            session_id,
            false,
            crate::media_negotiation::AudioMediaFlow::MacToPc,
        );

        assert!(matches!(
            exit,
            frd_protocol_api::ProtocolExit::Failed(ref error)
                if error.code() == super::APPLE_RUNTIME_FAILED
        ));
        assert!(mailbox.lock().unwrap().is_empty());
        assert!(events_rx.try_iter().next().is_none());
        let mut byte = [0u8; 1];
        match peer.read(&mut byte) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            other => panic!("容量拒绝前写出了 wire 数据: {other:?}"),
        }
    }

    #[test]
    fn production_session_disconnect_interrupts_blocked_writer_and_runs_cleanup() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        socket2::SockRef::from(&listener)
            .set_recv_buffer_size(1024)
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (peer_ready_tx, peer_ready_rx) = mpsc::channel();
        let (high_performance_ready_tx, high_performance_ready_rx) = mpsc::channel();
        let (release_peer_tx, release_peer_rx) = mpsc::channel();
        let peer = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            socket2::SockRef::from(&stream)
                .set_recv_buffer_size(1024)
                .unwrap();
            peer_ready_tx.send(()).unwrap();
            let mut crypto = crate::session::SessionCrypto::from_key_iv([0x31; 16], [0x42; 16]);
            let set_display = read_encrypted_test_message(&mut stream, &mut crypto);
            assert_eq!(set_display.first(), Some(&0x1d));
            assert_eq!(set_display.len(), 308);
            assert_eq!(
                read_encrypted_test_message(&mut stream, &mut crypto).first(),
                Some(&0x09)
            );
            assert_eq!(
                read_encrypted_test_message(&mut stream, &mut crypto),
                [3, 0, 0, 0, 0, 0, 0, 64, 0, 64]
            );
            write_encrypted_test_message(&mut stream, &mut crypto, &server_state_message(64, 64));
            assert_eq!(
                read_encrypted_test_message(&mut stream, &mut crypto),
                [3, 0, 0, 0, 0, 0, 0, 64, 0, 64]
            );
            high_performance_ready_tx.send(()).unwrap();
            let _ = release_peer_rx.recv_timeout(Duration::from_secs(5));
            drop(stream);
        });
        let client = TcpStream::connect(address).unwrap();
        socket2::SockRef::from(&client)
            .set_send_buffer_size(1024)
            .unwrap();
        peer_ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let mut connection = crate::AppleConnection::new(client);
        connection
            .set_crypto(crate::session::SessionCrypto::from_key_iv(
                [0x31; 16], [0x42; 16],
            ))
            .unwrap();
        let (session_trace_tx, session_trace_rx) = mpsc::channel();
        connection.set_session_runtime_test_events(session_trace_tx);
        let (writer_io_tx, writer_io_rx) = mpsc::channel();
        let writer = connection
            .writer_handle_with_io_events(writer_io_tx)
            .unwrap();
        let session_id = frd_core::SessionId::allocate();
        let (commands, command_rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::channel();
        let (frames_tx, frames_rx) = mpsc::channel();
        let runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events_tx)),
            Box::new(RecordingFrames(frames_tx)),
            None,
            Box::new(NoopWake),
        );
        let (exit_tx, exit_rx) = mpsc::channel();
        let session = thread::spawn(move || {
            let exit = super::run_established_hpss_session(
                connection,
                "session-shutdown-test".to_owned(),
                frd_core::PixelSize::new(64, 64).unwrap(),
                runtime,
                session_id,
                false,
                crate::media_negotiation::AudioMediaFlow::MacToPc,
            );
            exit_tx.send(exit).unwrap();
        });

        high_performance_ready_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(matches!(
            frames_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            frd_frame::SurfaceUpdate::Reset { generation: 1, .. }
        ));
        assert_eq!(
            session_trace_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            crate::connection::SessionRuntimeTestEvent::LoopEntered
        );
        while writer_io_rx.try_recv().is_ok() {}

        let active_writer = writer.clone();
        let (blocked_send_tx, blocked_send_rx) = mpsc::channel();
        let blocked_sender = thread::spawn(move || {
            let payload = vec![0x5a; 60_000];
            loop {
                if let Err(error) = active_writer.send_private_message(&payload) {
                    blocked_send_tx.send(error).unwrap();
                    break;
                }
            }
        });

        let write_block_deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let remaining = write_block_deadline
                .checked_duration_since(Instant::now())
                .expect("production writer never entered a blocked write_all");
            match writer_io_rx.recv_timeout(remaining).unwrap() {
                crate::connection::WriterIoTestEvent::Started { wire_bytes }
                    if wire_bytes >= 60_000 =>
                {
                    match writer_io_rx.recv_timeout(Duration::from_millis(150)) {
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Ok(crate::connection::WriterIoTestEvent::Finished { .. }) => continue,
                        Ok(other) => panic!("unexpected writer event after start: {other:?}"),
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            panic!("writer exited before Disconnect")
                        }
                    }
                }
                crate::connection::WriterIoTestEvent::Started { .. }
                | crate::connection::WriterIoTestEvent::Finished { .. } => {}
            }
        }

        let cleanup_deadline = Instant::now() + Duration::from_secs(3);
        commands.send(SessionCommand::Disconnect).unwrap();
        assert_eq!(
            session_trace_rx
                .recv_timeout(cleanup_deadline.duration_since(Instant::now()))
                .expect("production loop must dequeue Disconnect within the cleanup bound"),
            crate::connection::SessionRuntimeTestEvent::DisconnectDequeued
        );
        let exit = exit_rx
            .recv_timeout(cleanup_deadline.duration_since(Instant::now()))
            .expect("production session cleanup must return within 3s");
        let blocked_error = blocked_send_rx
            .recv_timeout(cleanup_deadline.duration_since(Instant::now()))
            .expect("blocked production sender must receive the shutdown error within 3s");
        session.join().unwrap();
        blocked_sender.join().unwrap();

        assert!(matches!(exit, frd_protocol_api::ProtocolExit::Closed));
        assert!(blocked_error.to_string().contains("写入失败"));
        assert!(writer.send_private_message(b"after-cleanup").is_err());
        assert!(matches!(
            exit_rx.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert_eq!(
            session_trace_rx.try_iter().collect::<Vec<_>>(),
            vec![
                crate::connection::SessionRuntimeTestEvent::MediaClosed,
                crate::connection::SessionRuntimeTestEvent::ConnectionShutdown,
                crate::connection::SessionRuntimeTestEvent::WriterShutdown,
            ]
        );
        let cleanup_events: Vec<_> = events_rx.try_iter().collect();
        let disconnecting = cleanup_events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    SessionEvent::StageChanged(frd_protocol_api::ConnectionStage::Disconnecting)
                )
            })
            .expect("production cleanup must publish Disconnecting");
        let audio_stopped = cleanup_events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    SessionEvent::AudioState(frd_protocol_api::AudioState::Stopped)
                )
            })
            .expect("production cleanup must publish AudioState::Stopped");
        assert!(disconnecting < audio_stopped);

        release_peer_tx.send(()).unwrap();
        peer.join().unwrap();
    }
}
