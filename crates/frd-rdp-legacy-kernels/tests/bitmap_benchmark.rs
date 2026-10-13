//! 有界本机 microbench；reference 是普通优化 Rust 循环/标准 slice copy，允许编译器优化。
#[path = "common/bitmap.rs"]
mod oracle;
use frd_rdp_legacy_kernels::NativeKernels;
use oracle::*;
use sha2::{Digest, Sha256};
use std::{hint::black_box, time::Instant};
fn measure(f: &mut impl FnMut()) -> (u128, u128) {
    for _ in 0..128 {
        f();
    }
    let mut samples = Vec::with_capacity(501);
    for _ in 0..501 {
        let start = Instant::now();
        for _ in 0..64 {
            f();
        }
        samples.push(start.elapsed().as_nanos() / 64);
    }
    samples.sort_unstable();
    (samples[250], samples[475])
}
fn compare(name: &str, native: &mut impl FnMut(), reference: &mut impl FnMut()) {
    let (nm, np) = measure(native);
    let (rm, rp) = measure(reference);
    println!("{name}: native_median_ns={nm} native_p95_ns={np} reference_median_ns={rm} reference_p95_ns={rp} reference_over_native={:.3}", rm as f64 / nm as f64);
}
#[test]
#[ignore = "显式 release 运行；不设置性能断言"]
#[allow(clippy::assertions_on_constants)]
fn packed_leaf_bounded_host_benchmark() {
    assert!(!cfg!(debug_assertions), "benchmark 要求 --release");
    let k = NativeKernels::detect().unwrap();
    let a = [0xb7, 0x83, 0x29];
    let b = [0x34, 0xe1, 0x56];
    println!(
        "backend={:?} arch={} os={} input_sha256={:x} warmup=128 samples=501 batch=64",
        k.backend(),
        std::env::consts::ARCH,
        std::env::consts::OS,
        Sha256::digest(bytes(3072))
    );
    println!("baseline=普通优化Rust逐pixel循环与标准slice-copy；native含checked入口/pattern准备，无分配；reference无入口预检；compiler可能自动向量化reference；计时顺序native后reference");
    for (depth, width) in DEPTHS {
        for n in [1, 7, 8, 16, 65, 1024] {
            let src = bytes(n * width);
            let mut native_out = vec![0; src.len()];
            let mut reference_out = native_out.clone();
            k.copy_u8(&src, &mut native_out).unwrap();
            assert_eq!(native_out, src);
            compare(
                &format!("copy/{depth:?}/{n}"),
                &mut || {
                    k.copy_u8(black_box(&src), black_box(&mut native_out))
                        .unwrap();
                    black_box(&native_out);
                },
                &mut || {
                    black_box(&mut reference_out).copy_from_slice(black_box(&src));
                    black_box(&reference_out);
                },
            );
            assert_eq!(native_out, reference_out);
            k.fill_pixel(depth, &a[..width], &mut native_out).unwrap();
            assert_eq!(native_out, fill(&a[..width], n));
            compare(
                &format!("fill/{depth:?}/{n}"),
                &mut || {
                    k.fill_pixel(
                        black_box(depth),
                        black_box(&a[..width]),
                        black_box(&mut native_out),
                    )
                    .unwrap();
                    black_box(&native_out);
                },
                &mut || {
                    let aa = black_box(&a[..width]);
                    for p in black_box(&mut reference_out).chunks_exact_mut(width) {
                        p.copy_from_slice(aa);
                    }
                    black_box(&reference_out);
                },
            );
            assert_eq!(native_out, reference_out);
            k.xor_previous(depth, &src, &a[..width], &mut native_out)
                .unwrap();
            assert_eq!(native_out, xor(&src, &a[..width]));
            compare(
                &format!("xor/{depth:?}/{n}"),
                &mut || {
                    k.xor_previous(
                        black_box(depth),
                        black_box(&src),
                        black_box(&a[..width]),
                        black_box(&mut native_out),
                    )
                    .unwrap();
                    black_box(&native_out);
                },
                &mut || {
                    let aa = black_box(&a[..width]);
                    for (p, bg) in black_box(&mut reference_out)
                        .chunks_exact_mut(width)
                        .zip(black_box(&src).chunks_exact(width))
                    {
                        for j in 0..width {
                            p[j] = bg[j] ^ aa[j];
                        }
                    }
                    black_box(&reference_out);
                },
            );
            assert_eq!(native_out, reference_out);
            k.dither_pixels(depth, &a[..width], &b[..width], true, &mut native_out)
                .unwrap();
            assert_eq!(native_out, dither(&a[..width], &b[..width], true, n));
            compare(
                &format!("dither/{depth:?}/{n}"),
                &mut || {
                    k.dither_pixels(
                        black_box(depth),
                        black_box(&a[..width]),
                        black_box(&b[..width]),
                        black_box(true),
                        black_box(&mut native_out),
                    )
                    .unwrap();
                    black_box(&native_out);
                },
                &mut || {
                    let aa = black_box(&a[..width]);
                    let bb = black_box(&b[..width]);
                    for (i, p) in black_box(&mut reference_out)
                        .chunks_exact_mut(width)
                        .enumerate()
                    {
                        p.copy_from_slice(if i % 2 == 0 { bb } else { aa });
                    }
                    black_box(&reference_out);
                },
            );
            assert_eq!(native_out, reference_out);
        }
        for n in [1, 7, 8] {
            let src = bytes(n * width);
            let mut native_out = vec![0; src.len()];
            let mut reference_out = native_out.clone();
            for with_previous in [false, true] {
                let previous = with_previous.then_some(src.as_slice());
                k.mask_previous(depth, previous, &a[..width], 0x97, 0, &mut native_out)
                    .unwrap();
                assert_eq!(native_out, mask(previous, &a[..width], 0x97, 0, n));
                compare(
                    &format!("mask/{depth:?}/{n}/previous={with_previous}"),
                    &mut || {
                        k.mask_previous(
                            black_box(depth),
                            black_box(previous),
                            black_box(&a[..width]),
                            black_box(0x97),
                            black_box(0),
                            black_box(&mut native_out),
                        )
                        .unwrap();
                        black_box(&native_out);
                    },
                    &mut || {
                        let aa = black_box(&a[..width]);
                        let prev = black_box(previous);
                        let bits = black_box(0x97);
                        for (i, p) in black_box(&mut reference_out)
                            .chunks_exact_mut(width)
                            .enumerate()
                        {
                            for j in 0..width {
                                let bg = prev.map_or(0, |s| s[i * width + j]);
                                p[j] = bg ^ if bits & (1 << i) != 0 { aa[j] } else { 0 };
                            }
                        }
                        black_box(&reference_out);
                    },
                );
                assert_eq!(native_out, reference_out);
            }
        }
    }
}
