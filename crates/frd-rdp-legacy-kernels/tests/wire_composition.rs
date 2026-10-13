//! 冻结原版 RLGR 系数上的叶子组合；真实 wire/session 运算与错误验证在独立 oracle。
#[path = "common/frozen.rs"]
mod frozen;
use frd_rdp_legacy_kernels::{NativeKernels, Quantization};
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
fn captured_original_coefficients_and_native_leaf_composition() {
    let k = NativeKernels::detect().unwrap();
    let mut planes = Vec::new();
    for (q, coeff, expected) in frozen::composition() {
        let mut actual = replay_materialization(k, &coeff);
        assert_eq!(actual, coeff);
        k.differential(&mut actual[4032..]);
        k.dequantize(&mut actual, Quantization(q)).unwrap();
        k.inverse_dwt(&mut actual, &mut [0; 4096]).unwrap();
        assert_eq!(actual, expected);
        planes.push(actual);
    }
    let mut rgba = vec![0; 16384];
    k.ycbcr_to_rgba(&planes[0], &planes[1], &planes[2], &mut rgba)
        .unwrap();
    for p in rgba.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    assert_eq!(rgba, include_bytes!("fixtures/decoded_image.bin"));
}
#[test]
fn original_dwt_and_captured_rlgr_vectors() {
    let k = NativeKernels::detect().unwrap();
    let mut data = frozen::format::i16s(include_bytes!("fixtures/encoded_dwt.bin"));
    k.inverse_dwt(&mut data, &mut [0; 4096]).unwrap();
    assert_eq!(
        data,
        frozen::format::i16s(include_bytes!("fixtures/decoded_dwt.bin"))
    );
    // 保留旧 RLGR modes/每字节截断成功输出上的全部 zero 重放；不声称 parser hook。
    let captured = frozen::rlgr_outputs();
    assert_eq!(captured.len(), 546);
    for expected in captured {
        assert_eq!(replay_materialization(k, &expected), expected);
    }
    for expected in [
        include_bytes!("fixtures/y_data_decoded.bin").as_slice(),
        include_bytes!("fixtures/cb_data_decoded.bin").as_slice(),
        include_bytes!("fixtures/cr_data_decoded.bin").as_slice(),
    ] {
        let data = frozen::format::i16s(expected);
        assert_eq!(replay_materialization(k, &data), data);
    }
}
