//! 窄范围整输入错误原子性与颜色字节回归；只通过公开 checked 入口。
#[path = "common/frozen.rs"]
mod frozen;
use frd_rdp_legacy_kernels::{KernelError, NativeKernels};
use frozen::color_conversion::{ycbcr_to_rgba, YCbCrBuffer};

fn native() -> NativeKernels {
    NativeKernels::detect().expect("本测试需要获准架构上的原生 SIMD")
}

// 独立 scalar checked 只分类合法域，不计算像素，也不模拟两遍实现。
fn valid(y: i16, cb: i16, cr: i16) -> bool {
    let Some(yy) = y
        .checked_add(4096)
        .map(i32::from)
        .and_then(|y| y.checked_mul(65536))
    else {
        return false;
    };
    let (Some(rc), Some(gc), Some(gu), Some(bc)) = (
        i32::from(cr).checked_mul(91915),
        i32::from(cr).checked_mul(46818),
        i32::from(cb).checked_mul(22526),
        i32::from(cb).checked_mul(115992),
    ) else {
        return false;
    };
    yy.checked_add(rc).is_some()
        && yy.checked_sub(gc).and_then(|g| g.checked_sub(gu)).is_some()
        && yy.checked_add(bc).is_some()
}

fn valid_pixels() -> Vec<([i16; 3], [u8; 4])> {
    // 极值、灰度阈值及既有常量 discriminator 的独立固定锚点。
    let mut pixels = vec![
        ([i16::MIN, 0, 0], [0, 0, 0, 255]),
        ([28671, 0, 0], [255, 255, 255, 255]),
        ([-4096, 18514, 23363], [255, 0, 255, 255]),
        ([-4096, -18514, -23363], [0, 255, 0, 255]),
        ([-4065, 0, 0], [0, 0, 0, 255]),
        ([-4064, 0, 0], [1, 1, 1, 255]),
        ([4063, 0, 0], [254, 254, 254, 255]),
        ([4064, 0, 0], [255, 255, 255, 255]),
        ([-8265, 8234, -20324], [0, 234, 255, 255]),
        ([0, 0, 0], [128, 128, 128, 255]),
    ];
    assert!(pixels.iter().all(|(p, _)| valid(p[0], p[1], p[2])));
    // 与已冻结 300000/seed23 registry 输入流相同，有限取前 64 个合法键。
    // 每个期望字节来自原版记录，不链接原版 decoder，也不生成新的 oracle。
    let mut state = 23u32;
    let mut accepted = 0;
    for _ in 0..100000 {
        let p = std::array::from_fn::<_, 3, _>(|_| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 16) as i16
        });
        if valid(p[0], p[1], p[2]) {
            let mut expected = [0; 4];
            ycbcr_to_rgba(
                YCbCrBuffer {
                    y: &p[..1],
                    cb: &p[1..2],
                    cr: &p[2..],
                },
                &mut expected,
            )
            .unwrap();
            pixels.push((p, expected));
            accepted += 1;
            if accepted == 64 {
                break;
            }
        }
    }
    assert_eq!(accepted, 64);
    pixels
}

#[test]
fn mixed_extremes_frozen_bytes_all_tails_and_source_immutability() {
    let k = native();
    let pixels = valid_pixels();
    for len in [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 15, 16, 17, 63, 64, 65, 4093, 4094, 4095, 4096, 4097, 4098,
        4099,
    ] {
        for offset in [1, 3, 7, 15] {
            let mut y = vec![i16::MAX; len + 6];
            let mut cb = y.clone();
            let mut cr = y.clone();
            let mut expected = vec![0xa5; offset + len * 4 + 19];
            for i in 0..len {
                let (p, rgba) = pixels[i % pixels.len()];
                y[i + 3] = p[0];
                cb[i + 3] = p[1];
                cr[i + 3] = p[2];
                expected[offset + i * 4..offset + i * 4 + 4].copy_from_slice(&rgba);
            }
            let before = (y.clone(), cb.clone(), cr.clone());
            let mut actual = vec![0xa5; expected.len()];
            assert_eq!(
                k.ycbcr_to_rgba(
                    &y[3..3 + len],
                    &cb[3..3 + len],
                    &cr[3..3 + len],
                    &mut actual[offset..offset + len * 4],
                ),
                Ok(()),
                "len={len} offset={offset}"
            );
            assert_eq!(actual, expected, "len={len} offset={offset}");
            assert_eq!((y, cb, cr), before, "len={len} offset={offset}");
        }
    }
}

fn reject_without_mutation(k: NativeKernels, y: &[i16], cb: &[i16], cr: &[i16]) {
    assert!(y
        .iter()
        .zip(cb)
        .zip(cr)
        .any(|((&y, &cb), &cr)| !valid(y, cb, cr)));
    let before = (y.to_vec(), cb.to_vec(), cr.to_vec());
    let mut out = vec![0xa5; 3 + y.len() * 4 + 17];
    let untouched = out.clone();
    assert_eq!(
        k.ycbcr_to_rgba(y, cb, cr, &mut out[3..3 + y.len() * 4]),
        Err(KernelError::ColorOverflow)
    );
    assert_eq!(out, untouched);
    assert_eq!((y, cb, cr), (&before.0[..], &before.1[..], &before.2[..]));
}

#[test]
fn late_illegal_y_cr_cb_and_checked_intermediates_preserve_every_output_byte() {
    let k = native();
    let illegal = [
        [28672, 0, 0],
        [-4096, 0, 23364],
        [-4096, 0, -23364],
        [-4096, 18515, 0],
        [-4096, -18515, 0],
        [28671, 0, 1],
        [i16::MIN, 0, -3000],
        [28671, 0, -2],
        [i16::MIN, 0, 6000],
        [28671, -3, 0],
        [i16::MIN, 12000, 0],
        // g 第一次减法溢出，第二次减法后数学结果可回到 i32；禁止重结合。
        [28668, 2, -6],
        [28671, 1, 0],
        [i16::MIN, -3000, 0],
    ];
    for p in illegal {
        assert!(!valid(p[0], p[1], p[2]), "{p:?}");
        // 后一个向量每个实际 lane，含 1/2/3 个元素短尾，都位于合法前缀之后。
        for len in 13..=16 {
            for index in 8..len {
                let mut y = vec![0; len];
                let mut cb = y.clone();
                let mut cr = y.clone();
                y[index] = p[0];
                cb[index] = p[1];
                cr[index] = p[2];
                reject_without_mutation(k, &y, &cb, &cr);
            }
        }
        for len in 4093..=4096 {
            let mut y = vec![0; len];
            let mut cb = y.clone();
            let mut cr = y.clone();
            y[len - 1] = p[0];
            cb[len - 1] = p[1];
            cr[len - 1] = p[2];
            reject_without_mutation(k, &y, &cb, &cr);
        }
    }
}

#[test]
fn mixed_illegal_lanes_after_valid_prefix_are_atomic() {
    let k = native();
    for len in 13..=16 {
        let mut y = vec![0; len];
        let mut cb = y.clone();
        let mut cr = y.clone();
        y[8] = 28672;
        cr[9] = 23364;
        cb[10] = 18515;
        y[len - 1] = i16::MIN;
        cb[len - 1] = -3000;
        reject_without_mutation(k, &y, &cb, &cr);
    }
}

#[test]
fn exact_length_errors_precede_color_overflow_and_leave_output_untouched() {
    let k = native();
    for len in [1, 3, 4, 5, 4096] {
        let y = vec![i16::MAX; len];
        let cb = vec![0; len];
        let cr = cb.clone();
        let before = (y.clone(), cb.clone(), cr.clone());
        for (us, vs, out_len) in [
            (&cb[..len - 1], &cr[..], len * 4),
            (&cb[..], &cr[..len - 1], len * 4),
            (&cb[..], &cr[..], len * 4 - 1),
            (&cb[..], &cr[..], len * 4 + 1),
        ] {
            let mut out = vec![0xa5; 1 + out_len + 17];
            let untouched = out.clone();
            assert_eq!(
                k.ycbcr_to_rgba(&y, us, vs, &mut out[1..1 + out_len]),
                Err(KernelError::Length)
            );
            assert_eq!(out, untouched);
        }
        assert_eq!((y, cb, cr), before);
    }
    let mut out = [0xa5; 3];
    assert_eq!(
        k.ycbcr_to_rgba(&[], &[], &[], &mut out[1..2]),
        Err(KernelError::Length)
    );
    assert_eq!(out, [0xa5; 3]);
}
