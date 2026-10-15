//! SSE2：4×u32 lanes，每 lane 一个 RGBA tuple；无 SSSE3/SSE4/AVX。
use crate::{CursorBitmapMode, CursorXorFormat};
#[cfg(target_arch = "x86")]
use core::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;
pub(super) type V = __m128i;
#[target_feature(enable = "sse2")]
unsafe fn choose(mask: V, yes: V, no: V) -> V {
    _mm_or_si128(_mm_and_si128(mask, yes), _mm_andnot_si128(mask, no))
}
#[target_feature(enable = "sse2")]
unsafe fn bits(byte: u8, low: bool) -> V {
    let mut v = _mm_set1_epi32(i32::from(byte));
    if low {
        v = _mm_slli_epi32::<4>(v);
    }
    let mask = _mm_set_epi32(16, 32, 64, 128);
    _mm_xor_si128(
        _mm_cmpeq_epi32(_mm_and_si128(v, mask), _mm_setzero_si128()),
        _mm_set1_epi32(-1),
    )
}
#[target_feature(enable = "sse2")]
unsafe fn packed(r: V, g: V, b: V, a: V) -> V {
    _mm_or_si128(
        _mm_or_si128(r, _mm_slli_epi32::<8>(g)),
        _mm_or_si128(_mm_slli_epi32::<16>(b), _mm_slli_epi32::<24>(a)),
    )
}
#[target_feature(enable = "sse2")]
unsafe fn square(c: V) -> V {
    // 每个 dword 高16位为零；低16位 unsigned product<=65025，高半无交叉乘积。
    _mm_srli_epi32::<8>(_mm_mullo_epi16(c, c))
}
/// # Safety
/// ISA 已检查；source 指向16byte staging（BGR24只从原行拷12byte），mask是当前字节。
#[target_feature(enable = "sse2")]
pub(super) unsafe fn decode4(
    format: CursorXorFormat,
    mode: CursorBitmapMode,
    source: *const u8,
    and_byte: u8,
    low: bool,
) -> V {
    let byte = _mm_set1_epi32(255);
    let zero = _mm_setzero_si128();
    let color = match format {
        CursorXorFormat::Mono1 => {
            let c = _mm_and_si128(bits(*source, low), byte);
            packed(c, c, c, byte)
        }
        CursorXorFormat::Rgb565 => {
            let words = _mm_loadl_epi64(source.cast());
            let v = _mm_unpacklo_epi16(words, zero);
            let v5 = _mm_set1_epi32(31);
            let v6 = _mm_set1_epi32(63);
            let r = _mm_and_si128(_mm_srli_epi32::<11>(v), v5);
            let g = _mm_and_si128(_mm_srli_epi32::<5>(v), v6);
            let b = _mm_and_si128(v, v5);
            let rb_scale = _mm_set1_epi32(527);
            let g_scale = _mm_set1_epi32(259);
            let rb_bias = _mm_set1_epi32(23);
            let g_bias = _mm_set1_epi32(33);
            let r = _mm_srli_epi32::<6>(_mm_add_epi32(_mm_mullo_epi16(r, rb_scale), rb_bias));
            let g = _mm_srli_epi32::<6>(_mm_add_epi32(_mm_mullo_epi16(g, g_scale), g_bias));
            let b = _mm_srli_epi32::<6>(_mm_add_epi32(_mm_mullo_epi16(b, rb_scale), rb_bias));
            packed(r, g, b, byte)
        }
        CursorXorFormat::Bgr24 | CursorXorFormat::Bgra32 => {
            let raw = _mm_loadu_si128(source.cast());
            let v = if format == CursorXorFormat::Bgr24 {
                // 只用 SSE2 byte shifts/unpacks 将连续12byte排成四个低24bit dword。
                let p0 = raw;
                let p1 = _mm_srli_si128::<3>(raw);
                let p2 = _mm_srli_si128::<6>(raw);
                let p3 = _mm_srli_si128::<9>(raw);
                _mm_unpacklo_epi64(_mm_unpacklo_epi32(p0, p1), _mm_unpacklo_epi32(p2, p3))
            } else {
                raw
            };
            let r = _mm_and_si128(_mm_srli_epi32::<16>(v), byte);
            let g = _mm_and_si128(_mm_srli_epi32::<8>(v), byte);
            let b = _mm_and_si128(v, byte);
            let a = if format == CursorXorFormat::Bgra32 {
                _mm_srli_epi32::<24>(v)
            } else {
                byte
            };
            packed(r, g, b, a)
        }
    };
    let and_mask = bits(and_byte, low);
    // 分类必须在平方前按完整 RGBA equality，partial-alpha 黑白都不匹配。
    let transparent = _mm_and_si128(
        and_mask,
        _mm_cmpeq_epi32(color, _mm_set1_epi32(0xff000000u32 as i32)),
    );
    let invert = _mm_and_si128(and_mask, _mm_cmpeq_epi32(color, _mm_set1_epi32(-1)));
    let (ordinary, inverted) = match mode {
        CursorBitmapMode::Software => {
            let r = _mm_and_si128(color, byte);
            let g = _mm_and_si128(_mm_srli_epi32::<8>(color), byte);
            let b = _mm_and_si128(_mm_srli_epi32::<16>(color), byte);
            (
                packed(square(r), square(g), square(b), _mm_srli_epi32::<24>(color)),
                _mm_set1_epi32(0x00ffffff),
            )
        }
        CursorBitmapMode::Accelerated { odd_row } => {
            let mut check = _mm_set_epi32(0, 0x00ffffff, 0, 0x00ffffff);
            if odd_row {
                check = _mm_xor_si128(check, _mm_set1_epi32(0x00ffffff));
            }
            (
                color,
                _mm_or_si128(check, _mm_set1_epi32(0xff000000u32 as i32)),
            )
        }
    };
    choose(transparent, zero, choose(invert, inverted, ordinary))
}
#[target_feature(enable = "sse2")]
unsafe fn blend_channel(source: V, destination: V, inverse_alpha: V) -> V {
    let contribution = _mm_srli_epi32::<8>(_mm_mullo_epi16(destination, inverse_alpha));
    // 和最大509；显式 mask 为 modulo256，绝不是饱和 pack 的偶然截断。
    _mm_and_si128(_mm_add_epi32(source, contribution), _mm_set1_epi32(255))
}
/// # Safety
/// 两个指针各可读16byte；ISA 已检查。
#[target_feature(enable = "sse2")]
pub(super) unsafe fn composite4(source: *const u8, background: *const u8) -> V {
    let s = _mm_loadu_si128(source.cast());
    let d = _mm_loadu_si128(background.cast());
    let byte = _mm_set1_epi32(255);
    let a = _mm_srli_epi32::<24>(s);
    let inverse = _mm_sub_epi32(byte, a);
    let r = blend_channel(_mm_and_si128(s, byte), _mm_and_si128(d, byte), inverse);
    let g = blend_channel(
        _mm_and_si128(_mm_srli_epi32::<8>(s), byte),
        _mm_and_si128(_mm_srli_epi32::<8>(d), byte),
        inverse,
    );
    let b = blend_channel(
        _mm_and_si128(_mm_srli_epi32::<16>(s), byte),
        _mm_and_si128(_mm_srli_epi32::<16>(d), byte),
        inverse,
    );
    let ordinary = packed(r, g, b, _mm_srli_epi32::<24>(d));
    let alpha_zero = _mm_cmpeq_epi32(a, _mm_setzero_si128());
    let invert = _mm_cmpeq_epi32(s, _mm_set1_epi32(0x00ffffff));
    let inverted = _mm_or_si128(
        _mm_xor_si128(d, _mm_set1_epi32(0x00ffffff)),
        _mm_set1_epi32(0xff000000u32 as i32),
    );
    choose(invert, inverted, choose(alpha_zero, d, ordinary))
}
#[target_feature(enable = "sse2")]
pub(super) unsafe fn store(v: V, dst: *mut u8) {
    _mm_storeu_si128(dst.cast(), v)
}
