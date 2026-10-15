use super::*;
// 完整固定原版 image/compositor；入口桥只填初始背景，未改原版计算。
#[allow(dead_code, unreachable_pub)]
mod original {
    include!("../../../crates/frd-rdp-legacy-kernels/tests/cursor_original/source/image.rs");
    pub(super) fn seeded(format: PixelFormat, w: u16, h: u16, data: &[u8]) -> DecodedImage {
        let mut image = DecodedImage::new(format, w, h);
        image.data.copy_from_slice(data);
        image
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
fn pointer(w: u16, h: u16, hx: u16, hy: u16) -> Arc<DecodedPointer> {
    Arc::new(DecodedPointer {
        width: w,
        height: h,
        hotspot_x: hx,
        hotspot_y: hy,
        bitmap_data: (0..usize::from(w) * usize::from(h))
            .flat_map(|i| match i % 4 {
                0 => [16, 25, 36, 128],
                1 => [255, 255, 255, 0],
                2 => [9, 8, 7, 0],
                _ => [1, 2, 3, 255],
            })
            .collect(),
    })
}
fn clear_calls() {
    NATIVE_CURSOR_CALLS.with(|v| v.borrow_mut().clear());
}
fn calls() -> Vec<(&'static str, usize)> {
    NATIVE_CURSOR_CALLS.with(|v| v.borrow().clone())
}
#[test]
fn original_complete_image_parity_clips_hotspot_tails_background_and_default() {
    let k = NativeKernels::detect().unwrap();
    for format in FORMATS {
        for native in [false, true] {
            let kernels = if native { Some(&k) } else { None };
            let mut image = DecodedImage::new(format, 40, 15);
            let background: Vec<_> = (0..40 * 15 * 4).map(|i| (i * 13 + 17) as u8).collect();
            image.data.copy_from_slice(&background);
            let mut expected = original::seeded(format, 40, 15, &background);
            clear_calls();
            for (w, h, hx, hy) in [(1, 1, 0, 0), (3, 2, 1, 1), (7, 3, 1, 1), (17, 4, 2, 1), (33, 5, 1, 1)] {
                // 先回到完整可见位置，随后触碰四边/角、末行末列及 offscreen。
                assert_eq!(
                    image.move_pointer_with_kernels(4, 4, kernels).unwrap(),
                    expected.move_pointer(4, 4).unwrap()
                );
                assert_eq!(
                    image
                        .update_pointer_with_kernels(pointer(w, h, hx, hy), kernels)
                        .unwrap(),
                    expected.update_pointer(pointer(w, h, hx, hy)).unwrap()
                );
                assert_eq!(image.data(), expected.data());
                for (x, y) in [
                    (0, 0),
                    (0, 5),
                    (5, 0),
                    (38, 2),
                    (2, 13),
                    (38, 13),
                    (39, 14),
                    (50, 20),
                    (4, 4),
                ] {
                    assert_eq!(
                        image.move_pointer_with_kernels(x, y, kernels).unwrap(),
                        expected.move_pointer(x, y).unwrap(),
                        "{w}/{h}/{x}/{y}"
                    );
                    assert_eq!(image.data(), expected.data(), "{format:?}/{w}/{h}/{x}/{y}/{native}");
                }
                assert_eq!(
                    image.hide_pointer_with_kernels(kernels).unwrap(),
                    expected.hide_pointer().unwrap()
                );
                assert_eq!(image.data(), expected.data());
                assert_eq!(
                    image.show_pointer_with_kernels(kernels).unwrap(),
                    expected.show_pointer().unwrap()
                );
                assert_eq!(image.data(), expected.data());
            }
            assert_eq!(
                image.hide_pointer_with_kernels(kernels).unwrap(),
                expected.hide_pointer().unwrap()
            );
            assert_eq!(image.data(), background);
            let c = calls();
            assert_eq!(c.is_empty(), !native);
            if native {
                assert!(c.iter().any(|(name, n)| *name == "cursor_composite" && *n == 132));
                assert!(c.iter().any(|(name, _)| *name == "cursor_copy"));
            }
        }
    }
}
#[test]
fn native_malformed_geometry_bitmap_and_late_row_preserve_all_image_state() {
    let k = NativeKernels::detect().unwrap();
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 12, 10);
    image.data.iter_mut().enumerate().for_each(|(i, p)| *p = (i * 7) as u8);
    image.move_pointer_with_kernels(3, 3, Some(&k)).unwrap();
    image
        .update_pointer_with_kernels(pointer(3, 2, 1, 1), Some(&k))
        .unwrap();
    for bad in [
        DecodedPointer {
            width: 0,
            height: 1,
            hotspot_x: 0,
            hotspot_y: 0,
            bitmap_data: vec![],
        },
        DecodedPointer {
            width: 2,
            height: 2,
            hotspot_x: 0,
            hotspot_y: 0,
            bitmap_data: vec![1; 15],
        },
        DecodedPointer {
            width: 2,
            height: 2,
            hotspot_x: 20,
            hotspot_y: 20,
            bitmap_data: vec![1; 16],
        },
    ] {
        let before = format!("{image:?}");
        clear_calls();
        assert!(image.update_pointer_with_kernels(Arc::new(bad), Some(&k)).is_err());
        assert_eq!(format!("{image:?}"), before);
        assert!(calls().is_empty());
    }
    // 最后一行 source 不足不能先 erase cursor 或写前一行。
    let before = format!("{image:?}");
    clear_calls();
    assert!(
        image
            .apply_bitmap_native(
                &[3; 15],
                PixelRowSource::Raw32(Pixel32Format::RgbA),
                2,
                2,
                8,
                false,
                &InclusiveRectangle {
                    left: 2,
                    top: 2,
                    right: 3,
                    bottom: 3
                },
                None,
                &k
            )
            .is_err()
    );
    assert_eq!(format!("{image:?}"), before);
    assert!(calls().is_empty());
    // 原几何 draw_x+1 无定义的极值，必须在 position/image 写入前报告。
    image
        .update_pointer_with_kernels(pointer(3, 2, 0, 0), Some(&k))
        .unwrap();
    let before = format!("{image:?}");
    assert!(image.move_pointer_with_kernels(u16::MAX, u16::MAX, Some(&k)).is_err());
    assert_eq!(format!("{image:?}"), before);
}
#[test]
fn native_bitmap_and_tile_overlap_keep_background_and_damage_union() {
    let k = NativeKernels::detect().unwrap();
    for overlap in [false, true] {
        for tile in [false, true] {
            let mut image = DecodedImage::new(PixelFormat::RgbA32, 16, 12);
            image.data.fill(71);
            image.move_pointer_with_kernels(4, 3, Some(&k)).unwrap();
            image
                .update_pointer_with_kernels(pointer(7, 3, 1, 1), Some(&k))
                .unwrap();
            let update = if overlap {
                InclusiveRectangle {
                    left: 4,
                    top: 3,
                    right: 7,
                    bottom: 5,
                }
            } else {
                InclusiveRectangle {
                    left: 12,
                    top: 8,
                    right: 15,
                    bottom: 10,
                }
            };
            let mut baseline = vec![71u8; 16 * 12 * 4];
            for y in update.top..=update.bottom {
                for x in update.left..=update.right {
                    let offset = (usize::from(y) * 16 + usize::from(x)) * 4;
                    baseline[offset..offset + 4].copy_from_slice(&[12, 23, 34, 255]);
                }
            }
            clear_calls();
            let damage = if tile {
                let mut clips = Region::new();
                clips.union_rectangle(update.clone());
                image
                    .apply_tile_native(
                        &[12, 23, 34, 255].repeat(64 * 64),
                        PixelFormat::RgbA32,
                        &clips,
                        &update,
                        &k,
                    )
                    .unwrap()
                    .unwrap()
            } else {
                image
                    .apply_bitmap_native(
                        &[12, 23, 34, 255].repeat(12),
                        PixelRowSource::Raw32(Pixel32Format::RgbA),
                        4,
                        3,
                        16,
                        false,
                        &update,
                        None,
                        &k,
                    )
                    .unwrap()
                    .unwrap()
            };
            let c = calls();
            assert_eq!(c.is_empty(), !overlap);
            if overlap {
                assert!(c.iter().any(|(n, _)| *n == "cursor_copy"));
                assert!(c.iter().any(|(n, _)| *n == "cursor_composite"));
                assert_ne!(damage, update);
            } else {
                assert_eq!(damage, update);
            }
            image.hide_pointer_with_kernels(Some(&k)).unwrap();
            assert_eq!(image.data(), baseline, "overlap={overlap},tile={tile}");
        }
    }
}
#[cfg(not(debug_assertions))]
#[test]
fn explicit_native_wrapping_matches_fixed_original_release_for_legal_partial_alpha_rows() {
    let k = NativeKernels::detect().unwrap();
    for format in FORMATS {
        let mut image = DecodedImage::new(format, 40, 12);
        let background: Vec<_> = (0..40 * 12 * 4).map(|i| (i * 53 + 255) as u8).collect();
        image.data.copy_from_slice(&background);
        let mut old = original::seeded(format, 40, 12, &background);
        let cursor = Arc::new(DecodedPointer {
            width: 33,
            height: 3,
            hotspot_x: 1,
            hotspot_y: 1,
            bitmap_data: (0..99)
                .flat_map(|i| match i % 5 {
                    0 => [254, 254, 254, 1],
                    1 => [255, 255, 255, 0],
                    2 => [128, 254, 255, 2],
                    3 => [9, 8, 7, 0],
                    _ => [16, 25, 36, 128],
                })
                .collect(),
        });
        image.move_pointer_with_kernels(3, 3, Some(&k)).unwrap();
        old.move_pointer(3, 3).unwrap();
        assert_eq!(
            image.update_pointer_with_kernels(cursor.clone(), Some(&k)).unwrap(),
            old.update_pointer(cursor).unwrap()
        );
        assert_eq!(image.data(), old.data());
        for (x, y) in [(0, 0), (4, 4), (38, 9), (50, 20), (3, 3)] {
            assert_eq!(
                image.move_pointer_with_kernels(x, y, Some(&k)).unwrap(),
                old.move_pointer(x, y).unwrap()
            );
            assert_eq!(image.data(), old.data());
        }
        image.hide_pointer_with_kernels(Some(&k)).unwrap();
        old.hide_pointer().unwrap();
        assert_eq!(image.data(), background);
        assert_eq!(image.data(), old.data());
    }
}
#[test]
fn offscreen_smaller_replacement_keeps_old_background_restore_geometry() {
    let k = NativeKernels::detect().unwrap();
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 40, 15);
    let background = vec![91u8; 40 * 15 * 4];
    image.data.copy_from_slice(&background);
    let mut old = original::seeded(PixelFormat::RgbA32, 40, 15, &background);
    image.move_pointer_with_kernels(4, 4, Some(&k)).unwrap();
    old.move_pointer(4, 4).unwrap();
    image
        .update_pointer_with_kernels(pointer(17, 4, 0, 0), Some(&k))
        .unwrap();
    old.update_pointer(pointer(17, 4, 0, 0)).unwrap();
    image.move_pointer_with_kernels(50, 20, Some(&k)).unwrap();
    old.move_pointer(50, 20).unwrap();
    assert_eq!(
        image
            .update_pointer_with_kernels(pointer(1, 1, 0, 0), Some(&k))
            .unwrap(),
        old.update_pointer(pointer(1, 1, 0, 0)).unwrap()
    );
    assert_eq!(
        image.hide_pointer_with_kernels(Some(&k)).unwrap(),
        old.hide_pointer().unwrap()
    );
    assert_eq!(image.data(), old.data());
    assert_eq!(image.data(), background);
}
// 只在此 lib-test 二进制生效；失败条件为当前测试线程的精确一次尺寸，不影响其他线程。
#[cfg(not(frd_decoded_coverage_optional_allocator))]
struct CursorTestAllocator;
std::thread_local! { static FAIL_CURSOR_ALLOCATION:std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
pub(super) fn fail_allocation(size: usize) {
    FAIL_CURSOR_ALLOCATION.with(|v| v.set(size));
}
pub(super) fn allocation_failure_size() -> usize {
    FAIL_CURSOR_ALLOCATION.with(|v| v.get())
}
pub(super) fn allocation_is_denied(size: usize) -> bool {
    FAIL_CURSOR_ALLOCATION
        .try_with(|v| {
            if v.get() != 0 && v.get() == size {
                v.set(0);
                true
            } else {
                false
            }
        })
        .unwrap_or(false)
}
#[cfg(not(frd_decoded_coverage_optional_allocator))]
unsafe impl std::alloc::GlobalAlloc for CursorTestAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        if allocation_is_denied(layout.size()) {
            return core::ptr::null_mut();
        }
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        if allocation_is_denied(new_size) {
            return core::ptr::null_mut();
        }
        unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
    }
}
#[cfg(not(frd_decoded_coverage_optional_allocator))]
#[global_allocator]
static CURSOR_TEST_ALLOCATOR: CursorTestAllocator = CursorTestAllocator;
#[test]
fn native_backbuffer_reserve_failure_preserves_image_position_shape_and_background() {
    use core::error::Error as _;
    let k = NativeKernels::detect().unwrap();
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 40, 15);
    image.data.fill(71);
    image.move_pointer_with_kernels(3, 3, Some(&k)).unwrap();
    image
        .update_pointer_with_kernels(pointer(1, 1, 0, 0), Some(&k))
        .unwrap();
    let new = pointer(17, 4, 0, 0);
    let before = format!("{image:?}");
    clear_calls();
    fail_allocation(17 * 4 * 4);
    let err = image.update_pointer_with_kernels(new, Some(&k)).unwrap_err();
    assert_eq!(
        allocation_failure_size(),
        0,
        "实际 try_reserve 尺寸必须命中"
    );
    assert!(
        err.source()
            .unwrap()
            .downcast_ref::<std::collections::TryReserveError>()
            .is_some()
    );
    assert_eq!(format!("{image:?}"), before);
    assert!(calls().is_empty());
}
