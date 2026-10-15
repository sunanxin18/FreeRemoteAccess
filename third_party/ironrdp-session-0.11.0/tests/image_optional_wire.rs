use super::*;
use core::error::Error as _;
use ironrdp_core::{Encode as _, WriteCursor};
use ironrdp_pdu::fast_path::{
    EncryptionFlags, FastPathHeader, FastPathUpdatePdu, Fragmentation, UpdateCode,
};
use ironrdp_pdu::surface_commands::{ExtendedBitmapDataPdu, SurfaceBitsPdu};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
// 超限分配在System调用前终止；不触及大内存。
std::thread_local! {
    static ALLOCATION_CEILING: Cell<usize> = const {Cell::new(usize::MAX)};
    static FAIL_ALLOCATION: Cell<usize> = const {Cell::new(0)};
    static FIRST_ALLOCATION_LIMIT: Cell<usize> = const {Cell::new(0)};
}
struct ProbeAllocator;
unsafe impl GlobalAlloc for ProbeAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if FIRST_ALLOCATION_LIMIT
            .try_with(|v| {
                let max = v.replace(0);
                max != 0 && layout.size() > max
            })
            .unwrap_or(false)
        {
            unsafe { rejected_large_allocation() }
        }
        if ALLOCATION_CEILING
            .try_with(|v| layout.size() > v.get())
            .unwrap_or(false)
        {
            unsafe { rejected_large_allocation() }
        }
        let fail = FAIL_ALLOCATION
            .try_with(|v| {
                if v.get() != 0 && layout.size() == v.get() {
                    v.set(0);
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        #[cfg(frd_decoded_coverage_optional_allocator)]
        let fail = fail || crate::image::coverage_allocation_is_denied(layout.size());
        if fail {
            core::ptr::null_mut()
        } else {
            unsafe { System.alloc(layout) }
        }
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ALLOCATION_CEILING
            .try_with(|v| size > v.get())
            .unwrap_or(false)
        {
            unsafe { rejected_large_allocation() }
        }
        let fail = FAIL_ALLOCATION
            .try_with(|v| {
                if v.get() != 0 && size == v.get() {
                    v.set(0);
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        #[cfg(frd_decoded_coverage_optional_allocator)]
        let fail = fail || crate::image::coverage_allocation_is_denied(size);
        if fail {
            core::ptr::null_mut()
        } else {
            unsafe { System.realloc(p, layout, size) }
        }
    }
}
#[global_allocator]
static ALLOCATOR: ProbeAllocator = ProbeAllocator;
#[cfg(unix)]
unsafe fn rejected_large_allocation() -> ! {
    unsafe extern "C" {
        fn _exit(status: i32) -> !;
    }
    unsafe { _exit(73) }
}
#[cfg(not(unix))]
unsafe fn rejected_large_allocation() -> ! {
    std::process::abort()
}
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
fn frame(
    data: &[u8],
    codec: u8,
    sw: u16,
    sh: u16,
    set: bool,
    destination: ironrdp_pdu::geometry::ExclusiveRectangle,
) -> Vec<u8> {
    let bits = SurfaceBitsPdu {
        destination,
        extended_bitmap_data: ExtendedBitmapDataPdu {
            bpp: 32,
            codec_id: codec,
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
    let update = FastPathUpdatePdu {
        fragmentation: Fragmentation::Single,
        update_code: UpdateCode::SurfaceCommands,
        compression_flags: None,
        compression_type: None,
        data: &bytes,
    };
    let header = FastPathHeader::new(EncryptionFlags::empty(), update.size());
    let mut out = vec![0; header.size() + update.size()];
    let mut cursor = WriteCursor::new(&mut out);
    header.encode(&mut cursor).unwrap();
    update.encode(&mut cursor).unwrap();
    out
}
fn compress(data: &[u8]) -> Vec<u8> {
    let mut output = vec![0; zstd_safe::compress_bound(data.len())];
    let size = zstd_safe::compress(&mut output, data, 1).unwrap();
    output.truncate(size);
    output
}
#[test]
fn qoi_same_area_different_header_shape_is_rejected_before_publication() {
    let source: Vec<u8> = (0..18).collect();
    let wire = qoi::encode_to_vec(&source, 2, 3).unwrap();
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 8, 6);
    let initial = image.data().to_vec();
    let state = format!("{image:?}");
    let input = frame(
        &wire,
        10,
        3,
        2,
        false,
        ironrdp_pdu::geometry::ExclusiveRectangle {
            left: 1,
            top: 1,
            right: 2,
            bottom: 2,
        },
    );
    reset_trace();
    let result = processor(true).process(&mut image, &input, &mut WriteBuf::new());
    assert!(
        result.is_err(),
        "native accepts mismatched 2x3 header / 3x2 declared source"
    );
    assert_eq!(image.data(), initial);
    assert_eq!(format!("{image:?}"), state);
    check_no_publication(&image, &initial, &state);
    assert_eq!(trace(), [("qoi_header", 0)]);
}
#[test]
fn qoi_oversize_header_rejects_before_any_large_allocation() {
    let header =
        qoi::Header::try_new(8193, 8192, qoi::Channels::Rgba, qoi::ColorSpace::Srgb).unwrap();
    let mut wire = header.encode().to_vec();
    wire.extend([0, 0, 0, 0, 0, 0, 0, 1]);
    let input = frame(
        &wire,
        10,
        8193,
        8192,
        false,
        ironrdp_pdu::geometry::ExclusiveRectangle {
            left: 1,
            top: 1,
            right: 2,
            bottom: 2,
        },
    );
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 8, 6);
    let initial = image.data().to_vec();
    let before = format!("{image:?}");
    reset_trace();
    ALLOCATION_CEILING.with(|v| v.set(ironrdp_graphics::rle::MAX_NATIVE_BITMAP_BYTES));
    let result = processor(true).process(&mut image, &input, &mut WriteBuf::new());
    ALLOCATION_CEILING.with(|v| v.set(usize::MAX));
    let e = result.unwrap_err();
    assert_eq!(
        e.source().unwrap().downcast_ref::<KernelError>(),
        Some(&KernelError::InvalidDimensions)
    );
    assert_eq!(image.data(), initial);
    assert_eq!(format!("{image:?}"), before);
    check_no_publication(&image, &initial, &before);
    assert_eq!(trace(), [("qoi_header", 0)]);
}
fn noisy_qoi() -> Vec<u8> {
    let mut state = 0x41c64e6du32;
    let source: Vec<u8> = (0..64 * 128 * 3)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    qoi::encode_to_vec(&source, 64, 128).unwrap()
}
#[test]
fn qoiz_first_staging_is_bounded_independently_of_compressed_length() {
    let wire = noisy_qoi();
    let compressed = compress(&wire);
    assert!(compressed.len() > 16384);
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 80, 140);
    let mut p = processor(true);
    let bits = SurfaceBitsPdu {
        destination: ironrdp_pdu::geometry::ExclusiveRectangle {
            left: 1,
            top: 1,
            right: 65,
            bottom: 129,
        },
        extended_bitmap_data: ExtendedBitmapDataPdu {
            bpp: 32,
            codec_id: 11,
            width: 64,
            height: 128,
            header: None,
            data: &compressed,
        },
    };
    // 跳过独立的外层fragmentation输入复制，执行实际Surface dispatch/staging。
    let commands = vec![SurfaceCommand::SetSurfaceBits(bits)];
    reset_trace();
    FIRST_ALLOCATION_LIMIT.with(|v| v.set(16384));
    let result = p.process_surface_commands(&mut image, &mut WriteBuf::new(), commands);
    FIRST_ALLOCATION_LIMIT.with(|v| v.set(0));
    assert!(result.is_ok());
    assert_eq!(trace().first(), Some(&("qoiz_allocation", 16384)));
}
fn reset_trace() {
    OPTIONAL_STAGING_CALLS.with(|v| {
        let mut v = v.borrow_mut();
        v.clear();
        v.reserve(64);
    });
    crate::image::optional_native_calls_clear();
}
fn trace() -> Vec<(&'static str, usize)> {
    OPTIONAL_STAGING_CALLS.with(|v| v.borrow().clone())
}
fn cursor_image() -> DecodedImage {
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 8, 6);
    image.move_pointer(1, 1).unwrap();
    image
        .update_pointer(Arc::new(DecodedPointer {
            width: 2,
            height: 1,
            hotspot_x: 0,
            hotspot_y: 0,
            bitmap_data: vec![5, 7, 9, 128, 11, 13, 17, 255],
        }))
        .unwrap();
    image
}
fn check_no_publication(image: &DecodedImage, initial: &[u8], state: &str) {
    assert_eq!(image.data(), initial);
    assert_eq!(format!("{image:?}"), state);
    assert_eq!(crate::image::optional_native_calls_count(), 0);
    assert!(!trace().iter().any(|(name, _)| *name == "qoi_apply"));
}
#[test]
fn checked_qoi_and_qoiz_resource_math_without_huge_allocation() {
    let cap = ironrdp_graphics::rle::MAX_NATIVE_BITMAP_BYTES;
    assert_eq!(checked_qoi_pixel_size(8192, 8192, 4).unwrap(), cap);
    for (w, h, c) in [(8193, 8192, 4), (usize::MAX, 2, 4), (1, 1, 2), (0, 8, 4)] {
        let e = checked_qoi_pixel_size(w, h, c).unwrap_err();
        assert_eq!(
            e.source().unwrap().downcast_ref::<KernelError>(),
            Some(&KernelError::InvalidDimensions)
        );
    }
    for size in [0, cap + 1, usize::MAX, isize::MAX as usize + 1] {
        assert!(checked_native_staging_size(size, usize::MAX).is_err());
    }
    assert_eq!(
        checked_qoi_pixel_size(3840, 2160, 4).unwrap(),
        3840 * 2160 * 4
    );
    assert_eq!(
        checked_qoi_pixel_size(7680, 4320, 4).unwrap(),
        7680 * 4320 * 4
    );
    assert!(checked_native_staging_size(1025, 1024).is_err());
}
#[test]
fn qoi_allocation_failure_keeps_typed_source_and_visible_cursor() {
    let wire = qoi::encode_to_vec(
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
        2,
        2,
    )
    .unwrap();
    let mut image = cursor_image();
    let initial = image.data().to_vec();
    let state = format!("{image:?}");
    let k = NativeKernels::detect().unwrap();
    reset_trace();
    FAIL_ALLOCATION.with(|v| v.set(16));
    let result = qoi_apply_native(
        &mut image,
        InclusiveRectangle {
            left: 1,
            top: 1,
            right: 2,
            bottom: 2,
        },
        &wire,
        2,
        2,
        &k,
    );
    FAIL_ALLOCATION.with(|v| v.set(0));
    assert!(
        result
            .unwrap_err()
            .source()
            .unwrap()
            .downcast_ref::<std::collections::TryReserveError>()
            .is_some()
    );
    assert_eq!(trace(), [("qoi_header", 0), ("qoi_allocation", 16)]);
    check_no_publication(&image, &initial, &state);
}
#[test]
fn qoi_header_and_decode_errors_are_typed_and_do_not_publish() {
    for wire in [vec![0; 22], {
        let mut v = qoi::Header::try_new(2, 2, qoi::Channels::Rgba, qoi::ColorSpace::Srgb)
            .unwrap()
            .encode()
            .to_vec();
        v.extend([0, 0, 0, 0, 0, 0, 0, 1]);
        v
    }] {
        let input = frame(
            &wire,
            10,
            2,
            2,
            true,
            ironrdp_pdu::geometry::ExclusiveRectangle {
                left: 1,
                top: 1,
                right: 3,
                bottom: 3,
            },
        );
        let mut image = cursor_image();
        let initial = image.data().to_vec();
        let state = format!("{image:?}");
        reset_trace();
        let e = processor(true)
            .process(&mut image, &input, &mut WriteBuf::new())
            .unwrap_err();
        assert!(e.source().unwrap().downcast_ref::<qoi::Error>().is_some());
        check_no_publication(&image, &initial, &state);
    }
}
#[test]
fn qoiz_initial_and_growth_reserve_failures_leave_image_cursor_and_pending_mouse() {
    let wire = noisy_qoi();
    let compressed = compress(&wire);
    let input = frame(
        &compressed,
        11,
        64,
        128,
        false,
        ironrdp_pdu::geometry::ExclusiveRectangle {
            left: 1,
            top: 1,
            right: 2,
            bottom: 2,
        },
    );
    for fail in [16384, 32768] {
        let mut image = cursor_image();
        let initial = image.data().to_vec();
        let state = format!("{image:?}");
        let mut p = processor(true);
        p.update_mouse_pos(4, 3);
        reset_trace();
        FAIL_ALLOCATION.with(|v| v.set(fail));
        let result = p.process(&mut image, &input, &mut WriteBuf::new());
        FAIL_ALLOCATION.with(|v| v.set(0));
        let e = result.unwrap_err();
        assert!(
            e.source()
                .unwrap()
                .downcast_ref::<std::collections::TryReserveError>()
                .is_some()
        );
        assert_eq!(p.mouse_pos_update, Some((4, 3)));
        check_no_publication(&image, &initial, &state);
        assert_eq!(trace().last(), Some(&("qoiz_allocation", fail)));
        assert!(!trace().iter().any(|(name, _)| *name == "qoi_header"));
    }
}
#[test]
fn qoiz_real_growth_budget_is_checked_before_reserve_and_decode() {
    let compressed = compress(&noisy_qoi());
    let mut context = zstd_safe::DCtx::default();
    reset_trace();
    // 同一shipping staging helper的更小测试budget；调用者不能放大统一256MiB门。
    ALLOCATION_CEILING.with(|v| v.set(1024));
    let result = decompress_qoiz_native(&compressed, &mut context, 1024);
    ALLOCATION_CEILING.with(|v| v.set(usize::MAX));
    assert_eq!(
        result
            .unwrap_err()
            .source()
            .unwrap()
            .downcast_ref::<KernelError>(),
        Some(&KernelError::InvalidDimensions)
    );
    assert_eq!(trace(), [("qoiz_allocation", 1024)]);
    assert_eq!(crate::image::optional_native_calls_count(), 0);
    reset_trace();
    assert!(decompress_qoiz_native(&compressed, &mut context, 0).is_err());
    assert!(trace().is_empty());
}
#[test]
fn qoiz_header_shape_and_oversize_reject_before_qoi_decode_and_publication() {
    let mismatch = qoi::encode_to_vec((0u8..18).collect::<Vec<_>>(), 2, 3).unwrap();
    let mut huge = qoi::Header::try_new(8193, 8192, qoi::Channels::Rgba, qoi::ColorSpace::Srgb)
        .unwrap()
        .encode()
        .to_vec();
    huge.extend([0, 0, 0, 0, 0, 0, 0, 1]);
    for (wire, w, h) in [(mismatch, 3, 2), (huge, 8193, 8192)] {
        let compressed = compress(&wire);
        let input = frame(
            &compressed,
            11,
            w,
            h,
            false,
            ironrdp_pdu::geometry::ExclusiveRectangle {
                left: 1,
                top: 1,
                right: 2,
                bottom: 2,
            },
        );
        let mut image = cursor_image();
        let initial = image.data().to_vec();
        let state = format!("{image:?}");
        reset_trace();
        ALLOCATION_CEILING.with(|v| v.set(ironrdp_graphics::rle::MAX_NATIVE_BITMAP_BYTES));
        let result = processor(true).process(&mut image, &input, &mut WriteBuf::new());
        ALLOCATION_CEILING.with(|v| v.set(usize::MAX));
        assert_eq!(
            result
                .unwrap_err()
                .source()
                .unwrap()
                .downcast_ref::<KernelError>(),
            Some(&KernelError::InvalidDimensions)
        );
        check_no_publication(&image, &initial, &state);
        assert!(
            !trace()
                .iter()
                .any(|(name, _)| *name == "qoi_decode" || *name == "qoi_allocation")
        );
    }
}
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
#[test]
fn optional_wire_rgb_rgba_all_formats_set_stream_default_parity_and_canaries() {
    for channels in [3, 4] {
        let input: Vec<u8> = (0..6 * channels).map(|i| (i * 37 + 11) as u8).collect();
        let encoded = qoi::encode_to_vec(&input, 3, 2).unwrap();
        for codec in [10, 11] {
            let wire = if codec == 11 {
                compress(&encoded)
            } else {
                encoded.clone()
            };
            for format in FORMATS {
                for set in [false, true] {
                    let packet = frame(
                        &wire,
                        codec,
                        3,
                        2,
                        set,
                        ironrdp_pdu::geometry::ExclusiveRectangle {
                            left: 1,
                            top: 1,
                            right: 4,
                            bottom: 3,
                        },
                    );
                    let mut old = DecodedImage::new(format, 8, 6);
                    let mut new = DecodedImage::new(format, 8, 6);
                    processor(false)
                        .process(&mut old, &packet, &mut WriteBuf::new())
                        .unwrap();
                    reset_trace();
                    let updates = processor(true)
                        .process(&mut new, &packet, &mut WriteBuf::new())
                        .unwrap();
                    assert_eq!(new.data(), old.data());
                    assert_eq!(format!("{new:?}"), format!("{old:?}"));
                    assert!(updates.iter().any(|u| matches!(u, UpdateKind::Region(_))));
                    assert_eq!(crate::image::optional_native_calls_count(), 4);
                    assert_eq!(
                        trace()
                            .iter()
                            .filter(|(name, _)| *name == "qoi_decode")
                            .count(),
                        1
                    );
                    // Native Stream crop取自同一stock helper输出的完整source行，独立于native重排。
                    let crop = frame(
                        &wire,
                        codec,
                        3,
                        2,
                        false,
                        ironrdp_pdu::geometry::ExclusiveRectangle {
                            left: 1,
                            top: 1,
                            right: 3,
                            bottom: 2,
                        },
                    );
                    let mut cropped = DecodedImage::new(format, 8, 6);
                    reset_trace();
                    processor(true)
                        .process(&mut cropped, &crop, &mut WriteBuf::new())
                        .unwrap();
                    assert_eq!(&cropped.data()[36..44], &old.data()[36..44]);
                    assert!(
                        cropped.data()[..36]
                            .iter()
                            .chain(cropped.data()[44..].iter())
                            .all(|b| *b == 0)
                    );
                    assert_eq!(crate::image::optional_native_calls_count(), 3);
                }
            }
        }
    }
}
#[test]
fn optional_wire_rgba_matches_frozen_original_alpha_anchor() {
    let expected = include_bytes!(
        "../../../crates/frd-rdp-legacy-kernels/tests/pixel_original/qoi-original.bin"
    );
    let input: Vec<u8> = [0u8, 1, 127, 128, 254, 255]
        .into_iter()
        .flat_map(|a| [0x11, 0x22, 0x33, a])
        .collect();
    let encoded = qoi::encode_to_vec(&input, 6, 1).unwrap();
    for codec in [10, 11] {
        let wire = if codec == 11 {
            compress(&encoded)
        } else {
            encoded.clone()
        };
        let packet = frame(
            &wire,
            codec,
            6,
            1,
            true,
            ironrdp_pdu::geometry::ExclusiveRectangle {
                left: 1,
                top: 1,
                right: 7,
                bottom: 2,
            },
        );
        for (i, format) in FORMATS.into_iter().enumerate() {
            let mut image = DecodedImage::new(format, 9, 3);
            reset_trace();
            processor(true)
                .process(&mut image, &packet, &mut WriteBuf::new())
                .unwrap();
            assert_eq!(&image.data()[40..64], &expected[i * 24..i * 24 + 24]);
            assert_eq!(&image.data()[..40], &[0; 40]);
            assert_eq!(&image.data()[64..], &[0; 44]);
        }
    }
}
#[test]
fn qoi_destination_preflight_precedes_allocation() {
    let wire = qoi::encode_to_vec([1u8; 16], 2, 2).unwrap();
    let mut image = cursor_image();
    let initial = image.data().to_vec();
    let state = format!("{image:?}");
    let k = NativeKernels::detect().unwrap();
    reset_trace();
    let e = qoi_apply_native(
        &mut image,
        InclusiveRectangle {
            left: 1,
            top: 1,
            right: 3,
            bottom: 2,
        },
        &wire,
        2,
        2,
        &k,
    )
    .unwrap_err();
    assert_eq!(
        e.source().unwrap().downcast_ref::<KernelError>(),
        Some(&KernelError::InvalidDimensions)
    );
    assert_eq!(trace(), [("qoi_header", 0)]);
    check_no_publication(&image, &initial, &state);
}
#[test]
fn malformed_qoiz_does_not_decode_apply_or_consume_pending_move() {
    let input = frame(
        &[0; 22],
        11,
        2,
        2,
        true,
        ironrdp_pdu::geometry::ExclusiveRectangle {
            left: 1,
            top: 1,
            right: 3,
            bottom: 3,
        },
    );
    let mut image = cursor_image();
    let initial = image.data().to_vec();
    let state = format!("{image:?}");
    let mut p = processor(true);
    p.update_mouse_pos(4, 3);
    reset_trace();
    assert!(p.process(&mut image, &input, &mut WriteBuf::new()).is_err());
    assert_eq!(p.mouse_pos_update, Some((4, 3)));
    check_no_publication(&image, &initial, &state);
    assert!(!trace().iter().any(|(name, _)| *name == "qoi_header"));
}
#[test]
fn old_default_optional_wire_regression_digest() {
    let mut digest = 0xcbf29ce484222325u64;
    let mut cases = 0;
    for channels in [3, 4] {
        let input: Vec<u8> = (0..6 * channels).map(|i| (i * 37 + 11) as u8).collect();
        let encoded = qoi::encode_to_vec(&input, 3, 2).unwrap();
        for codec in [10, 11] {
            let wire = if codec == 11 {
                compress(&encoded)
            } else {
                encoded.clone()
            };
            for format in FORMATS {
                for set in [false, true] {
                    let input = frame(
                        &wire,
                        codec,
                        3,
                        2,
                        set,
                        ironrdp_pdu::geometry::ExclusiveRectangle {
                            left: 1,
                            top: 1,
                            right: 4,
                            bottom: 3,
                        },
                    );
                    let mut image = DecodedImage::new(format, 8, 6);
                    let updates = processor(false)
                        .process(&mut image, &input, &mut WriteBuf::new())
                        .unwrap();
                    for byte in image
                        .data()
                        .iter()
                        .copied()
                        .chain(format!("{image:?}{updates:?}").bytes())
                    {
                        digest = (digest ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 64);
    println!("DEFAULT_DIGEST {cases} {digest:016x}");
}
