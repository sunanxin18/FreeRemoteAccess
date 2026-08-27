use std::fmt;

pub const RFB_BANNER_BYTES: usize = 12;
const PREFIX: &[u8; 4] = b"RFB ";
const VERSION_FIELD_BYTES: usize = 3;
const MAJOR_OFFSET: usize = PREFIX.len();
const MAJOR_END: usize = MAJOR_OFFSET + VERSION_FIELD_BYTES;
const SEPARATOR_OFFSET: usize = MAJOR_END;
const MINOR_OFFSET: usize = SEPARATOR_OFFSET + 1;
const MINOR_END: usize = MINOR_OFFSET + VERSION_FIELD_BYTES;
const TERMINATOR_OFFSET: usize = MINOR_END;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedBanner {
    pub wire: [u8; RFB_BANNER_BYTES],
    pub display: String,
    pub major: u16,
    pub minor: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireError {
    message: String,
}

impl WireError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for WireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WireError {}

pub fn decode_banner(bytes: &[u8]) -> Result<ParsedBanner, WireError> {
    let wire: [u8; RFB_BANNER_BYTES] = bytes
        .try_into()
        .map_err(|_| WireError::new("RFB banner 长度必须为 12 字节"))?;
    if &wire[..PREFIX.len()] != PREFIX {
        return Err(WireError::new("不是 RFB banner"));
    }
    if wire[SEPARATOR_OFFSET] != b'.' {
        return Err(WireError::new("RFB banner 缺少版本分隔符"));
    }
    if wire[TERMINATOR_OFFSET] != b'\n' {
        return Err(WireError::new("RFB banner 终止符非法"));
    }

    let major = decode_decimal_field(&wire[MAJOR_OFFSET..MAJOR_END], "major")?;
    let minor = decode_decimal_field(&wire[MINOR_OFFSET..MINOR_END], "minor")?;
    let display = String::from_utf8_lossy(&wire[..TERMINATOR_OFFSET]).into_owned();
    Ok(ParsedBanner {
        wire,
        display,
        major,
        minor,
    })
}

pub fn encode_banner(major: u16, minor: u16) -> Result<[u8; RFB_BANNER_BYTES], WireError> {
    let major = encode_decimal_field(major, "major")?;
    let minor = encode_decimal_field(minor, "minor")?;
    let mut wire = *b"RFB 000.000\n";
    wire[MAJOR_OFFSET..MAJOR_END].copy_from_slice(&major);
    wire[MINOR_OFFSET..MINOR_END].copy_from_slice(&minor);
    Ok(wire)
}

fn decode_decimal_field(field: &[u8], name: &str) -> Result<u16, WireError> {
    if field.len() != VERSION_FIELD_BYTES {
        return Err(WireError::new(format!("RFB {name} 版本字段长度非法")));
    }
    field.iter().try_fold(0_u16, |value, byte| {
        if !byte.is_ascii_digit() {
            return Err(WireError::new(format!("RFB {name} 版本字段不是十进制数字")));
        }
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u16::from(*byte - b'0')))
            .ok_or_else(|| WireError::new(format!("RFB {name} 版本字段溢出")))
    })
}

fn encode_decimal_field(value: u16, name: &str) -> Result<[u8; VERSION_FIELD_BYTES], WireError> {
    if value > 999 {
        return Err(WireError::new(format!(
            "RFB {name} 版本值超过三位十进制字段: {value}"
        )));
    }
    Ok([
        b'0' + u8::try_from(value / 100).expect("三位版本字段首位必定可放入 u8"),
        b'0' + u8::try_from((value / 10) % 10).expect("三位版本字段中位必定可放入 u8"),
        b'0' + u8::try_from(value % 10).expect("三位版本字段末位必定可放入 u8"),
    ])
}
