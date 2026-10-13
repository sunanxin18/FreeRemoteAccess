// 仅 SSE2；不使用需要 SSE3 的 lddqu，也不需要 SSE4.1 mullo。
#[cfg(target_arch = "x86")]
use core::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;
pub(super) type V = __m128i;
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn load(a: &[i16; 4]) -> V {
    {
        let a = _mm_loadl_epi64(a.as_ptr().cast());
        _mm_unpacklo_epi16(a, _mm_srai_epi16::<15>(a))
    }
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn splat(a: i32) -> V {
    _mm_set1_epi32(a)
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn add(a: V, b: V) -> V {
    _mm_add_epi32(a, b)
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn sub(a: V, b: V) -> V {
    _mm_sub_epi32(a, b)
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn and(a: V, b: V) -> V {
    _mm_and_si128(a, b)
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn xor(a: V, b: V) -> V {
    _mm_xor_si128(a, b)
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn not(a: V) -> V {
    _mm_xor_si128(a, _mm_set1_epi32(-1))
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn gt(a: V, b: V) -> V {
    _mm_cmpgt_epi32(a, b)
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn shr<const N: i32>(a: V) -> V {
    _mm_srai_epi32::<N>(a)
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn shl<const N: i32>(a: V) -> V {
    _mm_slli_epi32::<N>(a)
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn mul(a: V, b: i32) -> V {
    {
        let b = splat(b);
        let even = _mm_mul_epu32(a, b);
        let odd = _mm_mul_epu32(_mm_srli_si128::<4>(a), b);
        _mm_unpacklo_epi32(
            _mm_shuffle_epi32::<0x88>(even),
            _mm_shuffle_epi32::<0x88>(odd),
        )
    }
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn any_negative(a: V) -> bool {
    _mm_movemask_epi8(_mm_srai_epi32::<31>(a)) != 0
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn narrow(a: V) -> [i16; 4] {
    {
        let a = shr::<16>(shl::<16>(a));
        let a = _mm_packs_epi32(a, a);
        let mut out = [0; 4];
        _mm_storel_epi64(out.as_mut_ptr().cast(), a);
        out
    }
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn prefix(a: V) -> V {
    {
        let a = add(a, _mm_slli_si128::<4>(a));
        add(a, _mm_slli_si128::<8>(a))
    }
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn clamp_byte(a: V) -> V {
    {
        let a = _mm_andnot_si128(_mm_srai_epi32::<31>(a), a);
        let mask = gt(a, splat(255));
        _mm_or_si128(and(mask, splat(255)), _mm_andnot_si128(mask, a))
    }
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn rgba(r: V, g: V, b: V) -> [u8; 16] {
    {
        let v = _mm_or_si128(
            _mm_or_si128(r, shl::<8>(g)),
            _mm_or_si128(shl::<16>(b), splat(0xff000000u32 as i32)),
        );
        let mut out = [0; 16];
        _mm_storeu_si128(out.as_mut_ptr().cast(), v);
        out
    }
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn shift8(data: &mut [i16; 8], n: u8) {
    {
        let v = _mm_loadu_si128(data.as_ptr().cast());
        _mm_storeu_si128(
            data.as_mut_ptr().cast(),
            _mm_sll_epi16(v, _mm_cvtsi32_si128(i32::from(n))),
        );
    }
}
#[inline]
#[target_feature(enable = "sse2")]
// 显式 store 防止 LLVM 把整段 intrinsic 循环折叠为未经本任务验证的 libc memset。
pub(super) unsafe fn zero8(data: &mut [i16; 8]) {
    // SAFETY: 恰好 16 个可写字节；ISA 已检测，不要求额外对齐或 padding。
    core::arch::asm!("pxor xmm0, xmm0\nmovdqu [{ptr}], xmm0", ptr = in(reg) data.as_mut_ptr(), out("xmm0") _, options(nostack, preserves_flags));
}
