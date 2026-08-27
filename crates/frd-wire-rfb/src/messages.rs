use crate::banner::WireError;
use crate::server_init::{PixelFormat, RFB_PIXEL_FORMAT_BYTES};

pub const MAX_SECURITY_TYPES: usize = 4096;
pub const RECTANGLE_HEADER_BYTES: usize = 12;
pub const SET_PIXEL_FORMAT_MESSAGE_BYTES: usize = 20;
pub const SET_ENCODINGS_HEADER_BYTES: usize = 4;
pub const FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RectangleHeader {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub encoding: i32,
}

pub fn decode_security_types(minor: u8, bytes: &[u8]) -> Result<Vec<u8>, WireError> {
    if minor == 3 {
        let value: [u8; 4] = bytes
            .try_into()
            .map_err(|_| WireError::new("RFB 3.3 security type 必须为 4 字节"))?;
        let security_type = u32::from_be_bytes(value);
        if security_type == 0 {
            return Ok(Vec::new());
        }
        return Ok(vec![u8::try_from(security_type).map_err(|_| {
            WireError::new("RFB 3.3 security type 超出 u8 表示范围")
        })?]);
    }
    let (header_bytes, count) = decode_security_types_header(minor, bytes)?;
    let expected_length = header_bytes
        .checked_add(count)
        .ok_or_else(|| WireError::new("RFB security type 列表长度溢出"))?;
    if bytes.len() != expected_length {
        return Err(WireError::new(format!(
            "RFB security type 列表长度不一致: 期望 {expected_length}，实际 {}",
            bytes.len()
        )));
    }
    Ok(bytes[header_bytes..].to_vec())
}

pub fn decode_security_types_header(minor: u8, bytes: &[u8]) -> Result<(usize, usize), WireError> {
    match minor {
        3 => {
            let value: [u8; 4] = bytes
                .try_into()
                .map_err(|_| WireError::new("RFB 3.3 security type 必须为 4 字节"))?;
            let security_type = u32::from_be_bytes(value);
            if security_type != 0 {
                u8::try_from(security_type)
                    .map_err(|_| WireError::new("RFB 3.3 security type 超出 u8 表示范围"))?;
            }
            Ok((4, 0))
        }
        7 => {
            let prefix: [u8; 2] = bytes
                .get(..2)
                .ok_or_else(|| WireError::new("RFB 3.7 security type 数量被截断"))?
                .try_into()
                .expect("固定长度切片必须可转换为数组");
            let count = usize::from(u16::from_be_bytes(prefix));
            validate_security_type_count(count)?;
            Ok((2, count))
        }
        _ => {
            let count = usize::from(
                *bytes
                    .first()
                    .ok_or_else(|| WireError::new("RFB security type 数量被截断"))?,
            );
            validate_security_type_count(count)?;
            Ok((1, count))
        }
    }
}

pub fn decode_rectangle_header(bytes: &[u8]) -> Result<RectangleHeader, WireError> {
    let bytes: &[u8; RECTANGLE_HEADER_BYTES] = bytes
        .try_into()
        .map_err(|_| WireError::new("RFB 矩形头长度必须为 12 字节"))?;
    let width = u16::from_be_bytes([bytes[4], bytes[5]]);
    if width == 0 {
        return Err(WireError::new("RFB 矩形宽度不能为零"));
    }
    let height = u16::from_be_bytes([bytes[6], bytes[7]]);
    if height == 0 {
        return Err(WireError::new("RFB 矩形高度不能为零"));
    }
    Ok(RectangleHeader {
        x: u16::from_be_bytes([bytes[0], bytes[1]]),
        y: u16::from_be_bytes([bytes[2], bytes[3]]),
        width,
        height,
        encoding: i32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
    })
}

pub fn encode_set_pixel_format(pixel_format: PixelFormat) -> [u8; SET_PIXEL_FORMAT_MESSAGE_BYTES] {
    let mut bytes = [0_u8; SET_PIXEL_FORMAT_MESSAGE_BYTES];
    bytes[0] = 0;
    bytes[4..4 + RFB_PIXEL_FORMAT_BYTES].copy_from_slice(&pixel_format.to_bytes());
    bytes
}

pub fn encode_set_encodings(encodings: &[i32]) -> Result<Vec<u8>, WireError> {
    let count = u16::try_from(encodings.len())
        .map_err(|_| WireError::new("SetEncodings 编码数量超出 u16 表示范围"))?;
    let entries = encodings
        .len()
        .checked_mul(size_of::<i32>())
        .ok_or_else(|| WireError::new("SetEncodings 编码字节数溢出"))?;
    let capacity = SET_ENCODINGS_HEADER_BYTES
        .checked_add(entries)
        .ok_or_else(|| WireError::new("SetEncodings 消息长度溢出"))?;
    let mut bytes = Vec::with_capacity(capacity);
    bytes.extend_from_slice(&[2, 0]);
    bytes.extend_from_slice(&count.to_be_bytes());
    for encoding in encodings {
        bytes.extend_from_slice(&encoding.to_be_bytes());
    }
    Ok(bytes)
}

pub fn encode_framebuffer_update_request(
    incremental: bool,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
) -> Result<[u8; FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES], WireError> {
    if width == 0 {
        return Err(WireError::new("FramebufferUpdateRequest 宽度不能为零"));
    }
    if height == 0 {
        return Err(WireError::new("FramebufferUpdateRequest 高度不能为零"));
    }
    let mut bytes = [0_u8; FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES];
    bytes[0] = 3;
    bytes[1] = u8::from(incremental);
    bytes[2..4].copy_from_slice(&x.to_be_bytes());
    bytes[4..6].copy_from_slice(&y.to_be_bytes());
    bytes[6..8].copy_from_slice(&width.to_be_bytes());
    bytes[8..].copy_from_slice(&height.to_be_bytes());
    Ok(bytes)
}

fn validate_security_type_count(count: usize) -> Result<(), WireError> {
    if count > MAX_SECURITY_TYPES {
        return Err(WireError::new(format!(
            "RFB security type 数量超过资源预算: {count}"
        )));
    }
    Ok(())
}
