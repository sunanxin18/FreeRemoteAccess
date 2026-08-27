//! RFB 协议的根侧常量、认证策略与消息兼容包装。
//! 纯切片编解码由 `frd-wire-rfb` 提供；本模块保留会话策略和既有调用接口。

use anyhow::{bail, Result};

pub use super::auth::VNC_AUTH_CHALLENGE_BYTES;
pub use frd_wire_rfb::{ParsedBanner as ParsedRfbBanner, PixelFormat, RFB_BANNER_BYTES};

pub const RFB_SECURITY_RESULT_OK: u32 = 0;
pub const RFB_VERSION_3_3: (u8, u8) = (3, 3);
pub const RFB_VERSION_3_7: (u8, u8) = (3, 7);
pub const RFB_VERSION_3_8: (u8, u8) = (3, 8);
pub const RFB_PIXEL_WIDTH_BYTES: usize = size_of::<u32>();
pub const SECURITY_FAILURE_REASON_MAX_BYTES: usize = 4096;
pub const SERVER_COLOUR_MAP_PADDING_BYTES: usize = 3;
pub const SERVER_COLOUR_MAP_ENTRY_WIDTH_BYTES: usize = 6;
pub const SERVER_CUT_TEXT_PADDING_BYTES: usize = 3;
pub const SERVER_CUT_TEXT_MAX_BYTES: usize = 1 << 20;
pub const SET_PIXEL_FORMAT_MESSAGE_BYTES: usize = frd_wire_rfb::SET_PIXEL_FORMAT_MESSAGE_BYTES;
pub const FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES: usize =
    frd_wire_rfb::FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES;

/// 帧编码：原始像素。
pub const RAW: i32 = 0;
/// 帧编码：矩形拷贝。
pub const COPYRECT: i32 = 1;
pub const SUPPORTED_ENCODINGS: &[i32] = &[COPYRECT, RAW];

pub mod limits {
    pub const RFB_BYTES_PER_PIXEL: usize = super::RFB_PIXEL_WIDTH_BYTES;
    pub const BINARY_MEBIBYTE_BYTES: usize = 1024 * 1024;
    pub const MAX_FRAMEBUFFER_BYTES: usize = 256 * BINARY_MEBIBYTE_BYTES;
    pub const MAX_FRAMEBUFFER_PIXELS: usize = MAX_FRAMEBUFFER_BYTES / RFB_BYTES_PER_PIXEL;
    pub const MAX_RECTS_PER_UPDATE: usize = 4096;
    pub const MAX_UPDATE_RAW_BYTES: usize = 128 * BINARY_MEBIBYTE_BYTES;
}

#[cfg(feature = "viewer")]
pub mod pointer {
    pub const PRIMARY: u8 = 1;
    pub const MIDDLE: u8 = 2;
    pub const SECONDARY: u8 = 4;
    pub const WHEEL_UP: u8 = 8;
    pub const WHEEL_DOWN: u8 = 16;
    pub const WHEEL_RIGHT: u8 = 32;
    pub const WHEEL_LEFT: u8 = 64;
}

pub mod apple_session {
    pub const SHARED_CLIENT_INIT: u8 = 0x01;
    pub const ENCRYPTED_SESSION_CLIENT_INIT: u8 = 0xc1;
    pub const SERVER_KEEPALIVE_MESSAGE_TYPE: u8 = 0x14;
    pub const SERVER_KEEPALIVE_MESSAGE_LEN: usize = 8;
    pub const SERVER_KEEPALIVE_TYPE_FIELD_LEN: usize = size_of::<u8>();
}

pub mod security {
    pub const INVALID: u8 = 0;
    pub const NONE: u8 = 1;
    pub const VNC_AUTH: u8 = 2;
    pub const TIGHT: u8 = 16;
    pub const ULTRA: u8 = 17;
    pub const VENCRYPT: u8 = 19;
    pub const TLS: u8 = 22;
    pub const APPLE_ARD: u8 = 30;
    pub const APPLE_SRP: u8 = 36;
    pub const APPLE_RSA_SRP: u8 = 33;
    pub const APPLE_ARD_39: u8 = 35;

    pub const fn requires_apple_account_credentials(value: u8) -> bool {
        matches!(value, APPLE_ARD | APPLE_RSA_SRP | APPLE_ARD_39 | APPLE_SRP)
    }
}

pub fn security_type_name(value: u8) -> &'static str {
    match value {
        security::INVALID => "Invalid",
        security::NONE => "None（无认证）",
        security::VNC_AUTH => "VNC Authentication（DES 挑战-响应）",
        security::TIGHT => "Tight",
        security::ULTRA => "Ultra",
        security::VENCRYPT => "VeNCrypt",
        security::TLS => "TLS",
        security::APPLE_ARD => "Apple Remote Desktop（Diffie-Hellman，macOS 私有）",
        security::APPLE_RSA_SRP => "Apple RSA-SRP 混合（macOS 私有）",
        security::APPLE_SRP => "Apple SRP-6a（SHA-512 + PBKDF2，macOS 私有）",
        security::APPLE_ARD_39 => "Apple Remote Desktop v3.9（macOS 私有）",
        _ => "未知",
    }
}

pub fn parse_rfb_banner(bytes: &[u8]) -> Result<ParsedRfbBanner> {
    Ok(frd_wire_rfb::decode_banner(bytes)?)
}

pub fn encode_rfb_banner(major: u16, minor: u16) -> Result<[u8; RFB_BANNER_BYTES]> {
    Ok(frd_wire_rfb::encode_banner(major, minor)?)
}

pub fn msg_set_pixel_format(pixel_format: &PixelFormat) -> [u8; SET_PIXEL_FORMAT_MESSAGE_BYTES] {
    frd_wire_rfb::encode_set_pixel_format(*pixel_format)
}

pub fn msg_set_encodings(encodings: &[i32]) -> Result<Vec<u8>> {
    Ok(frd_wire_rfb::encode_set_encodings(encodings)?)
}

pub fn msg_fb_update_request(
    incremental: bool,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
) -> Result<[u8; FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES]> {
    Ok(frd_wire_rfb::encode_framebuffer_update_request(
        incremental,
        x,
        y,
        width,
        height,
    )?)
}

/// 标准 RFB 客户端消息类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
#[repr(u8)]
pub enum RfbClientMessageType {
    SetPixelFormat = 0,
    SetEncodings = 2,
    FramebufferUpdateRequest = 3,
    KeyEvent = 4,
    PointerEvent = 5,
}

/// 标准 RFB 服务器消息类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RfbServerMessageType {
    FramebufferUpdate = 0,
    SetColourMapEntries = 1,
    Bell = 2,
    ServerCutText = 3,
}

impl TryFrom<u8> for RfbServerMessageType {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::FramebufferUpdate),
            1 => Ok(Self::SetColourMapEntries),
            2 => Ok(Self::Bell),
            3 => Ok(Self::ServerCutText),
            _ => bail!("未知标准 RFB 服务器消息类型 {value}"),
        }
    }
}

#[cfg(feature = "viewer")]
pub fn msg_key_event(down: bool, keysym: u32) -> [u8; 8] {
    let mut message = [0_u8; 8];
    message[0] = RfbClientMessageType::KeyEvent as u8;
    message[1] = u8::from(down);
    message[4..].copy_from_slice(&keysym.to_be_bytes());
    message
}

#[cfg(feature = "viewer")]
pub fn msg_pointer_event(button_mask: u8, x: u16, y: u16) -> [u8; 6] {
    let mut message = [0_u8; 6];
    message[0] = RfbClientMessageType::PointerEvent as u8;
    message[1] = button_mask;
    message[2..4].copy_from_slice(&x.to_be_bytes());
    message[4..].copy_from_slice(&y.to_be_bytes());
    message
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(feature = "viewer")]
    fn pointer_mask_owners_match_independent_boundary_and_combined_fixtures() {
        use super::{msg_pointer_event, pointer};

        assert_eq!(
            [
                pointer::PRIMARY,
                pointer::MIDDLE,
                pointer::SECONDARY,
                pointer::WHEEL_UP,
                pointer::WHEEL_DOWN,
                pointer::WHEEL_RIGHT,
                pointer::WHEEL_LEFT,
            ],
            [1, 2, 4, 8, 16, 32, 64]
        );
        assert_eq!(
            msg_pointer_event(pointer::PRIMARY | pointer::WHEEL_LEFT, u16::MAX, 0),
            [5, 65, 0xff, 0xff, 0, 0]
        );
    }
}
