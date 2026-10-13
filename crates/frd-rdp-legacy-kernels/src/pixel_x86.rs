//! 非 macOS x86/i686 与 AMD64 SSE2 数值核；不要求 SSSE3。
use crate::{PackedDepth, Pixel32Format, PixelRowSource};
use core::arch::asm;
#[cfg(target_arch = "x86")]
use core::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;
pub(super) type V = __m128i;
#[target_feature(enable = "sse2")]
unsafe fn byte(v: V, offset: u8) -> V {
    _mm_and_si128(
        match offset {
            0 => v,
            1 => _mm_srli_epi32::<8>(v),
            2 => _mm_srli_epi32::<16>(v),
            3 => _mm_srli_epi32::<24>(v),
            _ => unreachable!(),
        },
        _mm_set1_epi32(255),
    )
}
#[target_feature(enable = "sse2")]
unsafe fn position(v: V, offset: u8) -> V {
    match offset {
        0 => v,
        1 => _mm_slli_epi32::<8>(v),
        2 => _mm_slli_epi32::<16>(v),
        3 => _mm_slli_epi32::<24>(v),
        _ => unreachable!(),
    }
}
#[target_feature(enable = "sse2")]
unsafe fn pack(r: V, g: V, b: V, a: V, format: Pixel32Format) -> V {
    let [ri, gi, bi, ai] = format.offsets();
    _mm_or_si128(
        _mm_or_si128(position(r, ri), position(g, gi)),
        _mm_or_si128(position(b, bi), position(a, ai)),
    )
}
#[target_feature(enable = "sse2")]
unsafe fn expand5(v: V) -> V {
    _mm_unpacklo_epi16(
        _mm_srli_epi16::<6>(_mm_add_epi16(
            _mm_mullo_epi16(v, _mm_set1_epi16(527)),
            _mm_set1_epi16(23),
        )),
        _mm_setzero_si128(),
    )
}
#[target_feature(enable = "sse2")]
unsafe fn gather(source: *const u8, palette: *const u8) -> V {
    let result: V;
    // 两种 pointer width 均由 reg 的 usize 指针决定；movzx 的32bit输出在AMD64零扩展。
    // 四个 GPR 和两个 XMM 由编译器管理，含 Win64 非易失寄存器和 i686/PIC 保存。
    asm!(
        "movzx {index:e}, byte ptr [{source} + 0]",
        "lea {index}, [{index} + {index}*2]",
        "movzx {color:e}, word ptr [{palette} + {index}]",
        "movzx {index:e}, byte ptr [{palette} + {index} + 2]",
        "shl {index:e}, 16",
        "or {color:e}, {index:e}",
        "or {color:e}, 0xff000000",
        "movd {result}, {color:e}",
        "movzx {index:e}, byte ptr [{source} + 1]",
        "lea {index}, [{index} + {index}*2]",
        "movzx {color:e}, word ptr [{palette} + {index}]",
        "movzx {index:e}, byte ptr [{palette} + {index} + 2]",
        "shl {index:e}, 16",
        "or {color:e}, {index:e}",
        "or {color:e}, 0xff000000",
        "movd {temporary}, {color:e}",
        "pslldq {temporary}, 4",
        "por {result}, {temporary}",
        "movzx {index:e}, byte ptr [{source} + 2]",
        "lea {index}, [{index} + {index}*2]",
        "movzx {color:e}, word ptr [{palette} + {index}]",
        "movzx {index:e}, byte ptr [{palette} + {index} + 2]",
        "shl {index:e}, 16",
        "or {color:e}, {index:e}",
        "or {color:e}, 0xff000000",
        "movd {temporary}, {color:e}",
        "pslldq {temporary}, 8",
        "por {result}, {temporary}",
        "movzx {index:e}, byte ptr [{source} + 3]",
        "lea {index}, [{index} + {index}*2]",
        "movzx {color:e}, word ptr [{palette} + {index}]",
        "movzx {index:e}, byte ptr [{palette} + {index} + 2]",
        "shl {index:e}, 16",
        "or {color:e}, {index:e}",
        "or {color:e}, 0xff000000",
        "movd {temporary}, {color:e}",
        "pslldq {temporary}, 12",
        "por {result}, {temporary}",
        source=in(reg) source,palette=in(reg) palette,
        index=out(reg) _,color=out(reg) _,temporary=out(xmm_reg) _,result=out(xmm_reg) result,
        options(nostack,readonly),
    );
    result
}
/// opaque RGBA 固定排列，仅 SSE2 lane mask/shift，不要求 pshufb。
#[target_feature(enable = "sse2")]
unsafe fn opaque_format(value: V, dst: Pixel32Format) -> V {
    match dst {
        Pixel32Format::RgbA | Pixel32Format::RgbX => value,
        Pixel32Format::ARgb | Pixel32Format::XRgb => {
            _mm_or_si128(_mm_slli_epi32::<8>(value), _mm_srli_epi32::<24>(value))
        }
        Pixel32Format::ABgr | Pixel32Format::XBgr => {
            let rg = _mm_or_si128(
                _mm_slli_epi32::<8>(_mm_and_si128(value, _mm_set1_epi32(0x0000ff00))),
                _mm_srli_epi32::<8>(_mm_and_si128(value, _mm_set1_epi32(0x00ff0000))),
            );
            _mm_or_si128(
                rg,
                _mm_or_si128(_mm_slli_epi32::<24>(value), _mm_srli_epi32::<24>(value)),
            )
        }
        Pixel32Format::BgrA | Pixel32Format::BgrX => _mm_or_si128(
            _mm_and_si128(value, _mm_set1_epi32(0xff00ff00u32 as i32)),
            _mm_or_si128(
                _mm_slli_epi32::<16>(_mm_and_si128(value, _mm_set1_epi32(255))),
                _mm_and_si128(_mm_srli_epi32::<16>(value), _mm_set1_epi32(255)),
            ),
        ),
    }
}
#[target_feature(enable = "sse2")]
unsafe fn packed24(source: *const u8) -> (V, V, V) {
    let bytes = _mm_or_si128(
        _mm_loadl_epi64(source.cast()),
        _mm_slli_si128::<8>(_mm_loadu_si32(source.add(8).cast())),
    );
    // 每3 byte的first channel形成四个dword；SSE2 byte shifts 不跨越输入12byte边界。
    let m = _mm_set_epi32(0, 0, 0, 255);
    let c0 = _mm_or_si128(
        _mm_or_si128(
            _mm_and_si128(bytes, m),
            _mm_slli_si128::<4>(_mm_and_si128(_mm_srli_si128::<3>(bytes), m)),
        ),
        _mm_or_si128(
            _mm_slli_si128::<8>(_mm_and_si128(_mm_srli_si128::<6>(bytes), m)),
            _mm_slli_si128::<12>(_mm_and_si128(_mm_srli_si128::<9>(bytes), m)),
        ),
    );
    let next = _mm_srli_si128::<1>(bytes);
    let c1 = _mm_or_si128(
        _mm_or_si128(
            _mm_and_si128(next, m),
            _mm_slli_si128::<4>(_mm_and_si128(_mm_srli_si128::<3>(next), m)),
        ),
        _mm_or_si128(
            _mm_slli_si128::<8>(_mm_and_si128(_mm_srli_si128::<6>(next), m)),
            _mm_slli_si128::<12>(_mm_and_si128(_mm_srli_si128::<9>(next), m)),
        ),
    );
    let next = _mm_srli_si128::<2>(bytes);
    let c2 = _mm_or_si128(
        _mm_or_si128(
            _mm_and_si128(next, m),
            _mm_slli_si128::<4>(_mm_and_si128(_mm_srli_si128::<3>(next), m)),
        ),
        _mm_or_si128(
            _mm_slli_si128::<8>(_mm_and_si128(_mm_srli_si128::<6>(next), m)),
            _mm_slli_si128::<12>(_mm_and_si128(_mm_srli_si128::<9>(next), m)),
        ),
    );
    (c0, c1, c2)
}
#[target_feature(enable = "sse2")]
pub(super) unsafe fn block(
    format: PixelRowSource,
    dst: Pixel32Format,
    source: *const u8,
    palette: Option<&[[u8; 3]; 256]>,
) -> V {
    let opaque = _mm_set1_epi32(255);
    let (r, g, b, a) = match format {
        PixelRowSource::Packed(PackedDepth::Rgb555 | PackedDepth::Rgb565) => {
            let word = _mm_loadl_epi64(source.cast());
            let m5 = _mm_set1_epi16(31);
            let (r, g) = if format == PixelRowSource::Packed(PackedDepth::Rgb555) {
                (
                    expand5(_mm_and_si128(_mm_srli_epi16::<10>(word), m5)),
                    expand5(_mm_and_si128(_mm_srli_epi16::<5>(word), m5)),
                )
            } else {
                (
                    expand5(_mm_and_si128(_mm_srli_epi16::<11>(word), m5)),
                    _mm_unpacklo_epi16(
                        _mm_srli_epi16::<6>(_mm_add_epi16(
                            _mm_mullo_epi16(
                                _mm_and_si128(_mm_srli_epi16::<5>(word), _mm_set1_epi16(63)),
                                _mm_set1_epi16(259),
                            ),
                            _mm_set1_epi16(33),
                        )),
                        _mm_setzero_si128(),
                    ),
                )
            };
            (r, g, expand5(_mm_and_si128(word, m5)), opaque)
        }
        PixelRowSource::Packed(PackedDepth::Index8) => {
            let v = gather(source, palette.unwrap().as_ptr().cast());
            return opaque_format(v, dst);
        }
        PixelRowSource::Packed(PackedDepth::Bgr24) | PixelRowSource::Rgb24 => {
            let (r, g, b) = packed24(source);
            if format == PixelRowSource::Rgb24 {
                (r, g, b, opaque)
            } else {
                (b, g, r, opaque)
            }
        }
        PixelRowSource::Raw32(src) | PixelRowSource::Region32(src) => {
            let v = _mm_loadu_si128(source.cast());
            let copy = match format {
                PixelRowSource::Raw32(_) => src == dst,
                PixelRowSource::Region32(_) => src.offsets() == dst.offsets(),
                _ => unreachable!(),
            };
            if copy {
                return v;
            }
            let [ri, gi, bi, ai] = src.offsets();
            let preserve =
                src.has_alpha() && (matches!(format, PixelRowSource::Raw32(_)) || dst.has_alpha());
            (
                byte(v, ri),
                byte(v, gi),
                byte(v, bi),
                if preserve { byte(v, ai) } else { opaque },
            )
        }
    };
    pack(r, g, b, a, dst)
}
#[target_feature(enable = "sse2")]
pub(super) unsafe fn store(v: V, output: *mut u8) {
    _mm_storeu_si128(output.cast(), v);
}
