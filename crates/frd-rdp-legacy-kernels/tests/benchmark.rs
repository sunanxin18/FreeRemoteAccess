use frd_rdp_legacy_kernels::{NativeKernels, Quantization};
use ironrdp_graphics::{
    color_conversion::{ycbcr_to_rgba, YCbCrBuffer},
    dwt, quantization, rlgr, subband_reconstruction,
};
use ironrdp_pdu::{
    codecs::rfx::{Block, CodecChannel, Quant},
    Decode, ReadCursor,
};
use sha2::{Digest, Sha256};
use std::{hint::black_box, time::Instant};

fn measure(name: &str, mut f: impl FnMut()) -> u128 {
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
    times[250]
}
fn q(q: &Quant) -> Quantization {
    Quantization([
        q.hl1, q.lh1, q.hh1, q.hl2, q.lh2, q.hh2, q.hl3, q.lh3, q.hh3, q.ll3,
    ])
}
#[test]
#[ignore = "显式运行 release benchmark；不设置性能断言"]
// 允许 debug 编译其他测试，只在用户显式运行此测试时拒绝 debug benchmark。
#[allow(clippy::assertions_on_constants)]
fn native_leaf_benchmark() {
    let k = NativeKernels::detect().unwrap();
    let wire = include_bytes!("fixtures/encoded_messages.bin");
    println!(
        "backend={:?} arch={} os={} debug_assertions={} input_sha256={:x}",
        k.backend(),
        std::env::consts::ARCH,
        std::env::consts::OS,
        cfg!(debug_assertions),
        Sha256::digest(wire)
    );
    assert!(!cfg!(debug_assertions), "benchmark 要求 --release");
    let mut cursor = ReadCursor::new(wire);
    let set = loop {
        if let Block::CodecChannel(CodecChannel::TileSet(set)) = Block::decode(&mut cursor).unwrap()
        {
            break set;
        }
    };
    let tile = &set.tiles[0];
    let wires = [tile.y_data, tile.cb_data, tile.cr_data];
    let qs = [
        &set.quants[usize::from(tile.y_quant_index)],
        &set.quants[usize::from(tile.cb_quant_index)],
        &set.quants[usize::from(tile.cr_quant_index)],
    ];
    let mut coefficients = vec![vec![0; 4096]; 3];
    for i in 0..3 {
        rlgr::decode(set.entropy_algorithm, wires[i], &mut coefficients[i]).unwrap();
    }
    let mut decoded = coefficients.clone();
    let mut scratch = vec![0; 4096];
    for i in 0..3 {
        subband_reconstruction::decode(&mut decoded[i][4032..]);
        quantization::decode(&mut decoded[i], qs[i]);
        dwt::decode(&mut decoded[i], &mut scratch);
    }
    let mut data = vec![0; 4096];
    let mut out = vec![0; 16384];
    let mut expected = out.clone();
    ycbcr_to_rgba(
        YCbCrBuffer {
            y: &decoded[0],
            cb: &decoded[1],
            cr: &decoded[2],
        },
        &mut expected,
    )
    .unwrap();
    let comparison = |name: &str, native_ns: u128, oracle_ns: u128| {
        println!(
            "{name}: oracle_over_native={:.3}",
            oracle_ns as f64 / native_ns as f64
        )
    };
    let n = measure("dequant_native_4096", || {
        data.copy_from_slice(&coefficients[0]);
        k.dequantize(black_box(&mut data), q(qs[0])).unwrap();
    });
    let o = measure("dequant_oracle_4096", || {
        data.copy_from_slice(&coefficients[0]);
        quantization::decode(black_box(&mut data), qs[0]);
    });
    comparison("dequant", n, o);
    let n = measure("dwt_native_4096", || {
        data.copy_from_slice(&coefficients[0]);
        k.inverse_dwt(black_box(&mut data), &mut scratch).unwrap();
    });
    let o = measure("dwt_oracle_4096", || {
        data.copy_from_slice(&coefficients[0]);
        dwt::decode(black_box(&mut data), &mut scratch);
    });
    comparison("dwt", n, o);
    let n = measure("color_native_4096", || {
        k.ycbcr_to_rgba(&decoded[0], &decoded[1], &decoded[2], black_box(&mut out))
            .unwrap();
    });
    let o = measure("color_oracle_4096", || {
        ycbcr_to_rgba(
            YCbCrBuffer {
                y: &decoded[0],
                cb: &decoded[1],
                cr: &decoded[2],
            },
            black_box(&mut out),
        )
        .unwrap();
    });
    comparison("color", n, o);
    measure("zero_native_4096", || {
        k.zero_i16(black_box(&mut data));
    });
    measure("zero_oracle_4096", || {
        black_box(&mut data).fill(0);
    });
    measure("ll3_native_64", || {
        data[..64].copy_from_slice(&coefficients[0][4032..]);
        k.differential(black_box(&mut data[..64]));
    });
    measure("ll3_oracle_64", || {
        data[..64].copy_from_slice(&coefficients[0][4032..]);
        subband_reconstruction::decode(black_box(&mut data[..64]));
    });
    measure("rlgr_original_parser_and_materialization_4096", || {
        rlgr::decode(set.entropy_algorithm, wires[0], black_box(&mut data)).unwrap();
    });
    // 预分配输出和 scratch；component/full-tile 包含原版 RLGR 的 materialization。
    let n = measure("component_native_composition_4096", || {
        rlgr::decode(set.entropy_algorithm, wires[0], &mut data).unwrap();
        k.differential(&mut data[4032..]);
        k.dequantize(&mut data, q(qs[0])).unwrap();
        k.inverse_dwt(black_box(&mut data), &mut scratch).unwrap();
    });
    let o = measure("component_oracle_4096", || {
        rlgr::decode(set.entropy_algorithm, wires[0], &mut data).unwrap();
        subband_reconstruction::decode(&mut data[4032..]);
        quantization::decode(&mut data, qs[0]);
        dwt::decode(black_box(&mut data), &mut scratch);
    });
    comparison("component", n, o);
    let mut planes = vec![vec![0; 4096]; 3];
    let n = measure("tile_native_composition_3x4096", || {
        for i in 0..3 {
            rlgr::decode(set.entropy_algorithm, wires[i], &mut planes[i]).unwrap();
            k.differential(&mut planes[i][4032..]);
            k.dequantize(&mut planes[i], q(qs[i])).unwrap();
            k.inverse_dwt(&mut planes[i], &mut scratch).unwrap();
        }
        k.ycbcr_to_rgba(&planes[0], &planes[1], &planes[2], black_box(&mut out))
            .unwrap();
    });
    assert_eq!(out, expected);
    let o = measure("tile_oracle_3x4096", || {
        for i in 0..3 {
            rlgr::decode(set.entropy_algorithm, wires[i], &mut planes[i]).unwrap();
            subband_reconstruction::decode(&mut planes[i][4032..]);
            quantization::decode(&mut planes[i], qs[i]);
            dwt::decode(&mut planes[i], &mut scratch);
        }
        ycbcr_to_rgba(
            YCbCrBuffer {
                y: &planes[0],
                cb: &planes[1],
                cr: &planes[2],
            },
            black_box(&mut out),
        )
        .unwrap();
    });
    assert_eq!(out, expected);
    comparison("tile", n, o);
}
