//! Apple MVS payload wire grammar.

use anyhow::{bail, Result};

const TABLE_BYTES: usize = 64;
const TABLE_RECORD_BYTES: usize = 1 + 2 * TABLE_BYTES;
const FULL_HEADER_BYTES: usize = 6;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsTables {
    pub luminance: [u8; 64],
    pub chrominance: [u8; 64],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MvsFullRecord<'a> {
    pub scale_threshold_a: u8,
    pub scale_threshold_b: u8,
    pub mode_stream: &'a [u8],
    pub data_stream: &'a [u8],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MvsWirePayload<'a> {
    Tables(MvsTables),
    Full(MvsFullRecord<'a>),
    Partial(&'a [u8]),
}

pub fn parse_payload(payload: &[u8]) -> Result<MvsWirePayload<'_>> {
    let Some(&tag) = payload.first() else {
        bail!("MVS payload 为空");
    };
    match tag {
        2 => {
            if payload.len() != TABLE_RECORD_BYTES {
                bail!(
                    "MVS type-2 长度非法: {}（需 {TABLE_RECORD_BYTES}B）",
                    payload.len()
                );
            }
            Ok(MvsWirePayload::Tables(MvsTables {
                luminance: payload[1..1 + TABLE_BYTES].try_into()?,
                chrominance: payload[1 + TABLE_BYTES..TABLE_RECORD_BYTES].try_into()?,
            }))
        }
        0 => {
            if payload.len() < FULL_HEADER_BYTES {
                bail!(
                    "MVS type-0 长度非法: {}（需至少 {FULL_HEADER_BYTES}B）",
                    payload.len()
                );
            }
            let data_offset = (usize::from(payload[3]) << 16)
                | (usize::from(payload[4]) << 8)
                | usize::from(payload[5]);
            if data_offset <= FULL_HEADER_BYTES || data_offset > payload.len() {
                bail!("MVS type-0 数据偏移非法: {data_offset}");
            }
            Ok(MvsWirePayload::Full(MvsFullRecord {
                scale_threshold_a: payload[1],
                scale_threshold_b: payload[2],
                mode_stream: &payload[FULL_HEADER_BYTES..data_offset],
                data_stream: &payload[data_offset..],
            }))
        }
        1 => Ok(MvsWirePayload::Partial(payload)),
        _ => bail!("未知 MVS payload 类型: {tag}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exact_type_two_tables() {
        let mut payload = vec![2];
        payload.extend(0u8..64);
        payload.extend(64u8..128);
        let MvsWirePayload::Tables(tables) = parse_payload(&payload).unwrap() else {
            panic!("expected tables");
        };
        assert_eq!(tables.luminance[0], 0);
        assert_eq!(tables.luminance[63], 63);
        assert_eq!(tables.chrominance[0], 64);
        assert_eq!(tables.chrominance[63], 127);
    }

    #[test]
    fn type_two_rejects_non_129_byte_payload() {
        assert!(parse_payload(&[2; 128]).is_err());
        assert!(parse_payload(&[2; 130]).is_err());
    }

    #[test]
    fn type_zero_uses_big_endian_u24_data_offset() {
        let payload = [0, 15, 25, 0, 0, 8, 0xaa, 0xbb, 0xcc, 0xdd];
        let MvsWirePayload::Full(full) = parse_payload(&payload).unwrap() else {
            panic!("expected full record");
        };
        assert_eq!(full.scale_threshold_a, 15);
        assert_eq!(full.scale_threshold_b, 25);
        assert_eq!(full.mode_stream, &[0xaa, 0xbb]);
        assert_eq!(full.data_stream, &[0xcc, 0xdd]);
    }

    #[test]
    fn type_zero_rejects_offsets_before_header_or_after_record() {
        assert!(parse_payload(&[0, 1, 2, 0, 0, 5]).is_err());
        assert!(parse_payload(&[0, 1, 2, 0, 0, 6]).is_err());
        assert!(parse_payload(&[0, 1, 2, 0, 0, 7]).is_err());
    }

    #[test]
    fn type_one_is_kept_opaque() {
        let payload = [1, 0x6d, 0x76, 0x73, 0xaa];
        assert!(matches!(
            parse_payload(&payload).unwrap(),
            MvsWirePayload::Partial(bytes) if bytes == payload
        ));
    }
}
