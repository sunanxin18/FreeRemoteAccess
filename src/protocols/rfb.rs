use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use crossbeam_channel::{Receiver, TryRecvError};
use secrecy::ExposeSecret;

use crate::app::connection::ProtocolKind;
use crate::core::{FrameRect, RemotePixelFormat, RenderUpdate};
use crate::framebuffer::Framebuffer;
use crate::protocols::ProtocolAdapter;
use crate::session::{
    ProtocolContext, SessionCommand, SessionError, SessionEvent, SessionEventSink,
};
use crate::vnc::client::{self, RectOp, SecurityPolicy, ServerEvent, VncClient};
use crate::vnc::{protocol, session};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_POLL_TIMEOUT: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RfbAuthenticationMode {
    StandardCompatible,
    AppleNativeAccount,
}

pub struct RfbAdapter {
    authentication_mode: RfbAuthenticationMode,
}

impl RfbAdapter {
    pub const fn standard() -> Self {
        Self {
            authentication_mode: RfbAuthenticationMode::StandardCompatible,
        }
    }

    pub const fn apple_native() -> Self {
        Self {
            authentication_mode: RfbAuthenticationMode::AppleNativeAccount,
        }
    }

    fn connect(&self, context: ProtocolContext) -> Result<VncClient, SessionError> {
        let connection = context.into_connection();
        let expected_protocol = match self.authentication_mode {
            RfbAuthenticationMode::StandardCompatible => ProtocolKind::StandardRfb,
            RfbAuthenticationMode::AppleNativeAccount => ProtocolKind::AppleRfb,
        };
        if connection.protocol != expected_protocol {
            return Err(SessionError::new("rfb_protocol_mismatch"));
        }
        let address = resolve_endpoint(connection.endpoint.host(), connection.endpoint.port())?;
        let policy = match self.authentication_mode {
            RfbAuthenticationMode::StandardCompatible => SecurityPolicy::PreferAppleThenVnc,
            RfbAuthenticationMode::AppleNativeAccount => SecurityPolicy::AppleNativeOnly,
        };
        VncClient::connect_timeout_with_policy(
            &address,
            CONNECT_TIMEOUT,
            Some(&connection.username),
            Some(connection.password.expose_secret()),
            session::SessionEncodingProfile::Raw,
            policy,
        )
        .map_err(|_| SessionError::new("rfb_connect_failed"))
    }
}

impl ProtocolAdapter for RfbAdapter {
    fn run(
        self: Box<Self>,
        context: ProtocolContext,
        commands: Receiver<SessionCommand>,
        events: SessionEventSink,
    ) -> Result<(), SessionError> {
        events.emit(SessionEvent::Connecting)?;
        let mut client = self.connect(context)?;
        client
            .init_session()
            .map_err(|_| SessionError::new("rfb_session_init_failed"))?;
        client
            .conn
            .set_read_timeout(Some(IO_POLL_TIMEOUT))
            .map_err(|_| SessionError::new("rfb_timeout_config_failed"))?;
        let width = u32::from(client.width);
        let height = u32::from(client.height);
        let generation = 1;
        let mut normalizer = RfbFrameNormalizer::new(generation, width, height)?;
        events.emit(SessionEvent::SurfaceReset {
            generation,
            width,
            height,
            format: RemotePixelFormat::Bgra8Srgb,
        })?;
        events.emit(SessionEvent::Render(
            RenderUpdate::reset(generation, width, height, RemotePixelFormat::Bgra8Srgb)
                .map_err(|_| SessionError::new("rfb_surface_invalid"))?,
        ))?;
        events.emit(SessionEvent::Connected { generation })?;
        client
            .request_update(false)
            .map_err(|_| SessionError::new("rfb_initial_update_failed"))?;

        loop {
            if drain_commands(&mut client, &commands, &events)? {
                return Ok(());
            }
            match client::read_server_message(&mut client.conn) {
                Ok(ServerEvent::Update(ops)) => {
                    for update in normalizer.normalize(ops, false)? {
                        events.emit(SessionEvent::Render(update))?;
                    }
                    client
                        .request_update(true)
                        .map_err(|_| SessionError::new("rfb_incremental_request_failed"))?;
                }
                Ok(ServerEvent::ServerCutText(text)) => {
                    events.emit(SessionEvent::ClipboardText(text))?;
                }
                Ok(ServerEvent::Bell) => events.emit(SessionEvent::Bell)?,
                Ok(ServerEvent::Ignored) => {}
                Err(error) if client::is_timeout(&error) => {}
                Err(_) => return Err(SessionError::new("rfb_read_failed")),
            }
        }
    }
}

fn drain_commands(
    client: &mut VncClient,
    commands: &Receiver<SessionCommand>,
    events: &SessionEventSink,
) -> Result<bool, SessionError> {
    loop {
        let command = match commands.try_recv() {
            Ok(command) => command,
            Err(TryRecvError::Empty) => return Ok(false),
            Err(TryRecvError::Disconnected) => SessionCommand::Disconnect,
        };
        match command {
            SessionCommand::Pointer { x, y, buttons } => {
                let x = u16::try_from(x).map_err(|_| SessionError::new("pointer_out_of_range"))?;
                let y = u16::try_from(y).map_err(|_| SessionError::new("pointer_out_of_range"))?;
                client
                    .conn
                    .write_all(&protocol::msg_pointer_event(buttons, x, y))
                    .map_err(|_| SessionError::new("rfb_pointer_send_failed"))?;
            }
            SessionCommand::Key { scan_code, pressed } => {
                client
                    .conn
                    .write_all(&protocol::msg_key_event(pressed, scan_code))
                    .map_err(|_| SessionError::new("rfb_key_send_failed"))?;
            }
            SessionCommand::ClipboardText(text) => {
                let message = protocol::msg_client_cut_text(&text)
                    .map_err(|_| SessionError::new("rfb_clipboard_invalid"))?;
                client
                    .conn
                    .write_all(&message)
                    .map_err(|_| SessionError::new("rfb_clipboard_send_failed"))?;
            }
            SessionCommand::Resize { .. } | SessionCommand::RequestFullFrame => {
                client
                    .request_update(false)
                    .map_err(|_| SessionError::new("rfb_full_update_failed"))?;
            }
            SessionCommand::Disconnect => {
                events.emit(SessionEvent::Disconnecting)?;
                events.emit(SessionEvent::Disconnected)?;
                return Ok(true);
            }
        }
    }
}

fn resolve_endpoint(host: &str, port: u16) -> Result<SocketAddr, SessionError> {
    (host, port)
        .to_socket_addrs()
        .map_err(|_| SessionError::new("endpoint_resolution_failed"))?
        .next()
        .ok_or_else(|| SessionError::new("endpoint_resolution_empty"))
}

pub struct RfbFrameNormalizer {
    generation: u64,
    width: u32,
    height: u32,
    framebuffer: Framebuffer,
}

impl RfbFrameNormalizer {
    pub fn new(generation: u64, width: u32, height: u32) -> Result<Self, SessionError> {
        let framebuffer = Framebuffer::new(
            usize::try_from(width).map_err(|_| SessionError::new("rfb_surface_invalid"))?,
            usize::try_from(height).map_err(|_| SessionError::new("rfb_surface_invalid"))?,
        )
        .map_err(|_| SessionError::new("rfb_surface_invalid"))?;
        Ok(Self {
            generation,
            width,
            height,
            framebuffer,
        })
    }

    pub fn normalize(
        &mut self,
        ops: Vec<RectOp>,
        include_reset: bool,
    ) -> Result<Vec<RenderUpdate>, SessionError> {
        let mut rects = Vec::with_capacity(ops.len());
        for op in &ops {
            let (x, y, width, height) = match op {
                RectOp::Raw { x, y, w, h, pixels } => {
                    if pixels.len() != w.saturating_mul(*h) {
                        return Err(SessionError::new("rfb_raw_length_mismatch"));
                    }
                    (*x, *y, *w, *h)
                }
                RectOp::Copy { x, y, w, h, sx, sy } => {
                    validate_rect(*sx, *sy, *w, *h, self.width, self.height)?;
                    (*x, *y, *w, *h)
                }
            };
            validate_rect(x, y, width, height, self.width, self.height)?;
            rects.push((x, y, width, height));
        }
        self.framebuffer.apply(&ops);

        let mut updates = Vec::with_capacity(rects.len() + 2);
        if include_reset {
            updates.push(
                RenderUpdate::reset(
                    self.generation,
                    self.width,
                    self.height,
                    RemotePixelFormat::Bgra8Srgb,
                )
                .map_err(|_| SessionError::new("rfb_surface_invalid"))?,
            );
        }
        for (x, y, width, height) in rects {
            let rect = FrameRect::new(
                u32::try_from(x).map_err(|_| SessionError::new("rfb_rect_invalid"))?,
                u32::try_from(y).map_err(|_| SessionError::new("rfb_rect_invalid"))?,
                u32::try_from(width).map_err(|_| SessionError::new("rfb_rect_invalid"))?,
                u32::try_from(height).map_err(|_| SessionError::new("rfb_rect_invalid"))?,
            )
            .map_err(|_| SessionError::new("rfb_rect_invalid"))?;
            let pixels = extract_bgra(&self.framebuffer, x, y, width, height);
            let bytes_per_row = rect
                .width()
                .checked_mul(RemotePixelFormat::Bgra8Srgb.bytes_per_pixel())
                .ok_or_else(|| SessionError::new("rfb_stride_overflow"))?;
            updates.push(
                RenderUpdate::dirty_rect(
                    self.generation,
                    rect,
                    RemotePixelFormat::Bgra8Srgb,
                    bytes_per_row,
                    pixels.into_boxed_slice(),
                )
                .map_err(|_| SessionError::new("rfb_render_update_invalid"))?,
            );
        }
        updates.push(RenderUpdate::present(self.generation));
        Ok(updates)
    }
}

pub fn normalize_rect_ops(
    generation: u64,
    width: u32,
    height: u32,
    ops: Vec<RectOp>,
    include_reset: bool,
) -> Result<Vec<RenderUpdate>, SessionError> {
    RfbFrameNormalizer::new(generation, width, height)?.normalize(ops, include_reset)
}

fn validate_rect(
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    surface_width: u32,
    surface_height: u32,
) -> Result<(), SessionError> {
    let right = x
        .checked_add(width)
        .ok_or_else(|| SessionError::new("rfb_rect_overflow"))?;
    let bottom = y
        .checked_add(height)
        .ok_or_else(|| SessionError::new("rfb_rect_overflow"))?;
    if width == 0
        || height == 0
        || right > surface_width as usize
        || bottom > surface_height as usize
    {
        return Err(SessionError::new("rfb_rect_out_of_bounds"));
    }
    Ok(())
}

fn extract_bgra(
    framebuffer: &Framebuffer,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> Vec<u8> {
    let mut output = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let offset = (y + row) * framebuffer.width + x;
        for pixel in &framebuffer.pixels()[offset..offset + width] {
            let [blue, green, red, _] = pixel.to_le_bytes();
            output.extend_from_slice(&[blue, green, red, 0xff]);
        }
    }
    output
}
