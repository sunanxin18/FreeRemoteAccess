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
//! 整块 effective AC 为零时，原反量化 DC 为 D = i64(c[0]) * i64(q[0])。
//! 第一遍 Wdc = (8192D + 1024) >> 11 = 4D；第二遍
//! S = (8192Wdc + 131072) >> 18 = (Wdc + 16) >> 5 = (D + 4) >> 3。
//! 负半数向上舍入，完整 D 域 [-8_355_840, 8_355_585] 在 i64 内精确。
//! 仅在每个 AC 的系数或量化为零时早返；仍使用原 Apple 10 位环绕表。
//! 非 DC 块保持完整 CSD 蝶形；新增检测成本与稀疏块性能须独立测量。
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

// 每组八 lane，偏移 0..56 覆盖完整数组；仅首组清除自然序 DC 的 lane0。
// AND 的是独立非零比较掩码，不是原系数/量化位值或截断乘积。
#[inline]
#[target_feature(enable = "neon")]
unsafe fn effective_ac_is_zero(coefficients: &[i16; 64], quantization: &[u8; 64]) -> bool {
    unsafe {
        let mut any = vdupq_n_u16(0);
        for offset in (0..64).step_by(8) {
            let c = vld1q_s16(coefficients.as_ptr().add(offset));
            let q = vmovl_u8(vld1_u8(quantization.as_ptr().add(offset)));
            let mut active = vandq_u16(
                vmvnq_u16(vceqq_s16(c, vdupq_n_s16(0))),
                vmvnq_u16(vceqq_u16(q, vdupq_n_u16(0))),
            );
            if offset == 0 {
                active = vsetq_lane_u16::<0>(0, active);
            }
            any = vorrq_u16(any, active);
        }
        vmaxvq_u16(any) == 0
    }
}

#[inline]
#[target_feature(enable = "neon")]
unsafe fn effective_dc_sample(coefficients: int16x8_t, quantization: uint8x8_t) -> int64x2_t {
    let c = vget_low_s32(vmovl_s16(vget_low_s16(coefficients)));
    let q = vget_low_u32(vmovl_u16(vget_low_u16(vmovl_u8(quantization))));
    // c 符号扩展，q 无符号扩展至 0..255，随后 signed widening multiply 安全。
    let sample: int64x2_t;
    // SAFETY: 仅操作声明的向量寄存器，无内存/栈/标志副作用。
    // 实际 LLVM 曾利用后续 10 位 mask 消去符号扩展并改为 UMLAL/USHR。
    // 此最小屏障保留原 D 的 signed widening、i64 舍入与 SSHR 契约。
    unsafe {
        std::arch::asm!(
            "smull {sample:v}.2d, {c:v}.2s, {q:v}.2s",
            "dup {sample:v}.2d, {sample:v}.d[0]",
            "add {sample:v}.2d, {sample:v}.2d, {round:v}.2d",
            "sshr {sample:v}.2d, {sample:v}.2d, #3",
            sample = out(vreg) sample,
            c = in(vreg) c,
            q = in(vreg) q,
            round = in(vreg) vdupq_n_s64(4),
            options(pure, nomem, nostack, preserves_flags),
        );
    }
    sample
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
        if effective_ac_is_zero(coefficients, quantization) {
            let sample = effective_dc_sample(
                vld1q_s16(coefficients.as_ptr()),
                vld1_u8(quantization.as_ptr()),
            );
            let bytes = vdupq_lane_u8::<0>(range_limit_pair(sample));
            let mut output = [0u8; 64];
            // 四次有界 16-byte 写入，不要求额外对齐或数组尾部 padding。
            vst1q_u8(output.as_mut_ptr(), bytes);
            vst1q_u8(output.as_mut_ptr().add(16), bytes);
            vst1q_u8(output.as_mut_ptr().add(32), bytes);
            vst1q_u8(output.as_mut_ptr().add(48), bytes);
            return output;
        }
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

#[cfg(test)]
mod effective_dc_shared_tests {
    use super::*;

    fn table(sample: i128) -> u8 {
        match sample.rem_euclid(1024) {
            k @ 0..=127 => (k + 128) as u8,
            128..=383 => 255,
            384..=895 => 0,
            k => (k - 896) as u8,
        }
    }

    fn check_entry(c: &[i16; 64], q: &[u8; 64]) {
        assert!(std::arch::is_aarch64_feature_detected!("neon"));
        let expected = super::super::inverse_dct_8x8_scalar(c, q).unwrap();
        // SAFETY: 已检查 NEON，定长输入输出。
        assert_eq!(unsafe { inverse_dct_8x8_neon(c, q) }, expected);
    }

    #[test]
    fn idct_dc_full_domain_premap_two_pass_i128() {
        assert!(std::arch::is_aarch64_feature_detected!("neon"));
        let mut count = 0;
        // 每个 i16×u8 组合；两个 lane 的系数/量化独立、符号相反。
        for q in 0..=255i32 {
            for c in i32::from(i16::MIN)..=i32::from(i16::MAX) {
                let cs = [c, -1 - c];
                let qs = [q, 255 - q];
                unsafe {
                    let d = vmull_s32(vld1_s32(cs.as_ptr()), vld1_s32(qs.as_ptr()));
                    let mut input = [vdupq_n_s64(0); 8];
                    input[0] = std::hint::black_box(d);
                    let w = transform::<11>(input);
                    let mut second = [vdupq_n_s64(0); 8];
                    second[0] = w[0];
                    let s = transform::<18>(second);
                    let mut actual_d = [0i64; 2];
                    vst1q_s64(actual_d.as_mut_ptr(), d);
                    for lane in 0..2 {
                        let expected_d = i128::from(cs[lane]) * i128::from(qs[lane]);
                        let expected_w = (8192 * expected_d + 1024).div_euclid(2048);
                        let expected_s = (8192 * expected_w + 131072).div_euclid(262144);
                        assert_eq!(i128::from(actual_d[lane]), expected_d);
                        assert_eq!(expected_w, 4 * expected_d);
                        assert_eq!(expected_s, (expected_d + 4).div_euclid(8));
                        for column in 0..8 {
                            let mut actual_w = [0i64; 2];
                            let mut actual_s = [0i64; 2];
                            vst1q_s64(actual_w.as_mut_ptr(), w[column]);
                            vst1q_s64(actual_s.as_mut_ptr(), s[column]);
                            assert_eq!(i128::from(actual_w[lane]), expected_w);
                            assert_eq!(i128::from(actual_s[lane]), expected_s);
                        }
                    }
                }
                count += 1;
            }
        }
        assert_eq!(count, 16_777_216);
    }

    #[test]
    fn idct_dc_entry_table_periods_effective_zero_and_all_ac_positions() {
        for period in -3..=3 {
            for index in 0..1024 {
                let sample = period * 1024 + index;
                let mut c = [0i16; 64];
                let q = [1u8; 64];
                c[0] = (sample * 8) as i16;
                check_entry(&c, &q);
                assert_eq!(
                    unsafe { inverse_dct_8x8_neon(&c, &q) },
                    [table(sample.into()); 64]
                );
            }
        }
        for dc in [i16::MIN, -8192, -12, -5, -4, -1, 0, 4, 12, 8192, i16::MAX] {
            for dq in [0, 1, 2, 127, 128, 255] {
                for style in 0..3 {
                    let mut c = [0i16; 64];
                    let mut q = [0u8; 64];
                    c[0] = dc;
                    q[0] = dq;
                    for i in 1..64 {
                        if style == 0 || (style == 2 && i % 2 == 0) {
                            c[i] = if i % 3 == 0 { i16::MIN } else { i16::MAX };
                        } else {
                            q[i] = 255;
                        }
                    }
                    check_entry(&c, &q);
                    let s = (i128::from(dc) * i128::from(dq) + 4).div_euclid(8);
                    assert_eq!(unsafe { inverse_dct_8x8_neon(&c, &q) }, [table(s); 64]);
                }
            }
        }
        for position in 1..64 {
            for (ac, aq) in [
                (2, 1),
                (-32768, 128),
                (-32768, 2),
                (32767, 255),
                (-1, 1),
                (1, 1),
            ] {
                for dq in [0, 255] {
                    let mut c = [0i16; 64];
                    let mut q = [0u8; 64];
                    c[0] = -12;
                    q[0] = dq;
                    c[position] = ac;
                    q[position] = aq;
                    check_entry(&c, &q);
                }
            }
        }
    }

    #[test]
    fn idct_dc_mutant_arithmetic_and_predicate_controls_are_rejected() {
        // 真实执行错误控制，并要求同一精确相等断言失败；不修改旧 oracle。
        fn rejected<T: std::fmt::Debug + PartialEq + std::panic::RefUnwindSafe>(a: T, b: T) {
            assert!(std::panic::catch_unwind(|| assert_eq!(a, b)).is_err());
        }
        rejected((-5i128 + 4) / 8, (-5i128 + 4).div_euclid(8));
        rejected((12i128 + 16) >> 5, (12i128 + 4) >> 3);
        rejected(
            (8192i128 + 4).div_euclid(8).clamp(-128, 127) as i16 + 128,
            i16::from(table(1024)),
        );
        for (c, q) in [(2i16, 1u8), (-32768, 128), (-32768, 2)] {
            let expected = i64::from(c) * i64::from(q) != 0;
            let mutant = if c == 2 {
                (c as u16 & u16::from(q)) != 0
            } else if q == 128 {
                c as u8 != 0 && q != 0
            } else {
                (i64::from(c) * i64::from(q)) as i16 != 0
            };
            rejected(mutant, expected);
        }
        for (d, s) in [(-4, 0), (-5, -1), (-12, -1), (4, 1)] {
            assert_eq!((d + 4i128).div_euclid(8), s);
        }
    }
}

#[cfg(test)]
mod effective_dc_candidate_tests {
    use super::*;

    #[test]
    fn idct_dc_candidate_exact_predicate_all_positions_and_unique_exclusion() {
        assert!(std::arch::is_aarch64_feature_detected!("neon"));
        for position in 0..64 {
            for (value, quant) in [
                (2, 1),
                (-32768, 128),
                (-32768, 2),
                (32767, 255),
                (-1, 1),
                (1, 1),
            ] {
                let mut c = [0i16; 64];
                let mut q = [0u8; 64];
                c[position] = value;
                q[position] = quant;
                assert_eq!(unsafe { effective_ac_is_zero(&c, &q) }, position == 0);
                q[position] = 0;
                assert!(unsafe { effective_ac_is_zero(&c, &q) });
                q[position] = quant;
                c[position] = 0;
                assert!(unsafe { effective_ac_is_zero(&c, &q) });
            }
        }
        let mut c = [i16::MIN; 64];
        let mut q = [0u8; 64];
        q[0] = 255;
        assert!(unsafe { effective_ac_is_zero(&c, &q) });
        for position in 1..64 {
            if position % 2 == 0 {
                c[position] = 0;
                q[position] = 255;
            }
        }
        assert!(unsafe { effective_ac_is_zero(&c, &q) });
        q[0] = 0;
        c[63] = 1;
        q[63] = 1;
        assert!(!unsafe { effective_ac_is_zero(&c, &q) });
        // 小 AC 经过最终舍入可能同色，仍必须判定为非 DC。
        c.fill(0);
        q.fill(0);
        c[1] = 1;
        q[1] = 1;
        assert_eq!(unsafe { inverse_dct_8x8_neon(&c, &q) }, [128; 64]);
        assert!(!unsafe { effective_ac_is_zero(&c, &q) });
    }

    #[test]
    fn idct_dc_candidate_full_domain_actual_sample_before_table() {
        assert!(std::arch::is_aarch64_feature_detected!("neon"));
        for q in 0..=255u16 {
            for c in i32::from(i16::MIN)..=i32::from(i16::MAX) {
                let mut cs = [0i16; 8];
                let mut qs = [0u8; 8];
                cs[0] = c as i16;
                cs[1] = (-1 - c) as i16;
                qs[0] = q as u8;
                qs[1] = (255 - q) as u8;
                let expected = (i128::from(c) * i128::from(q) + 4).div_euclid(8);
                let mut actual = [0i64; 2];
                unsafe {
                    let sample = effective_dc_sample(
                        std::hint::black_box(vld1q_s16(cs.as_ptr())),
                        std::hint::black_box(vld1_u8(qs.as_ptr())),
                    );
                    vst1q_s64(actual.as_mut_ptr(), sample);
                }
                assert_eq!(actual.map(i128::from), [expected; 2]);
            }
        }
    }
}
