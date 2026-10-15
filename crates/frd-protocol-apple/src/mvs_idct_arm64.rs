//! Apple MVS 的完整 i64 两遍 IDCT，仅由显式实验开关选择。
//!
//! 完整输入域：|coefficient * quant| <= 32768 * 255 = B = 8_355_840。
//! 对原始蝶形逐项取绝对值三角界：even tmp11/tmp12 <= 40_387 B，
//! odd tmp2 <= 137_832 B，所以任一输出在舍入前 <= 178_219 B。
//! 第一遍加 1024 后右移 11；含负数向下取整的保守 workspace 界
//! W = ceil((178_219 B + 1024) / 2048) = 727_133_521。
//! 第二遍全部中间量（含最终加 131072）<= 178_219 W + 131072
//! = 129_589_009_110_171 < 2^47。所有蝶形运算保留 i64，不窄化。
//! 12 个常量的 CSD 绝对位权和最多 41_556；常量乘法输入 <= 4W。
//! 降序 Horner 的每次移位、加减和负 C 最后取负均被绝对位权和覆盖：
//! 41_556 * 4W = 120_867_042_394_704 < 上述蝶形界 < 2^47。
//! 因而包括抵消前的部分和亦不溢出，不依赖 i32 截断或 i64 环绕。
//!
//! 不另走 DC 快捷路径：AC 为零时蝶形输出恰为 DC * 8192；第一遍
//! descale(..., 11) 恰为 DC * 4，第二遍 descale(..., 18) 恰为
//! descale(DC, 5)。量化零使原始系数非零也不影响这个恒等式。
//! 这保证语义相同；稀疏块性能需要独立测量，当前不作加速声明。
use super::{
    FIX_0_298631336, FIX_0_390180644, FIX_0_541196100, FIX_0_765366865, FIX_0_899976223,
    FIX_1_175875602, FIX_1_501321110, FIX_1_847759065, FIX_1_961570560, FIX_2_053119869,
    FIX_2_562915447, FIX_3_072711026, IDCT_CONST_BITS, IDCT_PASS1_BITS,
};
use std::arch::aarch64::*;

// AArch64 NEON 没有 i64 向量乘法。常量移加仅用 2d SIMD 加/减法，
// 保持两个独立变换并行；不抽取 lane 做标量变换，不使用 i32 窄化。
#[inline]
#[target_feature(enable = "neon")]
unsafe fn multiply<const C: i64>(value: int64x2_t) -> int64x2_t {
    // |C| 的 canonical signed-digit（NAF）；只在编译期生成两个位掩码。
    // 正数的最高非零 digit 恒为 +1，从 value 开始按降序 gap-shift Horner。
    const fn digits(mut coefficient: u64) -> (u64, u64) {
        let mut positive = 0;
        let mut negative = 0;
        let mut bit = 0;
        while coefficient != 0 {
            if coefficient & 1 != 0 {
                if coefficient & 3 == 1 {
                    positive |= 1u64 << bit;
                } else {
                    negative |= 1u64 << bit;
                    // (coefficient + 1) / 2，不让编译期 u64 加法溢出。
                    coefficient = (coefficient >> 1) + 1;
                    bit += 1;
                    continue;
                }
            }
            coefficient >>= 1;
            bit += 1;
        }
        (positive, negative)
    }
    let (positive, negative) = const { digits(C.unsigned_abs()) };
    let mut remaining = positive | negative;
    if remaining == 0 {
        return vdupq_n_s64(0);
    }
    let mut previous = 63 - remaining.leading_zeros();
    remaining ^= 1u64 << previous;
    let mut result = value;
    while remaining != 0 {
        let bit = 63 - remaining.leading_zeros();
        let gap = previous - bit;
        result = vshlq_s64(result, vdupq_n_s64(i64::from(gap)));
        // SAFETY: 只读写两个 NEON 寄存器；无内存、栈或标志寄存器副作用。
        // 显式 2d ADD/SUB 防止 LLVM 重构为逐 lane 的标量 MUL。
        unsafe {
            if positive & (1u64 << bit) != 0 {
                std::arch::asm!(
                    "add {result:v}.2d, {result:v}.2d, {value:v}.2d",
                    result = inout(vreg) result,
                    value = in(vreg) value,
                    options(pure, nomem, nostack, preserves_flags),
                );
            } else {
                std::arch::asm!(
                    "sub {result:v}.2d, {result:v}.2d, {value:v}.2d",
                    result = inout(vreg) result,
                    value = in(vreg) value,
                    options(pure, nomem, nostack, preserves_flags),
                );
            }
        }
        remaining ^= 1u64 << bit;
        previous = bit;
    }
    result = vshlq_s64(result, vdupq_n_s64(i64::from(previous)));
    if C < 0 {
        vnegq_s64(result)
    } else {
        result
    }
}

#[inline]
#[target_feature(enable = "neon")]
unsafe fn transform<const SHIFT: i32>(input: [int64x2_t; 8]) -> [int64x2_t; 8] {
    // SAFETY: 调用者已启用 NEON；所有参数为值，以下无指针访问。
    unsafe {
        let z1 = multiply::<FIX_0_541196100>(vaddq_s64(input[2], input[6]));
        let tmp2_even = vsubq_s64(z1, multiply::<FIX_1_847759065>(input[6]));
        let tmp3_even = vaddq_s64(z1, multiply::<FIX_0_765366865>(input[2]));
        let tmp0_even = vshlq_n_s64::<13>(vaddq_s64(input[0], input[4]));
        let tmp1_even = vshlq_n_s64::<13>(vsubq_s64(input[0], input[4]));
        let tmp10 = vaddq_s64(tmp0_even, tmp3_even);
        let tmp13 = vsubq_s64(tmp0_even, tmp3_even);
        let tmp11 = vaddq_s64(tmp1_even, tmp2_even);
        let tmp12 = vsubq_s64(tmp1_even, tmp2_even);

        let z1 = multiply::<{ -FIX_0_899976223 }>(vaddq_s64(input[7], input[1]));
        let z2 = multiply::<{ -FIX_2_562915447 }>(vaddq_s64(input[5], input[3]));
        let z3 = vaddq_s64(input[7], input[3]);
        let z4 = vaddq_s64(input[5], input[1]);
        let z5 = multiply::<FIX_1_175875602>(vaddq_s64(z3, z4));
        let z3 = vaddq_s64(multiply::<{ -FIX_1_961570560 }>(z3), z5);
        let z4 = vaddq_s64(multiply::<{ -FIX_0_390180644 }>(z4), z5);
        let tmp0 = vaddq_s64(multiply::<FIX_0_298631336>(input[7]), vaddq_s64(z1, z3));
        let tmp1 = vaddq_s64(multiply::<FIX_2_053119869>(input[5]), vaddq_s64(z2, z4));
        let tmp2 = vaddq_s64(multiply::<FIX_3_072711026>(input[3]), vaddq_s64(z2, z3));
        let tmp3 = vaddq_s64(multiply::<FIX_1_501321110>(input[1]), vaddq_s64(z1, z4));
        let values = [
            vaddq_s64(tmp10, tmp3),
            vaddq_s64(tmp11, tmp2),
            vaddq_s64(tmp12, tmp1),
            vaddq_s64(tmp13, tmp0),
            vsubq_s64(tmp13, tmp0),
            vsubq_s64(tmp12, tmp1),
            vsubq_s64(tmp11, tmp2),
            vsubq_s64(tmp10, tmp3),
        ];
        let round = vdupq_n_s64(1i64 << (SHIFT - 1));
        values.map(|value| vshrq_n_s64::<SHIFT>(vaddq_s64(value, round)))
    }
}

/// # Safety
/// 调用者须先确认 ARM64 NEON 可用；定长引用保证全部 load/store 范围有效。
#[inline(never)]
#[target_feature(enable = "neon")]
pub(super) unsafe fn inverse_dct_8x8_neon(
    coefficients: &[i16; 64],
    quantization: &[u8; 64],
) -> [u8; 64] {
    // SAFETY: NEON 前置条件由调用者保证；每次两个元素只访问有效相邻列/行。
    unsafe {
        let mut workspace = [0i64; 64];
        for column in (0..8).step_by(2) {
            let mut input = [vdupq_n_s64(0); 8];
            for row in 0..8 {
                let offset = row * 8 + column;
                let coefficients = [
                    i32::from(coefficients[offset]),
                    i32::from(coefficients[offset + 1]),
                ];
                let quantization = [
                    i32::from(quantization[offset]),
                    i32::from(quantization[offset + 1]),
                ];
                input[row] = vmull_s32(
                    vld1_s32(coefficients.as_ptr()),
                    vld1_s32(quantization.as_ptr()),
                );
            }
            let values = transform::<{ (IDCT_CONST_BITS - IDCT_PASS1_BITS) as i32 }>(input);
            for row in 0..8 {
                vst1q_s64(workspace.as_mut_ptr().add(row * 8 + column), values[row]);
            }
        }
        let mut output = [0u8; 64];
        for row in (0..8).step_by(2) {
            let input = std::array::from_fn(|column| {
                let pair = [
                    workspace[row * 8 + column],
                    workspace[(row + 1) * 8 + column],
                ];
                vld1q_s64(pair.as_ptr())
            });
            let values = transform::<{ (IDCT_CONST_BITS + IDCT_PASS1_BITS + 3) as i32 }>(input);
            for column in 0..8 {
                let bytes = range_limit_pair(values[column]);
                // 向量已完成环绕表与窄化；仅把两个字节散写到相邻输出行。
                output[row * 8 + column] = vget_lane_u8::<0>(bytes);
                output[(row + 1) * 8 + column] = vget_lane_u8::<1>(bytes);
            }
        }
        output
    }
}

/// Apple 的 10 位环绕表；全部 i64（包括负数和极值）先按位掩码。
/// 两个输出均处于 0..255，窄化不丢失有效位；其余字节固定为零。
/// # Safety
/// 调用者须先确认 ARM64 NEON 可用。
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn range_limit_pair(value: int64x2_t) -> uint8x8_t {
    let index = vandq_u64(vreinterpretq_u64_s64(value), vdupq_n_u64(0x3ff));
    let high_ramp = vaddq_u64(index, vdupq_n_u64(128));
    let low_ramp = vsubq_u64(index, vdupq_n_u64(896));
    let mapped = vbslq_u64(
        vcltq_u64(index, vdupq_n_u64(128)),
        high_ramp,
        vbslq_u64(
            vcltq_u64(index, vdupq_n_u64(384)),
            vdupq_n_u64(255),
            vbslq_u64(vcgeq_u64(index, vdupq_n_u64(896)), low_ramp, vdupq_n_u64(0)),
        ),
    );
    let words = vcombine_u32(vmovn_u64(mapped), vdup_n_u32(0));
    let halves = vcombine_u16(vmovn_u32(words), vdup_n_u16(0));
    vmovn_u16(halves)
}

#[cfg(test)]
mod constant_multiply_tests {
    use super::*;

    const B: i128 = 8_355_840;
    const W: i128 = 727_133_521;
    const INPUT_BOUND: i128 = 4 * W;
    const CSD_BOUND: i128 = 120_867_042_394_704;

    // 独立整数模型：按奇偶和模 4 递推 NAF，不调用生产 helper 或 NEON。
    fn signed_digits(mut coefficient: i128) -> Vec<(u32, i128)> {
        let mut digits = Vec::new();
        let mut bit = 0;
        while coefficient != 0 {
            let digit = if coefficient % 2 == 0 {
                0
            } else {
                2 - coefficient.rem_euclid(4)
            };
            if digit != 0 {
                digits.push((bit, digit));
            }
            coefficient = (coefficient - digit) / 2;
            bit += 1;
        }
        digits
    }

    fn check_model(coefficient: i128, input: i128) {
        let digits = signed_digits(coefficient);
        assert_eq!(
            digits
                .iter()
                .map(|&(bit, digit)| digit * (1i128 << bit))
                .sum::<i128>(),
            coefficient
        );
        assert!(digits.windows(2).all(|pair| pair[1].0 > pair[0].0 + 1));
        let weight: i128 = digits.iter().map(|&(bit, _)| 1i128 << bit).sum();
        assert!(weight <= 41_556);
        assert!(weight * INPUT_BOUND <= CSD_BOUND);
        let mut prefix = 0i128;
        let mut partial = 0i128;
        let mut previous = digits.last().unwrap().0;
        for &(bit, digit) in digits.iter().rev() {
            let gap = previous - bit;
            partial <<= gap;
            assert!(partial.abs() <= CSD_BOUND);
            partial += digit * input;
            prefix = (prefix << gap) + digit;
            assert_eq!(partial, input * prefix);
            assert!(partial.abs() <= CSD_BOUND);
            previous = bit;
        }
        partial <<= previous;
        assert!(partial.abs() <= CSD_BOUND);
        assert_eq!(partial, coefficient * input);
        // 生产实现以 |C| 构造序列，负 C 最后取负；两个符号都在同一界内。
        assert!((-partial).abs() <= CSD_BOUND);
    }

    fn pairs() -> Vec<[i64; 2]> {
        let edges = [
            0,
            1,
            -1,
            4 * B,
            -4 * B,
            INPUT_BOUND,
            -INPUT_BOUND,
            (1i128 << 31) - 1,
            1i128 << 31,
            (1i128 << 31) + 1,
            -(1i128 << 31) - 1,
            -(1i128 << 31),
            -(1i128 << 31) + 1,
        ];
        let mut values = Vec::new();
        for left in edges {
            for right in edges {
                values.push([i64::try_from(left).unwrap(), i64::try_from(right).unwrap()]);
            }
        }
        let mut state = 0x5a7c_5d64_2026_1016u64;
        for _ in 0..256 {
            let pair = std::array::from_fn(|_| {
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                let random = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
                (i128::from(random) % (2 * INPUT_BOUND + 1) - INPUT_BOUND) as i64
            });
            values.push(pair);
        }
        values
    }

    fn check<const C: i64>() {
        assert!(std::arch::is_aarch64_feature_detected!("neon"));
        for pair in pairs() {
            let expected = pair.map(|value| {
                check_model(i128::from(C), i128::from(value));
                i64::try_from(i128::from(value) * i128::from(C)).unwrap()
            });
            let mut actual = [0i64; 2];
            // SAFETY: 上方检查 NEON；定长数组保证两 lane load/store 都有效。
            unsafe {
                vst1q_s64(actual.as_mut_ptr(), multiply::<C>(vld1q_s64(pair.as_ptr())));
            }
            assert_eq!(actual, expected, "常量 C={C}，独立 lane 输入={pair:?}");
        }
    }

    #[test]
    fn idct_constant_multiply_full_bound_i128_oracle_and_signed_digit_model() {
        macro_rules! both_signs {
            ($($constant:ident),+ $(,)?) => { $(
                check::<$constant>();
                check::<{ -$constant }>();
            )+ };
        }
        both_signs!(
            FIX_0_298631336,
            FIX_0_390180644,
            FIX_0_541196100,
            FIX_0_765366865,
            FIX_0_899976223,
            FIX_1_175875602,
            FIX_1_501321110,
            FIX_1_847759065,
            FIX_1_961570560,
            FIX_2_053119869,
            FIX_2_562915447,
            FIX_3_072711026,
        );
    }
}
