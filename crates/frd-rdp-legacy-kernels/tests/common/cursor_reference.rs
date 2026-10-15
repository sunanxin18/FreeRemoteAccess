//! 只供测试：原版源码保存在 cursor_original/source；未导入生产数值实现。
//! 565 与 composite 函数逐字节摘自固定 registry 原版；decode 只把原版逐像素
//! 分支适配为无 stride 的单行。wrapping_reference 是 root 冻结的 release 合同。
#![allow(dead_code)]
use frd_rdp_legacy_kernels::{CursorBitmapMode, CursorXorFormat};
include!("../cursor_original/rgb565_original.rs");
include!("../cursor_original/copy_original.rs");

pub const WIDTHS: [usize; 13] = [1, 2, 3, 4, 7, 8, 9, 15, 16, 17, 31, 32, 33];
pub const FORMATS: [CursorXorFormat; 4] = [
    CursorXorFormat::Mono1,
    CursorXorFormat::Rgb565,
    CursorXorFormat::Bgr24,
    CursorXorFormat::Bgra32,
];
pub const MODES: [CursorBitmapMode; 3] = [
    CursorBitmapMode::Software,
    CursorBitmapMode::Accelerated { odd_row: false },
    CursorBitmapMode::Accelerated { odd_row: true },
];
pub fn encoded_len(format: CursorXorFormat, n: usize) -> usize {
    match format {
        CursorXorFormat::Mono1 => n.div_ceil(8),
        CursorXorFormat::Rgb565 => 2 * n,
        CursorXorFormat::Bgr24 => 3 * n,
        CursorXorFormat::Bgra32 => 4 * n,
    }
}
pub fn decode(
    format: CursorXorFormat,
    mode: CursorBitmapMode,
    xor: &[u8],
    and: Option<&[u8]>,
    n: usize,
) -> Vec<u8> {
    let mut out = vec![0; n * 4];
    decode_into(format, mode, xor, and, &mut out);
    out
}
pub fn decode_into(
    format: CursorXorFormat,
    mode: CursorBitmapMode,
    xor: &[u8],
    and: Option<&[u8]>,
    out: &mut [u8],
) {
    let n = out.len() / 4;
    for col in 0..n {
        let color = match format {
            CursorXorFormat::Mono1 => {
                if xor[col / 8] & (0x80 >> (col % 8)) != 0 {
                    [255, 255, 255, 255]
                } else {
                    [0, 0, 0, 255]
                }
            }
            CursorXorFormat::Rgb565 => {
                let [r, g, b] =
                    rdp_16bit_to_rgb(u16::from_le_bytes([xor[col * 2], xor[col * 2 + 1]]));
                [r, g, b, 255]
            }
            CursorXorFormat::Bgr24 => [xor[col * 3 + 2], xor[col * 3 + 1], xor[col * 3], 255],
            CursorXorFormat::Bgra32 => [
                xor[col * 4 + 2],
                xor[col * 4 + 1],
                xor[col * 4],
                xor[col * 4 + 3],
            ],
        };
        let and_bit = and.map_or(0, |a| (a[col / 8] >> (7 - col % 8)) & 1);
        let result = if and_bit == 1 && color == [0, 0, 0, 255] {
            [0, 0, 0, 0]
        } else if and_bit == 1 && color == [255, 255, 255, 255] {
            match mode {
                CursorBitmapMode::Software => [255, 255, 255, 0],
                CursorBitmapMode::Accelerated { odd_row } => {
                    if (col + usize::from(odd_row)) % 2 == 0 {
                        [255, 255, 255, 255]
                    } else {
                        [0, 0, 0, 255]
                    }
                }
            }
        } else if mode == CursorBitmapMode::Software {
            [
                ((u16::from(color[0]) * u16::from(color[0])) >> 8) as u8,
                ((u16::from(color[1]) * u16::from(color[1])) >> 8) as u8,
                ((u16::from(color[2]) * u16::from(color[2])) >> 8) as u8,
                color[3],
            ]
        } else {
            color
        };
        out[col * 4..col * 4 + 4].copy_from_slice(&result);
    }
}
pub fn wrapping_reference(src: &[u8], background: &[u8]) -> Vec<u8> {
    let mut out = background.to_vec();
    for (s, d) in src.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
        if s == [255, 255, 255, 0] {
            d[0] = 255 - d[0];
            d[1] = 255 - d[1];
            d[2] = 255 - d[2];
            d[3] = 255;
        } else if s[3] != 0 {
            for c in 0..3 {
                d[c] = s[c].wrapping_add(((u16::from(d[c]) * u16::from(255 - s[3])) >> 8) as u8);
            }
        }
    }
    out
}
/// 原版完整私有函数的精确单行入口；release overflow-checks=false 才覆盖 wrap 域。
pub fn copied_original_composite(src: &[u8], background: &[u8]) -> Vec<u8> {
    let mut out = background.to_vec();
    copied_original_composite_into(src, background, &mut out);
    out
}
pub fn copied_original_composite_into(src: &[u8], background: &[u8], out: &mut [u8]) {
    let n = src.len() / 4;
    out.copy_from_slice(background);
    copy_cursor_data(src, (0, 0), n * 4, out, n * 4, (0, 0), (n, 1), (n, 1), true);
}
pub fn bytes(n: usize, seed: u32) -> Vec<u8> {
    let mut state = seed;
    (0..n)
        .map(|_| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 24) as u8
        })
        .collect()
}
