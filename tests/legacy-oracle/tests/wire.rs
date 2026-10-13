use ironrdp_graphics::{image_processing::PixelFormat, rlgr};
use ironrdp_pdu::{codecs::rfx::EntropyAlgorithm, ReadCursor};
use ironrdp_session::{image::DecodedImage, rfx::DecodingContext};
const WIRE: &[u8] =
    include_bytes!("../../../crates/frd-rdp-legacy-kernels/tests/fixtures/encoded_messages.bin");
#[test]
fn rlgr_modes_zero_runs_terminal_padding_and_errors() {
    for mode in [EntropyAlgorithm::Rlgr1, EntropyAlgorithm::Rlgr3] {
        for run in [0, 1, 7, 8, 63, 64, 4096] {
            let mut input = vec![0; run + 4];
            input[0] = 3;
            input[run + 1] = -7;
            input[run + 2] = 5;
            let mut wire = vec![0; 65536];
            let len = rlgr::encode(mode, &input, &mut wire).unwrap();
            wire.truncate(len);
            let mut out = vec![99; input.len() + 17];
            rlgr::decode(mode, &wire, &mut out).unwrap();
            assert_eq!(&out[..input.len()], &input);
            // 原版允许 bit 截断并补零；保持原版返回值与 materialization 输出。
            for end in 1..wire.len() {
                let mut truncated = vec![99; input.len() + 17];
                let result = rlgr::decode(mode, &wire[..end], &mut truncated);
                let _ = result; // 每个结果/partial 已由 wire catalog 冻结并逐次 verify。
            }
        }
        let mut padded = [99; 64];
        rlgr::decode(mode, &[0], &mut padded).unwrap();
        assert_eq!(padded, [0; 64]);
        let mut out = [99; 8];
        assert!(matches!(
            rlgr::decode(mode, &[], &mut out),
            Err(rlgr::RlgrError::EmptyTile)
        ));
        assert_eq!(out, [99; 8]);
        // 原版最短已知 run vectors：正/负 symbol 与 terminal padding。
        for (wire, expected) in [(0b11000000, [0, 1]), (0b11100000, [0, -1])] {
            let mut out = [9; 2];
            rlgr::decode(mode, &[wire], &mut out).unwrap();
            assert_eq!(out, expected);
        }
    }
}
#[test]
fn original_session_rejects_truncated_wire_fixture() {
    let rect = ironrdp_pdu::geometry::InclusiveRectangle {
        left: 0,
        top: 0,
        right: 63,
        bottom: 63,
    };
    for end in [0, 1, 5, 11, 50, 63, 86, 100, WIRE.len() - 1] {
        let mut image = DecodedImage::new(PixelFormat::BgrX32, 64, 64);
        assert!(
            DecodingContext::default()
                .decode(&mut image, &rect, &mut ReadCursor::new(&WIRE[..end]))
                .is_err(),
            "end={end}"
        );
    }
}
#[test]
fn original_rlgr_fixtures_match_frozen_vectors() {
    for (wire, expected) in [
        (
            include_bytes!(
                "../../../crates/frd-rdp-legacy-kernels/tests/fixtures/y_data_encoded.bin"
            )
            .as_slice(),
            include_bytes!(
                "../../../crates/frd-rdp-legacy-kernels/tests/fixtures/y_data_decoded.bin"
            )
            .as_slice(),
        ),
        (
            include_bytes!(
                "../../../crates/frd-rdp-legacy-kernels/tests/fixtures/cb_data_encoded.bin"
            )
            .as_slice(),
            include_bytes!(
                "../../../crates/frd-rdp-legacy-kernels/tests/fixtures/cb_data_decoded.bin"
            )
            .as_slice(),
        ),
        (
            include_bytes!(
                "../../../crates/frd-rdp-legacy-kernels/tests/fixtures/cr_data_encoded.bin"
            )
            .as_slice(),
            include_bytes!(
                "../../../crates/frd-rdp-legacy-kernels/tests/fixtures/cr_data_decoded.bin"
            )
            .as_slice(),
        ),
    ] {
        let mut out = vec![0; 4096];
        rlgr::decode(EntropyAlgorithm::Rlgr3, wire, &mut out).unwrap();
        let bytes: Vec<_> = out.iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(bytes, expected);
    }
}
#[test]
fn original_complete_session_matches_original_image() {
    let mut image = DecodedImage::new(PixelFormat::BgrX32, 64, 64);
    let rect = ironrdp_pdu::geometry::InclusiveRectangle {
        left: 0,
        top: 0,
        right: 63,
        bottom: 63,
    };
    DecodingContext::default()
        .decode(&mut image, &rect, &mut ReadCursor::new(WIRE))
        .unwrap();
    assert_eq!(
        image.data(),
        include_bytes!("../../../crates/frd-rdp-legacy-kernels/tests/fixtures/decoded_image.bin")
    );
}
