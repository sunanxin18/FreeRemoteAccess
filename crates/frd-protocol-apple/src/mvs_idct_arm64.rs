//! Apple MVS 的完整 i64 两遍 IDCT，仅由显式实验开关选择。
//!
//! 完整输入域：|coefficient * quant| <= 32768 * 255 = B = 8_355_840。
//! 对原始蝶形逐项取绝对值三角界：even tmp11/tmp12 <= 40_387 B，
//! odd tmp2 <= 137_832 B，所以任一输出在舍入前 <= 178_219 B。
//! 第一遍加 1024 后右移 11；含负数向下取整的保守 workspace 界
//! W = ceil((178_219 B + 1024) / 2048) = 727_133_521。
//! 第二遍全部中间量（含最终加 131072）<= 178_219 W + 131072
//! = 129_589_009_110_171 < 2^47。所有蝶形运算保留 i64，不窄化。
//! 常量乘法的二进制移加中间项最多为 32768 * 4W，亦小于此界。
//!
//! 不另走 DC 快捷路径：AC 为零时蝶形输出恰为 DC * 8192；第一遍
//! descale(..., 11) 恰为 DC * 4，第二遍 descale(..., 18) 恰为
//! descale(DC, 5)。量化零使原始系数非零也不影响这个恒等式。
//! 这保证语义相同；稀疏块性能需要独立测量，当前不作加速声明。
use super::{
    apple_idct_range_limit, FIX_0_298631336, FIX_0_390180644, FIX_0_541196100, FIX_0_765366865,
    FIX_0_899976223, FIX_1_175875602, FIX_1_501321110, FIX_1_847759065, FIX_1_961570560,
    FIX_2_053119869, FIX_2_562915447, FIX_3_072711026, IDCT_CONST_BITS, IDCT_PASS1_BITS,
};
use std::arch::aarch64::*;

// AArch64 NEON 没有 i64 向量乘法。常量移加仅用 2d SIMD 加/减法，
// 保持两个独立变换并行；不抽取 lane 做标量变换，不使用 i32 窄化。
#[inline]
#[target_feature(enable = "neon")]
unsafe fn multiply<const C: i64>(value: int64x2_t) -> int64x2_t {
    let mut result = vdupq_n_s64(0);
    let mut term = value;
    let mut bits = C.unsigned_abs();
    while bits != 0 {
        if bits & 1 != 0 {
            // SAFETY: 只读写两个 NEON 寄存器；无内存、栈或标志寄存器副作用。
            // LLVM 会把纯 intrinsic 移加重新合成为 i64 乘法，再拆成标量 MUL；
            // 显式 2d ADD 保留双 lane 计算，避免目标代码抽取 lane 做变换乘法。
            unsafe {
                std::arch::asm!(
                    "add {result:v}.2d, {result:v}.2d, {term:v}.2d",
                    result = inout(vreg) result,
                    term = in(vreg) term,
                    options(pure, nomem, nostack, preserves_flags),
                );
            }
        }
        bits >>= 1;
        if bits != 0 {
            term = vaddq_s64(term, term);
        }
    }
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
                output[row * 8 + column] =
                    apple_idct_range_limit(vgetq_lane_s64::<0>(values[column]));
                output[(row + 1) * 8 + column] =
                    apple_idct_range_limit(vgetq_lane_s64::<1>(values[column]));
            }
        }
        output
    }
}
