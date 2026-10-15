//! 固定 row 微基准；仅供 root 释放后执行，不代表 consumer / 整 Stage 性能。
#[path = "common/cursor_reference.rs"]
mod reference;
use frd_rdp_legacy_kernels::{CursorBitmapMode as M, CursorXorFormat as F, NativeKernels};
use reference::*;
use std::{hint::black_box, time::Instant};
#[test]
#[ignore = "需要 release；先正确性、ABI、object 再由 root 放行"]
fn fixed_cursor_row_alternating_microbenchmark() {
    assert!(!cfg!(debug_assertions));
    let k = NativeKernels::detect().unwrap();
    eprintln!("cursor rows backend {:?}; cold=first operation per fixture; warmup8 samples31 iterations64; ref/native same preallocated output memory; row scope only",k.backend());
    for n in [1, 3, 4, 7, 16, 33, 64] {
        let mut x = bytes(n * 4, 77);
        let a = bytes(n.div_ceil(8), 17);
        let bg = bytes(n * 4, 27);
        for (i, p) in x.chunks_exact_mut(4).enumerate() {
            p[3] = [0, 1, 2, 127, 128, 254, 255][i % 7];
            if i % 3 == 0 {
                p.copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let expected = decode(F::Bgra32, M::Software, &x, Some(&a), n);
        let expected_blend = copied_original_composite(&expected, &bg);
        for composite in [false, true] {
            let mut out = vec![0; n * 4];
            let mut ref_out = vec![0; n * 4];
            let want = if composite {
                &expected_blend
            } else {
                &expected
            };
            let mut reference_ns = Vec::with_capacity(31);
            let mut native_ns = Vec::with_capacity(31);
            let mut cold = [0u128; 2];
            for sample in 0..40 {
                for native in if sample % 2 == 0 {
                    [false, true]
                } else {
                    [true, false]
                } {
                    let iterations = if sample == 0 { 1 } else { 64 };
                    let start = Instant::now();
                    for _ in 0..iterations {
                        if native {
                            if composite {
                                k.composite_cursor_row(
                                    black_box(&expected),
                                    black_box(&bg),
                                    black_box(&mut out),
                                )
                                .unwrap();
                            } else {
                                k.decode_cursor_row(
                                    F::Bgra32,
                                    M::Software,
                                    black_box(&x),
                                    Some(black_box(&a)),
                                    black_box(&mut out),
                                )
                                .unwrap();
                            }
                            black_box(&out);
                        } else {
                            if composite {
                                copied_original_composite_into(
                                    black_box(&expected),
                                    black_box(&bg),
                                    black_box(&mut ref_out),
                                );
                            } else {
                                decode_into(
                                    F::Bgra32,
                                    M::Software,
                                    black_box(&x),
                                    Some(black_box(&a)),
                                    black_box(&mut ref_out),
                                );
                            }
                            black_box(&ref_out);
                        }
                    }
                    let elapsed = start.elapsed().as_nanos();
                    if sample == 0 {
                        cold[usize::from(native)] = elapsed;
                    } else if sample >= 9 {
                        if native {
                            native_ns.push(elapsed)
                        } else {
                            reference_ns.push(elapsed)
                        }
                    }
                }
                assert_eq!(&out, want);
                assert_eq!(&ref_out, want);
            }
            let checksum: u64 = out.iter().map(|&v| u64::from(v)).sum();
            reference_ns.sort();
            native_ns.sort();
            eprintln!("n{n} composite{composite} cold_ref_ns{} cold_native_ns{} ref_median{} ref_p95{} native_median{} native_p95{} calls_each={} pixels_each={} checksum{checksum} timed_allocations_each=0 output_bytes_each={}",
                cold[0],cold[1],reference_ns[15],reference_ns[29],native_ns[15],native_ns[29],31*64,31*64*n,n*4);
        }
    }
}
