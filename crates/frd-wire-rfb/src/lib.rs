//! 无状态、切片驱动的 RFB 线协议编解码。

mod banner;
mod messages;
mod server_init;

pub use banner::{decode_banner, encode_banner, ParsedBanner, WireError, RFB_BANNER_BYTES};
pub use messages::{
    decode_rectangle_header, decode_security_types, decode_security_types_header,
    encode_framebuffer_update_request, encode_set_encodings, encode_set_pixel_format,
    RectangleHeader, FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES, SET_ENCODINGS_HEADER_BYTES,
    SET_PIXEL_FORMAT_MESSAGE_BYTES,
};
pub use server_init::{
    decode_server_init, decode_server_init_header, PixelFormat, ServerInit, RFB_PIXEL_FORMAT_BYTES,
    SERVER_INIT_DESKTOP_NAME_MAX_BYTES, SERVER_INIT_HEADER_BYTES,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_preserves_apple_003_889_wire_bytes() {
        let banner = decode_banner(b"RFB 003.889\n").expect("Apple banner 必须可解析");

        assert_eq!(banner.display, "RFB 003.889");
        assert_eq!(banner.major, 3);
        assert_eq!(banner.minor, 889);
        assert_eq!(banner.wire, *b"RFB 003.889\n");
    }

    #[test]
    fn banner_rejects_malformed_fixed_fields_without_panicking() {
        assert!(decode_banner(b"RFB 03x.008\n").is_err());
        assert!(decode_banner(b"RFB 003-008\n").is_err());
        assert!(decode_banner(b"RFB 003.008!").is_err());
        assert!(decode_banner(&[
            b'R', b'F', b'B', b' ', 0xf0, 0x9f, 0x92, b'.', b'0', b'0', b'8', b'\n'
        ])
        .is_err());
    }

    #[test]
    fn banner_encoder_rejects_versions_that_exceed_three_decimal_digits() {
        for (major, minor) in [(1000, 0), (0, 1000), (u16::MAX, u16::MAX)] {
            assert!(encode_banner(major, minor).is_err());
        }
    }

    #[test]
    fn banner_encoder_keeps_standard_reply_fixtures() {
        for ((major, minor), expected) in [
            ((3, 3), *b"RFB 003.003\n"),
            ((3, 7), *b"RFB 003.007\n"),
            ((3, 8), *b"RFB 003.008\n"),
        ] {
            assert_eq!(encode_banner(major, minor).unwrap(), expected);
        }
    }

    #[test]
    fn security_types_rejects_rfb_33_value_that_overflows_u8() {
        let error = decode_security_types(3, &257_u32.to_be_bytes())
            .expect_err("RFB 3.3 security type 257 必须被拒绝");

        assert!(error.to_string().contains("u8"), "{error}");
    }

    #[test]
    fn security_types_rejects_truncated_declared_list() {
        let error = decode_security_types(7, &[0, 2, 1])
            .expect_err("声明两个 security type 但仅提供一个时必须被拒绝");

        assert!(error.to_string().contains("长度不一致"), "{error}");
    }

    #[test]
    fn server_init_rejects_zero_dimensions_and_excessive_name_length() {
        let mut zero_width = valid_server_init_prefix(0, 4, 0);
        zero_width.extend_from_slice(&[]);
        assert!(decode_server_init(&zero_width).is_err());

        let excessive_name = valid_server_init_prefix(8, 4, 65_537);
        assert!(decode_server_init_header(&excessive_name).is_err());
    }

    #[test]
    fn server_init_rejects_truncated_pixel_format_and_name() {
        assert!(decode_server_init(&[0; 23]).is_err());

        let truncated_name = valid_server_init_prefix(8, 4, 1);
        assert!(decode_server_init(&truncated_name).is_err());
    }

    #[test]
    fn rectangle_header_decodes_basic_wire_fixture_and_rejects_truncation() {
        let header = decode_rectangle_header(&[0, 2, 0, 3, 0, 4, 0, 5, 0, 0, 0, 1])
            .expect("完整矩形头必须可解析");
        assert_eq!(header.x, 2);
        assert_eq!(header.y, 3);
        assert_eq!(header.width, 4);
        assert_eq!(header.height, 5);
        assert_eq!(header.encoding, 1);
        assert!(decode_rectangle_header(&[0; 11]).is_err());
    }

    #[test]
    fn set_encodings_rejects_count_overflow_before_serialization() {
        let encodings = vec![0_i32; usize::from(u16::MAX) + 1];
        let error = encode_set_encodings(&encodings).expect_err("超出 u16 的数量必须被拒绝");
        assert!(error.to_string().contains("u16"), "{error}");
    }

    fn valid_server_init_prefix(width: u16, height: u16, name_length: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
        bytes.extend_from_slice(&name_length.to_be_bytes());
        bytes
    }
}
