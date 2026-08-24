//! RFB（Remote Framebuffer，RFC 6143）协议常量与客户端消息编码。
//! 除像素数据（取决于协商出的像素格式）外，所有多字节整数一律大端序。

use anyhow::{bail, ensure, Context, Result};

pub use super::auth::VNC_AUTH_CHALLENGE_BYTES;

pub const RFB_SECURITY_RESULT_OK: u32 = 0;
pub const RFB_VERSION_3_3: (u8, u8) = (3, 3);
pub const RFB_VERSION_3_7: (u8, u8) = (3, 7);
pub const RFB_VERSION_3_8: (u8, u8) = (3, 8);

pub const RFB_BANNER_PREFIX: &[u8; 4] = b"RFB ";
pub const RFB_VERSION_FIELD_BYTES: usize = 3;
pub const RFB_VERSION_SEPARATOR: u8 = b'.';
pub const RFB_BANNER_TERMINATOR: u8 = b'\n';
pub const RFB_BANNER_MAJOR_OFFSET: usize = RFB_BANNER_PREFIX.len();
pub const RFB_BANNER_MAJOR_END: usize = RFB_BANNER_MAJOR_OFFSET + RFB_VERSION_FIELD_BYTES;
pub const RFB_BANNER_SEPARATOR_OFFSET: usize = RFB_BANNER_MAJOR_END;
pub const RFB_BANNER_MINOR_OFFSET: usize = RFB_BANNER_SEPARATOR_OFFSET + size_of::<u8>();
pub const RFB_BANNER_MINOR_END: usize = RFB_BANNER_MINOR_OFFSET + RFB_VERSION_FIELD_BYTES;
pub const RFB_BANNER_TERMINATOR_OFFSET: usize = RFB_BANNER_MINOR_END;
pub const RFB_BANNER_BYTES: usize = RFB_BANNER_TERMINATOR_OFFSET + size_of::<u8>();
const RFB_VERSION_DECIMAL_RADIX: u16 = 10;
const RFB_VERSION_FIELD_MAX: u16 = 999;

#[derive(Debug, Eq, PartialEq)]
pub struct ParsedRfbBanner {
    pub wire: [u8; RFB_BANNER_BYTES],
    pub display: String,
    pub major: u16,
    pub minor: u16,
}

fn parse_rfb_decimal_field(field: &[u8], name: &str) -> Result<u16> {
    ensure!(
        field.len() == RFB_VERSION_FIELD_BYTES,
        "RFB {name} 版本字段长度非法"
    );
    field.iter().try_fold(0u16, |value, byte| {
        ensure!(byte.is_ascii_digit(), "RFB {name} 版本字段不是十进制数字");
        value
            .checked_mul(RFB_VERSION_DECIMAL_RADIX)
            .and_then(|v| v.checked_add(u16::from(*byte - b'0')))
            .ok_or_else(|| anyhow::anyhow!("RFB {name} 版本字段溢出"))
    })
}

fn encode_rfb_decimal_field(value: u16, name: &str) -> Result<[u8; RFB_VERSION_FIELD_BYTES]> {
    ensure!(
        value <= RFB_VERSION_FIELD_MAX,
        "RFB {name} 版本值超过三位十进制字段: {value}"
    );
    let mut field = [b'0'; RFB_VERSION_FIELD_BYTES];
    let mut remaining = value;
    for digit in field.iter_mut().rev() {
        *digit = b'0'
            + u8::try_from(remaining % RFB_VERSION_DECIMAL_RADIX)
                .expect("十进制单个数字一定可放入 u8");
        remaining /= RFB_VERSION_DECIMAL_RADIX;
    }
    debug_assert_eq!(remaining, 0);
    Ok(field)
}

pub fn encode_rfb_banner(major: u16, minor: u16) -> Result<[u8; RFB_BANNER_BYTES]> {
    let major = encode_rfb_decimal_field(major, "major")?;
    let minor = encode_rfb_decimal_field(minor, "minor")?;
    let mut wire = [0u8; RFB_BANNER_BYTES];
    wire[..RFB_BANNER_PREFIX.len()].copy_from_slice(RFB_BANNER_PREFIX);
    wire[RFB_BANNER_MAJOR_OFFSET..RFB_BANNER_MAJOR_END].copy_from_slice(&major);
    wire[RFB_BANNER_SEPARATOR_OFFSET] = RFB_VERSION_SEPARATOR;
    wire[RFB_BANNER_MINOR_OFFSET..RFB_BANNER_MINOR_END].copy_from_slice(&minor);
    wire[RFB_BANNER_TERMINATOR_OFFSET] = RFB_BANNER_TERMINATOR;
    Ok(wire)
}

pub fn parse_rfb_banner(bytes: &[u8]) -> Result<ParsedRfbBanner> {
    let wire: [u8; RFB_BANNER_BYTES] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("RFB banner 长度必须为 {RFB_BANNER_BYTES} 字节"))?;
    ensure!(
        &wire[..RFB_BANNER_PREFIX.len()] == RFB_BANNER_PREFIX,
        "不是 RFB banner"
    );
    ensure!(
        wire[RFB_BANNER_SEPARATOR_OFFSET] == RFB_VERSION_SEPARATOR,
        "RFB banner 缺少版本分隔符"
    );
    ensure!(
        wire[RFB_BANNER_TERMINATOR_OFFSET] == RFB_BANNER_TERMINATOR,
        "RFB banner 终止符非法"
    );

    let major = parse_rfb_decimal_field(
        &wire[RFB_BANNER_MAJOR_OFFSET..RFB_BANNER_MAJOR_END],
        "major",
    )?;
    let minor = parse_rfb_decimal_field(
        &wire[RFB_BANNER_MINOR_OFFSET..RFB_BANNER_MINOR_END],
        "minor",
    )?;
    let display = std::str::from_utf8(&wire[..RFB_BANNER_TERMINATOR_OFFSET])?.to_owned();
    Ok(ParsedRfbBanner {
        wire,
        display,
        major,
        minor,
    })
}

/// 标准 RFB 客户端消息类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "media"), allow(dead_code))]
#[repr(u8)]
pub enum RfbClientMessageType {
    SetPixelFormat = 0,
    SetEncodings = 2,
    FramebufferUpdateRequest = 3,
    KeyEvent = 4,
    PointerEvent = 5,
    ClientCutText = 6,
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
            value if value == Self::FramebufferUpdate as u8 => Ok(Self::FramebufferUpdate),
            value if value == Self::SetColourMapEntries as u8 => Ok(Self::SetColourMapEntries),
            value if value == Self::Bell as u8 => Ok(Self::Bell),
            value if value == Self::ServerCutText as u8 => Ok(Self::ServerCutText),
            _ => bail!("未知标准 RFB 服务器消息类型 {value}"),
        }
    }
}

pub const RFB_CLIENT_MESSAGE_TYPE_WIDTH_BYTES: usize = size_of::<u8>();
pub const RFB_CLIENT_MESSAGE_TYPE_OFFSET: usize = 0;
pub const RFB_PIXEL_FORMAT_BYTES: usize = 16;
pub const RFB_PIXEL_WIDTH_BYTES: usize = size_of::<u32>();
pub const SERVER_INIT_DESKTOP_NAME_MAX_BYTES: usize = 65536;
pub const SECURITY_FAILURE_REASON_MAX_BYTES: usize = 4096;
pub const SERVER_COLOUR_MAP_PADDING_BYTES: usize = 3;
pub const SERVER_COLOUR_MAP_ENTRY_WIDTH_BYTES: usize = 6;
pub const SERVER_CUT_TEXT_PADDING_BYTES: usize = 3;
pub const SERVER_CUT_TEXT_MAX_BYTES: usize = 1 << 20;

pub const SET_PIXEL_FORMAT_PADDING_BYTES: usize = 3;
pub const SET_PIXEL_FORMAT_MESSAGE_BYTES: usize =
    RFB_CLIENT_MESSAGE_TYPE_WIDTH_BYTES + SET_PIXEL_FORMAT_PADDING_BYTES + RFB_PIXEL_FORMAT_BYTES;
pub const SET_ENCODINGS_PADDING_BYTES: usize = size_of::<u8>();
pub const SET_ENCODINGS_COUNT_WIDTH_BYTES: usize = size_of::<u16>();
pub const SET_ENCODINGS_HEADER_BYTES: usize = RFB_CLIENT_MESSAGE_TYPE_WIDTH_BYTES
    + SET_ENCODINGS_PADDING_BYTES
    + SET_ENCODINGS_COUNT_WIDTH_BYTES;
pub const SET_ENCODINGS_ENTRY_WIDTH_BYTES: usize = size_of::<i32>();
pub const FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES: usize = 10;
pub const FRAMEBUFFER_UPDATE_REQUEST_INCREMENTAL_OFFSET: usize =
    RFB_CLIENT_MESSAGE_TYPE_WIDTH_BYTES;
pub const FRAMEBUFFER_UPDATE_REQUEST_X_OFFSET: usize =
    FRAMEBUFFER_UPDATE_REQUEST_INCREMENTAL_OFFSET + size_of::<u8>();
pub const FRAMEBUFFER_UPDATE_REQUEST_Y_OFFSET: usize =
    FRAMEBUFFER_UPDATE_REQUEST_X_OFFSET + size_of::<u16>();
pub const FRAMEBUFFER_UPDATE_REQUEST_WIDTH_OFFSET: usize =
    FRAMEBUFFER_UPDATE_REQUEST_Y_OFFSET + size_of::<u16>();
pub const FRAMEBUFFER_UPDATE_REQUEST_HEIGHT_OFFSET: usize =
    FRAMEBUFFER_UPDATE_REQUEST_WIDTH_OFFSET + size_of::<u16>();
#[cfg_attr(not(feature = "media"), allow(dead_code))]
pub const KEY_EVENT_MESSAGE_BYTES: usize = 8;
#[cfg_attr(not(feature = "media"), allow(dead_code))]
pub const KEY_EVENT_DOWN_OFFSET: usize = RFB_CLIENT_MESSAGE_TYPE_WIDTH_BYTES;
#[cfg_attr(not(feature = "media"), allow(dead_code))]
pub const KEY_EVENT_KEYSYM_OFFSET: usize = RFB_CLIENT_MESSAGE_TYPE_WIDTH_BYTES + 3;
#[cfg_attr(not(feature = "media"), allow(dead_code))]
pub const POINTER_EVENT_MESSAGE_BYTES: usize = 6;
#[cfg_attr(not(feature = "media"), allow(dead_code))]
pub const POINTER_EVENT_BUTTON_MASK_OFFSET: usize = RFB_CLIENT_MESSAGE_TYPE_WIDTH_BYTES;
#[cfg_attr(not(feature = "media"), allow(dead_code))]
pub const POINTER_EVENT_X_OFFSET: usize = POINTER_EVENT_BUTTON_MASK_OFFSET + size_of::<u8>();
#[cfg_attr(not(feature = "media"), allow(dead_code))]
pub const POINTER_EVENT_Y_OFFSET: usize = POINTER_EVENT_X_OFFSET + size_of::<u16>();
pub const CLIENT_CUT_TEXT_PADDING_BYTES: usize = 3;

/// 帧编码：原始像素
pub const RAW: i32 = 0;
/// 帧编码：矩形拷贝（滚动时服务器直接告知“从源位置搬过来”）
pub const COPYRECT: i32 = 1;

/// 本客户端请求的编码集合。Raw 是协议强制要求所有服务器支持的编码，
/// CopyRect 几乎所有服务器都支持；带宽优化编码（Hextile/Tight/ZRLE）暂未实现。
pub const SUPPORTED_ENCODINGS: &[i32] = &[COPYRECT, RAW];

/// 对不可信 RFB/HPSS 尺寸和批量更新的资源预算。
pub mod limits {
    pub const RFB_BYTES_PER_PIXEL: usize = super::RFB_PIXEL_WIDTH_BYTES;
    pub const BINARY_MEBIBYTE_BYTES: usize = 1024 * 1024;
    pub const MAX_FRAMEBUFFER_BYTES: usize = 256 * BINARY_MEBIBYTE_BYTES;
    pub const MAX_FRAMEBUFFER_PIXELS: usize = MAX_FRAMEBUFFER_BYTES / RFB_BYTES_PER_PIXEL;
    pub const MAX_RECTS_PER_UPDATE: usize = 4096;
    pub const MAX_UPDATE_RAW_BYTES: usize = 128 * BINARY_MEBIBYTE_BYTES;
}

/// 标准 RFB PointerEvent 按键与滚轮位掩码。
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
    /// 标准 RFB ClientInit：共享桌面。
    pub const SHARED_CLIENT_INIT: u8 = 0x01;
    /// Apple 私有 ClientInit：共享 + 会话选择 + 独占会话位，进入 SelectSession。
    pub const ENCRYPTED_SESSION_CLIENT_INIT: u8 = 0xc1;
    /// 服务端单向保活通知。客户端只消费，不回写；回写会被服务端当成未知命令 20。
    pub const SERVER_KEEPALIVE_MESSAGE_TYPE: u8 = 0x14;
    pub const SERVER_KEEPALIVE_MESSAGE_LEN: usize = 8;
    pub const SERVER_KEEPALIVE_TYPE_FIELD_LEN: usize = size_of::<u8>();
}

pub mod security {
    pub use crate::protocols::rfb_security::{
        APPLE_ARD, APPLE_ARD_39, APPLE_RSA_SRP, APPLE_SRP, NONE, VNC_AUTH,
    };

    pub const INVALID: u8 = 0;
    pub const TIGHT: u8 = 16;
    pub const ULTRA: u8 = 17;
    pub const VENCRYPT: u8 = 19;
    pub const TLS: u8 = 22;
    pub const fn requires_apple_account_credentials(value: u8) -> bool {
        matches!(value, APPLE_ARD | APPLE_RSA_SRP | APPLE_ARD_39 | APPLE_SRP)
    }
}

pub fn security_type_name(t: u8) -> &'static str {
    match t {
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

/// RFB 像素格式（ServerInit 中占 16 字节）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelFormat {
    pub bits_per_pixel: u8,
    pub depth: u8,
    pub big_endian: u8,
    pub true_colour: u8,
    pub red_max: u16,
    pub green_max: u16,
    pub blue_max: u16,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
}

impl PixelFormat {
    /// 我们向服务器要求的像素格式：
    /// 32bpp / 24 位深度 / 小端 / 真彩色，RGB 排列为 R<<16 | G<<8 | B。
    /// 这样收到的每个 4 字节像素按小端读成 u32 后与 Framebuffer 内部
    /// 存储格式完全一致，无需任何逐像素换算。
    pub const OURS: PixelFormat = PixelFormat {
        bits_per_pixel: 32,
        depth: 24,
        big_endian: 0,
        true_colour: 1,
        red_max: 255,
        green_max: 255,
        blue_max: 255,
        red_shift: 16,
        green_shift: 8,
        blue_shift: 0,
    };

    pub fn parse(b: &[u8]) -> Self {
        Self {
            bits_per_pixel: b[0],
            depth: b[1],
            big_endian: b[2],
            true_colour: b[3],
            red_max: u16::from_be_bytes([b[4], b[5]]),
            green_max: u16::from_be_bytes([b[6], b[7]]),
            blue_max: u16::from_be_bytes([b[8], b[9]]),
            red_shift: b[10],
            green_shift: b[11],
            blue_shift: b[12],
        }
    }

    pub fn to_bytes(self) -> [u8; RFB_PIXEL_FORMAT_BYTES] {
        let mut b = [0u8; RFB_PIXEL_FORMAT_BYTES];
        b[0] = self.bits_per_pixel;
        b[1] = self.depth;
        b[2] = self.big_endian;
        b[3] = self.true_colour;
        b[4..6].copy_from_slice(&self.red_max.to_be_bytes());
        b[6..8].copy_from_slice(&self.green_max.to_be_bytes());
        b[8..10].copy_from_slice(&self.blue_max.to_be_bytes());
        b[10] = self.red_shift;
        b[11] = self.green_shift;
        b[12] = self.blue_shift;
        b
    }
}

/// SetPixelFormat（客户端消息 0，定长 20 字节）
pub fn msg_set_pixel_format(pf: &PixelFormat) -> [u8; SET_PIXEL_FORMAT_MESSAGE_BYTES] {
    let mut m = [0u8; SET_PIXEL_FORMAT_MESSAGE_BYTES];
    m[RFB_CLIENT_MESSAGE_TYPE_OFFSET] = RfbClientMessageType::SetPixelFormat as u8;
    m[RFB_CLIENT_MESSAGE_TYPE_WIDTH_BYTES + SET_PIXEL_FORMAT_PADDING_BYTES..]
        .copy_from_slice(&pf.to_bytes());
    m
}

/// SetEncodings（客户端消息 2）。头部定长 4 字节：类型 + 1 字节填充 + u16 数量
pub fn msg_set_encodings(encodings: &[i32]) -> Result<Vec<u8>> {
    let count = u16::try_from(encodings.len()).context("SetEncodings 编码数量超出 u16 表示范围")?;
    let entry_bytes = encodings
        .len()
        .checked_mul(SET_ENCODINGS_ENTRY_WIDTH_BYTES)
        .context("SetEncodings 编码字节数溢出")?;
    let capacity = SET_ENCODINGS_HEADER_BYTES
        .checked_add(entry_bytes)
        .context("SetEncodings 消息长度溢出")?;
    let mut m = Vec::with_capacity(capacity);
    m.push(RfbClientMessageType::SetEncodings as u8);
    m.extend(std::iter::repeat_n(0, SET_ENCODINGS_PADDING_BYTES));
    m.extend_from_slice(&count.to_be_bytes());
    for e in encodings {
        m.extend_from_slice(&e.to_be_bytes());
    }
    Ok(m)
}

/// FramebufferUpdateRequest（客户端消息 3）
pub fn msg_fb_update_request(
    incremental: bool,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
) -> [u8; FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES] {
    let mut m = [0u8; FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES];
    m[RFB_CLIENT_MESSAGE_TYPE_OFFSET] = RfbClientMessageType::FramebufferUpdateRequest as u8;
    m[FRAMEBUFFER_UPDATE_REQUEST_INCREMENTAL_OFFSET] = u8::from(incremental);
    m[FRAMEBUFFER_UPDATE_REQUEST_X_OFFSET..FRAMEBUFFER_UPDATE_REQUEST_Y_OFFSET]
        .copy_from_slice(&x.to_be_bytes());
    m[FRAMEBUFFER_UPDATE_REQUEST_Y_OFFSET..FRAMEBUFFER_UPDATE_REQUEST_WIDTH_OFFSET]
        .copy_from_slice(&y.to_be_bytes());
    m[FRAMEBUFFER_UPDATE_REQUEST_WIDTH_OFFSET..FRAMEBUFFER_UPDATE_REQUEST_HEIGHT_OFFSET]
        .copy_from_slice(&w.to_be_bytes());
    m[FRAMEBUFFER_UPDATE_REQUEST_HEIGHT_OFFSET..].copy_from_slice(&h.to_be_bytes());
    m
}

/// KeyEvent（客户端消息 4）。keysym 为 X11 键码（如 'a'=0x61, Return=0xff0d）
pub fn msg_key_event(down: bool, keysym: u32) -> [u8; KEY_EVENT_MESSAGE_BYTES] {
    let mut m = [0u8; KEY_EVENT_MESSAGE_BYTES];
    m[RFB_CLIENT_MESSAGE_TYPE_OFFSET] = RfbClientMessageType::KeyEvent as u8;
    m[KEY_EVENT_DOWN_OFFSET] = u8::from(down);
    m[KEY_EVENT_KEYSYM_OFFSET..].copy_from_slice(&keysym.to_be_bytes());
    m
}

/// PointerEvent（客户端消息 5）。
/// 按键位定义：bit0 左键、bit1 中键、bit2 右键、bit3 滚轮上、
/// bit4 滚轮下、bit5 滚轮右、bit6 滚轮左。
pub fn msg_pointer_event(button_mask: u8, x: u16, y: u16) -> [u8; POINTER_EVENT_MESSAGE_BYTES] {
    let mut m = [0u8; POINTER_EVENT_MESSAGE_BYTES];
    m[RFB_CLIENT_MESSAGE_TYPE_OFFSET] = RfbClientMessageType::PointerEvent as u8;
    m[POINTER_EVENT_BUTTON_MASK_OFFSET] = button_mask;
    m[POINTER_EVENT_X_OFFSET..POINTER_EVENT_Y_OFFSET].copy_from_slice(&x.to_be_bytes());
    m[POINTER_EVENT_Y_OFFSET..].copy_from_slice(&y.to_be_bytes());
    m
}

pub fn msg_client_cut_text(text: &str) -> Result<Vec<u8>> {
    let bytes = text.as_bytes();
    ensure!(
        bytes.len() <= SERVER_CUT_TEXT_MAX_BYTES,
        "ClientCutText 长度超过资源预算"
    );
    let length = u32::try_from(bytes.len()).context("ClientCutText 长度超出 u32")?;
    let mut message = Vec::with_capacity(
        RFB_CLIENT_MESSAGE_TYPE_WIDTH_BYTES
            + CLIENT_CUT_TEXT_PADDING_BYTES
            + size_of::<u32>()
            + bytes.len(),
    );
    message.push(RfbClientMessageType::ClientCutText as u8);
    message.extend(std::iter::repeat_n(0, CLIENT_CUT_TEXT_PADDING_BYTES));
    message.extend_from_slice(&length.to_be_bytes());
    message.extend_from_slice(bytes);
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfb_banner_parser_accepts_standard_and_apple_versions() {
        let standard = parse_rfb_banner(b"RFB 003.008\n").unwrap();
        assert_eq!(standard.display, "RFB 003.008");
        assert_eq!(standard.major, 3);
        assert_eq!(standard.minor, 8);

        let apple = parse_rfb_banner(b"RFB 003.889\n").unwrap();
        assert_eq!(apple.display, "RFB 003.889");
        assert_eq!(apple.major, 3);
        assert_eq!(apple.minor, 889);
    }

    #[test]
    fn rfb_banner_parser_rejects_non_ascii_without_panicking() {
        let malformed = [
            b'R', b'F', b'B', b' ', 0xf0, 0x9f, 0x92, b'.', b'0', b'0', b'8', b'\n',
        ];
        assert!(parse_rfb_banner(&malformed).is_err());
    }

    #[test]
    fn rfb_banner_parser_rejects_malformed_fixed_fields() {
        assert!(parse_rfb_banner(b"RFB 03x.008\n").is_err());
        assert!(parse_rfb_banner(b"RFB 003-008\n").is_err());
        assert!(parse_rfb_banner(b"RFB 003.008!").is_err());
    }

    #[test]
    fn rfb_banner_standard_version_owners_encode_independent_reply_fixtures() {
        for (version, expected) in [
            (RFB_VERSION_3_3, *b"RFB 003.003\n"),
            (RFB_VERSION_3_7, *b"RFB 003.007\n"),
            (RFB_VERSION_3_8, *b"RFB 003.008\n"),
        ] {
            assert_eq!(
                encode_rfb_banner(u16::from(version.0), u16::from(version.1)).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn rfb_banner_encoder_rejects_values_that_do_not_fit_three_digits() {
        for (major, minor) in [(1000, 0), (0, 1000), (u16::MAX, u16::MAX)] {
            assert!(encode_rfb_banner(major, minor).is_err());
        }
    }

    #[test]
    #[cfg(feature = "media")]
    fn pointer_mask_owners_match_independent_boundary_and_combined_fixtures() {
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

    #[test]
    fn set_encodings_rejects_count_overflow_before_serialization() {
        let encodings = vec![0i32; usize::from(u16::MAX) + 1];
        let error = msg_set_encodings(&encodings).unwrap_err();
        assert!(error.to_string().contains("u16"), "{error:#}");
    }
}
