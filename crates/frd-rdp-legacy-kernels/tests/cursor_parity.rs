#[path = "common/cursor_reference.rs"]
mod reference;
use frd_rdp_legacy_kernels::{
    CursorBitmapMode as M, CursorXorFormat as F, KernelError, NativeKernels,
};
use reference::*;

#[test]
fn literal_anchors_fix_square_opaque_equality_checker_and_wrapping() {
    let k = NativeKernels::detect().unwrap();
    let mut out = [0; 16];
    k.decode_cursor_row(
        F::Bgra32,
        M::Software,
        &[
            0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 1, 96, 80, 64, 128,
        ],
        Some(&[0xe0]),
        &mut out,
    )
    .unwrap();
    assert_eq!(
        out,
        [0, 0, 0, 0, 255, 255, 255, 0, 254, 254, 254, 1, 16, 25, 36, 128]
    );
    let mut alpha_white = [0; 4];
    k.decode_cursor_row(
        F::Bgr24,
        M::Software,
        &[255, 255, 255],
        None,
        &mut alpha_white,
    )
    .unwrap();
    assert_eq!(alpha_white, [254, 254, 254, 255]);
    let mut composite = [0; 16];
    k.composite_cursor_row(
        &out,
        &[
            1, 127, 255, 31, 0, 1, 254, 32, 255, 255, 255, 33, 70, 90, 110, 34,
        ],
        &mut composite,
    )
    .unwrap();
    assert_eq!(
        composite,
        [1, 127, 255, 31, 255, 254, 1, 255, 251, 251, 251, 33, 50, 69, 90, 34]
    );
    for (mode, expect) in [
        (
            M::Accelerated { odd_row: false },
            [255, 255, 255, 255, 0, 0, 0, 255],
        ),
        (
            M::Accelerated { odd_row: true },
            [0, 0, 0, 255, 255, 255, 255, 255],
        ),
    ] {
        let mut row = [0; 8];
        k.decode_cursor_row(F::Mono1, mode, &[0xc0], Some(&[0xc0]), &mut row)
            .unwrap();
        assert_eq!(row, expect);
    }
}

#[test]
fn all_565_values_match_the_copied_original_rounding_in_every_mode() {
    let k = NativeKernels::detect().unwrap();
    let source: Vec<u8> = (0..=u16::MAX).flat_map(u16::to_le_bytes).collect();
    for mode in MODES {
        for mask in [
            None,
            Some(vec![0x00; 8192]),
            Some(vec![0xff; 8192]),
            Some(bytes(8192, 97)),
        ] {
            let expected = decode(F::Rgb565, mode, &source, mask.as_deref(), 65536);
            let mut out = vec![0; 65536 * 4];
            k.decode_cursor_row(F::Rgb565, mode, &source, mask.as_deref(), &mut out)
                .unwrap();
            assert_eq!(out, expected);
        }
    }
}

#[test]
fn every_mono_xor_and_byte_pair_including_unused_tail_bits() {
    let k = NativeKernels::detect().unwrap();
    for x in 0..=255u8 {
        for a in 0..=255u8 {
            for mode in MODES {
                let expected = decode(F::Mono1, mode, &[x], Some(&[a]), 8);
                for n in 1..=8 {
                    let mut out = [0x39; 32];
                    k.decode_cursor_row(F::Mono1, mode, &[x], Some(&[a]), &mut out[..n * 4])
                        .unwrap();
                    assert_eq!(
                        &out[..n * 4],
                        &expected[..n * 4],
                        "xor{x} and{a} n{n} {mode:?}"
                    );
                    assert!(out[n * 4..].iter().all(|&v| v == 0x39));
                }
            }
        }
    }
}

#[test]
fn formats_modes_widths_masks_alpha_and_unaligned_canaries() {
    let k = NativeKernels::detect().unwrap();
    for format in FORMATS {
        for n in WIDTHS {
            for mode in MODES {
                for seed in 0..16u32 {
                    let mut source = bytes(encoded_len(format, n), seed + 7);
                    if format == F::Bgra32 {
                        for (i, p) in source.chunks_exact_mut(4).enumerate() {
                            p[3] = [0, 1, 2, 127, 128, 254, 255][(i + seed as usize) % 7];
                            if i % 3 == 0 {
                                p[..3].fill(255);
                            } else if i % 3 == 1 {
                                p[..3].fill(0);
                            }
                        }
                    }
                    let and = bytes(n.div_ceil(8), seed + 300);
                    for mask in [None, Some(and.as_slice())] {
                        let expected = decode(format, mode, &source, mask, n);
                        for offset in 0..16 {
                            let mut input = vec![0x72; source.len() + 32];
                            input[offset..offset + source.len()].copy_from_slice(&source);
                            let mut mask_input = vec![0x82; and.len() + 32];
                            mask_input[offset..offset + and.len()].copy_from_slice(&and);
                            let mut out = vec![0xa5; n * 4 + 32];
                            k.decode_cursor_row(
                                format,
                                mode,
                                &input[offset..offset + source.len()],
                                mask.map(|_| &mask_input[offset..offset + and.len()]),
                                &mut out[offset..offset + n * 4],
                            )
                            .unwrap();
                            assert_eq!(&out[offset..offset + n * 4], expected);
                            assert!(out[..offset]
                                .iter()
                                .chain(&out[offset + n * 4..])
                                .all(|&v| v == 0xa5));
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn composite_release_contract_mixed_blocks_alpha_background_and_byte3() {
    let k = NativeKernels::detect().unwrap();
    for n in WIDTHS {
        for alpha in [0, 1, 2, 127, 128, 254, 255] {
            for bg in [0, 1, 127, 254, 255] {
                let mut source = bytes(n * 4, 127);
                let mut background = bytes(n * 4, 17);
                for (i, (s, b)) in source
                    .chunks_exact_mut(4)
                    .zip(background.chunks_exact_mut(4))
                    .enumerate()
                {
                    s[3] = alpha;
                    b[..3].fill(bg);
                    if i % 4 == 0 {
                        s.copy_from_slice(&[255, 255, 255, 0]);
                    }
                    if i % 4 == 1 {
                        s.copy_from_slice(&[1, 71, 255, 0]);
                    }
                    if i % 4 == 2 {
                        s[..3].fill(254);
                    }
                }
                let expected = wrapping_reference(&source, &background);
                // 原版 debug 的加法 panic 是记录的区别；release 另测精确复制原函数。
                for offset in 0..16 {
                    let mut s = vec![0; source.len() + 32];
                    let mut b = vec![0; background.len() + 32];
                    let mut o = vec![0x67; source.len() + 32];
                    s[offset..offset + source.len()].copy_from_slice(&source);
                    b[offset..offset + source.len()].copy_from_slice(&background);
                    k.composite_cursor_row(
                        &s[offset..offset + source.len()],
                        &b[offset..offset + source.len()],
                        &mut o[offset..offset + source.len()],
                    )
                    .unwrap();
                    assert_eq!(&o[offset..offset + source.len()], expected);
                    assert!(o[..offset]
                        .iter()
                        .chain(&o[offset + source.len()..])
                        .all(|&v| v == 0x67));
                }
            }
        }
    }
}

#[test]
fn invalid_lengths_are_atomic_and_zero_rows_require_empty_sources() {
    assert!(matches!(
        NativeKernels::detect_if(false),
        Err(KernelError::Unavailable)
    ));
    let k = NativeKernels::detect().unwrap();
    for f in FORMATS {
        for n in [0, 1, 4, 9] {
            let good = vec![0; encoded_len(f, n)];
            let mut out = vec![0x93; n * 4];
            for len in [good.len().saturating_sub(1), good.len() + 1] {
                if len == good.len() {
                    continue;
                }
                assert_eq!(
                    k.decode_cursor_row(f, M::Software, &vec![0; len], None, &mut out),
                    Err(KernelError::Length)
                );
                assert!(out.iter().all(|&v| v == 0x93));
            }
            for mask_len in [n.div_ceil(8).saturating_sub(1), n.div_ceil(8) + 1] {
                if mask_len == n.div_ceil(8) {
                    continue;
                }
                let mask = vec![0; mask_len];
                assert_eq!(
                    k.decode_cursor_row(f, M::Software, &good, Some(&mask), &mut out),
                    Err(KernelError::Length)
                );
                assert!(out.iter().all(|&v| v == 0x93));
            }
        }
    }
    for f in FORMATS {
        k.decode_cursor_row(f, M::Software, &[], Some(&[]), &mut [])
            .unwrap();
        k.decode_cursor_row(f, M::Software, &[], None, &mut [])
            .unwrap();
        let mut out = [0x42; 3];
        assert_eq!(
            k.decode_cursor_row(f, M::Software, &[], None, &mut out),
            Err(KernelError::Length)
        );
        assert_eq!(out, [0x42; 3]);
    }
    for (s, b, o) in [
        (4, 4, 3),
        (4, 3, 4),
        (3, 4, 4),
        (3, 3, 3),
        (0, 4, 0),
        (4, 0, 0),
    ] {
        let mut out = vec![0x61; o];
        assert_eq!(
            k.composite_cursor_row(&vec![0; s], &vec![0; b], &mut out),
            Err(KernelError::Length)
        );
        assert!(out.iter().all(|&v| v == 0x61));
    }
    k.composite_cursor_row(&[], &[], &mut []).unwrap();
}

#[cfg(not(debug_assertions))]
#[test]
fn copied_original_release_compositor_includes_the_overflow_domain() {
    let k = NativeKernels::detect().unwrap();
    for n in WIDTHS {
        for alpha in [0, 1, 2, 127, 128, 254, 255] {
            let mut src = bytes(n * 4, 33);
            let mut bg = bytes(n * 4, 64);
            for (i, (s, b)) in src
                .chunks_exact_mut(4)
                .zip(bg.chunks_exact_mut(4))
                .enumerate()
            {
                s[3] = alpha;
                if i % 3 == 0 {
                    s.copy_from_slice(&[255, 255, 255, 0]);
                }
                if i % 3 == 1 {
                    s[..3].fill(254);
                    b[..3].fill(255);
                }
            }
            let original = copied_original_composite(&src, &bg);
            assert_eq!(original, wrapping_reference(&src, &bg));
            let mut out = vec![0; n * 4];
            k.composite_cursor_row(&src, &bg, &mut out).unwrap();
            assert_eq!(out, original);
        }
    }
}
