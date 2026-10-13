#[path = "common/bitmap.rs"]
mod oracle;
use frd_rdp_legacy_kernels::{KernelError, NativeKernels, PackedDepth};
use oracle::*;
const LENGTHS: &[usize] = &[0, 1, 7, 8, 15, 16, 31, 32, 63, 64, 65];
fn canary(len: usize, offset: usize, expected: &[u8], f: impl FnOnce(&mut [u8])) {
    let mut out = vec![0xc9; offset + len + 17];
    f(&mut out[offset..offset + len]);
    assert_eq!(&out[offset..offset + len], expected);
    assert!(out[..offset]
        .iter()
        .chain(&out[offset + len..])
        .all(|&v| v == 0xc9));
}
#[test]
fn copy_all_byte_tails_unaligned_and_canaries() {
    let k = NativeKernels::detect().unwrap();
    for n in 0..=195 {
        for offset in 0..16 {
            let source = bytes(offset + n + 17);
            let src = &source[offset..offset + n];
            canary(n, 15 - offset, src, |out| k.copy_u8(src, out).unwrap());
        }
    }
}
#[test]
fn packed_fill_xor_dither_all_depths_tails_and_byte_offsets() {
    let k = NativeKernels::detect().unwrap();
    for (depth, width) in DEPTHS {
        let fg = &([0xb7, 0x83, 0x29])[..width];
        let b = &([0x34, 0xe1, 0x56])[..width];
        for &n in LENGTHS {
            for offset in 0..16 {
                let source = bytes(offset + n * width + 17);
                let previous = &source[offset..offset + n * width];
                canary(n * width, offset, &fill(fg, n), |out| {
                    k.fill_pixel(depth, fg, out).unwrap()
                });
                canary(n * width, offset, &xor(previous, fg), |out| {
                    k.xor_previous(depth, previous, fg, out).unwrap()
                });
                for phase in [false, true] {
                    canary(n * width, offset, &dither(fg, b, phase, n), |out| {
                        k.dither_pixels(depth, fg, b, phase, out).unwrap()
                    });
                }
            }
        }
    }
}
#[test]
fn every_mask_valid_offset_count_and_previous_mode() {
    let k = NativeKernels::detect().unwrap();
    for (depth, width) in DEPTHS {
        let fg = &([0xb7, 0x83, 0x29])[..width];
        for m in 0..=255 {
            for offset in 0..=8 {
                for n in 0..=8 - offset {
                    let previous = bytes(n * width);
                    for bg in [None, Some(previous.as_slice())] {
                        for byte_offset in 0..16 {
                            canary(
                                n * width,
                                byte_offset,
                                &mask(bg, fg, m, offset as u8, n),
                                |out| {
                                    k.mask_previous(depth, bg, fg, m, offset as u8, out)
                                        .unwrap();
                                },
                            );
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn raw_555_high_bit_and_white_are_distinct_from_565() {
    let k = NativeKernels::detect().unwrap();
    let mut out = [0; 6];
    k.fill_pixel(PackedDepth::Rgb555, &[0xff, 0x7f], &mut out)
        .unwrap();
    assert_eq!(out, [0xff, 0x7f, 0xff, 0x7f, 0xff, 0x7f]);
    k.fill_pixel(PackedDepth::Rgb565, &[0xff, 0xff], &mut out)
        .unwrap();
    assert_eq!(out, [0xff; 6]);
    k.fill_pixel(PackedDepth::Rgb555, &[0x12, 0x80], &mut out)
        .unwrap();
    assert_eq!(out, [0x12, 0x80, 0x12, 0x80, 0x12, 0x80]);
    k.xor_previous(PackedDepth::Rgb555, &[0xff; 6], &[0x12, 0x80], &mut out)
        .unwrap();
    assert_eq!(out, [0xed, 0x7f, 0xed, 0x7f, 0xed, 0x7f]);
}
#[test]
fn packed24_cross_lane_and_dither_odd_split_phase() {
    let k = NativeKernels::detect().unwrap();
    let a = [0x11, 0x22, 0x33];
    let b = [0xa4, 0xb5, 0xc6];
    let expected = [
        0x11, 0x22, 0x33, 0xa4, 0xb5, 0xc6, 0x11, 0x22, 0x33, 0xa4, 0xb5, 0xc6, 0x11, 0x22, 0x33,
        0xa4, 0xb5, 0xc6,
    ];
    let mut out = [0; 18];
    k.dither_pixels(PackedDepth::Bgr24, &a, &b, false, &mut out)
        .unwrap();
    assert_eq!(out, expected);
    for (depth, width) in DEPTHS {
        for n in 1..=65 {
            for split in 0..=n {
                for phase in [false, true] {
                    let mut out = vec![0; n * width];
                    let (left, right) = out.split_at_mut(split * width);
                    k.dither_pixels(depth, &a[..width], &b[..width], phase, left)
                        .unwrap();
                    k.dither_pixels(
                        depth,
                        &a[..width],
                        &b[..width],
                        phase ^ (split % 2 != 0),
                        right,
                    )
                    .unwrap();
                    assert_eq!(out, dither(&a[..width], &b[..width], phase, n));
                }
            }
        }
    }
}
#[test]
fn errors_leave_all_output_bytes_unchanged_even_empty_input() {
    let k = NativeKernels::detect().unwrap();
    let mut out = [0x6d; 24];
    for (depth, width) in DEPTHS {
        for n in 0..=24 {
            for pixel_len in 0..=4 {
                let fg = &[0x9a; 4][..pixel_len];
                let dst = &mut out[..n];
                let expected = if pixel_len == width && n % width == 0 {
                    Ok(())
                } else {
                    Err(KernelError::Length)
                };
                let result = k.fill_pixel(depth, fg, dst);
                assert_eq!(result, expected);
                if result.is_err() {
                    assert!(dst.iter().all(|&v| v == 0x6d));
                }
                dst.fill(0x6d);
                let result = k.dither_pixels(depth, fg, &[1; 3][..width], false, dst);
                assert_eq!(result, expected);
                if result.is_err() {
                    assert!(dst.iter().all(|&v| v == 0x6d));
                }
                dst.fill(0x6d);
                let result = k.dither_pixels(depth, &[1; 3][..width], fg, true, dst);
                assert_eq!(result, expected);
                if result.is_err() {
                    assert!(dst.iter().all(|&v| v == 0x6d));
                }
                dst.fill(0x6d);
                for source_len in [n, n + 1] {
                    let source = vec![0x34; source_len];
                    let result = k.xor_previous(depth, &source, fg, dst);
                    assert_eq!(
                        result,
                        if source_len == n {
                            expected
                        } else {
                            Err(KernelError::Length)
                        }
                    );
                    if result.is_err() {
                        assert!(dst.iter().all(|&v| v == 0x6d));
                    }
                    dst.fill(0x6d);
                }
                for bit_offset in 0..=255 {
                    for bg in [None, Some(&[0x34; 24][..n]), Some(&[0x34; 25][..n + 1])] {
                        let result = k.mask_previous(depth, bg, fg, 0x97, bit_offset, dst);
                        let valid = expected.is_ok()
                            && bg.is_none_or(|p| p.len() == n)
                            && usize::from(bit_offset) + n / width <= 8;
                        assert_eq!(
                            result,
                            if valid {
                                Ok(())
                            } else {
                                Err(KernelError::Length)
                            }
                        );
                        if result.is_err() {
                            assert!(dst.iter().all(|&v| v == 0x6d));
                        }
                        dst.fill(0x6d);
                    }
                }
            }
        }
    }
    assert_eq!(k.copy_u8(&[1; 25], &mut out), Err(KernelError::Length));
    assert_eq!(out, [0x6d; 24]);
    assert_eq!(
        NativeKernels::detect_if(false).err(),
        Some(KernelError::Unavailable)
    );
}
