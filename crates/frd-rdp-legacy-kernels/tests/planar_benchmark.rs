//! 有界 host microbench；普通优化 Rust oracle 允许自动向量化，均复用输出。
#[path = "common/planar.rs"]
mod oracle;
use frd_rdp_legacy_kernels::{NativeKernels, PlanarLayout};
use oracle::*;
use sha2::{Digest, Sha256};
use std::{hint::black_box, time::Instant};
fn measure(f: &mut impl FnMut()) -> (u128, u128) {
    for _ in 0..128 {
        f();
    }
    let mut times = Vec::with_capacity(501);
    for _ in 0..501 {
        let start = Instant::now();
        for _ in 0..64 {
            f();
        }
        times.push(start.elapsed().as_nanos() / 64);
    }
    times.sort_unstable();
    (times[250], times[475])
}
fn compare(name: &str, native: &mut impl FnMut(), reference: &mut impl FnMut()) {
    let (nm, np) = measure(native);
    let (rm, rp) = measure(reference);
    println!("{name}: native_median_ns={nm} native_p95_ns={np} reference_median_ns={rm} reference_p95_ns={rp} reference_over_native={:.3}",rm as f64/nm as f64);
}
#[test]
#[ignore = "显式 release host benchmark，无性能断言"]
#[allow(clippy::assertions_on_constants)]
fn planar_bounded_host_benchmark() {
    assert!(!cfg!(debug_assertions));
    let k = NativeKernels::detect().unwrap();
    println!(
        "backend={:?} arch={} os={} input_sha256={:x} warmup=128 samples=501 batch=64",
        k.backend(),
        std::env::consts::ARCH,
        std::env::consts::OS,
        Sha256::digest(bytes(4096, 1))
    );
    println!("baseline=普通优化Rust逐pixel色彩/delta/pack循环；compiler允许自动向量化；native包含checked入口与尾部staging；双方预分配；delta双方每次先重置copy；native先计时");
    for n in [1, 7, 8, 16, 65, 1024, 4096] {
        let y = bytes(n, 1);
        let co = bytes(n, 2);
        let cg = bytes(n, 3);
        let mut a = vec![0; n * 3];
        let mut b = a.clone();
        compare(
            &format!("RGB/{n}"),
            &mut || {
                k.rgb_planes_to_rgb24(
                    black_box(&y),
                    black_box(&co),
                    black_box(&cg),
                    black_box(&mut a),
                )
                .unwrap();
                black_box(&a);
            },
            &mut || {
                rgb(
                    black_box(&y),
                    black_box(&co),
                    black_box(&cg),
                    black_box(&mut b),
                );
                black_box(&b);
            },
        );
        assert_eq!(a, b);
        let mut a = vec![0; n];
        let mut b = a.clone();
        compare(
            &format!("delta/{n}"),
            &mut || {
                a.copy_from_slice(black_box(&cg));
                k.resolve_planar_delta(black_box(&y), black_box(&mut a))
                    .unwrap();
                black_box(&a);
            },
            &mut || {
                b.copy_from_slice(black_box(&cg));
                delta(black_box(&y), black_box(&mut b));
                black_box(&b);
            },
        );
        assert_eq!(a, b);
        for sub in [false, true] {
            let l = PlanarLayout {
                width: n,
                height: 1,
                subsampled: sub,
                color_loss_level: 7,
                has_alpha: false,
            };
            let c = if sub { n.div_ceil(2) } else { n };
            let co = &co[..c];
            let cg = &cg[..c];
            let mut a = vec![0; 3 * n];
            let mut b = a.clone();
            compare(
                &format!("YCoCg/{n}/sub={sub}"),
                &mut || {
                    k.ycocg_planes_to_rgb24(
                        black_box(l),
                        black_box(&y),
                        black_box(co),
                        black_box(cg),
                        black_box(&mut a),
                    )
                    .unwrap();
                    black_box(&a);
                },
                &mut || {
                    color(
                        black_box(l),
                        black_box(&y),
                        black_box(co),
                        black_box(cg),
                        black_box(&mut b),
                    );
                    black_box(&b);
                },
            );
            assert_eq!(a, b);
        }
    }
}
