//! AArch64 NEON：4×u32 lanes，extract/classify/square/blend/invert 全部 SIMD。
use crate::{CursorBitmapMode, CursorXorFormat};
use core::arch::aarch64::*;
pub(super) type V = uint32x4_t;
#[target_feature(enable = "neon")]
unsafe fn packed(r: V, g: V, b: V, a: V) -> V {
    vorrq_u32(
        vorrq_u32(r, vshlq_n_u32::<8>(g)),
        vorrq_u32(vshlq_n_u32::<16>(b), vshlq_n_u32::<24>(a)),
    )
}
#[target_feature(enable = "neon")]
unsafe fn bits(byte: u8, low: bool) -> V {
    let mut v = vdupq_n_u32(u32::from(byte));
    if low {
        v = vshlq_n_u32::<4>(v);
    }
    let selectors = [128u32, 64, 32, 16];
    vmvnq_u32(vceqq_u32(
        vandq_u32(v, vld1q_u32(selectors.as_ptr())),
        vdupq_n_u32(0),
    ))
}
#[target_feature(enable = "neon")]
unsafe fn square(c: V) -> V {
    vshrq_n_u32::<8>(vmulq_u32(c, c))
}
/// # Safety
/// ISA 已检查；source 是16byte staging；只在 bounded layout 层读取原行有效字节。
#[target_feature(enable = "neon")]
pub(super) unsafe fn decode4(
    format: CursorXorFormat,
    mode: CursorBitmapMode,
    source: *const u8,
    and_byte: u8,
    low: bool,
) -> V {
    let byte = vdupq_n_u32(255);
    let zero = vdupq_n_u32(0);
    let color = match format {
        CursorXorFormat::Mono1 => {
            let c = vandq_u32(bits(*source, low), byte);
            packed(c, c, c, byte)
        }
        CursorXorFormat::Rgb565 => {
            let v = vmovl_u16(vld1_u16(source.cast()));
            let r = vandq_u32(vshrq_n_u32::<11>(v), vdupq_n_u32(31));
            let g = vandq_u32(vshrq_n_u32::<5>(v), vdupq_n_u32(63));
            let b = vandq_u32(v, vdupq_n_u32(31));
            let rb_scale = vdupq_n_u32(527);
            let g_scale = vdupq_n_u32(259);
            let rb_bias = vdupq_n_u32(23);
            let g_bias = vdupq_n_u32(33);
            let r = vshrq_n_u32::<6>(vaddq_u32(vmulq_u32(r, rb_scale), rb_bias));
            let g = vshrq_n_u32::<6>(vaddq_u32(vmulq_u32(g, g_scale), g_bias));
            let b = vshrq_n_u32::<6>(vaddq_u32(vmulq_u32(b, rb_scale), rb_bias));
            packed(r, g, b, byte)
        }
        CursorXorFormat::Bgr24 => {
            let indices = [2u8, 1, 0, 16, 5, 4, 3, 16, 8, 7, 6, 16, 11, 10, 9, 16];
            let rgba = vqtbl1q_u8(vld1q_u8(source), vld1q_u8(indices.as_ptr()));
            vorrq_u32(vreinterpretq_u32_u8(rgba), vdupq_n_u32(0xff000000))
        }
        CursorXorFormat::Bgra32 => {
            let v = vreinterpretq_u32_u8(vld1q_u8(source));
            packed(
                vandq_u32(vshrq_n_u32::<16>(v), byte),
                vandq_u32(vshrq_n_u32::<8>(v), byte),
                vandq_u32(v, byte),
                vshrq_n_u32::<24>(v),
            )
        }
    };
    let mask = bits(and_byte, low);
    let transparent = vandq_u32(mask, vceqq_u32(color, vdupq_n_u32(0xff000000)));
    let invert = vandq_u32(mask, vceqq_u32(color, vdupq_n_u32(u32::MAX)));
    let (ordinary, inverted) = match mode {
        CursorBitmapMode::Software => {
            let r = vandq_u32(color, byte);
            let g = vandq_u32(vshrq_n_u32::<8>(color), byte);
            let b = vandq_u32(vshrq_n_u32::<16>(color), byte);
            (
                packed(square(r), square(g), square(b), vshrq_n_u32::<24>(color)),
                vdupq_n_u32(0x00ffffff),
            )
        }
        CursorBitmapMode::Accelerated { odd_row } => {
            let pattern = [0x00ffffffu32, 0, 0x00ffffff, 0];
            let mut check = vld1q_u32(pattern.as_ptr());
            if odd_row {
                check = veorq_u32(check, vdupq_n_u32(0x00ffffff));
            }
            (color, vorrq_u32(check, vdupq_n_u32(0xff000000)))
        }
    };
    vbslq_u32(transparent, zero, vbslq_u32(invert, inverted, ordinary))
}
#[target_feature(enable = "neon")]
unsafe fn blend_channel(s: V, d: V, inverse: V) -> V {
    // 合法 byte 产品<=65025、和<=509；显式 modulo256，保留原版 release。
    vandq_u32(
        vaddq_u32(s, vshrq_n_u32::<8>(vmulq_u32(d, inverse))),
        vdupq_n_u32(255),
    )
}
/// # Safety
/// 各指针可读16byte，ISA 已检查；尾部只传本地零填充 staging。
#[target_feature(enable = "neon")]
pub(super) unsafe fn composite4(source: *const u8, background: *const u8) -> V {
    let s = vreinterpretq_u32_u8(vld1q_u8(source));
    let d = vreinterpretq_u32_u8(vld1q_u8(background));
    let byte = vdupq_n_u32(255);
    let a = vshrq_n_u32::<24>(s);
    let inverse = vsubq_u32(byte, a);
    let r = blend_channel(vandq_u32(s, byte), vandq_u32(d, byte), inverse);
    let g = blend_channel(
        vandq_u32(vshrq_n_u32::<8>(s), byte),
        vandq_u32(vshrq_n_u32::<8>(d), byte),
        inverse,
    );
    let b = blend_channel(
        vandq_u32(vshrq_n_u32::<16>(s), byte),
        vandq_u32(vshrq_n_u32::<16>(d), byte),
        inverse,
    );
    let ordinary = packed(r, g, b, vshrq_n_u32::<24>(d));
    let alpha_zero = vceqq_u32(a, vdupq_n_u32(0));
    let invert = vceqq_u32(s, vdupq_n_u32(0x00ffffff));
    let inverted = vorrq_u32(
        veorq_u32(d, vdupq_n_u32(0x00ffffff)),
        vdupq_n_u32(0xff000000),
    );
    vbslq_u32(invert, inverted, vbslq_u32(alpha_zero, d, ordinary))
}
#[target_feature(enable = "neon")]
pub(super) unsafe fn store(v: V, dst: *mut u8) {
    vst1q_u8(dst, vreinterpretq_u8_u32(v))
}
