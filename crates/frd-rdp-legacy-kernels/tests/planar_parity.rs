#[path = "common/planar.rs"]
mod oracle;
use frd_rdp_legacy_kernels::{KernelError, NativeKernels, PlanarLayout};
use oracle::*;
#[test]
fn delta_all_65536_pairs_and_boundaries() {
    let k = NativeKernels::detect().unwrap();
    for p in 0..=255 {
        let previous = vec![p; 256];
        let mut current = (0..=255).collect::<Vec<u8>>();
        let mut expected = current.clone();
        delta(&previous, &mut expected);
        k.resolve_planar_delta(&previous, &mut current).unwrap();
        assert_eq!(current, expected);
    }
    for n in [0, 1, 7, 8, 15, 16, 31, 32, 63, 64, 65] {
        for offset in 0..16 {
            let prev = bytes(n + offset, 3);
            let mut data = bytes(n + offset + 1, 1);
            let before = data.clone();
            let mut expected = data[offset..offset + n].to_vec();
            delta(&prev[offset..], &mut expected);
            k.resolve_planar_delta(&prev[offset..], &mut data[offset..offset + n])
                .unwrap();
            assert_eq!(&data[offset..offset + n], expected);
            assert_eq!(&data[..offset], &before[..offset]);
            assert_eq!(data[offset + n], before[offset + n]);
        }
    }
}
#[test]
fn dimensions_cll_alpha_subsampling_and_unaligned_canaries() {
    let k = NativeKernels::detect().unwrap();
    for width in DIMENSIONS {
        for height in DIMENSIONS {
            for cll in 1..=7 {
                for has_alpha in [false, true] {
                    for subsampled in [false, true] {
                        let l = PlanarLayout {
                            width,
                            height,
                            color_loss_level: cll,
                            has_alpha,
                            subsampled,
                        };
                        let n = width * height;
                        let c = if subsampled {
                            width.div_ceil(2) * height.div_ceil(2)
                        } else {
                            n
                        };
                        // 不同 plane 偏移确保完整块/短尾无需任何外部对齐或 padding。
                        let off = (width + height + usize::from(cll)) % 16;
                        let y = bytes(n + off, 1);
                        let co = bytes(c + off, 2);
                        let cg = bytes(c + off, 3);
                        let mut expected = vec![0; n * 3];
                        color(l, &y[off..], &co[off..], &cg[off..], &mut expected);
                        let mut out = vec![0xa5; n * 3 + off + 1];
                        k.ycocg_planes_to_rgb24(
                            l,
                            &y[off..],
                            &co[off..],
                            &cg[off..],
                            &mut out[off..off + n * 3],
                        )
                        .unwrap();
                        assert_eq!(&out[off..off + n * 3], expected, "{l:?}");
                        assert!(out[..off].iter().all(|&b| b == 0xa5));
                        assert_eq!(out[off + n * 3], 0xa5);
                    }
                }
            }
        }
    }
}
#[test]
fn rgb_marker_planes_and_all_offsets() {
    let k = NativeKernels::detect().unwrap();
    for n in [0, 1, 7, 8, 15, 16, 17, 31, 32, 63, 64, 65] {
        for off in 0..16 {
            let r = bytes(n + off, 1);
            let g = bytes(n + off, 2);
            let b = bytes(n + off, 3);
            let mut expected = vec![0; 3 * n];
            rgb(&r[off..], &g[off..], &b[off..], &mut expected);
            let mut out = vec![0xa5; off + 3 * n + 1];
            k.rgb_planes_to_rgb24(&r[off..], &g[off..], &b[off..], &mut out[off..off + 3 * n])
                .unwrap();
            assert_eq!(&out[off..off + 3 * n], expected);
            assert!(out[..off].iter().all(|&b| b == 0xa5));
            assert_eq!(out[off + 3 * n], 0xa5);
        }
    }
}
#[test]
fn errors_leave_whole_output_unchanged() {
    let k = NativeKernels::detect().unwrap();
    let valid = PlanarLayout {
        width: 3,
        height: 3,
        subsampled: true,
        color_loss_level: 1,
        has_alpha: true,
    };
    let mut out = [0xa5; 27];
    for l in [
        PlanarLayout { width: 0, ..valid },
        PlanarLayout { height: 0, ..valid },
        PlanarLayout {
            width: usize::MAX,
            height: 2,
            ..valid
        },
        PlanarLayout {
            width: usize::MAX,
            height: 1,
            ..valid
        },
        PlanarLayout {
            width: usize::MAX / 3 + 1,
            height: 1,
            ..valid
        },
    ] {
        assert_eq!(
            k.ycocg_planes_to_rgb24(l, &[0; 9], &[0; 4], &[0; 4], &mut out),
            Err(KernelError::InvalidDimensions)
        );
        assert_eq!(out, [0xa5; 27]);
    }
    for cll in [0, 8, 255] {
        assert_eq!(
            k.ycocg_planes_to_rgb24(
                PlanarLayout {
                    color_loss_level: cll,
                    ..valid
                },
                &[0; 9],
                &[0; 4],
                &[0; 4],
                &mut out
            ),
            Err(KernelError::InvalidColorLossLevel)
        );
        assert_eq!(out, [0xa5; 27]);
    }
    for (y, co, cg) in [(8, 4, 4), (9, 3, 4), (9, 4, 3), (10, 4, 4), (9, 5, 4)] {
        assert_eq!(
            k.ycocg_planes_to_rgb24(valid, &vec![0; y], &vec![0; co], &vec![0; cg], &mut out),
            Err(KernelError::Length)
        );
        assert_eq!(out, [0xa5; 27]);
    }
    for n in [26, 28] {
        let mut o = vec![0xa5; n];
        assert_eq!(
            k.ycocg_planes_to_rgb24(valid, &[0; 9], &[0; 4], &[0; 4], &mut o),
            Err(KernelError::Length)
        );
        assert!(o.iter().all(|&b| b == 0xa5));
    }
    assert_eq!(
        k.resolve_planar_delta(&[0; 26], &mut out),
        Err(KernelError::Length)
    );
    assert_eq!(out, [0xa5; 27]);
    assert_eq!(
        k.rgb_planes_to_rgb24(&[0; 9], &[0; 8], &[0; 9], &mut out),
        Err(KernelError::Length)
    );
    assert_eq!(out, [0xa5; 27]);
    assert_eq!(
        k.rgb_planes_to_rgb24(&[0; 9], &[0; 9], &[0; 8], &mut out),
        Err(KernelError::Length)
    );
    assert_eq!(out, [0xa5; 27]);
    assert_eq!(
        k.rgb_planes_to_rgb24(&[0; 8], &[0; 8], &[0; 8], &mut out),
        Err(KernelError::Length)
    );
    assert_eq!(out, [0xa5; 27]);
    k.copy_u8(&[], &mut []).unwrap();
}
#[test]
fn frozen_unmodified_decoder_outputs_and_discriminants() {
    use sha2::{Digest, Sha256};
    let blob = include_bytes!("fixtures/planar-original.bin");
    let expected_hash = include_str!("fixtures/planar-original.sha256").trim();
    assert_eq!(format!("{:x}", Sha256::digest(blob)), expected_hash);
    let k = NativeKernels::detect().unwrap();
    let records = records(blob);
    assert_eq!(records.len(), 64);
    for rec in records {
        if rec.header & 0x10 != 0 {
            let previous = vec![rec.wire[2]; 256];
            let mut current = (0..=255).collect::<Vec<u8>>();
            k.resolve_planar_delta(&previous, &mut current).unwrap();
            assert_eq!(
                current,
                rec.expected[256 * 3..]
                    .chunks_exact(3)
                    .map(|p| p[0])
                    .collect::<Vec<_>>()
            );
        } else {
            let n = rec.width * rec.height;
            let alpha = rec.header & 0x20 == 0;
            let cll = rec.header & 7;
            let sub = cll != 0 && rec.header & 8 != 0;
            let c = if sub {
                rec.width.div_ceil(2) * rec.height.div_ceil(2)
            } else {
                n
            };
            let start = 1 + if alpha { n } else { 0 };
            let y = &rec.wire[start..start + n];
            let co = &rec.wire[start + n..start + n + c];
            let cg = &rec.wire[start + n + c..start + n + 2 * c];
            let mut out = vec![0; n * 3];
            if cll == 0 {
                k.rgb_planes_to_rgb24(y, co, cg, &mut out).unwrap();
            } else {
                k.ycocg_planes_to_rgb24(
                    PlanarLayout {
                        width: rec.width,
                        height: rec.height,
                        subsampled: sub,
                        color_loss_level: cll,
                        has_alpha: alpha,
                    },
                    y,
                    co,
                    cg,
                    &mut out,
                )
                .unwrap();
            }
            assert_eq!(
                out, rec.expected,
                "{}x{} header={:#x}",
                rec.width, rec.height, rec.header
            );
        }
    }
    // 手写非灰 marker，alpha flag 显式交换 R/B；CLL7 左移截断 discriminant。
    for (cll, y, co, cg, expected) in [
        (1, 128, 1, 0, [129, 128, 127]),
        (2, 128, 127, 128, [126, 128, 130]),
        (7, 128, 129, 255, [255, 64, 128]),
        (1, 0, 255, 128, [127, 0, 129]),
    ] {
        let l = PlanarLayout {
            width: 1,
            height: 1,
            subsampled: false,
            color_loss_level: cll,
            has_alpha: true,
        };
        let mut out = [0; 3];
        k.ycocg_planes_to_rgb24(l, &[y], &[co], &[cg], &mut out)
            .unwrap();
        assert_eq!(out, expected);
        k.ycocg_planes_to_rgb24(
            PlanarLayout {
                has_alpha: false,
                ..l
            },
            &[y],
            &[co],
            &[cg],
            &mut out,
        )
        .unwrap();
        assert_eq!(out, [expected[2], expected[1], expected[0]]);
    }
}
