//! 测试期组合：原版 wire/RLGR decoder + 独立叶子；不是生产接线。
use frd_rdp_legacy_kernels::{NativeKernels, Quantization};
use ironrdp_graphics::{
    dwt, image_processing::PixelFormat, quantization, rlgr, subband_reconstruction,
};
use ironrdp_pdu::{
    codecs::rfx::{Block, CodecChannel, EntropyAlgorithm, Quant},
    Decode, ReadCursor,
};
use ironrdp_session::{image::DecodedImage, rfx::DecodingContext};

const WIRE: &[u8] = include_bytes!("fixtures/encoded_messages.bin");
fn i16s(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect()
}
fn native() -> NativeKernels {
    NativeKernels::detect().unwrap()
}
fn q(q: &Quant) -> Quantization {
    Quantization([
        q.hl1, q.lh1, q.hh1, q.hl2, q.lh2, q.hh2, q.hl3, q.lh3, q.hh3, q.ll3,
    ])
}

// 未修改 decoder 无 zero-run 回调；这里只重放其数值输出的零区间。
// 单个非零 symbol 原样赋值，不能把本函数当作 entropy hook 集成证明。
fn replay_materialization(k: NativeKernels, expected: &[i16]) -> Vec<i16> {
    let mut out = vec![123; expected.len()];
    let mut i = 0;
    while i < expected.len() {
        if expected[i] != 0 {
            out[i] = expected[i];
            i += 1;
        } else {
            let start = i;
            while i < expected.len() && expected[i] == 0 {
                i += 1;
            }
            k.zero_i16(&mut out[start..i]);
        }
    }
    out
}
#[test]
fn pinned_complete_wire_fixture_original_session_and_native_leaf_composition() {
    let k = native();
    let mut oracle = DecodedImage::new(PixelFormat::BgrX32, 64, 64);
    let rect = ironrdp_pdu::geometry::InclusiveRectangle {
        left: 0,
        top: 0,
        right: 63,
        bottom: 63,
    };
    DecodingContext::default()
        .decode(&mut oracle, &rect, &mut ReadCursor::new(WIRE))
        .unwrap();
    assert_eq!(oracle.data(), include_bytes!("fixtures/decoded_image.bin"));
    let mut cursor = ReadCursor::new(WIRE);
    let mut found = 0;
    while !cursor.is_empty() {
        if let Block::CodecChannel(CodecChannel::TileSet(set)) = Block::decode(&mut cursor).unwrap()
        {
            for tile in set.tiles {
                let mut planes = Vec::new();
                let mut scratch = [0; 4096];
                for (encoded, qi) in [
                    (tile.y_data, tile.y_quant_index),
                    (tile.cb_data, tile.cb_quant_index),
                    (tile.cr_data, tile.cr_quant_index),
                ] {
                    let mut coeff = vec![0; 4096];
                    rlgr::decode(set.entropy_algorithm, encoded, &mut coeff).unwrap();
                    let mut actual = replay_materialization(k, &coeff);
                    assert_eq!(actual, coeff);
                    k.differential(&mut actual[4032..]);
                    k.dequantize(&mut actual, q(&set.quants[usize::from(qi)]))
                        .unwrap();
                    k.inverse_dwt(&mut actual, &mut scratch).unwrap();
                    subband_reconstruction::decode(&mut coeff[4032..]);
                    quantization::decode(&mut coeff, &set.quants[usize::from(qi)]);
                    dwt::decode(&mut coeff, &mut scratch);
                    assert_eq!(actual, coeff);
                    planes.push(actual);
                }
                let mut rgba = vec![0; 4096 * 4];
                k.ycbcr_to_rgba(&planes[0], &planes[1], &planes[2], &mut rgba)
                    .unwrap();
                // fixture 是 BGRX；原版 apply_tile 保留 255 alpha。
                for pixel in rgba.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                }
                assert_eq!(rgba, oracle.data());
                found += 1;
            }
        }
    }
    assert_eq!(found, 1);
}
#[test]
fn original_dwt_and_rlgr_vectors() {
    let k = native();
    let mut data = i16s(include_bytes!("fixtures/encoded_dwt.bin"));
    k.inverse_dwt(&mut data, &mut [0; 4096]).unwrap();
    assert_eq!(data, i16s(include_bytes!("fixtures/decoded_dwt.bin")));
    for (wire, expected) in [
        (
            include_bytes!("fixtures/y_data_encoded.bin").as_slice(),
            include_bytes!("fixtures/y_data_decoded.bin").as_slice(),
        ),
        (
            include_bytes!("fixtures/cb_data_encoded.bin").as_slice(),
            include_bytes!("fixtures/cb_data_decoded.bin").as_slice(),
        ),
        (
            include_bytes!("fixtures/cr_data_encoded.bin").as_slice(),
            include_bytes!("fixtures/cr_data_decoded.bin").as_slice(),
        ),
    ] {
        let mut data = vec![0; 4096];
        rlgr::decode(EntropyAlgorithm::Rlgr3, wire, &mut data).unwrap();
        assert_eq!(data, i16s(expected));
        assert_eq!(replay_materialization(k, &data), data);
    }
}
#[test]
fn rlgr_modes_zero_runs_terminal_padding_and_errors() {
    let k = native();
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
            assert_eq!(replay_materialization(k, &out), out);
            // 原版允许 bit 截断并补零；保持原版返回值与 materialization 输出。
            for end in 1..wire.len() {
                let mut truncated = vec![99; input.len() + 17];
                let result = rlgr::decode(mode, &wire[..end], &mut truncated);
                if result.is_ok() {
                    assert_eq!(replay_materialization(k, &truncated), truncated);
                }
            }
        }
        let mut padded = [99; 64];
        rlgr::decode(mode, &[0], &mut padded).unwrap();
        assert_eq!(padded, [0; 64]);
        assert_eq!(replay_materialization(k, &padded), padded);
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
            assert_eq!(replay_materialization(k, &out), out);
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
