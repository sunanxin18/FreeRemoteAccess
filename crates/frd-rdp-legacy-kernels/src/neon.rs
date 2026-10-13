// SIMD 操作适配层：所有读写仅访问定长数组。
use core::arch::aarch64::*;
pub(super) type V = int32x4_t;
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn load(a: &[i16; 4]) -> V {
    vmovl_s16(vld1_s16(a.as_ptr()))
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn splat(a: i32) -> V {
    vdupq_n_s32(a)
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn add(a: V, b: V) -> V {
    vaddq_s32(a, b)
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn sub(a: V, b: V) -> V {
    vsubq_s32(a, b)
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn and(a: V, b: V) -> V {
    vandq_s32(a, b)
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn xor(a: V, b: V) -> V {
    veorq_s32(a, b)
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn not(a: V) -> V {
    vmvnq_s32(a)
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn gt(a: V, b: V) -> V {
    vreinterpretq_s32_u32(vcgtq_s32(a, b))
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn shr<const N: i32>(a: V) -> V {
    vshrq_n_s32::<N>(a)
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn shl<const N: i32>(a: V) -> V {
    vshlq_n_s32::<N>(a)
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn mul(a: V, b: i32) -> V {
    vmulq_n_s32(a, b)
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn any_negative(a: V) -> bool {
    vminvq_s32(a) < 0
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn narrow(a: V) -> [i16; 4] {
    {
        let mut out = [0; 4];
        vst1_s16(out.as_mut_ptr(), vmovn_s32(a));
        out
    }
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn prefix(a: V) -> V {
    {
        let z = splat(0);
        let a = add(a, vextq_s32::<3>(z, a));
        add(a, vextq_s32::<2>(z, a))
    }
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn clamp_byte(a: V) -> V {
    vminq_s32(vmaxq_s32(a, splat(0)), splat(255))
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn rgba(r: V, g: V, b: V) -> [u8; 16] {
    {
        let mut out = [0; 16];
        let v = vorrq_u32(
            vorrq_u32(
                vreinterpretq_u32_s32(r),
                vshlq_n_u32::<8>(vreinterpretq_u32_s32(g)),
            ),
            vorrq_u32(
                vshlq_n_u32::<16>(vreinterpretq_u32_s32(b)),
                vdupq_n_u32(0xff000000),
            ),
        );
        vst1q_u8(out.as_mut_ptr(), vreinterpretq_u8_u32(v));
        out
    }
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn shift8(data: &mut [i16; 8], n: u8) {
    {
        let v = vld1q_s16(data.as_ptr());
        vst1q_s16(data.as_mut_ptr(), vshlq_s16(v, vdupq_n_s16(i16::from(n))));
    }
}
#[inline]
#[target_feature(enable = "neon")]
// 显式 store 防止 LLVM 把整段 intrinsic 循环折叠为未经本任务验证的 libc memset。
pub(super) unsafe fn zero8(data: &mut [i16; 8]) {
    // SAFETY: 恰好 16 个可写字节；ISA 已检测，不要求额外对齐或 padding。
    core::arch::asm!("movi v0.8h, #0\nstr q0, [{ptr}]", ptr = in(reg) data.as_mut_ptr(), out("v0") _, options(nostack, preserves_flags));
}
