use ironrdp_graphics::{
    color_conversion::{ycbcr_to_rgba, YCbCrBuffer},
    dwt, quantization, rlgr, subband_reconstruction,
};
use ironrdp_pdu::{
    codecs::rfx::{Block, CodecChannel},
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
#[test]
#[ignore = "显式运行 release benchmark；不设置性能断言"]
// 允许 debug 编译其他测试，只在用户显式运行此测试时拒绝 debug benchmark。
#[allow(clippy::assertions_on_constants)]
fn original_decoder_benchmark() {
    let wire = include_bytes!(
        "../../../crates/frd-rdp-legacy-kernels/tests/fixtures/encoded_messages.bin"
    );
    println!(
        "oracle=registry arch={} os={} debug_assertions={} input_sha256={:x}",
        std::env::consts::ARCH,
        std::env::consts::OS,
        cfg!(debug_assertions),
        Sha256::digest(wire)
    );
    println!(
        "coefficients_sha256={:x}",
        Sha256::digest(include_bytes!(
            "../../../crates/frd-rdp-legacy-kernels/tests/fixtures/original-composition.bin"
        ))
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
    measure("dequant_oracle_4096", || {
        data.copy_from_slice(&coefficients[0]);
        quantization::decode(black_box(&mut data), qs[0]);
    });
    measure("dwt_oracle_4096", || {
        data.copy_from_slice(&coefficients[0]);
        dwt::decode(black_box(&mut data), &mut scratch);
    });
    measure("color_oracle_4096", || {
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
    measure("zero_oracle_4096", || {
        black_box(&mut data).fill(0);
    });
    measure("ll3_oracle_64", || {
        data[..64].copy_from_slice(&coefficients[0][4032..]);
        subband_reconstruction::decode(black_box(&mut data[..64]));
    });
    measure("rlgr_original_parser_and_materialization_4096", || {
        rlgr::decode(set.entropy_algorithm, wires[0], black_box(&mut data)).unwrap();
    });
    // 预分配输出和 scratch；component/full-tile 包含原版 RLGR 的 materialization。
    measure("component_oracle_4096", || {
        rlgr::decode(set.entropy_algorithm, wires[0], &mut data).unwrap();
        subband_reconstruction::decode(&mut data[4032..]);
        quantization::decode(&mut data, qs[0]);
        dwt::decode(black_box(&mut data), &mut scratch);
    });
    let mut planes = vec![vec![0; 4096]; 3];
    measure("tile_oracle_3x4096", || {
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

    measure("component_original_coefficients_4096", || {
        data.copy_from_slice(&coefficients[0]);
        subband_reconstruction::decode(&mut data[4032..]);
        quantization::decode(&mut data, qs[0]);
        dwt::decode(black_box(&mut data), &mut scratch);
    });
    measure("tile_original_coefficients_3x4096", || {
        for i in 0..3 {
            planes[i].copy_from_slice(&coefficients[i]);
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
}
