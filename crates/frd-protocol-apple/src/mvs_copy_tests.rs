// 独立字面答案：每个像素的三个通道均不同，检测误用 memmove/整块快照。
#[test]
fn native_copy_forward_literal_right_left_same() {
    let original = [
        11, 21, 31, 42, 52, 62, 73, 83, 93, 104, 114, 124, 135, 145, 155,
    ];
    let mut right = original;
    copy_rgb_forward_span(&mut right, 0, 3, 4);
    assert_eq!(
        right,
        [11, 21, 31, 11, 21, 31, 11, 21, 31, 11, 21, 31, 11, 21, 31]
    );
    let mut left = original;
    copy_rgb_forward_span(&mut left, 3, 0, 4);
    assert_eq!(
        left,
        [42, 52, 62, 73, 83, 93, 104, 114, 124, 135, 145, 155, 135, 145, 155]
    );
    copy_rgb_forward_span(&mut left, 3, 3, 3);
    assert_eq!(
        left,
        [42, 52, 62, 73, 83, 93, 104, 114, 124, 135, 145, 155, 135, 145, 155]
    );
    copy_rgb_forward_span(&mut left, 15, 15, 0);
}

#[test]
fn native_copy_exact_leaf_literal_and_canaries() {
    for pixels in 1..=8 {
        for src_align in 0..16 {
            for dst_align in 0..16 {
                let input: Vec<u8> = (0..64).map(|i| (i * 13 + 7) as u8).collect();
                let mut output = [0xa5; 64];
                crate::mvs_pixels::copy_rgb_nonoverlap(
                    &input[src_align..src_align + pixels * 3],
                    &mut output[dst_align..dst_align + pixels * 3],
                );
                for i in 0..64 {
                    let expected = if (dst_align..dst_align + pixels * 3).contains(&i) {
                        ((i - dst_align + src_align) * 13 + 7) as u8
                    } else {
                        0xa5
                    };
                    assert_eq!(
                        output[i], expected,
                        "pixels={pixels} align={src_align}/{dst_align} at={i}"
                    );
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn scalar_surface_copy(
    surface_rgb: &[u8],
    surface_width: usize,
    surface_height: usize,
    rect_rgb: &mut [u8],
    rect_width: usize,
    rect_height: usize,
    source_x: usize,
    source_y: usize,
    tile_x: usize,
    tile_y: usize,
) -> Result<()> {
    let copy_width = TILE_EDGE.min(rect_width - tile_x);
    let copy_height = TILE_EDGE.min(rect_height - tile_y);
    if source_x
        .checked_add(copy_width)
        .is_none_or(|right| right > surface_width)
        || source_y
            .checked_add(copy_height)
            .is_none_or(|bottom| bottom > surface_height)
    {
        bail!("MVS type-0 copy source 超出 committed surface");
    }
    for dy in 0..copy_height {
        for dx in 0..copy_width {
            let source = pixel_offset(surface_width, source_x + dx, source_y + dy);
            let color = [
                surface_rgb[source],
                surface_rgb[source + 1],
                surface_rgb[source + 2],
            ];
            write_pixel(rect_rgb, rect_width, tile_x + dx, tile_y + dy, color);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn scalar_overlay_copy(
    committed_rgb: &[u8],
    surface_width: usize,
    surface_height: usize,
    rect_rgb: &mut [u8],
    rect_x: usize,
    rect_y: usize,
    rect_width: usize,
    rect_height: usize,
    source_x: usize,
    source_y: usize,
    destination_x: usize,
    destination_y: usize,
) -> Result<()> {
    let copy_width = TILE_EDGE.min(rect_width - destination_x);
    let copy_height = TILE_EDGE.min(rect_height - destination_y);
    if source_x
        .checked_add(copy_width)
        .is_none_or(|right| right > surface_width)
        || source_y
            .checked_add(copy_height)
            .is_none_or(|bottom| bottom > surface_height)
    {
        bail!("MVS type-0 overlay copy source 超出 committed surface");
    }
    for dy in 0..copy_height {
        for dx in 0..copy_width {
            let source_global_x = source_x + dx;
            let source_global_y = source_y + dy;
            let color = if source_global_x >= rect_x
                && source_global_x < rect_x + rect_width
                && source_global_y >= rect_y
                && source_global_y < rect_y + rect_height
            {
                let source = pixel_offset(
                    rect_width,
                    source_global_x - rect_x,
                    source_global_y - rect_y,
                );
                [rect_rgb[source], rect_rgb[source + 1], rect_rgb[source + 2]]
            } else {
                let source = pixel_offset(surface_width, source_global_x, source_global_y);
                [
                    committed_rgb[source],
                    committed_rgb[source + 1],
                    committed_rgb[source + 2],
                ]
            };
            write_pixel(
                rect_rgb,
                rect_width,
                destination_x + dx,
                destination_y + dy,
                color,
            );
        }
    }
    Ok(())
}

#[test]
fn native_copy_downward_rows_use_live_predecessor_not_tile_snapshot() {
    let committed = [0; 18];
    let mut rgb = [
        11, 21, 31, 12, 22, 32, 41, 51, 61, 42, 52, 62, 71, 81, 91, 72, 82, 92,
    ];
    copy_overlay_or_committed_tile_to_rect(&committed, 2, 3, &mut rgb, 0, 0, 2, 3, 0, 0, 0, 1)
        .unwrap();
    assert_eq!(
        rgb,
        [11, 21, 31, 12, 22, 32, 11, 21, 31, 12, 22, 32, 11, 21, 31, 12, 22, 32]
    );
}

#[test]
fn native_copy_all_small_geometry_matches_original_live_loops() {
    let mut cases = 0;
    for width in 1..=8 {
        for height in 1..=8 {
            let sw = width + 4;
            let sh = height + 4;
            let committed: Vec<u8> = (0..sw * sh * 3).map(|i| (i * 47 + 19) as u8).collect();
            for (rx, ry) in [(0, 0), (1, 2), (3, 1)] {
                for dx in 0..width {
                    for dy in 0..height {
                        let cw = width - dx;
                        let ch = height - dy;
                        for sx in 0..=sw - cw {
                            for sy in 0..=sh - ch {
                                let initial: Vec<u8> = (0..width * height * 3)
                                    .map(|i| (i * 29 + 91) as u8)
                                    .collect();
                                let mut expected = initial.clone();
                                let mut actual = initial.clone();
                                scalar_overlay_copy(
                                    &committed,
                                    sw,
                                    sh,
                                    &mut expected,
                                    rx,
                                    ry,
                                    width,
                                    height,
                                    sx,
                                    sy,
                                    dx,
                                    dy,
                                )
                                .unwrap();
                                copy_overlay_or_committed_tile_to_rect(
                                    &committed,
                                    sw,
                                    sh,
                                    &mut actual,
                                    rx,
                                    ry,
                                    width,
                                    height,
                                    sx,
                                    sy,
                                    dx,
                                    dy,
                                )
                                .unwrap();
                                assert_eq!(actual, expected, "overlay {width}x{height} origin={rx}/{ry} source={sx}/{sy} dest={dx}/{dy}");
                                scalar_surface_copy(
                                    &committed,
                                    sw,
                                    sh,
                                    &mut expected,
                                    width,
                                    height,
                                    sx,
                                    sy,
                                    dx,
                                    dy,
                                )
                                .unwrap();
                                copy_surface_tile_to_rect(
                                    &committed,
                                    sw,
                                    sh,
                                    &mut actual,
                                    width,
                                    height,
                                    sx,
                                    sy,
                                    dx,
                                    dy,
                                )
                                .unwrap();
                                assert_eq!(
                                    actual, expected,
                                    "committed {width}x{height} source={sx}/{sy} dest={dx}/{dy}"
                                );
                                cases += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    eprintln!("native_copy geometry_cases={cases} helpers_each=2 widths_heights=1..8 origins=3 all_valid_source_and_destination_offsets=true");
}

#[test]
fn native_copy_rows_strides_alignments_and_canaries() {
    let mut cases = 0;
    for width in 1..=8 {
        for height in 1..=8 {
            for source_alignment in 0..16 {
                for destination_alignment in 0..16 {
                    let sw = width + 6;
                    let sh = height + 5;
                    let src_len = sw * sh * 3;
                    let dst_len = (width + 2) * (height + 1) * 3;
                    let mut src = vec![0x63; source_alignment + src_len + 16];
                    for (i, value) in src[source_alignment..source_alignment + src_len]
                        .iter_mut()
                        .enumerate()
                    {
                        *value = (i * 37 + 49) as u8;
                    }
                    let unchanged_src = src.clone();
                    let mut actual = vec![0xa5; destination_alignment + dst_len + 16];
                    let mut expected = actual.clone();
                    for (overlay, sx, sy) in [
                        (false, sw - width, sh - height),
                        (true, 0, 0),
                        (true, 2, 2),
                        (true, sw - width, sh - height),
                    ] {
                        let input = &src[source_alignment..source_alignment + src_len];
                        let a = &mut actual[destination_alignment..destination_alignment + dst_len];
                        let e =
                            &mut expected[destination_alignment..destination_alignment + dst_len];
                        if overlay {
                            scalar_overlay_copy(
                                input,
                                sw,
                                sh,
                                e,
                                2,
                                2,
                                width + 2,
                                height + 1,
                                sx,
                                sy,
                                2,
                                1,
                            )
                            .unwrap();
                            copy_overlay_or_committed_tile_to_rect(
                                input,
                                sw,
                                sh,
                                a,
                                2,
                                2,
                                width + 2,
                                height + 1,
                                sx,
                                sy,
                                2,
                                1,
                            )
                            .unwrap();
                        } else {
                            scalar_surface_copy(
                                input,
                                sw,
                                sh,
                                e,
                                width + 2,
                                height + 1,
                                sx,
                                sy,
                                2,
                                1,
                            )
                            .unwrap();
                            copy_surface_tile_to_rect(
                                input,
                                sw,
                                sh,
                                a,
                                width + 2,
                                height + 1,
                                sx,
                                sy,
                                2,
                                1,
                            )
                            .unwrap();
                        }
                        assert_eq!(actual, expected);
                        assert!(actual[..destination_alignment].iter().all(|v| *v == 0xa5));
                        assert!(actual[destination_alignment + dst_len..]
                            .iter()
                            .all(|v| *v == 0xa5));
                        assert_eq!(src, unchanged_src);
                        cases += 1;
                    }
                }
            }
        }
    }
    eprintln!("native_copy stride_alignment_cases={cases} source_destination_offsets=0..15 widths_heights=1..8");
}

#[test]
fn native_copy_invalid_source_and_short_private_spans_write_nothing() {
    let committed = [41; 6 * 5 * 3];
    for (sx, sy) in [(3, 0), (0, 2), (usize::MAX, 0), (0, usize::MAX)] {
        let mut rgb = [0xa5; 4 * 4 * 3];
        assert!(copy_surface_tile_to_rect(&committed, 6, 5, &mut rgb, 4, 4, sx, sy, 0, 0).is_err());
        assert_eq!(rgb, [0xa5; 48]);
        assert!(copy_overlay_or_committed_tile_to_rect(
            &committed, 6, 5, &mut rgb, 1, 1, 4, 4, sx, sy, 0, 0
        )
        .is_err());
        assert_eq!(rgb, [0xa5; 48]);
    }
    use std::panic::{catch_unwind, AssertUnwindSafe};
    for (src, dst, pixels) in [(0, 3, 8), (3, 0, 8), (0, 0, 9), (1, 0, 1), (0, 1, 1)] {
        let mut rgb = [0xa5; 24];
        assert!(catch_unwind(AssertUnwindSafe(|| copy_rgb_forward_span(
            &mut rgb, src, dst, pixels
        )))
        .is_err());
        assert_eq!(rgb, [0xa5; 24]);
    }
    for (src_bytes, dst_bytes) in [(3, 2), (2, 3), (0, 0), (25, 25)] {
        let mut rgb = [0xa5; 25];
        assert!(
            catch_unwind(AssertUnwindSafe(|| crate::mvs_pixels::copy_rgb_nonoverlap(
                &[1; 25][..src_bytes],
                &mut rgb[..dst_bytes]
            )))
            .is_err()
        );
        assert_eq!(rgb, [0xa5; 25]);
    }
}

// 输入由已有 bitstream builder 编码；期望仅使用独立的黑/白坐标规律。
#[test]
fn native_copy_full_mode_one_two_and_first_tile_literal_records() {
    for mode in [1, 2] {
        let mut d = decoder();
        let seed_modes = mode_stream(&[(3, 1), (0, 1)]);
        let seed_data = two_asymmetric_mode_three_tiles_data();
        let seed = d.prepare(&record(&seed_modes, &seed_data), 16, 16).unwrap();
        d.commit(seed);
        let modes = mode_stream(&[(mode, 3)]);
        let data = terminal_only_data();
        let prepared = d.prepare(&record(&modes, &data), 16, 16).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);
        for y in 0..16 {
            for x in 0..16 {
                let white_x = if mode == 2 && x >= 8 {
                    7 - y % 8
                } else {
                    y % 8
                };
                assert_eq!(
                    pixel(decoded, x, y),
                    if x % 8 == white_x { [255; 3] } else { [0; 3] },
                    "mode={mode} at={x}/{y}"
                );
            }
        }
        let committed = d.commit(prepared);
        assert_eq!(committed.rgb.len(), 16 * 16 * 3);
    }
}

#[test]
fn native_copy_cache_misses_preserve_literal_committed_pixels() {
    for mode in [6, 7] {
        let mut d = decoder();
        let seed_modes = mode_stream(&[(3, 1), (0, 1)]);
        let seed_data = two_asymmetric_mode_three_tiles_data();
        let seed = d.prepare(&record(&seed_modes, &seed_data), 16, 16).unwrap();
        d.commit(seed);
        let modes = mode_stream(&[(mode, 3)]);
        let data = if mode == 6 {
            cache_mode_data(&[9, 10, 11, 12])
        } else {
            terminal_only_data()
        };
        let prepared = d.prepare(&record(&modes, &data), 16, 16).unwrap();
        let decoded = MvsFullDecoder::decoded(&prepared);
        for y in 0..16 {
            for x in 0..16 {
                let white = y >= 8 || (x < 8 && x == y) || (x >= 8 && x - 8 == 7 - y);
                assert_eq!(
                    pixel(decoded, x, y),
                    if white { [255; 3] } else { [0; 3] },
                    "mode={mode} at={x}/{y}"
                );
            }
        }
    }
}

#[test]
fn native_copy_clipped_mixed_source_literal_and_failed_drop_transaction() {
    let mut d = decoder();
    let seed_modes = mode_stream(&[(3, 3)]);
    let mut seed_data = TestBitWriter::new();
    for _ in 0..4 {
        seed_data.write_bits(0, 8);
        for _ in 0..8 {
            seed_data.write_bits(0, 8);
        }
    }
    seed_data.write_bits(STREAM_TERMINAL as u32, 8);
    let seed_data = seed_data.finish();
    let seed = d.prepare(&record(&seed_modes, &seed_data), 16, 16).unwrap();
    d.commit(seed);
    let surface_before = format!("{:?}", d.surface_state);
    let cache_before = format!("{:?}", d.cache_state);
    let count_before = d.committed_records;
    let modes = mode_stream(&[(0, 1), (1, 0), (0, 0)]);
    let data = terminal_only_data();
    let prepared = d
        .prepare_rect(&record(&modes, &data), 0, 0, 10, 16, 16, 16)
        .unwrap();
    for y in 0..16 {
        for x in 0..10 {
            let white = y < 8 || x < 2 || x >= 8;
            assert_eq!(
                pixel(MvsFullDecoder::decoded(&prepared), x, y),
                if white { [255; 3] } else { [0; 3] }
            );
        }
    }
    drop(prepared);
    assert_eq!(format!("{:?}", d.surface_state), surface_before);
    assert_eq!(format!("{:?}", d.cache_state), cache_before);
    assert_eq!(d.committed_records, count_before);
    assert!(d
        .prepare_rect(&record(&modes, &[0x6c]), 0, 0, 10, 16, 16, 16)
        .is_err());
    assert_eq!(format!("{:?}", d.surface_state), surface_before);
    assert_eq!(format!("{:?}", d.cache_state), cache_before);
    assert_eq!(d.committed_records, count_before);
}

// Unix 真实不可访问 guard page 检测越界读与越界写；canary 只检测写越界。
#[cfg(unix)]
#[test]
fn native_copy_exact_leaf_guard_pages_all_short_tails() {
    use std::ffi::c_void;
    unsafe extern "C" {
        fn mmap(
            addr: *mut c_void,
            len: usize,
            prot: i32,
            flags: i32,
            fd: i32,
            offset: isize,
        ) -> *mut c_void;
        fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
        fn munmap(addr: *mut c_void, len: usize) -> i32;
        fn getpagesize() -> i32;
    }
    struct Guard {
        base: *mut c_void,
        page: usize,
    }
    impl Guard {
        fn new() -> Self {
            #[cfg(target_os = "macos")]
            let anon = 0x1000;
            #[cfg(not(target_os = "macos"))]
            let anon = 0x20;
            // SAFETY: 独占匿名映射；仅中间页开放访问，两端保持 PROT_NONE。
            unsafe {
                let page = getpagesize() as usize;
                let base = mmap(std::ptr::null_mut(), page * 3, 0, 2 | anon, -1, 0);
                assert_ne!(base as isize, -1);
                assert_eq!(mprotect(base.cast::<u8>().add(page).cast(), page, 3), 0);
                Self { base, page }
            }
        }
        fn span(&mut self, len: usize, at_end: bool) -> &mut [u8] {
            assert!(len <= self.page);
            let offset = if at_end {
                self.page * 2 - len
            } else {
                self.page
            };
            // SAFETY: 只返回中间可访问页的唯一可变跨度，独占借用覆盖其存活期。
            unsafe { std::slice::from_raw_parts_mut(self.base.cast::<u8>().add(offset), len) }
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: 映射由该 Guard 独占且仍有效，返回的借用已结束。
            unsafe {
                assert_eq!(munmap(self.base, self.page * 3), 0);
            }
        }
    }
    let mut src = Guard::new();
    let mut dst = Guard::new();
    for pixels in 1..=8 {
        for src_end in [false, true] {
            for dst_end in [false, true] {
                let input = src.span(pixels * 3, src_end);
                for (i, v) in input.iter_mut().enumerate() {
                    *v = (i * 29 + 17) as u8;
                }
                let output = dst.span(pixels * 3, dst_end);
                output.fill(0xa5);
                crate::mvs_pixels::copy_rgb_nonoverlap(input, output);
                for (i, v) in output.iter().enumerate() {
                    assert_eq!(*v, (i * 29 + 17) as u8);
                }
            }
        }
    }
    eprintln!("native_copy guarded_spans=32 pixels=1..8 input_output_start_end=true");
}

#[test]
fn native_copy_empty_tile_edges_preserve_buffers() {
    for (dx, dy) in [(2, 0), (0, 2), (2, 2)] {
        let mut rgb = [0xa5; 12];
        copy_surface_tile_to_rect(&[37; 12], 2, 2, &mut rgb, 2, 2, 0, 0, dx, dy).unwrap();
        copy_overlay_or_committed_tile_to_rect(&[37; 12], 2, 2, &mut rgb, 0, 0, 2, 2, 0, 0, dx, dy)
            .unwrap();
        assert_eq!(rgb, [0xa5; 12]);
    }
}
