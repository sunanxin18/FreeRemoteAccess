use super::*;
use crate::fast_path::{Processor, ProcessorBuilder, UpdateKind};
use ironrdp_graphics::legacy_native::NativeKernels;
use ironrdp_pdu::bitmap::{BitmapData, BitmapUpdateData, Compression};
fn processor(native: bool) -> Processor {
    let mut p = ProcessorBuilder {
        io_channel_id: 1003,
        user_channel_id: 1001,
        share_id: 1,
        enable_server_pointer: false,
        pointer_software_rendering: false,
        bulk_decompressor: None,
    }
    .build();
    if native {
        p.set_native_legacy_kernels(NativeKernels::detect().unwrap());
    }
    p
}
fn bitmap<'a>(wire: &'a [u8], bpp: u16, sw: u16, sh: u16, r: InclusiveRectangle, compressed: bool) -> BitmapData<'a> {
    BitmapData {
        rectangle: r,
        width: sw,
        height: sh,
        bits_per_pixel: bpp,
        compression_flags: if compressed {
            Compression::BITMAP_COMPRESSION | Compression::NO_BITMAP_COMPRESSION_HDR
        } else {
            Compression::empty()
        },
        compressed_data_header: None,
        bitmap_data: wire,
    }
}
#[test]
fn native_rle_underfill_never_publishes_zero_filled_baseline() {
    for bpp in [8, 15, 16, 24] {
        for wire in [&[][..], &[0xfd][..], &[0xfe][..]] {
            let r = InclusiveRectangle {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            };
            let mut image = DecodedImage::new(PixelFormat::RgbA32, 2, 2);
            let initial = image.data().to_vec();
            let update = BitmapUpdateData {
                rectangles: vec![bitmap(wire, bpp, 2, 2, r, true)],
            };
            assert!(
                processor(true).process_bitmap_update(&mut image, update).is_err(),
                "underfill bpp={bpp} wire={wire:?}"
            );
            assert_eq!(image.data(), initial);
        }
    }
}
use ironrdp_graphics::legacy_native::{KernelError, PackedDepth, Pixel32Format, PixelRowSource};
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
fn palette() -> [[u8; 3]; 256] {
    std::array::from_fn(|i| [i as u8, (i as u8).wrapping_mul(37), (i as u8).wrapping_mul(73)])
}
fn field<'a>(input: &mut &'a [u8]) -> &'a [u8] {
    let n = u32::from_le_bytes(input[..4].try_into().unwrap()) as usize;
    let v = &input[4..4 + n];
    *input = &input[4 + n..];
    v
}
fn records() -> Vec<[&'static [u8]; 6]> {
    let data = include_bytes!("fixtures/image-consumers-original.bin");
    assert_eq!(&data[..8], b"FRDIMG01");
    let count = u32::from_le_bytes(data[8..12].try_into().unwrap());
    let mut input = &data[12..];
    let mut out = Vec::new();
    for _ in 0..count {
        out.push(std::array::from_fn(|_| field(&mut input)));
    }
    assert!(input.is_empty());
    out
}
fn source_format(kind: u8, sf: u8) -> PixelRowSource {
    match kind {
        0 => PixelRowSource::Packed(PackedDepth::Index8),
        1 => PixelRowSource::Packed(PackedDepth::Rgb555),
        2 => PixelRowSource::Packed(PackedDepth::Rgb565),
        3 => PixelRowSource::Packed(PackedDepth::Bgr24),
        4 => PixelRowSource::Rgb24,
        5 => PixelRowSource::Raw32(native_pixel_format(FORMATS[usize::from(sf)])),
        6 => PixelRowSource::Region32(native_pixel_format(FORMATS[usize::from(sf)])),
        _ => unreachable!(),
    }
}
fn source_bytes(kind: u8) -> usize {
    match kind {
        0 => 1,
        1 | 2 => 2,
        3 | 4 => 3,
        _ => 4,
    }
}
#[test]
fn frozen_original_complete_images_crop_all_formats_and_native_calls() {
    let kernels = NativeKernels::detect().unwrap();
    let mut checked = 0;
    for [name, meta, wire, initial, expected, _] in records() {
        let kind = meta[0];
        if kind > 6 {
            continue;
        }
        let dim = |i| usize::from(u16::from_le_bytes(meta[i..i + 2].try_into().unwrap()));
        let (sw, sh, dw, dh) = (dim(4), dim(6), dim(8), dim(10));
        let r = InclusiveRectangle {
            left: 1,
            top: 1,
            right: dw as u16,
            bottom: dh as u16,
        };
        let mut image = DecodedImage::new(FORMATS[usize::from(meta[1])], dw as u16 + 3, dh as u16 + 3);
        image.data.copy_from_slice(initial);
        NATIVE_IMAGE_CALLS.with(|v| v.borrow_mut().clear());
        let region = if kind == 6 {
            let mut clips = Region::new();
            clips.union_rectangle(r.clone());
            image.apply_tile_native(
                wire,
                FORMATS[usize::from(meta[2])],
                &clips,
                &InclusiveRectangle {
                    left: 1,
                    top: 1,
                    right: 64,
                    bottom: 64,
                },
                &kernels,
            )
        } else {
            image.apply_bitmap_native(
                wire,
                source_format(kind, meta[2]),
                sw,
                sh,
                sw * source_bytes(kind),
                true,
                &r,
                Some(&palette()),
                &kernels,
            )
        }
        .unwrap();
        assert_eq!(region, Some(r), "{}", String::from_utf8_lossy(name));
        assert_eq!(image.data(), expected, "{}", String::from_utf8_lossy(name));
        NATIVE_IMAGE_CALLS.with(|v| {
            let v = v.borrow();
            assert_eq!(v.iter().filter(|(tag, _)| *tag == "begin").count(), 1);
            assert_eq!(v.iter().filter(|(tag, _)| *tag == "end").count(), 1);
            assert_eq!(v.iter().filter(|(tag, _)| *tag == "row").count(), dh);
            assert!(v.iter().filter(|(tag, _)| *tag == "row").all(|(_, n)| *n == dw * 4));
        });
        checked += 1;
    }
    assert_eq!(checked, 2312);
}
#[test]
fn cursor_frozen_background_move_hide_and_preflight_error_are_atomic() {
    let k = NativeKernels::detect().unwrap();
    let mut checked = 0;
    for [name, meta, wire, initial, expected, result] in records() {
        if meta[0] != 7 {
            continue;
        }
        let mut image = DecodedImage::new(FORMATS[usize::from(meta[1])], 6, 5);
        let pointer = Arc::new(DecodedPointer {
            width: 2,
            height: 2,
            hotspot_x: 0,
            hotspot_y: 0,
            bitmap_data: vec![5, 7, 9, 128, 255, 255, 255, 0, 0, 0, 0, 0, 11, 13, 17, 255],
        });
        image.move_pointer(2, 1).unwrap();
        image.update_pointer(pointer).unwrap();
        if meta[3] == 0 {
            image.hide_pointer().unwrap();
        }
        assert_eq!(image.data(), initial);
        let r = InclusiveRectangle {
            left: 1,
            top: 1,
            right: 3,
            bottom: 2,
        };
        let before = format!("{image:?}");
        for (data, sw, sh, stride, pal) in [
            (&wire[..17], 3, 2, 9, Some(palette())),
            (wire, 2, 2, 6, Some(palette())),
            (wire, 3, 2, 8, Some(palette())),
        ] {
            assert!(
                image
                    .apply_bitmap_native(
                        data,
                        PixelRowSource::Packed(PackedDepth::Bgr24),
                        sw,
                        sh,
                        stride,
                        true,
                        &r,
                        pal.as_ref(),
                        &k
                    )
                    .is_err()
            );
            assert_eq!(image.data(), initial);
            assert_eq!(format!("{image:?}"), before);
        }
        let region = image
            .apply_bitmap_native(
                wire,
                PixelRowSource::Packed(PackedDepth::Bgr24),
                3,
                2,
                9,
                true,
                &r,
                None,
                &k,
            )
            .unwrap()
            .unwrap();
        assert_eq!(format!("{region:?}").as_bytes(), result);
        assert_eq!(image.data(), &expected[..120], "{}", String::from_utf8_lossy(name));
        image.move_pointer(3, 2).unwrap();
        image.hide_pointer().unwrap();
        assert_eq!(image.data(), &expected[120..]);
        checked += 1;
    }
    assert_eq!(checked, 16);
}
use ironrdp_core::{Encode as _, WriteBuf, WriteCursor};
use ironrdp_pdu::fast_path::{EncryptionFlags, FastPathHeader, FastPathUpdatePdu, Fragmentation, UpdateCode};
fn packet(data: &[u8], code: UpdateCode) -> Vec<u8> {
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
fn bitmap_packet(b: BitmapData<'_>) -> Vec<u8> {
    let update = BitmapUpdateData { rectangles: vec![b] };
    let mut data = vec![0; update.size()];
    update.encode(&mut WriteCursor::new(&mut data)).unwrap();
    packet(&data, UpdateCode::Bitmap)
}
fn set_palette(p: &mut Processor, image: &mut DecodedImage) {
    let mut data = vec![0, 0];
    data.extend(256u32.to_le_bytes());
    for [r, g, b] in palette() {
        data.extend([b, g, r, 0]);
    }
    p.process(image, &packet(&data, UpdateCode::Palette), &mut WriteBuf::new())
        .unwrap();
}
#[test]
fn public_raw_padding_and_compressed_full_source_crop_match_original() {
    let mut count = 0;
    for [name, meta, wire, _, expected, _] in records() {
        let kind = meta[0];
        if kind > 5 || meta[3] != 0 || (kind == 5 && meta[2] != 5) {
            continue;
        }
        let d = |i| u16::from_le_bytes(meta[i..i + 2].try_into().unwrap());
        let (sw, sh, dw, dh) = (d(4), d(6), d(8), d(10));
        let bpp = match kind {
            0 => 8,
            1 => 15,
            2 => 16,
            3 => 24,
            _ => 32,
        };
        let r = InclusiveRectangle {
            left: 1,
            top: 1,
            right: dw,
            bottom: dh,
        };
        let mut raw = Vec::new();
        let tight = usize::from(sw) * source_bytes(kind);
        let stride = (tight + 3) & !3;
        for row in wire.chunks_exact(tight) {
            raw.extend(row);
            raw.extend(std::iter::repeat_n(0xee, stride - tight));
        }
        let mut compressed = vec![0x80 + u8::try_from(u32::from(sw) * u32::from(sh)).unwrap()];
        compressed.extend(wire);
        if kind == 4 {
            compressed = vec![0x20];
            for channel in 0..3 {
                compressed.extend(wire.chunks_exact(3).map(|px| px[channel]));
            }
            compressed.push(0);
        }
        for route in [0, 1] {
            if kind == 5 && route == 1 {
                continue;
            }
            if kind == 4 && route == 0 {
                continue;
            }
            let data = if route == 0 { &raw } else { &compressed };
            let mut image = DecodedImage::new(FORMATS[usize::from(meta[1])], dw + 3, dh + 3);
            let mut p = processor(true);
            set_palette(&mut p, &mut image);
            NATIVE_IMAGE_CALLS.with(|v| v.borrow_mut().clear());
            let updates = p
                .process(
                    &mut image,
                    &bitmap_packet(bitmap(data, bpp, sw, sh, r.clone(), route == 1)),
                    &mut WriteBuf::new(),
                )
                .unwrap();
            assert_eq!(
                image.data(),
                expected,
                "{} route={route}",
                String::from_utf8_lossy(name)
            );
            assert!(matches!(&updates[..],[UpdateKind::Region(rect)] if rect==&r));
            NATIVE_IMAGE_CALLS
                .with(|v| assert_eq!(v.borrow().iter().filter(|(t, _)| *t == "row").count(), usize::from(dh)));
            if route == 0 {
                for bad in [
                    &data[..data.len() - 1],
                    &data[..data.len().saturating_sub(stride - tight + 1)],
                    &[data.as_slice(), &vec![0xcc; stride]].concat(),
                ] {
                    let initial = image.data().to_vec();
                    let state = format!("{image:?}");
                    assert!(
                        p.process(
                            &mut image,
                            &bitmap_packet(bitmap(bad, bpp, sw, sh, r.clone(), false)),
                            &mut WriteBuf::new()
                        )
                        .is_err()
                    );
                    assert_eq!(image.data(), initial);
                    assert_eq!(format!("{image:?}"), state);
                }
            }
            count += 1;
        }
    }
    assert_eq!(count, 1040);
}
fn surface_packet(
    set: bool,
    data: &[u8],
    sw: u16,
    sh: u16,
    bpp: u8,
    destination: ironrdp_pdu::geometry::ExclusiveRectangle,
) -> Vec<u8> {
    use ironrdp_pdu::surface_commands::{ExtendedBitmapDataPdu, SurfaceBitsPdu, SurfaceCommand};
    let bits = SurfaceBitsPdu {
        destination,
        extended_bitmap_data: ExtendedBitmapDataPdu {
            bpp,
            codec_id: 0,
            width: sw,
            height: sh,
            header: None,
            data,
        },
    };
    let command = if set {
        SurfaceCommand::SetSurfaceBits(bits)
    } else {
        SurfaceCommand::StreamSurfaceBits(bits)
    };
    let mut bytes = vec![0; command.size()];
    command.encode(&mut WriteCursor::new(&mut bytes)).unwrap();
    packet(&bytes, UpdateCode::SurfaceCommands)
}
#[test]
fn public_set_ignores_bounds_stream_crops_same_bytes_and_exact_surface_layout() {
    use ironrdp_pdu::geometry::ExclusiveRectangle;
    let wire = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc];
    let mut set = DecodedImage::new(PixelFormat::RgbA32, 6, 5);
    let mut stream = DecodedImage::new(PixelFormat::RgbA32, 6, 5);
    let dest = ExclusiveRectangle {
        left: 2,
        top: 1,
        right: 3,
        bottom: 2,
    };
    let a = processor(true)
        .process(
            &mut set,
            &surface_packet(true, &wire, 2, 2, 24, dest.clone()),
            &mut WriteBuf::new(),
        )
        .unwrap();
    assert!(matches!(&a[..],[UpdateKind::Region(r)] if r==&InclusiveRectangle{left:2,top:1,right:3,bottom:2}));
    let b = processor(true)
        .process(
            &mut stream,
            &surface_packet(false, &wire, 2, 2, 24, dest),
            &mut WriteBuf::new(),
        )
        .unwrap();
    assert!(matches!(&b[..],[UpdateKind::Region(r)] if r==&InclusiveRectangle{left:2,top:1,right:2,bottom:1}));
    assert_eq!(&set.data()[32..40], &[0x99, 0x88, 0x77, 255, 0xcc, 0xbb, 0xaa, 255]);
    assert_eq!(&stream.data()[32..36], &set.data()[32..36]);
    assert_eq!(&stream.data()[36..40], &[0; 4]);
    for (right, bottom) in [(0, 0), (1, 0), (65535, 65535)] {
        let mut image = DecodedImage::new(PixelFormat::RgbA32, 6, 5);
        processor(true)
            .process(
                &mut image,
                &surface_packet(
                    true,
                    &wire,
                    2,
                    2,
                    24,
                    ExclusiveRectangle {
                        left: 2,
                        top: 1,
                        right,
                        bottom,
                    },
                ),
                &mut WriteBuf::new(),
            )
            .unwrap();
        assert_eq!(image.data(), set.data());
    }
    for (right, bottom) in [(2, 2), (1, 2), (3, 1), (3, 0)] {
        let mut image = DecodedImage::new(PixelFormat::RgbA32, 6, 5);
        assert!(
            processor(true)
                .process(
                    &mut image,
                    &surface_packet(
                        false,
                        &wire,
                        2,
                        2,
                        24,
                        ExclusiveRectangle {
                            left: 2,
                            top: 1,
                            right,
                            bottom
                        }
                    ),
                    &mut WriteBuf::new()
                )
                .is_err()
        );
        assert_eq!(image.data(), [0; 120]);
    }
    for bad in [&wire[..11], &[wire.as_slice(), &[0xee; 4]].concat()] {
        let mut image = DecodedImage::new(PixelFormat::RgbA32, 6, 5);
        assert!(
            processor(true)
                .process(
                    &mut image,
                    &surface_packet(
                        true,
                        bad,
                        2,
                        2,
                        24,
                        ExclusiveRectangle {
                            left: 2,
                            top: 1,
                            right: 0,
                            bottom: 0
                        }
                    ),
                    &mut WriteBuf::new()
                )
                .is_err()
        );
        assert_eq!(image.data(), [0; 120]);
    }
}
#[test]
fn public_noop_and_real_origin_pixel_regions_remain_distinct() {
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 2, 2);
    let mut p = processor(true);
    let r = InclusiveRectangle {
        left: 2,
        top: 0,
        right: 2,
        bottom: 0,
    };
    let updates = p
        .process(
            &mut image,
            &bitmap_packet(bitmap(&[1, 2, 3, 0xee], 24, 1, 1, r, false)),
            &mut WriteBuf::new(),
        )
        .unwrap();
    assert!(updates.iter().all(|u| !matches!(u, UpdateKind::Region(_))));
    assert_eq!(image.data(), [0; 16]);
    use ironrdp_pdu::surface_commands::{FrameAction, FrameMarkerPdu, SurfaceCommand};
    let cmd = SurfaceCommand::FrameMarker(FrameMarkerPdu {
        frame_action: FrameAction::Begin,
        frame_id: Some(17),
    });
    let mut data = vec![0; cmd.size()];
    cmd.encode(&mut WriteCursor::new(&mut data)).unwrap();
    let updates = p
        .process(
            &mut image,
            &packet(&data, UpdateCode::SurfaceCommands),
            &mut WriteBuf::new(),
        )
        .unwrap();
    assert!(updates.iter().all(|u| !matches!(u, UpdateKind::Region(_))));
    let r = InclusiveRectangle {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    let updates = p
        .process(
            &mut image,
            &bitmap_packet(bitmap(&[1, 2, 3, 0xee], 24, 1, 1, r.clone(), false)),
            &mut WriteBuf::new(),
        )
        .unwrap();
    assert!(matches!(&updates[..],[UpdateKind::Region(rect)] if rect==&r));
    assert_eq!(&image.data()[..4], &[3, 2, 1, 255]);
}
use core::error::Error as _;
#[test]
fn original_rle_malformed_parity_and_strict_actual_progress_are_separate() {
    use ironrdp_graphics::rle::{self, CompleteRleError};
    let k = NativeKernels::detect().unwrap();
    let mut count = 0;
    for [_, meta, wire, initial, expected, result] in records() {
        if meta[0] != 8 {
            continue;
        }
        let mut v = initial.to_vec();
        let r = rle::decompress(wire, &mut v, 2, 2, usize::from(meta[1]));
        assert_eq!(format!("{r:?}").as_bytes(), result);
        assert_eq!(&v, expected);
        count += 1;
    }
    assert_eq!(count, 16);
    for (bpp, bytes) in [(8, 1), (15, 2), (16, 2), (24, 3)] {
        for (wire, written, consumed) in [(&[][..], 0, 0), (&[0xfd][..], bytes, 1), (&[0xfe][..], bytes, 1)] {
            let mut decoded = vec![99; 7];
            let e = rle::decompress_native_complete(wire, &mut decoded, 2, 2, bpp, &k).unwrap_err();
            assert!(
                matches!(e,CompleteRleError::Incomplete{expected,written:w,consumed:c} if expected==4*bytes && w==written && c==consumed)
            );
        }
    }
}
#[test]
fn compression_header_source_smaller_and_rectangle_failure_boundaries() {
    use ironrdp_pdu::bitmap::CompressedDataHeader;
    let r = InclusiveRectangle {
        left: 1,
        top: 1,
        right: 2,
        bottom: 2,
    };
    let wire = [0x84, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    for header in [false, true] {
        let mut b = bitmap(&wire, 24, 2, 2, r.clone(), true);
        if header {
            b.compression_flags = Compression::BITMAP_COMPRESSION;
            b.compressed_data_header = Some(CompressedDataHeader {
                main_body_size: 13,
                scan_width: 4,
                uncompressed_size: 12,
            });
        }
        let mut image = DecodedImage::new(PixelFormat::RgbA32, 4, 4);
        processor(true)
            .process(&mut image, &bitmap_packet(b), &mut WriteBuf::new())
            .unwrap();
        assert_eq!(&image.data()[20..28], &[9, 8, 7, 255, 12, 11, 10, 255]);
    }
    for bad in [
        bitmap(&[1, 2, 3, 0], 24, 1, 1, r.clone(), false),
        bitmap(&[1, 2, 3, 0], 7, 1, 1, r.clone(), false),
        bitmap(&[], 24, 0, 1, r.clone(), false),
        bitmap(
            &[],
            24,
            1,
            1,
            InclusiveRectangle {
                left: 2,
                top: 1,
                right: 1,
                bottom: 1,
            },
            false,
        ),
    ] {
        let mut image = DecodedImage::new(PixelFormat::RgbA32, 4, 4);
        let initial = image.data().to_vec();
        let e = processor(true)
            .process_bitmap_update(&mut image, BitmapUpdateData { rectangles: vec![bad] })
            .unwrap_err();
        assert!(e.source().is_some());
        assert_eq!(image.data(), initial);
    }
    for first_bad in [false, true] {
        let mut image = DecodedImage::new(PixelFormat::RgbA32, 4, 4);
        let first = bitmap(
            if first_bad { &[1, 2, 3][..] } else { &[1, 2, 3, 0][..] },
            24,
            1,
            1,
            InclusiveRectangle {
                left: 1,
                top: 1,
                right: 1,
                bottom: 1,
            },
            false,
        );
        let second = bitmap(
            &[0xfd],
            24,
            2,
            2,
            InclusiveRectangle {
                left: 2,
                top: 2,
                right: 3,
                bottom: 3,
            },
            true,
        );
        let e = processor(true)
            .process_bitmap_update(
                &mut image,
                BitmapUpdateData {
                    rectangles: vec![first, second],
                },
            )
            .unwrap_err();
        assert!(e.source().is_some());
        let mut expected = vec![0; 64];
        if !first_bad {
            expected[20..24].copy_from_slice(&[3, 2, 1, 255]);
        }
        assert_eq!(image.data(), expected);
    }
}
#[test]
fn native_resource_geometry_preflight_without_huge_allocation() {
    let image = DecodedImage::new(PixelFormat::RgbA32, 2, 2);
    let r = InclusiveRectangle {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    let f = PixelRowSource::Raw32(Pixel32Format::RgbA);
    let limit = ironrdp_graphics::rle::MAX_NATIVE_BITMAP_BYTES;
    for (w, h) in [(3840, 2160), (7680, 4320), (8192, 8192)] {
        assert_eq!(image.native_bitmap_size(f, w, h, w * 4, &r).unwrap(), w * h * 4);
    }
    for (w, h, stride) in [
        (8192, 8193, 8192 * 4),
        (65535, 65535, 65535 * 4),
        (usize::MAX, 2, usize::MAX),
    ] {
        let e = image.native_bitmap_size(f, w, h, stride, &r).unwrap_err();
        assert_eq!(
            e.source().unwrap().downcast_ref::<KernelError>(),
            Some(&KernelError::InvalidDimensions)
        );
    }
    assert_eq!(limit, 256 * 1024 * 1024);
    let k = NativeKernels::detect().unwrap();
    let mut dst = vec![77; 4];
    assert!(ironrdp_graphics::rle::decompress_native_complete(&[], &mut dst, 65535, 65535, 24, &k).is_err());
    assert_eq!(dst, [77; 4]);
    // Index8 palette validation includes empty capability preflight; no cursor/data publication.
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 2, 2);
    let before = format!("{image:?}");
    let e = image
        .apply_bitmap_native(
            &[1],
            PixelRowSource::Packed(PackedDepth::Index8),
            1,
            1,
            1,
            true,
            &r,
            None,
            &k,
        )
        .unwrap_err();
    assert_eq!(
        e.source().unwrap().downcast_ref::<KernelError>(),
        Some(&KernelError::MissingPalette)
    );
    assert_eq!(format!("{image:?}"), before);
}
fn rfx_variant(no_tiles: bool, empty_clip: bool) -> Vec<u8> {
    use ironrdp_core::{Decode as _, ReadCursor};
    use ironrdp_pdu::codecs::rfx;
    let mut cursor = ReadCursor::new(include_bytes!(
        "../../../crates/frd-rdp-legacy-kernels/tests/fixtures/encoded_messages.bin"
    ));
    let mut data = Vec::new();
    while !cursor.is_empty() {
        let mut block = rfx::Block::decode(&mut cursor).unwrap();
        match &mut block {
            rfx::Block::Channels(c) => {
                c.0[0].width = 128;
                c.0[0].height = 64;
            }
            rfx::Block::CodecChannel(rfx::CodecChannel::Region(r)) => {
                r.rectangles = vec![rfx::RfxRectangle {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                }];
            }
            rfx::Block::CodecChannel(rfx::CodecChannel::TileSet(t)) => {
                if no_tiles {
                    t.tiles.clear();
                } else if empty_clip {
                    t.tiles[0].x = 1;
                }
            }
            _ => (),
        }
        let mut bytes = vec![0; block.size()];
        block.encode(&mut WriteCursor::new(&mut bytes)).unwrap();
        data.extend(bytes);
    }
    use ironrdp_pdu::surface_commands::{ExtendedBitmapDataPdu, SurfaceBitsPdu, SurfaceCommand};
    let cmd = SurfaceCommand::SetSurfaceBits(SurfaceBitsPdu {
        destination: ironrdp_pdu::geometry::ExclusiveRectangle {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
        extended_bitmap_data: ExtendedBitmapDataPdu {
            bpp: 32,
            codec_id: 3,
            width: 128,
            height: 64,
            header: None,
            data: &data,
        },
    });
    let mut bytes = vec![0; cmd.size()];
    cmd.encode(&mut WriteCursor::new(&mut bytes)).unwrap();
    packet(&bytes, UpdateCode::SurfaceCommands)
}
#[test]
fn public_stage_reactivation_noop_bitmap_marker_rfx_and_origin_real_pixel() {
    use crate::{ActiveStageBuilder, ActiveStageOutput};
    use ironrdp_svc::StaticChannelSet;
    let mut stage = ActiveStageBuilder {
        static_channels: StaticChannelSet::new(),
        user_channel_id: 1001,
        io_channel_id: 1003,
        message_channel_id: None,
        share_id: 1,
        compression_type: None,
        enable_server_pointer: false,
        pointer_software_rendering: false,
    }
    .build();
    stage.set_native_legacy_kernels(NativeKernels::detect().unwrap());
    for _ in 0..3 {
        stage.set_fastpath_processor(processor(false));
        let mut image = DecodedImage::new(PixelFormat::RgbA32, 128, 64);
        let outside = bitmap_packet(bitmap(
            &[1, 2, 3, 0xee],
            24,
            1,
            1,
            InclusiveRectangle {
                left: 128,
                top: 0,
                right: 128,
                bottom: 0,
            },
            false,
        ));
        use ironrdp_pdu::surface_commands::{FrameAction, FrameMarkerPdu, SurfaceCommand};
        let marker = SurfaceCommand::FrameMarker(FrameMarkerPdu {
            frame_action: FrameAction::Begin,
            frame_id: Some(5),
        });
        let mut data = vec![0; marker.size()];
        marker.encode(&mut WriteCursor::new(&mut data)).unwrap();
        for input in [
            outside,
            packet(&data, UpdateCode::SurfaceCommands),
            rfx_variant(true, false),
            rfx_variant(false, true),
        ] {
            let before = format!("{image:?}");
            let outputs = stage
                .process(&mut image, ironrdp_pdu::Action::FastPath, &input)
                .unwrap();
            assert!(
                outputs
                    .iter()
                    .all(|u| !matches!(u, ActiveStageOutput::GraphicsUpdate(_)))
            );
            assert_eq!(format!("{image:?}"), before);
            assert!(image.data().iter().all(|b| *b == 0));
        }
        let input = bitmap_packet(bitmap(
            &[1, 2, 3, 0xee],
            24,
            1,
            1,
            InclusiveRectangle {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            false,
        ));
        let outputs = stage
            .process(&mut image, ironrdp_pdu::Action::FastPath, &input)
            .unwrap();
        assert!(outputs.iter().any(|u|matches!(u,ActiveStageOutput::GraphicsUpdate(r) if r==&InclusiveRectangle{left:0,top:0,right:0,bottom:0})));
        assert_eq!(&image.data()[..4], &[3, 2, 1, 255]);
        let input = bitmap_packet(bitmap(
            &[],
            24,
            1,
            1,
            InclusiveRectangle {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            true,
        ));
        assert!(
            stage
                .process(&mut image, ironrdp_pdu::Action::FastPath, &input)
                .is_err()
        );
    }
}
#[test]
fn original_multiple_tile_regions_intersections_stride_and_empty_clip() {
    let k = NativeKernels::detect().unwrap();
    let mut count = 0;
    for [name, meta, wire, initial, expected, _] in records() {
        if meta[0] != 9 {
            continue;
        }
        let mut image = DecodedImage::new(FORMATS[usize::from(meta[1])], 80, 75);
        image.data.copy_from_slice(initial);
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
        let intersection = clips.intersect_rectangle(&update);
        let region = image
            .apply_tile_native(wire, FORMATS[usize::from(meta[2])], &clips, &update, &k)
            .unwrap();
        assert_eq!(region, Some(intersection.extents));
        assert_eq!(image.data(), expected, "{}", String::from_utf8_lossy(name));
        let before = format!("{image:?}");
        let initial = image.data().to_vec();
        let outside = InclusiveRectangle {
            left: 71,
            top: 1,
            right: 134,
            bottom: 64,
        };
        NATIVE_IMAGE_CALLS.with(|v| v.borrow_mut().clear());
        assert_eq!(
            image
                .apply_tile_native(wire, FORMATS[usize::from(meta[2])], &clips, &outside, &k)
                .unwrap(),
            None
        );
        assert_eq!(image.data(), initial);
        assert_eq!(format!("{image:?}"), before);
        NATIVE_IMAGE_CALLS.with(|v| assert!(v.borrow().is_empty()));
        count += 1;
    }
    assert_eq!(count, 64);
}
#[test]
fn frozen_default_malformed_helper_behavior_is_not_native_contract() {
    let mut count = 0;
    for [name, meta, wire, initial, expected, result] in records() {
        if !(20..=25).contains(&meta[0]) {
            continue;
        }
        let kind = meta[0] - 20;
        let mut image = DecodedImage::new(FORMATS[usize::from(meta[1])], 4, 4);
        image.data.copy_from_slice(initial);
        let r = InclusiveRectangle {
            left: 1,
            top: 1,
            right: 2,
            bottom: 2,
        };
        let old = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match kind {
            0 => image.apply_rgb8_with_palette(wire, &r, &palette()).unwrap(),
            1 => image.apply_rgb15_bitmap(wire, &r).unwrap(),
            2 => image.apply_rgb16_bitmap(wire, &r).unwrap(),
            3 => image.apply_bgr24_bitmap(wire, &r).unwrap(),
            5 => image.apply_rgb32_bitmap(wire, PixelFormat::BgrX32, &r).unwrap(),
            _ => unreachable!(),
        }));
        assert_eq!(
            format!("{old:?}").as_bytes(),
            result,
            "{}",
            String::from_utf8_lossy(name)
        );
        assert_eq!(image.data(), expected);
        let mut native = DecodedImage::new(FORMATS[usize::from(meta[1])], 4, 4);
        let initial = native.data().to_vec();
        let state = format!("{native:?}");
        assert!(
            native
                .apply_bitmap_native(
                    wire,
                    source_format(kind, 5),
                    2,
                    2,
                    2 * source_bytes(kind),
                    true,
                    &r,
                    Some(&palette()),
                    &NativeKernels::detect().unwrap()
                )
                .is_err()
        );
        assert_eq!(native.data(), initial);
        assert_eq!(format!("{native:?}"), state);
        count += 1;
    }
    assert_eq!(count, 120);
}
#[test]
fn native_source_offsets_zero_to_fifteen_keep_canaries_and_crop_bytes() {
    let k = NativeKernels::detect().unwrap();
    let mut count = 0;
    for [name, meta, wire, initial, expected, _] in records() {
        let kind = meta[0];
        if kind > 6 {
            continue;
        }
        let d = |i| usize::from(u16::from_le_bytes(meta[i..i + 2].try_into().unwrap()));
        let (sw, sh, dw, dh) = (d(4), d(6), d(8), d(10));
        let r = InclusiveRectangle {
            left: 1,
            top: 1,
            right: dw as u16,
            bottom: dh as u16,
        };
        for offset in 0..16 {
            let mut backing = vec![0xd7; offset];
            backing.extend(wire);
            backing.extend([0xb9; 16]);
            let before = backing.clone();
            let mut image = DecodedImage::new(FORMATS[usize::from(meta[1])], dw as u16 + 3, dh as u16 + 3);
            image.data.copy_from_slice(initial);
            let source = &backing[offset..offset + wire.len()];
            if kind == 6 {
                let mut clips = Region::new();
                clips.union_rectangle(r.clone());
                image
                    .apply_tile_native(
                        source,
                        FORMATS[usize::from(meta[2])],
                        &clips,
                        &InclusiveRectangle {
                            left: 1,
                            top: 1,
                            right: 64,
                            bottom: 64,
                        },
                        &k,
                    )
                    .unwrap();
            } else {
                image
                    .apply_bitmap_native(
                        source,
                        source_format(kind, meta[2]),
                        sw,
                        sh,
                        sw * source_bytes(kind),
                        true,
                        &r,
                        Some(&palette()),
                        &k,
                    )
                    .unwrap();
            }
            assert_eq!(
                image.data(),
                expected,
                "{} offset={offset}",
                String::from_utf8_lossy(name)
            );
            assert_eq!(backing, before);
            count += 1;
        }
    }
    assert_eq!(count, 36992);
}
#[test]
fn visible_cursor_first_rectangle_retained_second_rejection_reads_new_background() {
    for [_, meta, wire, initial, expected, _] in records() {
        if meta[0] != 7 || meta[3] != 1 {
            continue;
        }
        let mut image = DecodedImage::new(FORMATS[usize::from(meta[1])], 6, 5);
        let pointer = Arc::new(DecodedPointer {
            width: 2,
            height: 2,
            hotspot_x: 0,
            hotspot_y: 0,
            bitmap_data: vec![5, 7, 9, 128, 255, 255, 255, 0, 0, 0, 0, 0, 11, 13, 17, 255],
        });
        image.move_pointer(2, 1).unwrap();
        image.update_pointer(pointer).unwrap();
        assert_eq!(image.data(), initial);
        let mut raw = Vec::new();
        for row in wire.chunks_exact(9) {
            raw.extend(row);
            raw.extend([0xde; 3]);
        }
        let first = bitmap(
            &raw,
            24,
            3,
            2,
            InclusiveRectangle {
                left: 1,
                top: 1,
                right: 3,
                bottom: 2,
            },
            false,
        );
        let second = bitmap(
            &[],
            24,
            1,
            1,
            InclusiveRectangle {
                left: 2,
                top: 1,
                right: 2,
                bottom: 1,
            },
            true,
        );
        assert!(
            processor(true)
                .process_bitmap_update(
                    &mut image,
                    BitmapUpdateData {
                        rectangles: vec![first, second]
                    }
                )
                .is_err()
        );
        assert_eq!(image.data(), &expected[..120]);
        image.move_pointer(3, 2).unwrap();
        image.hide_pointer().unwrap();
        assert_eq!(image.data(), &expected[120..]);
    }
}
#[test]
fn pending_mouse_preserved_on_first_failure_applied_once_after_success() {
    let record = records()
        .into_iter()
        .find(|v| v[1][0] == 7 && v[1][1] == 6 && v[1][3] == 1)
        .unwrap();
    let [_, _, wire, initial, expected, _] = record;
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 6, 5);
    image.move_pointer(2, 1).unwrap();
    image
        .update_pointer(Arc::new(DecodedPointer {
            width: 2,
            height: 2,
            hotspot_x: 0,
            hotspot_y: 0,
            bitmap_data: vec![5, 7, 9, 128, 255, 255, 255, 0, 0, 0, 0, 0, 11, 13, 17, 255],
        }))
        .unwrap();
    assert_eq!(image.data(), initial);
    let before = format!("{image:?}");
    let mut p = processor(true);
    p.update_mouse_pos(3, 2);
    let bad = bitmap_packet(bitmap(
        &[],
        24,
        3,
        2,
        InclusiveRectangle {
            left: 1,
            top: 1,
            right: 3,
            bottom: 2,
        },
        true,
    ));
    for input in [&[][..], bad.as_slice()] {
        assert!(p.process(&mut image, input, &mut WriteBuf::new()).is_err());
        assert_eq!(format!("{image:?}"), before);
        assert_eq!(image.data(), initial);
    }
    let mut raw = Vec::new();
    for row in wire.chunks_exact(9) {
        raw.extend(row);
        raw.extend([0xde; 3]);
    }
    p.process(
        &mut image,
        &bitmap_packet(bitmap(
            &raw,
            24,
            3,
            2,
            InclusiveRectangle {
                left: 1,
                top: 1,
                right: 3,
                bottom: 2,
            },
            false,
        )),
        &mut WriteBuf::new(),
    )
    .unwrap();
    assert_eq!((image.pointer_x, image.pointer_y), (3, 2));
    image.hide_pointer().unwrap();
    assert_eq!(image.data(), &expected[120..]);
    image.move_pointer(4, 3).unwrap();
    p.process(
        &mut image,
        &bitmap_packet(bitmap(
            &[1, 2, 3, 0],
            24,
            1,
            1,
            InclusiveRectangle {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            false,
        )),
        &mut WriteBuf::new(),
    )
    .unwrap();
    assert_eq!((image.pointer_x, image.pointer_y), (4, 3));
}
fn multi_packet(updates: &[(UpdateCode, Vec<u8>)]) -> Vec<u8> {
    let mut payload = Vec::new();
    for (code, data) in updates {
        let u = FastPathUpdatePdu {
            fragmentation: Fragmentation::Single,
            update_code: *code,
            compression_flags: None,
            compression_type: None,
            data,
        };
        let mut bytes = vec![0; u.size()];
        u.encode(&mut WriteCursor::new(&mut bytes)).unwrap();
        payload.extend(bytes);
    }
    let header = FastPathHeader::new(EncryptionFlags::empty(), payload.len());
    let mut frame = vec![0; header.size() + payload.len()];
    let mut c = WriteCursor::new(&mut frame);
    header.encode(&mut c).unwrap();
    c.write_slice(&payload);
    frame
}
#[test]
fn pending_mouse_server_position_wins_and_first_valid_rectangle_survives_later_error() {
    use ironrdp_pdu::pointer::Point16;
    use ironrdp_pdu::pointer::{ColorPointerAttribute, PointerAttribute};
    let mut p = ProcessorBuilder {
        io_channel_id: 1003,
        user_channel_id: 1001,
        share_id: 1,
        enable_server_pointer: true,
        pointer_software_rendering: true,
        bulk_decompressor: None,
    }
    .build();
    p.set_native_legacy_kernels(NativeKernels::detect().unwrap());
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 8, 6);
    let pointer = PointerAttribute {
        xor_bpp: 32,
        color_pointer: ColorPointerAttribute {
            cache_index: 1,
            hot_spot: Point16 { x: 0, y: 0 },
            width: 1,
            height: 1,
            xor_mask: &[3, 2, 1, 255],
            and_mask: &[0, 0],
        },
    };
    let mut data = vec![0; pointer.size()];
    pointer.encode(&mut WriteCursor::new(&mut data)).unwrap();
    p.process(&mut image, &packet(&data, UpdateCode::NewPointer), &mut WriteBuf::new())
        .unwrap();
    p.process(
        &mut image,
        &packet(&1u16.to_le_bytes(), UpdateCode::CachedPointer),
        &mut WriteBuf::new(),
    )
    .unwrap();
    p.update_mouse_pos(3, 2);
    let bitmap_update = BitmapUpdateData {
        rectangles: vec![bitmap(
            &[1, 2, 3, 0],
            24,
            1,
            1,
            InclusiveRectangle {
                left: 1,
                top: 1,
                right: 1,
                bottom: 1,
            },
            false,
        )],
    };
    let mut data = vec![0; bitmap_update.size()];
    bitmap_update.encode(&mut WriteCursor::new(&mut data)).unwrap();
    let frame = multi_packet(&[
        (UpdateCode::PositionPointer, vec![6, 0, 4, 0]),
        (UpdateCode::Bitmap, data),
    ]);
    p.process(&mut image, &frame, &mut WriteBuf::new()).unwrap();
    assert_eq!((image.pointer_x, image.pointer_y), (6, 4));
    assert_eq!(&image.data()[36..40], &[3, 2, 1, 255]);
    p.update_mouse_pos(2, 1);
    let bad = BitmapUpdateData {
        rectangles: vec![
            bitmap(
                &[5, 7, 11, 0],
                24,
                1,
                1,
                InclusiveRectangle {
                    left: 1,
                    top: 1,
                    right: 1,
                    bottom: 1,
                },
                false,
            ),
            bitmap(
                &[],
                24,
                1,
                1,
                InclusiveRectangle {
                    left: 2,
                    top: 1,
                    right: 2,
                    bottom: 1,
                },
                true,
            ),
        ],
    };
    let mut data = vec![0; bad.size()];
    bad.encode(&mut WriteCursor::new(&mut data)).unwrap();
    assert!(
        p.process(&mut image, &packet(&data, UpdateCode::Bitmap), &mut WriteBuf::new())
            .is_err()
    );
    assert_eq!((image.pointer_x, image.pointer_y), (6, 4));
    assert_eq!(&image.data()[36..40], &[11, 7, 5, 255]);
    p.process(&mut image, &packet(&[], UpdateCode::Palette), &mut WriteBuf::new())
        .unwrap();
    assert_eq!((image.pointer_x, image.pointer_y), (2, 1));
}
fn pointer_frame(alternate: bool) -> Vec<u8> {
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
    packet(&data, UpdateCode::NewPointer)
}
#[test]
fn frozen_original_pending_mouse_pointer_shape_hide_default_cached_event_order() {
    let mut count = 0;
    for [name, meta, wire, initial, expected, result] in records() {
        if meta[0] != 30 {
            continue;
        }
        let mut p = ProcessorBuilder {
            io_channel_id: 1003,
            user_channel_id: 1001,
            share_id: 1,
            enable_server_pointer: true,
            pointer_software_rendering: meta[3] != 0,
            bulk_decompressor: None,
        }
        .build();
        p.set_native_legacy_kernels(NativeKernels::detect().unwrap());
        let mut image = DecodedImage::new(FORMATS[usize::from(meta[1])], 8, 6);
        for frame in [
            pointer_frame(false),
            packet(&[1, 0], UpdateCode::CachedPointer),
            packet(&[2, 0, 1, 0], UpdateCode::PositionPointer),
        ] {
            p.process(&mut image, &frame, &mut WriteBuf::new()).unwrap();
        }
        assert_eq!(image.data(), initial);
        p.update_mouse_pos(3, 2);
        let updates = p.process(&mut image, wire, &mut WriteBuf::new()).unwrap();
        assert_eq!(image.data(), expected, "{}", String::from_utf8_lossy(name));
        assert_eq!(format!("updates={updates:?}\nimage={image:?}").as_bytes(), result);
        count += 1;
    }
    assert_eq!(count, 96);
}
#[test]
fn pending_mouse_cache_miss_or_pointer_decode_failure_keeps_queue_and_image() {
    use ironrdp_pdu::pointer::{ColorPointerAttribute, Point16, PointerAttribute};
    let mut p = ProcessorBuilder {
        io_channel_id: 1003,
        user_channel_id: 1001,
        share_id: 1,
        enable_server_pointer: true,
        pointer_software_rendering: true,
        bulk_decompressor: None,
    }
    .build();
    p.set_native_legacy_kernels(NativeKernels::detect().unwrap());
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 8, 6);
    for frame in [
        pointer_frame(false),
        packet(&[1, 0], UpdateCode::CachedPointer),
        packet(&[2, 0, 1, 0], UpdateCode::PositionPointer),
    ] {
        p.process(&mut image, &frame, &mut WriteBuf::new()).unwrap();
    }
    let initial = image.data().to_vec();
    let before = format!("{image:?}");
    p.update_mouse_pos(3, 2);
    let updates = p
        .process(
            &mut image,
            &packet(&[99, 0], UpdateCode::CachedPointer),
            &mut WriteBuf::new(),
        )
        .unwrap();
    assert!(updates.is_empty());
    assert_eq!(image.data(), initial);
    assert_eq!(format!("{image:?}"), before);
    let pointer = PointerAttribute {
        xor_bpp: 7,
        color_pointer: ColorPointerAttribute {
            cache_index: 2,
            hot_spot: Point16 { x: 0, y: 0 },
            width: 1,
            height: 1,
            xor_mask: &[3, 2, 1, 255],
            and_mask: &[0, 0],
        },
    };
    let mut data = vec![0; pointer.size()];
    pointer.encode(&mut WriteCursor::new(&mut data)).unwrap();
    assert!(
        p.process(&mut image, &packet(&data, UpdateCode::NewPointer), &mut WriteBuf::new())
            .is_err()
    );
    assert_eq!(image.data(), initial);
    assert_eq!(format!("{image:?}"), before);
    p.process(
        &mut image,
        &bitmap_packet(bitmap(
            &[1, 2, 3, 0],
            24,
            1,
            1,
            InclusiveRectangle {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            false,
        )),
        &mut WriteBuf::new(),
    )
    .unwrap();
    assert_eq!((image.pointer_x, image.pointer_y), (3, 2));
}
#[test]
fn pending_mouse_color_and_large_decode_failures_preserve_cursor_and_queue() {
    use ironrdp_pdu::pointer::{ColorPointerAttribute, LargePointerAttribute, Point16};
    for large in [false, true] {
        let mut p = ProcessorBuilder {
            io_channel_id: 1003,
            user_channel_id: 1001,
            share_id: 1,
            enable_server_pointer: true,
            pointer_software_rendering: true,
            bulk_decompressor: None,
        }
        .build();
        p.set_native_legacy_kernels(NativeKernels::detect().unwrap());
        let mut image = DecodedImage::new(PixelFormat::RgbA32, 8, 6);
        for frame in [
            pointer_frame(false),
            packet(&[1, 0], UpdateCode::CachedPointer),
            packet(&[2, 0, 1, 0], UpdateCode::PositionPointer),
        ] {
            p.process(&mut image, &frame, &mut WriteBuf::new()).unwrap();
        }
        let initial = image.data().to_vec();
        let before = format!("{image:?}");
        p.update_mouse_pos(3, 2);
        let (data, code) = if large {
            let pointer = LargePointerAttribute {
                xor_bpp: 7,
                cache_index: 2,
                hot_spot: Point16 { x: 0, y: 0 },
                width: 1,
                height: 1,
                xor_mask: &[3, 2, 1, 255],
                and_mask: &[0, 0],
            };
            let mut data = vec![0; pointer.size()];
            pointer.encode(&mut WriteCursor::new(&mut data)).unwrap();
            (data, UpdateCode::LargePointer)
        } else {
            let pointer = ColorPointerAttribute {
                cache_index: 2,
                hot_spot: Point16 { x: 0, y: 0 },
                width: 1,
                height: 1,
                xor_mask: &[3, 2],
                and_mask: &[0, 0],
            };
            let mut data = vec![0; pointer.size()];
            pointer.encode(&mut WriteCursor::new(&mut data)).unwrap();
            (data, UpdateCode::ColorPointer)
        };
        let e = p
            .process(&mut image, &packet(&data, code), &mut WriteBuf::new())
            .unwrap_err();
        assert!(e.source().is_some());
        assert_eq!(image.data(), initial);
        assert_eq!(format!("{image:?}"), before);
        p.process(
            &mut image,
            &bitmap_packet(bitmap(
                &[1, 2, 3, 0],
                24,
                1,
                1,
                InclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                },
                false,
            )),
            &mut WriteBuf::new(),
        )
        .unwrap();
        assert_eq!((image.pointer_x, image.pointer_y), (3, 2));
    }
}
