// 独立 stock image API producer；裁剪只复制已由原版完整图解码的字节，不重写颜色公式。
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_graphics::pointer::DecodedPointer;
use ironrdp_graphics::rectangle_processing::Region;
use ironrdp_pdu::geometry::InclusiveRectangle;
use std::sync::Arc;
const FORMATS: [PixelFormat; 8] = [
    PixelFormat::ARgb32,
    PixelFormat::XRgb32,
    PixelFormat::ABgr32,
    PixelFormat::XBgr32,
    PixelFormat::BgrA32,
    PixelFormat::BgrX32,
    PixelFormat::RgbA32,
    PixelFormat::RgbX32,
];
fn field(out: &mut Vec<u8>, data: &[u8]) {
    out.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
    out.extend(data);
}
fn record(out: &mut Vec<u8>, name: &str, meta: &[u8], wire: &[u8], initial: &[u8], expected: &[u8], result: &[u8]) {
    for v in [name.as_bytes(), meta, wire, initial, expected, result] {
        field(out, v);
    }
}
fn rect(w: u16, h: u16) -> InclusiveRectangle {
    InclusiveRectangle {
        left: 0,
        top: 0,
        right: w - 1,
        bottom: h - 1,
    }
}
fn palette() -> [[u8; 3]; 256] {
    std::array::from_fn(|i| [i as u8, (i as u8).wrapping_mul(37), (i as u8).wrapping_mul(73)])
}
fn apply(
    image: &mut image::DecodedImage,
    kind: u8,
    src: PixelFormat,
    data: &[u8],
    r: &InclusiveRectangle,
) -> InclusiveRectangle {
    match kind {
        0 => image.apply_rgb8_with_palette(data, r, &palette()),
        1 => image.apply_rgb15_bitmap(data, r),
        2 => image.apply_rgb16_bitmap(data, r),
        3 => image.apply_bgr24_bitmap(data, r),
        4 => image.apply_rgb24(data, r, true),
        5 => image.apply_rgb32_bitmap(data, src, r),
        6 => {
            let mut region = Region::new();
            region.union_rectangle(r.clone());
            image.apply_tile(data, src, &region, &rect(64, 64))
        }
        _ => unreachable!(),
    }
    .unwrap()
}
fn original_packet(data: &[u8], code: ironrdp_pdu::fast_path::UpdateCode) -> Vec<u8> {
    use ironrdp_core::{Encode as _, WriteCursor};
    use ironrdp_pdu::fast_path::{EncryptionFlags, FastPathHeader, FastPathUpdatePdu, Fragmentation};
    let update = FastPathUpdatePdu {
        fragmentation: Fragmentation::Single,
        update_code: code,
        compression_flags: None,
        compression_type: None,
        data,
    };
    let header = FastPathHeader::new(EncryptionFlags::empty(), update.size());
    let mut out = vec![0; header.size() + update.size()];
    let mut c = WriteCursor::new(&mut out);
    header.encode(&mut c).unwrap();
    update.encode(&mut c).unwrap();
    out
}
fn original_pointer_frame(alternate: bool) -> Vec<u8> {
    use ironrdp_core::{Encode as _, WriteCursor};
    use ironrdp_pdu::pointer::{ColorPointerAttribute, Point16, PointerAttribute};
    let pointer = PointerAttribute {
        xor_bpp: 32,
        color_pointer: ColorPointerAttribute {
            cache_index: 1,
            hot_spot: Point16 { x: 0, y: 0 },
            width: 1,
            height: 1,
            xor_mask: if alternate { &[13, 11, 7, 255] } else { &[3, 2, 1, 255] },
            and_mask: &[0, 0],
        },
    };
    let mut data = vec![0; pointer.size()];
    pointer.encode(&mut WriteCursor::new(&mut data)).unwrap();
    original_packet(&data, ironrdp_pdu::fast_path::UpdateCode::NewPointer)
}
fn original_color_frame() -> Vec<u8> {
    use ironrdp_core::{Encode as _, WriteCursor};
    use ironrdp_pdu::pointer::{ColorPointerAttribute, Point16};
    let pointer = ColorPointerAttribute {
        cache_index: 1,
        hot_spot: Point16 { x: 0, y: 0 },
        width: 1,
        height: 1,
        xor_mask: &[13, 11, 7, 0],
        and_mask: &[0, 0],
    };
    let mut data = vec![0; pointer.size()];
    pointer.encode(&mut WriteCursor::new(&mut data)).unwrap();
    original_packet(&data, ironrdp_pdu::fast_path::UpdateCode::ColorPointer)
}
fn original_large_frame() -> Vec<u8> {
    use ironrdp_core::{Encode as _, WriteCursor};
    use ironrdp_pdu::pointer::{LargePointerAttribute, Point16};
    let pointer = LargePointerAttribute {
        xor_bpp: 32,
        cache_index: 1,
        hot_spot: Point16 { x: 0, y: 0 },
        width: 1,
        height: 1,
        xor_mask: &[13, 11, 7, 255],
        and_mask: &[0, 0],
    };
    let mut data = vec![0; pointer.size()];
    pointer.encode(&mut WriteCursor::new(&mut data)).unwrap();
    original_packet(&data, ironrdp_pdu::fast_path::UpdateCode::LargePointer)
}
fn main() {
    let mut out = b"FRDIMG01".to_vec();
    out.extend(0u32.to_le_bytes());
    let mut count = 0u32;
    for kind in 0..=6u8 {
        for df in 0..8u8 {
            for sf in 0..(if kind >= 5 { 8 } else { 1 }) {
                for (sw, sh, dw, dh) in (1..=4u16)
                    .flat_map(|w| (1..=3u16).map(move |h| (w, h, w, h)))
                    .chain([(4, 3, 2, 2)])
                {
                    let bpp = match kind {
                        0 => 1,
                        1 | 2 => 2,
                        3 | 4 => 3,
                        _ => 4,
                    };
                    let (sw, sh, dw, dh) = if kind == 6 { (64, 64, dw, dh) } else { (sw, sh, dw, dh) };
                    let wire: Vec<u8> = (0..usize::from(sw) * usize::from(sh) * bpp)
                        .map(|i| ((i * 37 + i / 7 * 13) % 251) as u8)
                        .collect();
                    let dst = FORMATS[usize::from(df)];
                    let source = FORMATS[sf];
                    let mut full = image::DecodedImage::new(dst, sw, sh);
                    apply(&mut full, kind, source, &wire, &rect(sw, sh));
                    // 左上逻辑裁剪：原版完整源图后的copy control，不用目标宽rechunk。
                    let mut expected = image::DecodedImage::new(dst, dw + 3, dh + 3);
                    let initial = expected.data().to_vec();
                    let mut cropped = Vec::new();
                    for y in (0..usize::from(dh)).rev() {
                        let start = y * usize::from(sw) * 4;
                        cropped.extend(&full.data()[start..start + usize::from(dw) * 4]);
                    }
                    expected
                        .apply_rgb32_bitmap(
                            &cropped,
                            dst,
                            &InclusiveRectangle {
                                left: 1,
                                top: 1,
                                right: dw,
                                bottom: dh,
                            },
                        )
                        .unwrap();
                    let mut meta = vec![kind, df, sf as u8, 0];
                    for v in [sw, sh, dw, dh] {
                        meta.extend(v.to_le_bytes());
                    }
                    record(
                        &mut out,
                        &format!("image-{kind}-{df}-{sf}-{sw}-{sh}-{dw}-{dh}"),
                        &meta,
                        &wire,
                        &initial,
                        expected.data(),
                        b"Ok",
                    );
                    count += 1;
                }
            }
        }
    }
    // 六个 alpha 锚点，全部 64 Raw32 / Region32 source-target 组合来自真实 stock API。
    for kind in [5u8, 6] {
        for df in 0..8u8 {
            for sf in 0..8u8 {
                let source = FORMATS[usize::from(sf)];
                let [ri, gi, bi, ai] = source.channel_offsets();
                let mut wire = vec![0; if kind == 6 { 64 * 64 * 4 } else { 6 * 4 }];
                for (i, alpha) in [0u8, 1, 127, 128, 254, 255].iter().enumerate() {
                    let px = &mut wire[i * 4..i * 4 + 4];
                    px[ri] = 0x11;
                    px[gi] = 0x22;
                    px[bi] = 0x33;
                    px[ai] = *alpha;
                }
                let mut image = image::DecodedImage::new(FORMATS[usize::from(df)], 9, 4);
                let initial = image.data().to_vec();
                let r = InclusiveRectangle {
                    left: 1,
                    top: 1,
                    right: 6,
                    bottom: 1,
                };
                if kind == 6 {
                    let mut clips = Region::new();
                    clips.union_rectangle(r.clone());
                    image
                        .apply_tile(
                            &wire,
                            source,
                            &clips,
                            &InclusiveRectangle {
                                left: 1,
                                top: 1,
                                right: 64,
                                bottom: 64,
                            },
                        )
                        .unwrap();
                } else {
                    image.apply_rgb32_bitmap(&wire, source, &r).unwrap();
                }
                let mut meta = vec![kind, df, sf, 1];
                for dim in [if kind == 6 { 64u16 } else { 6 }, if kind == 6 { 64 } else { 1 }, 6, 1] {
                    meta.extend(dim.to_le_bytes());
                }
                record(
                    &mut out,
                    &format!("alpha-{kind}-{df}-{sf}"),
                    &meta,
                    &wire,
                    &initial,
                    image.data(),
                    b"Ok",
                );
                count += 1;
            }
        }
    }
    for df in 0..8u8 {
        for sf in 0..8u8 {
            let wire: Vec<u8> = (0..64 * 64 * 4).map(|i| ((i * 37 + i / 7 * 13) % 251) as u8).collect();
            let mut image = image::DecodedImage::new(FORMATS[usize::from(df)], 80, 75);
            let initial = image.data().to_vec();
            let mut clips = Region::new();
            for r in [
                InclusiveRectangle {
                    left: 3,
                    top: 2,
                    right: 6,
                    bottom: 4,
                },
                InclusiveRectangle {
                    left: 60,
                    top: 61,
                    right: 70,
                    bottom: 64,
                },
                InclusiveRectangle {
                    left: 6,
                    top: 3,
                    right: 9,
                    bottom: 5,
                },
            ] {
                clips.union_rectangle(r);
            }
            let update = InclusiveRectangle {
                left: 2,
                top: 1,
                right: 65,
                bottom: 64,
            };
            let result = image
                .apply_tile(&wire, FORMATS[usize::from(sf)], &clips, &update)
                .unwrap();
            record(
                &mut out,
                &format!("multi-region-{df}-{sf}"),
                &[9, df, sf, 0],
                &wire,
                &initial,
                image.data(),
                format!("{result:?}").as_bytes(),
            );
            count += 1;
        }
    }
    // 原版 cursor observed bytes，在全部八格式保持其旧硬编码 RGBA 合成行为。
    for df in 0..8u8 {
        for visible in [false, true] {
            let mut image = image::DecodedImage::new(FORMATS[usize::from(df)], 6, 5);
            let pointer = Arc::new(DecodedPointer {
                width: 2,
                height: 2,
                hotspot_x: 0,
                hotspot_y: 0,
                bitmap_data: vec![5, 7, 9, 128, 255, 255, 255, 0, 0, 0, 0, 0, 11, 13, 17, 255],
            });
            image.move_pointer(2, 1).unwrap();
            image.update_pointer(pointer).unwrap();
            if visible {
                image.show_pointer().unwrap();
            } else {
                image.hide_pointer().unwrap();
            }
            let initial = image.data().to_vec();
            let wire: Vec<u8> = (0..18).map(|i| (i * 11 + 3) as u8).collect();
            let r = InclusiveRectangle {
                left: 1,
                top: 1,
                right: 3,
                bottom: 2,
            };
            let result = apply(&mut image, 3, PixelFormat::BgrX32, &wire, &r);
            let rendered = image.data().to_vec();
            image.move_pointer(3, 2).unwrap();
            image.hide_pointer().unwrap();
            let mut meta = vec![7, df, 0, u8::from(visible)];
            for v in [3u16, 2, 3, 2] {
                meta.extend(v.to_le_bytes());
            }
            let mut after = rendered;
            after.extend(image.data());
            record(
                &mut out,
                &format!("cursor-{df}-{visible}"),
                &meta,
                &wire,
                &initial,
                &after,
                format!("{result:?}").as_bytes(),
            );
            count += 1;
        }
    }
    for df in 0..8u8 {
        for software in [false, true] {
            for action in 0..6u8 {
                use ironrdp_core::WriteBuf;
                use ironrdp_pdu::fast_path::UpdateCode;
                let mut p = ironrdp_session::fast_path::ProcessorBuilder {
                    io_channel_id: 1003,
                    user_channel_id: 1001,
                    share_id: 1,
                    enable_server_pointer: true,
                    pointer_software_rendering: software,
                    bulk_decompressor: None,
                }
                .build();
                let mut image = ironrdp_session::image::DecodedImage::new(FORMATS[usize::from(df)], 8, 6);
                for frame in [
                    original_pointer_frame(false),
                    original_packet(&[1, 0], UpdateCode::CachedPointer),
                    original_packet(&[2, 0, 1, 0], UpdateCode::PositionPointer),
                ] {
                    p.process(&mut image, &frame, &mut WriteBuf::new()).unwrap();
                }
                let initial = image.data().to_vec();
                p.update_mouse_pos(3, 2);
                let frame = match action {
                    0 => original_pointer_frame(true),
                    1 => original_packet(&[1, 0], UpdateCode::CachedPointer),
                    2 => original_packet(&[], UpdateCode::HiddenPointer),
                    3 => original_packet(&[], UpdateCode::DefaultPointer),
                    4 => original_color_frame(),
                    _ => original_large_frame(),
                };
                let updates = p.process(&mut image, &frame, &mut WriteBuf::new()).unwrap();
                record(
                    &mut out,
                    &format!("pointer-order-{df}-{software}-{action}"),
                    &[30, df, action, u8::from(software)],
                    &frame,
                    &initial,
                    image.data(),
                    format!("updates={updates:?}\nimage={image:?}").as_bytes(),
                );
                count += 1;
            }
        }
    }
    std::panic::set_hook(Box::new(|_| {}));
    for kind in 0..=5u8 {
        if kind == 4 {
            continue;
        }
        for df in 0..8u8 {
            let row_bytes = 2 * match kind {
                0 => 1,
                1 | 2 => 2,
                3 => 3,
                _ => 4,
            };
            for size in [row_bytes - 1, row_bytes, row_bytes * 3] {
                let wire: Vec<u8> = (0..size).map(|i| (i * 11 + 3) as u8).collect();
                let mut image = image::DecodedImage::new(FORMATS[usize::from(df)], 4, 4);
                let initial = image.data().to_vec();
                let r = InclusiveRectangle {
                    left: 1,
                    top: 1,
                    right: 2,
                    bottom: 2,
                };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    apply(&mut image, kind, PixelFormat::BgrX32, &wire, &r)
                }));
                record(
                    &mut out,
                    &format!("default-malformed-{kind}-{df}-{size}"),
                    &[20 + kind, df, 5, 0],
                    &wire,
                    &initial,
                    image.data(),
                    format!("{result:?}").as_bytes(),
                );
                count += 1;
            }
        }
    }
    for bpp in [8, 15, 16, 24] {
        for wire in [vec![], vec![0xfd], vec![0xfe], vec![0x61, 1, 2, 3]] {
            let mut decoded = vec![77; 8];
            let initial = decoded.clone();
            let result = ironrdp_graphics::rle::decompress(&wire, &mut decoded, 2, 2, bpp);
            let meta = vec![8, bpp as u8, 0, 0];
            record(
                &mut out,
                &format!("rle-{bpp}-{wire:?}"),
                &meta,
                &wire,
                &initial,
                &decoded,
                format!("{result:?}").as_bytes(),
            );
            count += 1;
        }
    }
    out[8..12].copy_from_slice(&count.to_le_bytes());
    std::fs::write(std::env::args().nth(1).unwrap(), out).unwrap();
    println!("original image records {count}");
}
