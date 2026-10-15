//! 定长已知答案先锁定方向、负数舍入和 Apple 环绕表；SIMD 与原始标量逐字节比较。
use super::{inverse_dct_8x8, inverse_dct_8x8_scalar};

#[test]
fn idct_foundation_fixed_answers_and_quantization_zero() {
    let mut coefficients = [i16::MIN; 64];
    assert_eq!(inverse_dct_8x8(&coefficients, &[0; 64]).unwrap(), [128; 64]);
    coefficients.fill(0);
    coefficients[0] = -4;
    assert_eq!(inverse_dct_8x8(&coefficients, &[1; 64]).unwrap(), [128; 64]);
    coefficients[0] = -5;
    assert_eq!(inverse_dct_8x8(&coefficients, &[1; 64]).unwrap(), [127; 64]);
    coefficients[0] = 8_192;
    assert_eq!(inverse_dct_8x8(&coefficients, &[1; 64]).unwrap(), [128; 64]);
    coefficients[0] = 0;
    coefficients[8] = 64;
    let expected_rows = [139, 137, 134, 130, 126, 122, 119, 117];
    let expected = std::array::from_fn(|i| expected_rows[i / 8]);
    assert_eq!(inverse_dct_8x8(&coefficients, &[1; 64]).unwrap(), expected);
    assert_eq!(
        inverse_dct_8x8_scalar(&coefficients, &[1; 64]).unwrap(),
        expected
    );
}

#[cfg(feature = "mvs-native-idct-arm64")]
fn assert_parity(coefficients: &[i16; 64], quantization: &[u8; 64], case: usize) {
    let expected = inverse_dct_8x8_scalar(coefficients, quantization).unwrap();
    let actual = inverse_dct_8x8(coefficients, quantization).unwrap();
    assert_eq!(
        actual, expected,
        "IDCT 全部 64 字节，case={case}，coef={coefficients:?}，quant={quantization:?}"
    );
}

#[cfg(feature = "mvs-native-idct-arm64")]
#[test]
fn idct_foundation_simd_extrema_impulses_and_all_quantization_values() {
    let mut case = 0;
    for quant in 0..=255 {
        for pattern in 0..5 {
            let coefficients = std::array::from_fn(|i| match pattern {
                0 => i16::MIN,
                1 => i16::MAX,
                2 => {
                    if i % 2 == 0 {
                        i16::MIN
                    } else {
                        i16::MAX
                    }
                }
                3 => {
                    if (i / 8 + i % 8) % 2 == 0 {
                        i16::MAX
                    } else {
                        i16::MIN
                    }
                }
                _ => {
                    if i < 32 {
                        i16::MIN
                    } else {
                        i16::MAX
                    }
                }
            });
            assert_parity(&coefficients, &[quant; 64], case);
            case += 1;
        }
    }
    for position in 0..64 {
        for value in [i16::MIN, -32767, -8193, -1024, -5, -1, 1, 4, 8192, i16::MAX] {
            for quant in [0, 1, 127, 128, 255] {
                let mut coefficients = [0; 64];
                coefficients[position] = value;
                assert_parity(&coefficients, &[quant; 64], case);
                case += 1;
            }
        }
    }
    assert_eq!(case, 4480);
}

#[cfg(feature = "mvs-native-idct-arm64")]
#[test]
fn idct_foundation_simd_dc_rounding_and_range_wrap_thresholds() {
    // 与 transform 无关的 1024 项 Apple 表定义；阈值和所有量化值交叉。
    let table: [u8; 1024] = std::array::from_fn(|i| match i {
        0..=127 => (i + 128) as u8,
        128..=383 => 255,
        384..=895 => 0,
        _ => (i - 896) as u8,
    });
    let values = [
        i16::MIN,
        -8193,
        -8192,
        -8191,
        -3077,
        -3076,
        -3075,
        -1029,
        -1028,
        -1027,
        -5,
        -4,
        -3,
        -1,
        0,
        1,
        3,
        4,
        5,
        1019,
        1020,
        1021,
        3067,
        3068,
        3069,
        7163,
        7164,
        7165,
        8191,
        8192,
        i16::MAX,
    ];
    for quant in 0..=255 {
        for (index, value) in values.into_iter().enumerate() {
            let mut coefficients = [0; 64];
            coefficients[0] = value;
            let sample = table[(((i64::from(value) * i64::from(quant) + 4) >> 3) & 1023) as usize];
            assert_eq!(
                inverse_dct_8x8(&coefficients, &[quant; 64]).unwrap(),
                [sample; 64]
            );
            assert_parity(
                &coefficients,
                &[quant; 64],
                usize::from(quant) * values.len() + index,
            );
        }
    }
}

#[cfg(feature = "mvs-native-idct-arm64")]
#[test]
fn idct_foundation_simd_reproducible_dense_sparse_and_zero_quantization() {
    // xorshift64*，固定 seed；整数抽样覆盖完整 i16/u8 位域，另含稀疏/列 DC/行 DC。
    let mut state = 0x5a1d_c7e6_2026_1015u64;
    let mut next = || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    for case in 0..16_384 {
        let mut coefficients = [0i16; 64];
        let mut quantization = [0u8; 64];
        for i in 0..64 {
            let value = next();
            coefficients[i] = match case % 8 {
                0 => {
                    if i < 8 {
                        value as i16
                    } else {
                        0
                    }
                }
                1 => {
                    if i % 8 == 0 {
                        value as i16
                    } else {
                        0
                    }
                }
                2 => {
                    if value & 7 == 0 {
                        (value >> 8) as i16
                    } else {
                        0
                    }
                }
                3 => ((value % 65) as i16) - 32,
                _ => value as i16,
            };
            let quant = next() as u8;
            quantization[i] = if case % 8 == 4 && i % 2 == 0 {
                0
            } else {
                quant
            };
        }
        assert_parity(&coefficients, &quantization, case);
    }
}

#[test]
fn native_completion_range_fixed_answers() {
    let values = [
        0,
        127,
        128,
        383,
        384,
        895,
        896,
        1023,
        -1,
        -128,
        -129,
        -1024,
        i64::MIN,
        i64::MAX,
    ];
    let expected = [128, 255, 255, 255, 0, 0, 0, 127, 127, 0, 0, 128, 128, 127];
    for (pair, expected) in values.chunks_exact(2).zip(expected.chunks_exact(2)) {
        assert_eq!(completion_range_pair([pair[0], pair[1]]), expected);
    }
}

fn completion_range_pair(values: [i64; 2]) -> [u8; 2] {
    #[cfg(not(feature = "mvs-native-idct-arm64"))]
    {
        values.map(super::apple_idct_range_limit)
    }
    #[cfg(feature = "mvs-native-idct-arm64")]
    {
        use std::arch::aarch64::*;
        assert!(std::arch::is_aarch64_feature_detected!("neon"));
        // SAFETY: 两个有效 i64；上方检查 NEON，抽取的是已完成映射的字节。
        unsafe {
            let bytes = super::idct_arm64::range_limit_pair(vld1q_s64(values.as_ptr()));
            [vget_lane_u8::<0>(bytes), vget_lane_u8::<1>(bytes)]
        }
    }
}

#[test]
fn native_completion_range_all_indices_periods_signs_and_extrema() {
    let mut cases = 0;
    for period in [
        i64::MIN,
        -1_048_576,
        -2048,
        -1024,
        0,
        1024,
        2048,
        i64::MAX - 1023,
    ] {
        for index in (0..1024).step_by(2) {
            let values = [period + index, period + index + 1];
            assert_eq!(
                completion_range_pair(values),
                values.map(super::apple_idct_range_limit),
                "{values:?}"
            );
            cases += 2;
        }
    }
    assert_eq!(cases, 8192);
}
