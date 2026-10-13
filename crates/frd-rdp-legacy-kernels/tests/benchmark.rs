//! 系数起点计时；原版同边界/完整 wire 计时位于独立 oracle，绝不计时 fixture/IPC。
#[path = "common/frozen.rs"]
mod frozen;
use frd_rdp_legacy_kernels::{NativeKernels, Quantization};
use sha2::{Digest, Sha256};
use std::{hint::black_box, time::Instant};
fn measure(name: &str, mut f: impl FnMut()) {
    for _ in 0..128 {
        f();
    }
    let mut times = Vec::with_capacity(501);
    for _ in 0..501 {
        let start = Instant::now();
        for _ in 0..16 {
            f();
        }
        times.push(start.elapsed().as_nanos() / 16);
    }
    times.sort_unstable();
    println!(
        "{name}: median_ns={} p95_ns={} warmup=128 samples=501 batch=16",
        times[250], times[475]
    );
}
#[test]
#[ignore = "显式 release benchmark，无性能断言"]
#[allow(clippy::assertions_on_constants)]
fn native_leaf_benchmark() {
    assert!(!cfg!(debug_assertions));
    let k = NativeKernels::detect().unwrap();
    let captured = frozen::composition();
    println!(
        "backend={:?} arch={} os={} debug_assertions={} input_sha256={:x} coefficients_sha256={:x}",
        k.backend(),
        std::env::consts::ARCH,
        std::env::consts::OS,
        cfg!(debug_assertions),
        Sha256::digest(include_bytes!("fixtures/encoded_messages.bin")),
        Sha256::digest(include_bytes!("fixtures/original-composition.bin"))
    );
    let mut data = vec![0; 4096];
    let mut scratch = vec![0; 4096];
    let mut out = vec![0; 16384];
    measure("dequant_native_4096", || {
        data.copy_from_slice(&captured[0].1);
        k.dequantize(black_box(&mut data), Quantization(captured[0].0))
            .unwrap();
    });
    measure("dwt_native_4096", || {
        data.copy_from_slice(&captured[0].1);
        k.inverse_dwt(black_box(&mut data), &mut scratch).unwrap();
    });
    measure("color_native_4096", || {
        k.ycbcr_to_rgba(
            &captured[0].2,
            &captured[1].2,
            &captured[2].2,
            black_box(&mut out),
        )
        .unwrap();
    });
    measure("zero_native_4096", || {
        k.zero_i16(black_box(&mut data));
    });
    measure("ll3_native_64", || {
        data[..64].copy_from_slice(&captured[0].1[4032..]);
        k.differential(black_box(&mut data[..64]));
    });
    measure("component_native_coefficients_4096", || {
        data.copy_from_slice(&captured[0].1);
        k.differential(&mut data[4032..]);
        k.dequantize(&mut data, Quantization(captured[0].0))
            .unwrap();
        k.inverse_dwt(black_box(&mut data), &mut scratch).unwrap();
    });
    assert_eq!(data, captured[0].2);
    let mut planes = vec![vec![0; 4096]; 3];
    measure("tile_native_coefficients_3x4096", || {
        for i in 0..3 {
            planes[i].copy_from_slice(&captured[i].1);
            k.differential(&mut planes[i][4032..]);
            k.dequantize(&mut planes[i], Quantization(captured[i].0))
                .unwrap();
            k.inverse_dwt(&mut planes[i], &mut scratch).unwrap();
        }
        k.ycbcr_to_rgba(&planes[0], &planes[1], &planes[2], black_box(&mut out))
            .unwrap();
    });
    for p in out.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    assert_eq!(out, include_bytes!("fixtures/decoded_image.bin"));
}
