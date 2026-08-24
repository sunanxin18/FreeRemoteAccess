use std::fmt;
use std::time::Duration;

use crossbeam_channel::{Receiver, TryRecvError};
use ironrdp::client::config::{Config, ConfigBuilder, Destination};
use ironrdp::client::rdp::{RdpClient, RdpInputEvent, RdpOutputEvent};
use ironrdp::input::{Database, MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use ironrdp::pdu::rdp::capability_sets::MajorPlatformType;
use secrecy::ExposeSecret;
use tokio::sync::mpsc;

use crate::app::connection::{ProtocolKind, ValidatedConnection};
use crate::core::{FrameRect, RemotePixelFormat, RenderUpdate};
use crate::protocols::ProtocolAdapter;
use crate::session::{
    ProtocolContext, SessionCommand, SessionError, SessionEvent, SessionEventSink,
};

const DEFAULT_RDP_WIDTH: u32 = 1280;
const DEFAULT_RDP_HEIGHT: u32 = 720;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RdpDesktopConfig {
    width: u16,
    height: u16,
}

pub struct RdpRuntimeConfig(Config);

impl RdpRuntimeConfig {
    pub fn destination(&self) -> &Destination {
        self.0.destination()
    }

    pub fn connector(&self) -> &ironrdp::connector::Config {
        self.0.connector()
    }

    pub fn connect_addr(&self) -> Option<std::net::SocketAddr> {
        self.0.connect_addr()
    }

    fn into_inner(self) -> Config {
        self.0
    }
}

impl fmt::Debug for RdpRuntimeConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RdpRuntimeConfig")
            .field("destination", self.destination())
            .field("desktop_size", &self.connector().desktop_size)
            .field("enable_credssp", &self.connector().enable_credssp)
            .field("enable_tls", &self.connector().enable_tls)
            .finish_non_exhaustive()
    }
}

impl RdpDesktopConfig {
    pub fn new(width: u32, height: u32) -> Result<Self, SessionError> {
        let width = u16::try_from(width)
            .map_err(|_| SessionError::new("rdp_desktop_dimensions_invalid"))?;
        let height = u16::try_from(height)
            .map_err(|_| SessionError::new("rdp_desktop_dimensions_invalid"))?;
        if width == 0 || height == 0 {
            return Err(SessionError::new("rdp_desktop_dimensions_invalid"));
        }
        Ok(Self { width, height })
    }

    pub const fn dimensions(self) -> (u16, u16) {
        (self.width, self.height)
    }
}

impl Default for RdpDesktopConfig {
    fn default() -> Self {
        Self {
            width: DEFAULT_RDP_WIDTH as u16,
            height: DEFAULT_RDP_HEIGHT as u16,
        }
    }
}

pub fn build_rdp_config(
    connection: ValidatedConnection,
    desktop: RdpDesktopConfig,
) -> Result<RdpRuntimeConfig, SessionError> {
    if connection.protocol != ProtocolKind::Rdp {
        return Err(SessionError::new("rdp_protocol_mismatch"));
    }
    let (width, height) = desktop.dimensions();
    let mut builder = ConfigBuilder::new()
        .with_destination(Destination::from_parts(
            connection.endpoint.host(),
            connection.endpoint.port(),
        ))
        .with_username(connection.username)
        .with_password(connection.password.expose_secret().to_owned())
        .with_desktop_width(width)
        .with_desktop_height(height)
        .with_desktop_scale_factor(100)
        .with_credssp(true)
        .with_tls(false)
        .with_autologon(true)
        .with_client_build(100)
        .with_client_dir("C:\\Windows\\System32\\mstscax.dll")
        .with_client_name("FreeRemoteAccess")
        .with_platform(current_platform());
    if let Some(address) = connection.endpoint.pinned_addr() {
        builder = builder.with_connect_addr(address);
    }
    if let Some(domain) = connection.domain {
        builder = builder.with_domain(domain);
    }
    builder
        .build()
        .map(RdpRuntimeConfig)
        .map_err(|_| SessionError::new("rdp_config_invalid"))
}

pub struct RdpAdapter;

impl RdpAdapter {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for RdpAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolAdapter for RdpAdapter {
    fn run(
        self: Box<Self>,
        context: ProtocolContext,
        commands: Receiver<SessionCommand>,
        events: SessionEventSink,
    ) -> Result<(), SessionError> {
        events.emit(SessionEvent::Connecting)?;
        let config = build_rdp_config(context.into_connection(), RdpDesktopConfig::default())?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("freeremote-rdp")
            .build()
            .map_err(|_| SessionError::new("rdp_runtime_create_failed"))?;
        tokio::task::LocalSet::new().block_on(&runtime, run_rdp(config, commands, events))
    }
}

async fn run_rdp(
    config: RdpRuntimeConfig,
    commands: Receiver<SessionCommand>,
    events: SessionEventSink,
) -> Result<(), SessionError> {
    const OUTPUT_CAPACITY: usize = 8;
    let (output_sender, mut output_receiver) = mpsc::channel(OUTPUT_CAPACITY);
    let client = RdpClient::new(config.into_inner(), output_sender);
    let input_sender = client.input_sender();
    let worker = tokio::task::spawn_local(client.run());
    let mut command_tick = tokio::time::interval(Duration::from_millis(8));
    command_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut input_database = Database::new();
    let mut pointer_buttons = 0u8;
    let mut surface: Option<(u64, u16, u16)> = None;
    let mut disconnect_requested = false;

    let outcome = loop {
        tokio::select! {
            output = output_receiver.recv() => {
                let Some(output) = output else {
                    break if disconnect_requested {
                        Ok(())
                    } else {
                        Err(SessionError::new("rdp_output_channel_closed"))
                    };
                };
                match output {
                    RdpOutputEvent::Image { buffer, width, height } => {
                        let width = width.get();
                        let height = height.get();
                        let generation = match surface {
                            Some((generation, old_width, old_height))
                                if old_width == width && old_height == height => generation,
                            Some((generation, _, _)) => generation.saturating_add(1),
                            None => 1,
                        };
                        if surface != Some((generation, width, height)) {
                            surface = Some((generation, width, height));
                            events.emit(SessionEvent::SurfaceReset {
                                generation,
                                width: u32::from(width),
                                height: u32::from(height),
                                format: RemotePixelFormat::Bgra8Srgb,
                            })?;
                            events.emit(SessionEvent::Render(RenderUpdate::reset(
                                generation,
                                u32::from(width),
                                u32::from(height),
                                RemotePixelFormat::Bgra8Srgb,
                            ).map_err(|_| SessionError::new("rdp_surface_invalid"))?))?;
                            events.emit(SessionEvent::Connected { generation })?;
                        }
                        for update in normalize_rdp_image(generation, &buffer, width, height)? {
                            events.emit(SessionEvent::Render(update))?;
                        }
                    }
                    RdpOutputEvent::ConnectionFailure(_) => {
                        break Err(SessionError::new("rdp_connection_failed"));
                    }
                    RdpOutputEvent::Terminated(result) => {
                        if result.is_err() && !disconnect_requested {
                            break Err(SessionError::new("rdp_session_failed"));
                        }
                        if !disconnect_requested {
                            events.emit(SessionEvent::Disconnecting)?;
                        }
                        events.emit(SessionEvent::Disconnected)?;
                        break Ok(());
                    }
                    RdpOutputEvent::PointerDefault
                    | RdpOutputEvent::PointerHidden
                    | RdpOutputEvent::PointerPosition { .. }
                    | RdpOutputEvent::PointerBitmap(_) => {}
                }
            }
            _ = command_tick.tick() => {
                loop {
                    let command = match commands.try_recv() {
                        Ok(command) => command,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => SessionCommand::Disconnect,
                    };
                    if send_rdp_command(
                        command,
                        &input_sender,
                        &mut input_database,
                        &mut pointer_buttons,
                    )? {
                        if !disconnect_requested {
                            disconnect_requested = true;
                            events.emit(SessionEvent::Disconnecting)?;
                        }
                        break;
                    }
                }
            }
        }
    };
    if outcome.is_err() {
        let _ = input_sender.send(RdpInputEvent::Close);
    }
    if !worker.is_finished() {
        worker.abort();
    }
    outcome
}

fn send_rdp_command(
    command: SessionCommand,
    sender: &mpsc::UnboundedSender<RdpInputEvent>,
    database: &mut Database,
    pointer_buttons: &mut u8,
) -> Result<bool, SessionError> {
    match command {
        SessionCommand::Pointer { x, y, buttons } => {
            let x = u16::try_from(x).map_err(|_| SessionError::new("rdp_pointer_out_of_range"))?;
            let y = u16::try_from(y).map_err(|_| SessionError::new("rdp_pointer_out_of_range"))?;
            let mut operations = vec![Operation::MouseMove(MousePosition { x, y })];
            for (mask, button) in [
                (1, MouseButton::Left),
                (2, MouseButton::Middle),
                (4, MouseButton::Right),
            ] {
                let was_pressed = *pointer_buttons & mask != 0;
                let is_pressed = buttons & mask != 0;
                if was_pressed != is_pressed {
                    operations.push(if is_pressed {
                        Operation::MouseButtonPressed(button)
                    } else {
                        Operation::MouseButtonReleased(button)
                    });
                }
            }
            for (mask, is_vertical, rotation_units) in [
                (8, true, 100),
                (16, true, -100),
                (32, false, 100),
                (64, false, -100),
            ] {
                if buttons & mask != 0 {
                    operations.push(Operation::WheelRotations(WheelRotations {
                        is_vertical,
                        rotation_units,
                    }));
                }
            }
            *pointer_buttons = buttons & 0x07;
            send_fast_path(sender, database.apply(operations))?;
        }
        SessionCommand::Key {
            physical_code: Some(code),
            pressed,
            ..
        } => {
            let code =
                u16::try_from(code).map_err(|_| SessionError::new("rdp_scancode_out_of_range"))?;
            let scancode = Scancode::from_u16(code);
            let operation = if pressed {
                Operation::KeyPressed(scancode)
            } else {
                Operation::KeyReleased(scancode)
            };
            send_fast_path(sender, database.apply([operation]))?;
        }
        SessionCommand::Key {
            physical_code: None,
            ..
        } => {}
        SessionCommand::Resize { width, height } => {
            let width =
                u16::try_from(width).map_err(|_| SessionError::new("rdp_resize_out_of_range"))?;
            let height =
                u16::try_from(height).map_err(|_| SessionError::new("rdp_resize_out_of_range"))?;
            if width != 0 && height != 0 {
                sender
                    .send(RdpInputEvent::Resize {
                        width,
                        height,
                        scale_factor: 100,
                        physical_size: None,
                    })
                    .map_err(|_| SessionError::new("rdp_input_channel_closed"))?;
            }
        }
        SessionCommand::Disconnect => {
            sender
                .send(RdpInputEvent::Close)
                .map_err(|_| SessionError::new("rdp_input_channel_closed"))?;
            return Ok(true);
        }
        SessionCommand::ClipboardText(_) | SessionCommand::RequestFullFrame => {}
    }
    Ok(false)
}

fn send_fast_path(
    sender: &mpsc::UnboundedSender<RdpInputEvent>,
    events: smallvec::SmallVec<[ironrdp::pdu::input::fast_path::FastPathInputEvent; 2]>,
) -> Result<(), SessionError> {
    if !events.is_empty() {
        sender
            .send(RdpInputEvent::FastPath(events))
            .map_err(|_| SessionError::new("rdp_input_channel_closed"))?;
    }
    Ok(())
}

pub fn normalize_rdp_image(
    generation: u64,
    pixels: &[u32],
    width: u16,
    height: u16,
) -> Result<Vec<RenderUpdate>, SessionError> {
    let expected = usize::from(width)
        .checked_mul(usize::from(height))
        .ok_or_else(|| SessionError::new("rdp_image_dimensions_overflow"))?;
    if pixels.len() != expected || width == 0 || height == 0 {
        return Err(SessionError::new("rdp_image_length_mismatch"));
    }
    let rect = FrameRect::new(0, 0, u32::from(width), u32::from(height))
        .map_err(|_| SessionError::new("rdp_image_rect_invalid"))?;
    let mut bgra = Vec::with_capacity(expected.saturating_mul(4));
    for pixel in pixels {
        let [blue, green, red, _] = pixel.to_le_bytes();
        bgra.extend_from_slice(&[blue, green, red, 0xff]);
    }
    let dirty = RenderUpdate::dirty_rect(
        generation,
        rect,
        RemotePixelFormat::Bgra8Srgb,
        u32::from(width) * RemotePixelFormat::Bgra8Srgb.bytes_per_pixel(),
        bgra.into_boxed_slice(),
    )
    .map_err(|_| SessionError::new("rdp_render_update_invalid"))?;
    Ok(vec![dirty, RenderUpdate::present(generation)])
}

const fn current_platform() -> MajorPlatformType {
    #[cfg(target_os = "windows")]
    {
        MajorPlatformType::WINDOWS
    }
    #[cfg(target_os = "macos")]
    {
        MajorPlatformType::MACINTOSH
    }
    #[cfg(target_os = "linux")]
    {
        MajorPlatformType::UNIX
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        MajorPlatformType::UNSPECIFIED
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::{Ipv4Addr, TcpListener};
    use std::thread;

    use secrecy::SecretString;

    use super::*;
    use crate::app::connection::{validate_connection, ConnectionRequest, ServiceKind};

    #[test]
    fn auto_rdp_config_separates_pinned_tcp_address_from_tls_identity() {
        let connection = validate_connection(ConnectionRequest {
            service: ServiceKind::Auto,
            host: "rdp.example".to_owned(),
            port: None,
            username: "desktop-user".to_owned(),
            password: SecretString::from("secret".to_owned()),
            domain: None,
        })
        .unwrap()
        .select_auto_protocol(ProtocolKind::Rdp, "192.0.2.55:3389".parse().unwrap())
        .unwrap();

        let config = build_rdp_config(connection, RdpDesktopConfig::default()).unwrap();

        assert_eq!(config.destination().name(), "rdp.example");
        assert_eq!(
            config.connect_addr(),
            Some("192.0.2.55:3389".parse().unwrap())
        );
    }

    #[test]
    fn ironrdp_direct_transport_connects_only_to_the_pinned_socket() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let pinned = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 64];
            let count = stream.read(&mut request).unwrap();
            request[..count].to_vec()
        });
        let connection = validate_connection(ConnectionRequest {
            service: ServiceKind::Auto,
            host: "must-not-resolve.invalid".to_owned(),
            port: None,
            username: "desktop-user".to_owned(),
            password: SecretString::from("secret".to_owned()),
            domain: None,
        })
        .unwrap()
        .select_auto_protocol(ProtocolKind::Rdp, pinned)
        .unwrap();
        let config = build_rdp_config(connection, RdpDesktopConfig::default())
            .unwrap()
            .into_inner();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let (output, _receiver) = mpsc::channel(4);
            tokio::time::timeout(Duration::from_secs(1), RdpClient::new(config, output).run())
                .await
                .unwrap();
        });

        let request = server.join().unwrap();
        assert!(request.starts_with(&[0x03, 0x00]));
    }
}
