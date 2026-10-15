//! 独立进程：完整固定原版 pointer.rs 原样 include；只链接原版 core/PDU，无 K/vendor。
#![allow(dead_code)]
mod color_conversion {
    include!("rgb565_original.rs");
}
#[path = "source/pointer.rs"]
mod pointer;
use ironrdp_pdu::pointer::{LargePointerAttribute, Point16};
use pointer::{DecodedPointer, PointerBitmapTarget};
use std::io::Write;
fn emit(out: &mut std::fs::File, bpp: u16, n: usize, raw: &[u8], mask: Option<&[u8]>) {
    let row_len = if bpp == 1 {
        n.div_ceil(8)
    } else {
        n * usize::from(bpp) / 8
    };
    let stride = row_len.div_ceil(2) * 2;
    let and_stride = n.div_ceil(16) * 2;
    // 两行颜色和 AND 相同；输出 checker 的两种 row parity 来自完整原版。
    let mut x = vec![0x91; stride * 2];
    let mut a = vec![0xa7; and_stride * 2];
    for row in 0..2 {
        x[row * stride..row * stride + row_len].copy_from_slice(raw);
        if let Some(mask) = mask {
            a[row * and_stride..row * and_stride + mask.len()].copy_from_slice(mask);
        }
    }
    let p = LargePointerAttribute {
        xor_bpp: bpp,
        width: n as u16,
        height: 2,
        cache_index: 7,
        hot_spot: Point16 { x: 0, y: 0 },
        xor_mask: &x,
        and_mask: if mask.is_some() { &a } else { &[] },
    };
    for target in [
        PointerBitmapTarget::Software,
        PointerBitmapTarget::Accelerated,
    ] {
        let decoded = DecodedPointer::decode_large_pointer_attribute(&p, target).unwrap();
        for row in 0..2 {
            let mode = match target {
                PointerBitmapTarget::Software => 0,
                PointerBitmapTarget::Accelerated => 1 + row as u8,
            };
            let format = match bpp {
                1 => 0,
                16 => 1,
                24 => 2,
                32 => 3,
                _ => unreachable!(),
            };
            out.write_all(&[format, mode, u8::from(mask.is_some()), 0])
                .unwrap();
            out.write_all(&(n as u32).to_le_bytes()).unwrap();
            out.write_all(raw).unwrap();
            if let Some(mask) = mask {
                out.write_all(mask).unwrap();
            }
            out.write_all(&decoded.bitmap_data[row * n * 4..(row + 1) * n * 4])
                .unwrap();
        }
    }
}
fn main() {
    let mut out = std::fs::File::create(std::env::args().nth(1).unwrap()).unwrap();
    out.write_all(b"FRDCUR01").unwrap();
    for bpp in [1, 16, 24, 32] {
        for n in [1usize, 2, 3, 4, 7, 8, 9, 15, 16, 17, 31, 32, 33] {
            let len = if bpp == 1 {
                n.div_ceil(8)
            } else {
                n * bpp as usize / 8
            };
            let mut raw: Vec<u8> = (0..len).map(|i| (i * 71 + i / 3 * 11 + 17) as u8).collect();
            if bpp == 32 {
                for (i, p) in raw.chunks_exact_mut(4).enumerate() {
                    p[3] = [0, 1, 2, 127, 128, 254, 255][i % 7];
                    if i % 3 == 0 {
                        p[..3].fill(255);
                    } else if i % 3 == 1 {
                        p[..3].fill(0);
                    }
                }
            }
            let mask: Vec<u8> = (0..n.div_ceil(8)).map(|i| (i * 93 + 177) as u8).collect();
            emit(&mut out, bpp, n, &raw, None);
            emit(&mut out, bpp, n, &raw, Some(&mask));
        }
    }
    for start in [0u32, 32768] {
        let raw: Vec<u8> = (start..start + 32768)
            .flat_map(|v| (v as u16).to_le_bytes())
            .collect();
        emit(&mut out, 16, 32768, &raw, None);
        emit(&mut out, 16, 32768, &raw, Some(&vec![255; 4096]));
    }
}
