use frd_core::PixelSize;

use crate::banner::WireError;

pub const RFB_PIXEL_FORMAT_BYTES: usize = 16;
pub const SERVER_INIT_HEADER_BYTES: usize = 24;
pub const SERVER_INIT_DESKTOP_NAME_MAX_BYTES: usize = 65_536;

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
    pub const OURS: Self = Self {
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

    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let bytes: &[u8; RFB_PIXEL_FORMAT_BYTES] = bytes
            .try_into()
            .map_err(|_| WireError::new("RFB 像素格式长度必须为 16 字节"))?;
        Ok(Self {
            bits_per_pixel: bytes[0],
            depth: bytes[1],
            big_endian: bytes[2],
            true_colour: bytes[3],
            red_max: u16::from_be_bytes([bytes[4], bytes[5]]),
            green_max: u16::from_be_bytes([bytes[6], bytes[7]]),
            blue_max: u16::from_be_bytes([bytes[8], bytes[9]]),
            red_shift: bytes[10],
            green_shift: bytes[11],
            blue_shift: bytes[12],
        })
    }

    pub fn to_bytes(self) -> [u8; RFB_PIXEL_FORMAT_BYTES] {
        let mut bytes = [0_u8; RFB_PIXEL_FORMAT_BYTES];
        bytes[0] = self.bits_per_pixel;
        bytes[1] = self.depth;
        bytes[2] = self.big_endian;
        bytes[3] = self.true_colour;
        bytes[4..6].copy_from_slice(&self.red_max.to_be_bytes());
        bytes[6..8].copy_from_slice(&self.green_max.to_be_bytes());
        bytes[8..10].copy_from_slice(&self.blue_max.to_be_bytes());
        bytes[10] = self.red_shift;
        bytes[11] = self.green_shift;
        bytes[12] = self.blue_shift;
        bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInitHeader {
    pub size: PixelSize,
    pub pixel_format: PixelFormat,
    pub name_length: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInit {
    pub size: PixelSize,
    pub pixel_format: PixelFormat,
    pub name: String,
}

pub fn decode_server_init_header(bytes: &[u8]) -> Result<ServerInitHeader, WireError> {
    let header = bytes
        .get(..SERVER_INIT_HEADER_BYTES)
        .ok_or_else(|| WireError::new("ServerInit 固定头被截断"))?;
    let width = u16::from_be_bytes([header[0], header[1]]);
    let height = u16::from_be_bytes([header[2], header[3]]);
    let size = PixelSize::new(u32::from(width), u32::from(height))
        .ok_or_else(|| WireError::new("ServerInit 分辨率不能为零"))?;
    let pixel_format = PixelFormat::decode(&header[4..4 + RFB_PIXEL_FORMAT_BYTES])?;
    let name_length_u32 = u32::from_be_bytes([header[20], header[21], header[22], header[23]]);
    let name_length = usize::try_from(name_length_u32)
        .map_err(|_| WireError::new("ServerInit 桌面名称长度无法转换为 usize"))?;
    if name_length > SERVER_INIT_DESKTOP_NAME_MAX_BYTES {
        return Err(WireError::new(format!(
            "ServerInit 桌面名称长度超过资源预算: {name_length}"
        )));
    }
    SERVER_INIT_HEADER_BYTES
        .checked_add(name_length)
        .ok_or_else(|| WireError::new("ServerInit 总长度溢出"))?;
    Ok(ServerInitHeader {
        size,
        pixel_format,
        name_length,
    })
}

pub fn decode_server_init(bytes: &[u8]) -> Result<ServerInit, WireError> {
    let header = decode_server_init_header(bytes)?;
    let expected_length = SERVER_INIT_HEADER_BYTES
        .checked_add(header.name_length)
        .ok_or_else(|| WireError::new("ServerInit 总长度溢出"))?;
    if bytes.len() != expected_length {
        return Err(WireError::new(format!(
            "ServerInit 长度与声明的桌面名称长度不一致: 期望 {expected_length}，实际 {}",
            bytes.len()
        )));
    }
    let name = String::from_utf8_lossy(&bytes[SERVER_INIT_HEADER_BYTES..]).into_owned();
    Ok(ServerInit {
        size: header.size,
        pixel_format: header.pixel_format,
        name,
    })
}
