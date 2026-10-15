use super::*;
use ironrdp_pdu::pointer::Point16;
// 固定完整原版源码，仅作为测试参考；不派生自新 row leaf。
#[allow(dead_code, unreachable_pub)]
#[path = "../../../crates/frd-rdp-legacy-kernels/tests/cursor_original/source/pointer.rs"]
mod original;

#[test]
fn native_all_wrappers_match_fixed_original_formats_modes_padding_and_tails() {
    let k = NativeKernels::detect().unwrap();
    for bpp in [1u16, 16, 24, 32] {
        for width in [1u16, 2, 3, 4, 7, 8, 9, 15, 16, 17, 31, 32, 33] {
            let stride = (usize::from(width) * usize::from(bpp)).div_ceil(16) * 2;
            let astride = usize::from(width).div_ceil(16) * 2;
            let xor: Vec<_> = (0..stride * 3).map(|i| (i * 71 + i / 3 * 11 + 17) as u8).collect();
            let and: Vec<_> = (0..astride * 3).map(|i| (i * 93 + 177) as u8).collect();
            for and in [&[][..], &and[..]] {
                for software in [true, false] {
                    let target = if software {
                        PointerBitmapTarget::Software
                    } else {
                        PointerBitmapTarget::Accelerated
                    };
                    let ot = if software {
                        original::PointerBitmapTarget::Software
                    } else {
                        original::PointerBitmapTarget::Accelerated
                    };
                    let large = LargePointerAttribute {
                        width,
                        height: 3,
                        xor_bpp: bpp,
                        hot_spot: Point16 { x: 1, y: 2 },
                        cache_index: 7,
                        xor_mask: &xor,
                        and_mask: and,
                    };
                    let expected = original::DecodedPointer::decode_large_pointer_attribute(&large, ot).unwrap();
                    let decoded = DecodedPointer::decode_large_pointer_attribute_native(&large, target, &k).unwrap();
                    assert_eq!(decoded.bitmap_data, expected.bitmap_data, "{bpp}/{width}/{software}");
                    assert_eq!(
                        (decoded.width, decoded.height, decoded.hotspot_x, decoded.hotspot_y),
                        (width, 3, 1, 2)
                    );
                    let color = ColorPointerAttribute {
                        width,
                        height: 3,
                        hot_spot: large.hot_spot,
                        cache_index: 7,
                        xor_mask: &xor,
                        and_mask: and,
                    };
                    let new = PointerAttribute {
                        xor_bpp: bpp,
                        color_pointer: color,
                    };
                    assert_eq!(
                        DecodedPointer::decode_pointer_attribute_native(&new, target, &k)
                            .unwrap()
                            .bitmap_data,
                        expected.bitmap_data
                    );
                    assert_eq!(
                        DecodedPointer::decode_pointer_attribute(&new, target)
                            .unwrap()
                            .bitmap_data,
                        expected.bitmap_data
                    );
                    if bpp == 24 {
                        assert_eq!(
                            DecodedPointer::decode_color_pointer_attribute_native(&color, target, &k)
                                .unwrap()
                                .bitmap_data,
                            expected.bitmap_data
                        );
                    }
                }
            }
        }
    }
}
#[test]
fn native_empty_precedes_bpp_and_masks_errors_and_exact_mask_priority() {
    let k = NativeKernels::detect().unwrap();
    let mut p = LargePointerAttribute {
        width: 0,
        height: 7,
        xor_bpp: 8,
        hot_spot: Point16 { x: 9, y: 9 },
        cache_index: 7,
        xor_mask: &[1],
        and_mask: &[1],
    };
    for (w, h) in [(0, 7), (7, 0)] {
        p.width = w;
        p.height = h;
        let v = DecodedPointer::decode_large_pointer_attribute_native(&p, PointerBitmapTarget::Software, &k).unwrap();
        assert_eq!(
            (v.width, v.height, v.hotspot_x, v.hotspot_y, v.bitmap_data.len()),
            (0, 0, 0, 0, 0)
        );
    }
    p.width = 1;
    p.height = 1;
    for bpp in [8, 15, 0, 65535] {
        p.xor_bpp = bpp;
        assert!(matches!(
            DecodedPointer::decode_large_pointer_attribute_native(&p, PointerBitmapTarget::Software, &k),
            Err(NativePointerError::Pointer(PointerError::NotSupportedBpp { .. }))
        ));
    }
    p.xor_bpp = 32;
    assert!(matches!(
        DecodedPointer::decode_large_pointer_attribute_native(&p, PointerBitmapTarget::Software, &k),
        Err(NativePointerError::Pointer(PointerError::InvalidXorMaskSize { .. }))
    ));
    p.xor_mask = &[0; 4];
    assert!(matches!(
        DecodedPointer::decode_large_pointer_attribute_native(&p, PointerBitmapTarget::Software, &k),
        Err(NativePointerError::Pointer(PointerError::InvalidAndMaskSize { .. }))
    ));
    for mask in [&[0; 1][..], &[0; 3][..]] {
        p.and_mask = mask;
        assert!(DecodedPointer::decode_large_pointer_attribute_native(&p, PointerBitmapTarget::Software, &k).is_err());
    }
}
#[test]
fn fixed_original_partial_alpha_black_white_and_invert_checker_rows() {
    let k = NativeKernels::detect().unwrap();
    let mut xor = Vec::new();
    for row in 0..3 {
        for col in 0..33 {
            let a = [0, 1, 2, 127, 128, 254, 255][(row + col) % 7];
            xor.extend_from_slice(&match col % 4 {
                0 => [255, 255, 255, a],
                1 => [0, 0, 0, a],
                2 => [255, 255, 255, 255],
                _ => [0, 0, 0, 255],
            });
        }
    }
    let p = LargePointerAttribute {
        width: 33,
        height: 3,
        xor_bpp: 32,
        hot_spot: Point16 { x: 0, y: 0 },
        cache_index: 1,
        xor_mask: &xor,
        and_mask: &[255; 18],
    };
    for (t, o) in [
        (PointerBitmapTarget::Software, original::PointerBitmapTarget::Software),
        (
            PointerBitmapTarget::Accelerated,
            original::PointerBitmapTarget::Accelerated,
        ),
    ] {
        assert_eq!(
            DecodedPointer::decode_large_pointer_attribute_native(&p, t, &k)
                .unwrap()
                .bitmap_data,
            original::DecodedPointer::decode_large_pointer_attribute(&p, o)
                .unwrap()
                .bitmap_data
        );
    }
}
#[test]
fn bottom_up_flips_both_masks_discriminating_black_white_rows() {
    let k = NativeKernels::detect().unwrap();
    for bpp in [1u16, 16, 24, 32] {
        let mut xor = Vec::new();
        let mut and = Vec::new();
        for row in 0..3 {
            let start = xor.len();
            if bpp == 1 {
                xor.extend([0x55, 0x80]);
            } else {
                for col in 0..9 {
                    let c = if (col + row) % 2 == 0 { 0 } else { 255 };
                    match bpp {
                        16 => xor.extend([c, c]),
                        24 => xor.extend([c, c, c]),
                        32 => xor.extend([c, c, c, 255]),
                        _ => unreachable!(),
                    }
                }
                if (xor.len() - start) % 2 != 0 {
                    xor.push(0xa9);
                }
            }
            and.extend([
                match row {
                    0 => 0xff,
                    1 => 0,
                    2 => 0x55,
                    _ => unreachable!(),
                },
                0x80,
            ]);
        }
        let p = LargePointerAttribute {
            width: 9,
            height: 3,
            xor_bpp: bpp,
            hot_spot: Point16 { x: 0, y: 0 },
            cache_index: 1,
            xor_mask: &xor,
            and_mask: &and,
        };
        for (t, o) in [
            (PointerBitmapTarget::Software, original::PointerBitmapTarget::Software),
            (
                PointerBitmapTarget::Accelerated,
                original::PointerBitmapTarget::Accelerated,
            ),
        ] {
            assert_eq!(
                DecodedPointer::decode_large_pointer_attribute_native(&p, t, &k)
                    .unwrap()
                    .bitmap_data,
                original::DecodedPointer::decode_large_pointer_attribute(&p, o)
                    .unwrap()
                    .bitmap_data,
                "{bpp}"
            );
        }
    }
}
#[test]
fn finite_native_allocation_policy_and_checked_strides_without_large_allocations() {
    assert_eq!(checked_bitmap_size(8192, 8192).unwrap(), (32768, 256 * 1024 * 1024));
    for (w, h) in [(8192, 8193), (usize::MAX, 1), (1, usize::MAX), (65535, 65535)] {
        assert!(matches!(checked_bitmap_size(w, h), Err(KernelError::InvalidDimensions)));
    }
    assert!(Stride::checked_from_bits(usize::MAX).is_err());
    for bits in [1usize, 7, 8, 9, 15, 16, 17, 33, 65535 * 32] {
        let old = Stride::from_bits(bits);
        let new = Stride::checked_from_bits(bits).unwrap();
        assert_eq!(
            (new.length, new.data_bytes, new.padding),
            (old.length, old.data_bytes, old.padding)
        );
    }
}
