//! Apple 专属的 RFB 安全类型与会话常量。

pub const RFB_SECURITY_RESULT_OK: u32 = 0;
pub const RAW: i32 = 0;
pub const MEDIA_STREAM_CONTROL_ENCODING: i32 = 0x03f2;
pub const FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES: usize =
    frd_wire_rfb::FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES;

pub mod limits {
    pub const RFB_BYTES_PER_PIXEL: usize = size_of::<u32>();
    pub const BINARY_MEBIBYTE_BYTES: usize = 1024 * 1024;
    pub const MAX_FRAMEBUFFER_BYTES: usize = 256 * BINARY_MEBIBYTE_BYTES;
    pub const MAX_FRAMEBUFFER_PIXELS: usize = MAX_FRAMEBUFFER_BYTES / RFB_BYTES_PER_PIXEL;
    pub const MAX_UPDATE_RAW_BYTES: usize = 128 * BINARY_MEBIBYTE_BYTES;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RfbClientMessageType {
    FramebufferUpdateRequest = 3,
    PointerEvent = 5,
}

pub mod pointer {
    pub const PRIMARY: u8 = 1;
    pub const MIDDLE: u8 = 2;
    pub const SECONDARY: u8 = 4;
    pub const WHEEL_UP: u8 = 8;
    pub const WHEEL_DOWN: u8 = 16;
    pub const WHEEL_RIGHT: u8 = 32;
    pub const WHEEL_LEFT: u8 = 64;
}

pub fn msg_fb_update_request(
    incremental: bool,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
) -> anyhow::Result<[u8; FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES]> {
    Ok(frd_wire_rfb::encode_framebuffer_update_request(
        incremental,
        x,
        y,
        width,
        height,
    )?)
}

pub fn msg_pointer_event(button_mask: u8, x: u16, y: u16) -> [u8; 6] {
    let mut message = [0_u8; 6];
    message[0] = RfbClientMessageType::PointerEvent as u8;
    message[1] = button_mask;
    message[2..4].copy_from_slice(&x.to_be_bytes());
    message[4..].copy_from_slice(&y.to_be_bytes());
    message
}

pub mod apple_session {
    pub const SHARED_CLIENT_INIT: u8 = 0x01;
    pub const ENCRYPTED_SESSION_CLIENT_INIT: u8 = 0xc1;
    pub const SERVER_KEEPALIVE_MESSAGE_TYPE: u8 = 0x14;
    pub const SERVER_KEEPALIVE_MESSAGE_LEN: usize = 8;
    pub const SERVER_KEEPALIVE_TYPE_FIELD_LEN: usize = size_of::<u8>();
}

pub mod security {
    pub const APPLE_ARD: u8 = 30;
    pub const APPLE_RSA_SRP: u8 = 33;
    pub const APPLE_ARD_39: u8 = 35;
    pub const APPLE_SRP: u8 = 36;

    pub const fn requires_apple_account_credentials(value: u8) -> bool {
        matches!(value, APPLE_ARD | APPLE_RSA_SRP | APPLE_ARD_39 | APPLE_SRP)
    }
}
