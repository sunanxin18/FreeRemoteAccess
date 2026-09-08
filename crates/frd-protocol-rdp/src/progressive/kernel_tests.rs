use super::*;

fn fixture(seed: u32) -> [i16; 4096] {
    let mut state = seed;
    std::array::from_fn(|_| {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        (state >> 16) as i16
    })
}
fn oracle_line(low: &[i16], high: &[i16], layout: BandLayout) -> Vec<i16> {
    let l = low.len();
    let h = high.len();
    let mut out = vec![0i16; l + h];
    let narrow = |x: i32| {
        if layout == BandLayout::Normal {
            x as i16
        } else {
            x.clamp(-32768, 32767) as i16
        }
    };
    out[0] = narrow(i32::from(low[0]) - i32::from(high[0]));
    for i in 1..h {
        let sum = i32::from(high[i - 1]) + i32::from(high[i]);
        let correction = if layout == BandLayout::Normal {
            (sum + 1) >> 1
        } else {
            sum / 2
        };
        out[2 * i] = narrow(i32::from(low[i]) - correction);
    }
    if l > h {
        out[2 * h] = narrow(
            i32::from(low[h])
                - if l == h + 1 {
                    i32::from(high[h - 1])
                } else {
                    i32::from(high[h - 1]) / 2
                },
        );
    }
    for i in 0..h {
        let even = i32::from(out[2 * i]);
        let next = if 2 * i + 2 < out.len() {
            i32::from(out[2 * i + 2])
        } else {
            even
        };
        let mean = if layout == BandLayout::Normal {
            (even + next) >> 1
        } else {
            (even + next) / 2
        };
        out[2 * i + 1] = narrow(mean + 2 * i32::from(high[i]));
    }
    if l == h + 2 {
        out[2 * h + 1] = narrow((i32::from(out[2 * h]) + i32::from(low[h + 1])) / 2);
    }
    out
}
fn oracle_dwt(c: &mut [i16; 4096], layout: BandLayout) {
    let levels = match layout {
        BandLayout::Normal => [(3840, 8, 8), (3072, 16, 16), (0, 32, 32)],
        BandLayout::ReduceExtrapolate => [(3807, 9, 8), (3007, 17, 16), (0, 33, 31)],
    };
    for (off, l, h) in levels {
        let w = l + h;
        let a = &c[off..];
        let lh = l * h;
        let hh = 2 * lh;
        let ll = hh + h * h;
        let mut tmp = vec![0i16; w * w];
        for row in 0..l {
            tmp[row * w..(row + 1) * w].copy_from_slice(&oracle_line(
                &a[ll + row * l..ll + (row + 1) * l],
                &a[row * h..(row + 1) * h],
                layout,
            ));
        }
        for row in 0..h {
            tmp[(l + row) * w..(l + row + 1) * w].copy_from_slice(&oracle_line(
                &a[lh + row * l..lh + (row + 1) * l],
                &a[hh + row * h..hh + (row + 1) * h],
                layout,
            ));
        }
        for col in 0..w {
            let low: Vec<_> = (0..l).map(|r| tmp[r * w + col]).collect();
            let high: Vec<_> = (0..h).map(|r| tmp[(l + r) * w + col]).collect();
            let line = oracle_line(&low, &high, layout);
            for (row, v) in line.into_iter().enumerate() {
                c[off + row * w + col] = v;
            }
        }
    }
}
#[test]
fn dwt_simd_matches_scalar_extremes_and_tail_layouts() {
    let Some(k) = NativeKernels::new() else {
        return;
    };
    for layout in [BandLayout::Normal, BandLayout::ReduceExtrapolate] {
        for seed in [0, 1, 0xffff, 0xdeadbeef] {
            let mut got = fixture(seed);
            let mut expected = got;
            oracle_dwt(&mut expected, layout);
            k.inverse_dwt(&mut got, &mut [0; 4096], layout).unwrap();
            assert_eq!(got, expected, "{layout:?} seed {seed}");
        }
        let mut zero = [0; 4096];
        k.inverse_dwt(&mut zero, &mut [0; 4096], layout).unwrap();
        assert_eq!(zero, [0; 4096]);
    }
}
#[test]
fn sign_add_shift_match_scalar_and_invalid_shift_is_transactional() {
    let Some(k) = NativeKernels::new() else {
        return;
    };
    let original = fixture(73);
    let mut sign = [0; 4096];
    k.capture_sign(&original, &mut sign);
    assert_eq!(sign, original.map(i16::signum));
    let delta = fixture(99);
    let mut got = original;
    k.add(&mut got, &delta);
    assert_eq!(
        got,
        std::array::from_fn(|i| original[i].wrapping_add(delta[i]))
    );
    for layout in [BandLayout::Normal, BandLayout::ReduceExtrapolate] {
        let shifts = [0, 1, 15, 16, 17, 20, 21, 22, 7, 8];
        let mut got = original;
        let mut expected = original;
        let mut off = 0;
        for (len, shift) in layout.lengths().into_iter().zip(shifts) {
            for v in &mut expected[off..off + len] {
                *v = i32::from(*v).wrapping_shl(shift as u32) as i16;
            }
            off += len;
        }
        assert_eq!(off, 4096);
        k.shift_bands(&mut got, &shifts, layout).unwrap();
        assert_eq!(got, expected);
        let before = got;
        let mut bad = shifts;
        bad[9] = 30;
        assert_eq!(
            k.shift_bands(&mut got, &bad, layout),
            Err(KernelError::InvalidShift)
        );
        assert_eq!(got, before);
    }
}
#[test]
fn fixedpoint_bgra_matches_scalar_and_neutral_gray() {
    let Some(k) = NativeKernels::new() else {
        return;
    };
    let y = fixture(51);
    let cb = fixture(52);
    let cr = fixture(53);
    let mut got = [0; 16384];
    k.ycbcr_to_bgra(&y, &cb, &cr, &mut got);
    for i in 0..4096 {
        let yy = (i32::from(y[i]) + 4096).wrapping_shl(16);
        let u = i32::from(cb[i]);
        let v = i32::from(cr[i]);
        let r = (yy.wrapping_add(v.wrapping_mul(91916)) >> 21).clamp(0, 255) as u8;
        let g = (yy
            .wrapping_sub(v.wrapping_mul(46819))
            .wrapping_sub(u.wrapping_mul(22527))
            >> 21)
            .clamp(0, 255) as u8;
        let b = (yy.wrapping_add(u.wrapping_mul(115992)) >> 21).clamp(0, 255) as u8;
        assert_eq!(&got[4 * i..4 * i + 4], &[b, g, r, 255]);
    }
    for (luma, gray) in [(-4096, 0), (0, 128), (4064, 255)] {
        k.ycbcr_to_bgra(&[luma; 4096], &[0; 4096], &[0; 4096], &mut got);
        assert!(got.chunks_exact(4).all(|p| p == [gray, gray, gray, 255]));
    }
}

#[test]
fn differential_both_layouts_matches_wrapping_scalar_prefix() {
    let Some(k) = NativeKernels::new() else {
        return;
    };
    for layout in [BandLayout::Normal, BandLayout::ReduceExtrapolate] {
        let mut c = fixture(991);
        let mut expected = c;
        let start = if layout == BandLayout::Normal {
            4032
        } else {
            4015
        };
        let mut carry = 0i16;
        for v in &mut expected[start..] {
            carry = carry.wrapping_add(*v);
            *v = carry;
        }
        k.prefix_sum_ll3(&mut c, layout);
        assert_eq!(c, expected);
    }
}
#[test]
fn refinement_matches_oracle_significance_and_shift_transaction() {
    let Some(k) = NativeKernels::new() else {
        return;
    };
    let mag = fixture(101).map(|v| v as u16);
    let negative = std::array::from_fn(|i| i % 3 == 0);
    let shifts = [0, 15, 16, 17, 20, 21, 22, 7, 8, 9];
    for layout in [BandLayout::Normal, BandLayout::ReduceExtrapolate] {
        let mut c = fixture(100);
        let mut d = std::array::from_fn(|i| match i % 3 {
            0 => -1,
            1 => 0,
            _ => 1,
        });
        let mut expected = c;
        let mut expected_sign = d;
        let mut off = 0;
        for (len, shift) in layout.lengths().into_iter().zip(shifts) {
            for i in off..off + len {
                let v = if negative[i] {
                    -i32::from(mag[i])
                } else {
                    i32::from(mag[i])
                };
                expected[i] = expected[i].wrapping_add(v.wrapping_shl(shift as u32) as i16);
                if expected_sign[i] == 0 {
                    expected_sign[i] = v.signum() as i16;
                }
            }
            off += len;
        }
        k.apply_refinement(&mut c, &mut d, &mag, &negative, &shifts, layout)
            .unwrap();
        assert_eq!(c, expected);
        assert_eq!(d, expected_sign);
        let before = c;
        let oldsign = d;
        let mut invalid = shifts;
        invalid[9] = 30;
        assert_eq!(
            k.apply_refinement(&mut c, &mut d, &mag, &negative, &invalid, layout),
            Err(KernelError::InvalidShift)
        );
        assert_eq!(c, before);
        assert_eq!(d, oldsign);
    }
}
#[test]
#[ignore = "manual target-native kernel benchmark; no timing acceptance threshold"]
fn benchmark_progressive_native_tile() {
    let k = NativeKernels::new().expect("target SIMD backend unavailable");
    let source = fixture(6).map(|v| v / 64);
    let mut scratch = [0; 4096];
    let mut out = [0; 16384];
    for layout in [BandLayout::Normal, BandLayout::ReduceExtrapolate] {
        let start = std::time::Instant::now();
        let count = 2000;
        for _ in 0..count {
            let mut c = std::hint::black_box(source);
            k.inverse_dwt(&mut c, &mut scratch, layout).unwrap();
            k.ycbcr_to_bgra(&c, &c, &c, &mut out);
            std::hint::black_box(&out);
        }
        eprintln!(
            "{layout:?}: {:.2} microseconds per one-component DWT + BGRA tile",
            start.elapsed().as_secs_f64() * 1e6 / count as f64
        );
    }
}

/// FNV-1a of little-endian i16 outputs from independently compiled FreeRDP C.
/// Pinned revision: 54a873e2710710841c6ec2b756df64285ec6e29a.
/// progressive.c SHA256: 102858436412f55a3042dfaefc1ce4d2135cc3fa8be0a58e51ed412f51201605
/// rfx_dwt.c SHA256: 6a72283bede3569f58d02e72b063e712eb0270d5cfecab4952f392bed17fa80b
/// Inputs are synthetic fixture(seed), never captured desktop data.
#[test]
fn dwt_matches_pinned_freerdp_c_output_fingerprints() {
    let Some(k) = NativeKernels::new() else {
        return;
    };
    for (layout, hashes) in [
        (
            BandLayout::Normal,
            [
                0x27b6c62004e91bd4u64,
                0x34a3cb28973135a0,
                0x6ca2ff4ef00559d2,
                0x744a1b110145dcd1,
            ],
        ),
        (
            BandLayout::ReduceExtrapolate,
            [
                0x691d74eb727523e4u64,
                0xc7fa06bd63dd9c79,
                0x4c77152104adc28c,
                0xf048ad62ac69318f,
            ],
        ),
    ] {
        for (seed, expected) in [0, 1, 65535, 3735928559].into_iter().zip(hashes) {
            let mut c = fixture(seed);
            k.inverse_dwt(&mut c, &mut [0; 4096], layout).unwrap();
            let hash = c
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .fold(0xcbf29ce484222325u64, |h, b| {
                    (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
                });
            assert_eq!(hash, expected, "{layout:?} seed {seed}");
        }
    }
}

#[test]
fn out_of_profile_shifts_reject_before_any_coefficient_or_sign_write() {
    let Some(k) = NativeKernels::new() else {
        return;
    };
    for layout in [BandLayout::Normal, BandLayout::ReduceExtrapolate] {
        for invalid in [23, 30, 31, 32, 255] {
            let mut c = fixture(71);
            let before = c;
            let mut das = [0; 4096];
            let mut shifts = [22; 10];
            shifts[9] = invalid;
            assert_eq!(
                k.shift_bands(&mut c, &shifts, layout),
                Err(KernelError::InvalidShift)
            );
            assert_eq!(c, before);
            assert_eq!(
                k.apply_refinement(
                    &mut c,
                    &mut das,
                    &[1; 4096],
                    &[false; 4096],
                    &shifts,
                    layout
                ),
                Err(KernelError::InvalidShift)
            );
            assert_eq!(c, before);
            assert_eq!(das, [0; 4096]);
        }
    }
}

#[test]
fn high_shifts_store_low16_bits_without_modulo16_shift_count() {
    let Some(k) = NativeKernels::new() else {
        return;
    };
    for layout in [BandLayout::Normal, BandLayout::ReduceExtrapolate] {
        for shift in [16, 17, 20, 21, 22] {
            let mut c = fixture(99);
            k.shift_bands(&mut c, &[shift; 10], layout).unwrap();
            assert_eq!(c, [0; 4096]);
            let mut current = fixture(51);
            let before = current;
            let mut das = [0; 4096];
            k.apply_refinement(
                &mut current,
                &mut das,
                &[65535; 4096],
                &[true; 4096],
                &[shift; 10],
                layout,
            )
            .unwrap();
            assert_eq!(current, before);
            assert_eq!(das, [-1; 4096]);
        }
    }
}
