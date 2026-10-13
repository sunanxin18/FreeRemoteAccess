use frd_rdp_legacy_kernels::{KernelError, NativeKernels, Quantization};
use ironrdp_graphics::{
    color_conversion::{ycbcr_to_rgba, YCbCrBuffer},
    dwt, quantization, subband_reconstruction,
};
use ironrdp_pdu::codecs::rfx::Quant;

fn native() -> NativeKernels {
    NativeKernels::detect().expect("本测试需要获准架构上的原生 SIMD")
}
fn samples(n: usize, mut state: u32) -> Vec<i16> {
    (0..n)
        .map(|_| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 16) as i16
        })
        .collect()
}
fn quant(q: [u8; 10]) -> Quant {
    Quant {
        hl1: q[0],
        lh1: q[1],
        hh1: q[2],
        hl2: q[3],
        lh2: q[4],
        hh2: q[5],
        hl3: q[6],
        lh3: q[7],
        hh3: q[8],
        ll3: q[9],
    }
}

#[test]
fn dequant_all_bands_zero_shift_and_truncation() {
    let k = native();
    for value in 0..=16 {
        for band in 0..10 {
            let mut q = [1; 10];
            q[band] = value;
            let mut actual = samples(4096, band as u32 + 42);
            let mut expected = actual.clone();
            quantization::decode(&mut expected, &quant(q));
            k.dequantize(&mut actual, Quantization(q)).unwrap();
            assert_eq!(actual, expected, "band={band} q={value}");
        }
    }
}

#[test]
fn dwt_three_levels_all_impulses_edges_extremes() {
    let k = native();
    let mut scratch = [0; 4096];
    let mut oracle_scratch = [0; 4096];
    for index in 0..4096 {
        let mut expected = [0; 4096];
        expected[index] = if index % 2 == 0 { 32767 } else { -32768 };
        let mut actual = expected;
        dwt::decode(&mut expected, &mut oracle_scratch);
        k.inverse_dwt(&mut actual, &mut scratch).unwrap();
        assert_eq!(actual, expected, "impulse={index}");
    }
    for seed in 0..12 {
        let mut actual = samples(4096, seed);
        let mut expected = actual.clone();
        dwt::decode(&mut expected, &mut oracle_scratch);
        k.inverse_dwt(&mut actual, &mut scratch).unwrap();
        assert_eq!(actual, expected, "seed={seed}");
    }
    for val in [i16::MIN, i16::MAX, -1, 0, 1] {
        let mut actual = [val; 4096];
        let mut expected = actual;
        dwt::decode(&mut expected, &mut oracle_scratch);
        k.inverse_dwt(&mut actual, &mut scratch).unwrap();
        assert_eq!(actual, expected);
    }
}

#[test]
fn arbitrary_tails_unaligned_prefix_zero_and_canaries() {
    let k = native();
    for len in 0..=137 {
        for offset in 0..8 {
            let mut actual = samples(len + 16, 37);
            let mut expected = actual.clone();
            subband_reconstruction::decode(&mut expected[offset..offset + len]);
            k.differential(&mut actual[offset..offset + len]);
            assert_eq!(actual, expected);
            expected[offset..offset + len].fill(0);
            k.zero_i16(&mut actual[offset..offset + len]);
            assert_eq!(actual, expected);
        }
    }
    for val in [i16::MIN, i16::MAX, -1, 1] {
        let mut a = [val; 64];
        let mut e = a;
        subband_reconstruction::decode(&mut e);
        k.differential(&mut a);
        assert_eq!(a, e);
    }
}

// 独立 checked scalar 只用于判定上游合法算术域，像素期望始终调用原版 oracle。
fn valid(y: i16, cb: i16, cr: i16) -> bool {
    let Some(y) = y
        .checked_add(4096)
        .map(i32::from)
        .and_then(|y| y.checked_mul(65536))
    else {
        return false;
    };
    let (Some(rc), Some(gc), Some(gu), Some(bc)) = (
        i32::from(cr).checked_mul(91915),
        i32::from(cr).checked_mul(46818),
        i32::from(cb).checked_mul(22526),
        i32::from(cb).checked_mul(115992),
    ) else {
        return false;
    };
    y.checked_add(rc).is_some()
        && y.checked_sub(gc).and_then(|x| x.checked_sub(gu)).is_some()
        && y.checked_add(bc).is_some()
}
#[test]
fn color_full_i16_domain_checked_arithmetic_and_original_oracle() {
    let k = native();
    let mut discriminating = [0; 4];
    k.ycbcr_to_rgba(&[-8265], &[8234], &[-20324], &mut discriminating)
        .unwrap();
    assert_eq!(discriminating, [0, 234, 255, 255]);
    let data = samples(300000, 23);
    let mut accepted = 0;
    let mut rejected = 0;
    for p in data.chunks_exact(3) {
        let mut actual = [0xa5; 4];
        let result = k.ycbcr_to_rgba(&p[..1], &p[1..2], &p[2..], &mut actual);
        if valid(p[0], p[1], p[2]) {
            let mut expected = [0; 4];
            ycbcr_to_rgba(
                YCbCrBuffer {
                    y: &p[..1],
                    cb: &p[1..2],
                    cr: &p[2..],
                },
                &mut expected,
            )
            .unwrap();
            assert_eq!(result, Ok(()), "{p:?}");
            assert_eq!(actual, expected, "{p:?}");
            accepted += 1;
        } else {
            assert_eq!(result, Err(KernelError::ColorOverflow), "{p:?}");
            assert_eq!(actual, [0xa5; 4]);
            rejected += 1;
        }
    }
    assert!(accepted > 10000 && rejected > 10000);
}
#[test]
fn color_shift_clamp_boundaries_tails_and_unaligned_output() {
    let k = native();
    for len in 0..=137 {
        for offset in 0..8 {
            let y: Vec<_> = samples(len + offset, 9)
                .into_iter()
                .map(|x| x / 8)
                .collect();
            let cb: Vec<_> = samples(len + offset, 13)
                .into_iter()
                .map(|x| x / 8)
                .collect();
            let cr: Vec<_> = samples(len + offset, 7)
                .into_iter()
                .map(|x| x / 8)
                .collect();
            let mut out = vec![0xa5; len * 4 + offset + 8];
            let mut expected = out.clone();
            if len > 0 {
                ycbcr_to_rgba(
                    YCbCrBuffer {
                        y: &y[offset..],
                        cb: &cb[offset..],
                        cr: &cr[offset..],
                    },
                    &mut expected[offset..offset + 4 * len],
                )
                .unwrap();
            }
            k.ycbcr_to_rgba(
                &y[offset..],
                &cb[offset..],
                &cr[offset..],
                &mut out[offset..offset + 4 * len],
            )
            .unwrap();
            assert_eq!(out, expected);
        }
    }
    // 灰度每个量化阈值两侧；无 rounding bias。
    for y in -4096..=4095 {
        let mut out = [0; 4];
        k.ycbcr_to_rgba(&[y], &[0], &[0], &mut out).unwrap();
        let v = ((i32::from(y) + 4096) >> 5).clamp(0, 255) as u8;
        assert_eq!(out, [v, v, v, 255]);
    }
    // 逐中间式乘法与加减边界附近的合法域检测。
    for y in [-32768, -4096, 0, 4095, 28670, 28671, 28672, 32767] {
        for coefficient in [91915, 46818, 22526, 115992] {
            for boundary in [i32::MIN / coefficient, i32::MAX / coefficient] {
                for delta in -2..=2 {
                    if let Ok(c) = i16::try_from(boundary + delta) {
                        for (cb, cr) in [(c, 0), (0, c), (c, c), (c, -c)] {
                            let mut out = [0xa5; 4];
                            let r = k.ycbcr_to_rgba(&[y], &[cb], &[cr], &mut out);
                            assert_eq!(r.is_ok(), valid(y, cb, cr), "{y} {cb} {cr}");
                            if r.is_err() {
                                assert_eq!(out, [0xa5; 4]);
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn checked_errors_do_not_mutate_and_capability_cannot_be_forced() {
    let k = native();
    assert!(matches!(
        NativeKernels::detect_if(false),
        Err(KernelError::Unavailable)
    ));
    let mut data = [7; 4096];
    let mut scratch = [9; 4096];
    assert_eq!(
        k.dequantize(&mut data[..4095], Quantization([1; 10])),
        Err(KernelError::Length)
    );
    let mut q = [1; 10];
    q[9] = 17;
    assert_eq!(
        k.dequantize(&mut data, Quantization(q)),
        Err(KernelError::InvalidQuant)
    );
    assert_eq!(data, [7; 4096]);
    assert_eq!(
        k.inverse_dwt(&mut data, &mut scratch[..4095]),
        Err(KernelError::Length)
    );
    assert_eq!(data, [7; 4096]);
    assert_eq!(scratch, [9; 4096]);
    let mut out = [0xa5; 40];
    let mut y = [0; 10];
    y[9] = 32767;
    assert_eq!(
        k.ycbcr_to_rgba(&y, &[0; 10], &[0; 10], &mut out),
        Err(KernelError::ColorOverflow)
    );
    assert_eq!(out, [0xa5; 40]);
    assert_eq!(
        k.ycbcr_to_rgba(&[0], &[], &[0], &mut out),
        Err(KernelError::Length)
    );
    assert_eq!(out, [0xa5; 40]);
}

#[test]
fn upstream_color_overflow_is_profile_dependent_native_is_explicit() {
    let mut native_output = [0xa5; 4];
    assert_eq!(
        native().ycbcr_to_rgba(&[32767], &[0], &[0], &mut native_output),
        Err(KernelError::ColorOverflow)
    );
    assert_eq!(native_output, [0xa5; 4]);
    let upstream = std::panic::catch_unwind(|| {
        let mut out = [0; 4];
        ycbcr_to_rgba(
            YCbCrBuffer {
                y: &[32767],
                cb: &[0],
                cr: &[0],
            },
            &mut out,
        )
        .unwrap();
        out
    });
    if cfg!(debug_assertions) {
        assert!(upstream.is_err());
    } else {
        assert_eq!(upstream.unwrap(), [0, 0, 0, 255]);
    }
}
