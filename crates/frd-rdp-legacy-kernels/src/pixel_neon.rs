//! AArch64 NEON 数值核；调色板索引读取和组装完全在目标汇编中。
use crate::{PackedDepth, Pixel32Format, PixelRowSource};
use core::arch::{aarch64::*, asm};
pub(super) type V = uint32x4_t;
#[target_feature(enable = "neon")]
unsafe fn byte(v: V, offset: u8) -> V {
    vandq_u32(
        match offset {
            0 => v,
            1 => vshrq_n_u32::<8>(v),
            2 => vshrq_n_u32::<16>(v),
            3 => vshrq_n_u32::<24>(v),
            _ => unreachable!(),
        },
        vdupq_n_u32(255),
    )
}
#[target_feature(enable = "neon")]
unsafe fn position(v: V, offset: u8) -> V {
    match offset {
        0 => v,
        1 => vshlq_n_u32::<8>(v),
        2 => vshlq_n_u32::<16>(v),
        3 => vshlq_n_u32::<24>(v),
        _ => unreachable!(),
    }
}
#[target_feature(enable = "neon")]
unsafe fn pack(r: V, g: V, b: V, a: V, format: Pixel32Format) -> V {
    let [ri, gi, bi, ai] = format.offsets();
    vorrq_u32(
        vorrq_u32(position(r, ri), position(g, gi)),
        vorrq_u32(position(b, bi), position(a, ai)),
    )
}
#[target_feature(enable = "neon")]
unsafe fn expand5(v: uint16x4_t) -> V {
    vmovl_u16(vshr_n_u16::<6>(vadd_u16(
        vmul_u16(v, vdup_n_u16(527)),
        vdup_n_u16(23),
    )))
}
#[target_feature(enable = "neon")]
unsafe fn gather(source: *const u8, palette: *const u8) -> V {
    let result: V;
    // 每项恰读取 RG 两 byte 与 B 一 byte；index255 不越过 palette 末尾。
    // out scratch 不能与尚在后续展开中使用的 src/palette 输入共用；编译器管理 ABI 保存。
    asm!(
        "ldrb {index:w}, [{source}, #0]",
        "add {index}, {index}, {index}, lsl #1",
        "add {index}, {palette}, {index}",
        "ldrh {color:w}, [{index}]",
        "ldrb {index:w}, [{index}, #2]",
        "orr {color:w}, {color:w}, {index:w}, lsl #16",
        "orr {color:w}, {color:w}, #0xff000000",
        "ins {result:v}.s[0], {color:w}",
        "ldrb {index:w}, [{source}, #1]",
        "add {index}, {index}, {index}, lsl #1",
        "add {index}, {palette}, {index}",
        "ldrh {color:w}, [{index}]",
        "ldrb {index:w}, [{index}, #2]",
        "orr {color:w}, {color:w}, {index:w}, lsl #16",
        "orr {color:w}, {color:w}, #0xff000000",
        "ins {result:v}.s[1], {color:w}",
        "ldrb {index:w}, [{source}, #2]",
        "add {index}, {index}, {index}, lsl #1",
        "add {index}, {palette}, {index}",
        "ldrh {color:w}, [{index}]",
        "ldrb {index:w}, [{index}, #2]",
        "orr {color:w}, {color:w}, {index:w}, lsl #16",
        "orr {color:w}, {color:w}, #0xff000000",
        "ins {result:v}.s[2], {color:w}",
        "ldrb {index:w}, [{source}, #3]",
        "add {index}, {index}, {index}, lsl #1",
        "add {index}, {palette}, {index}",
        "ldrh {color:w}, [{index}]",
        "ldrb {index:w}, [{index}, #2]",
        "orr {color:w}, {color:w}, {index:w}, lsl #16",
        "orr {color:w}, {color:w}, #0xff000000",
        "ins {result:v}.s[3], {color:w}",
        source=in(reg) source, palette=in(reg) palette,
        index=out(reg) _, color=out(reg) _, result=out(vreg) result,
        options(nostack, readonly, preserves_flags),
    );
    result
}
/// 汇编已输出 opaque RGBA；只做固定 byte permutation，避免重复拆通道。
#[target_feature(enable = "neon")]
unsafe fn opaque_format(value: V, dst: Pixel32Format) -> V {
    let indices = match dst {
        Pixel32Format::RgbA | Pixel32Format::RgbX => return value,
        Pixel32Format::ARgb | Pixel32Format::XRgb => {
            [3, 0, 1, 2, 7, 4, 5, 6, 11, 8, 9, 10, 15, 12, 13, 14]
        }
        Pixel32Format::ABgr | Pixel32Format::XBgr => {
            [3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12]
        }
        Pixel32Format::BgrA | Pixel32Format::BgrX => {
            [2, 1, 0, 3, 6, 5, 4, 7, 10, 9, 8, 11, 14, 13, 12, 15]
        }
    };
    vreinterpretq_u32_u8(vqtbl1q_u8(
        vreinterpretq_u8_u32(value),
        vld1q_u8(indices.as_ptr()),
    ))
}
#[target_feature(enable = "neon")]
pub(super) unsafe fn block(
    format: PixelRowSource,
    dst: Pixel32Format,
    source: *const u8,
    palette: Option<&[[u8; 3]; 256]>,
) -> V {
    let opaque = vdupq_n_u32(255);
    let (r, g, b, a) = match format {
        PixelRowSource::Packed(PackedDepth::Rgb555 | PackedDepth::Rgb565) => {
            let word = vld1_u16(source.cast());
            let m5 = vdup_n_u16(31);
            let (r, g) = if format == PixelRowSource::Packed(PackedDepth::Rgb555) {
                (
                    expand5(vand_u16(vshr_n_u16::<10>(word), m5)),
                    expand5(vand_u16(vshr_n_u16::<5>(word), m5)),
                )
            } else {
                (
                    expand5(vand_u16(vshr_n_u16::<11>(word), m5)),
                    vmovl_u16(vshr_n_u16::<6>(vadd_u16(
                        vmul_u16(
                            vand_u16(vshr_n_u16::<5>(word), vdup_n_u16(63)),
                            vdup_n_u16(259),
                        ),
                        vdup_n_u16(33),
                    ))),
                )
            };
            (r, g, expand5(vand_u16(word, m5)), opaque)
        }
        PixelRowSource::Packed(PackedDepth::Index8) => {
            let v = gather(source, palette.unwrap().as_ptr().cast());
            return opaque_format(v, dst);
        }
        PixelRowSource::Packed(PackedDepth::Bgr24) | PixelRowSource::Rgb24 => {
            // 只读 12 个有效 byte：8byte load 加 4byte lane load，无 row padding 假设。
            let low = vld1_u8(source);
            let high = vreinterpret_u8_u32(vld1_lane_u32::<0>(source.add(8).cast(), vdup_n_u32(0)));
            let bytes = vcombine_u8(low, high);
            let r = vreinterpretq_u32_u8(vqtbl1q_u8(
                bytes,
                vld1q_u8(
                    [
                        0, 255, 255, 255, 3, 255, 255, 255, 6, 255, 255, 255, 9, 255, 255, 255,
                    ]
                    .as_ptr(),
                ),
            ));
            let g = vreinterpretq_u32_u8(vqtbl1q_u8(
                bytes,
                vld1q_u8(
                    [
                        1, 255, 255, 255, 4, 255, 255, 255, 7, 255, 255, 255, 10, 255, 255, 255,
                    ]
                    .as_ptr(),
                ),
            ));
            let b = vreinterpretq_u32_u8(vqtbl1q_u8(
                bytes,
                vld1q_u8(
                    [
                        2, 255, 255, 255, 5, 255, 255, 255, 8, 255, 255, 255, 11, 255, 255, 255,
                    ]
                    .as_ptr(),
                ),
            ));
            if format == PixelRowSource::Rgb24 {
                (r, g, b, opaque)
            } else {
                (b, g, r, opaque)
            }
        }
        PixelRowSource::Raw32(src) | PixelRowSource::Region32(src) => {
            let v = vreinterpretq_u32_u8(vld1q_u8(source));
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
#[target_feature(enable = "neon")]
pub(super) unsafe fn store(v: V, output: *mut u8) {
    vst1q_u8(output, vreinterpretq_u8_u32(v));
}
