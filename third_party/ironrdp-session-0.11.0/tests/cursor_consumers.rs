use super::*;
use ironrdp_pdu::pointer::{ColorPointerAttribute, Point16, PointerAttribute};

fn processor(native: bool) -> Processor {
    let mut p = ProcessorBuilder {
        io_channel_id: 1003,
        user_channel_id: 1001,
        share_id: 1,
        enable_server_pointer: true,
        pointer_software_rendering: true,
        bulk_decompressor: None,
    }
    .build();
    if native {
        p.set_native_legacy_kernels(NativeKernels::detect().unwrap());
    }
    p
}
fn rect(left: u16, top: u16, right: u16, bottom: u16) -> InclusiveRectangle {
    InclusiveRectangle {
        left,
        top,
        right,
        bottom,
    }
}
#[test]
fn retained_native_new_cursor_wraps_partial_alpha_and_restores_byte3() {
    let k = NativeKernels::detect().unwrap();
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 8, 6);
    let background = [255, 255, 255, 17].repeat(48);
    image
        .apply_bitmap_native(
            &background,
            PixelRowSource::Raw32(Pixel32Format::RgbA),
            8,
            6,
            32,
            false,
            &rect(0, 0, 7, 5),
            None,
            &k,
        )
        .unwrap();
    let mut p = processor(true);
    let pointer = PointerAttribute {
        xor_bpp: 32,
        color_pointer: ColorPointerAttribute {
            cache_index: 1,
            hot_spot: Point16 { x: 0, y: 0 },
            width: 1,
            height: 1,
            xor_mask: &[255, 255, 255, 1],
            and_mask: &[],
        },
    };
    p.process_pointer_update(&mut image, PointerUpdateData::New(pointer))
        .unwrap();
    // 固定原版 release 算式的明确锚；debug 原版会 overflow panic。
    assert_eq!(&image.data()[..4], &[251, 251, 251, 17]);
    p.process_pointer_update(
        &mut image,
        PointerUpdateData::Cached(ironrdp_pdu::pointer::CachedPointerAttribute { cache_index: 1 }),
    )
    .unwrap();
    p.process_pointer_update(&mut image, PointerUpdateData::SetHidden)
        .unwrap();
    assert_eq!(image.data(), background);
}
fn new_pointer<'a>(xor: &'a [u8], and: &'a [u8]) -> PointerAttribute<'a> {
    PointerAttribute {
        xor_bpp: 32,
        color_pointer: ColorPointerAttribute {
            cache_index: 1,
            hot_spot: Point16 { x: 0, y: 0 },
            width: 3,
            height: 1,
            xor_mask: xor,
            and_mask: and,
        },
    }
}
#[test]
fn native_pending_cache_miss_decode_failure_server_position_and_hidden_order() {
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 12, 8);
    let mut p = processor(true);
    let xor = [255, 255, 255, 0, 255, 255, 255, 255, 64, 80, 96, 128];
    p.process_pointer_update(&mut image, PointerUpdateData::New(new_pointer(&xor, &[])))
        .unwrap();
    p.process_pointer_update(
        &mut image,
        PointerUpdateData::Cached(ironrdp_pdu::pointer::CachedPointerAttribute { cache_index: 1 }),
    )
    .unwrap();
    p.update_mouse_pos(4, 3);
    let before = format!("{image:?}");
    let miss = p
        .process_pointer_update(
            &mut image,
            PointerUpdateData::Cached(ironrdp_pdu::pointer::CachedPointerAttribute { cache_index: 99 }),
        )
        .unwrap();
    assert!(miss.is_empty());
    assert_eq!(p.mouse_pos_update, Some((4, 3)));
    assert_eq!(format!("{image:?}"), before);
    for update in [
        PointerUpdateData::New(new_pointer(&xor[..8], &[])),
        PointerUpdateData::Color(ColorPointerAttribute {
            width: 3,
            height: 1,
            xor_mask: &xor[..8],
            and_mask: &[],
            ..new_pointer(&xor, &[]).color_pointer
        }),
        PointerUpdateData::Large(ironrdp_pdu::pointer::LargePointerAttribute {
            xor_bpp: 32,
            width: 3,
            height: 1,
            hot_spot: Point16 { x: 0, y: 0 },
            cache_index: 1,
            xor_mask: &xor[..8],
            and_mask: &[],
        }),
    ] {
        assert!(p.process_pointer_update(&mut image, update).is_err());
        assert_eq!(p.mouse_pos_update, Some((4, 3)));
        assert_eq!(format!("{image:?}"), before);
    }
    let position = p
        .process_pointer_update(&mut image, PointerUpdateData::SetPosition(Point16 { x: 6, y: 4 }))
        .unwrap();
    assert!(position.iter().any(|u| matches!(u, UpdateKind::Region(_))));
    assert_eq!(p.mouse_pos_update, None);
    assert_eq!(
        &image.data()[(4 * 12 + 7) * 4..(4 * 12 + 7) * 4 + 4],
        &[254, 254, 254, 0]
    );
    p.update_mouse_pos(2, 2);
    let hidden = p
        .process_pointer_update(&mut image, PointerUpdateData::SetHidden)
        .unwrap();
    assert!(matches!(
        hidden.as_slice(),
        [UpdateKind::Region(_), UpdateKind::PointerHidden, UpdateKind::Region(_)]
    ));
    assert_eq!(p.mouse_pos_update, None);
    assert!(image.data().iter().all(|b| *b == 0));
    let default = p
        .process_pointer_update(&mut image, PointerUpdateData::SetDefault)
        .unwrap();
    assert!(matches!(default.as_slice(), [UpdateKind::PointerDefault]));
}
#[test]
fn pending_failed_native_move_is_not_consumed_and_bad_header_is_not_eager() {
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 12, 8);
    let mut p = processor(true);
    let xor = [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255];
    p.process_pointer_update(&mut image, PointerUpdateData::New(new_pointer(&xor, &[])))
        .unwrap();
    p.update_mouse_pos(u16::MAX, u16::MAX);
    let before = format!("{image:?}");
    assert!(p.process(&mut image, &[0], &mut WriteBuf::new()).is_err());
    assert_eq!(p.mouse_pos_update, Some((u16::MAX, u16::MAX)));
    assert_eq!(format!("{image:?}"), before);
    assert!(
        p.flush_native_mouse(&mut image, &mut Vec::new(), &mut CoverageCollector::default())
            .is_err()
    );
    assert_eq!(p.mouse_pos_update, Some((u16::MAX, u16::MAX)));
    assert_eq!(format!("{image:?}"), before);
}
