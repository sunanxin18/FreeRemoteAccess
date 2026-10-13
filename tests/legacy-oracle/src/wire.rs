//! 原版 public API 的真实结果；不推断 API 未公开的 byte-consumption/scratch。
use crate::{format, snapshots};
use ironrdp_graphics::{
    dwt, image_processing::PixelFormat, quantization, rdp6::BitmapStreamDecoder, rle, rlgr,
    subband_reconstruction,
};
use ironrdp_pdu::{
    codecs::rfx::{Block, CodecChannel, EntropyAlgorithm},
    geometry::InclusiveRectangle,
    Decode, ReadCursor,
};
use ironrdp_session::{image::DecodedImage, rfx::DecodingContext};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
const WIRE: &[u8] =
    include_bytes!("../../../crates/frd-rdp-legacy-kernels/tests/fixtures/encoded_messages.bin");
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
// Fixture 字段与公开 API 参数逐一对应，避免隐藏初始/部分输出边界。
#[allow(clippy::too_many_arguments)]
fn entry(
    name: &str,
    codec: &str,
    w: usize,
    h: usize,
    bpp: usize,
    input: &[u8],
    initial: &[u8],
    out: &[u8],
    result: String,
) -> Value {
    json!({"name":name,"codec":codec,"width":w,"height":h,"bpp":bpp,"input":input,"input_sha256":hash(input),"initial":initial,"initial_sha256":hash(initial),"output_or_partial":out,"expected_sha256":hash(out),"result":result,"consumed_bytes":null,"private_scratch":"not exposed by public API"})
}
fn bitmap(
    c: &mut Vec<Value>,
    name: &str,
    w: usize,
    h: usize,
    bpp: usize,
    src: &[u8],
    initial: &[u8],
) {
    let mut out = initial.to_vec();
    let r = rle::decompress(src, &mut out, w, h, bpp);
    c.push(entry(
        name,
        "bitmap",
        w,
        h,
        bpp,
        src,
        initial,
        &out,
        format!("{r:?}"),
    ));
}
fn planar(c: &mut Vec<Value>, name: &str, w: usize, h: usize, src: &[u8]) {
    let mut out = vec![0xa5, 0x17, 0x61];
    let initial = out.clone();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        BitmapStreamDecoder::default().decode_bitmap_stream_to_rgb24(src, &mut out, w, h)
    }));
    let result = match r {
        Ok(r) => format!("{r:?}"),
        Err(_) => "Panic (bounded input, catch_unwind; no output guarantee)".into(),
    };
    c.push(entry(name, "planar", w, h, 32, src, &initial, &out, result));
}
fn command(code: u8, n: usize, depth: usize, extended: bool, mask: u8) -> Vec<u8> {
    let fg: Vec<_> = (0..depth).map(|i| 0xa3 + i as u8 * 17).collect();
    let bg: Vec<_> = (0..depth).map(|i| 0x19 + i as u8 * 29).collect();
    let mut wire = if code >= 0xf0 {
        if code <= 0xf8 {
            vec![code, n as u8, (n >> 8) as u8]
        } else {
            vec![code]
        }
    } else if extended {
        let add = if code == 0x40 || code == 0xd0 {
            1
        } else if code >= 0xc0 {
            16
        } else {
            32
        };
        vec![code, (n - add) as u8]
    } else {
        vec![
            code | if code == 0x40 || code == 0xd0 {
                (n / 8) as u8
            } else {
                n as u8
            },
        ]
    };
    match code {
        0xc0 | 0xf6 => wire.extend(&fg),
        0x60 | 0xf3 => wire.extend(&bg),
        0xe0 | 0xf8 => {
            wire.extend(&fg);
            wire.extend(&bg)
        }
        0xd0 | 0xf7 => {
            wire.extend(&fg);
            wire.extend(vec![mask; n.div_ceil(8)])
        }
        0x40 | 0xf2 => wire.extend(vec![mask; n.div_ceil(8)]),
        0x80 | 0xf4 => wire.extend((0..n * depth).map(|i| (i * 37 + 19) as u8)),
        _ => {}
    }
    // FG checks a pixel without consuming it; following BLACK commands satisfy it.
    if code == 0x20 || code == 0xf1 {
        wire.extend(vec![0xfe; depth]);
    }
    wire
}
pub fn generate() -> (Value, Vec<u8>) {
    let mut c = Vec::new();
    let codes = [
        0x00, 0x20, 0x40, 0x60, 0x80, 0xc0, 0xd0, 0xe0, 0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf6, 0xf7,
        0xf8, 0xf9, 0xfa, 0xfd, 0xfe,
    ];
    for bpp in [8usize, 15, 16, 24] {
        let depth = bpp.div_ceil(8);
        for code in codes {
            for next in [false, true] {
                for ext in [false, true] {
                    if ext && code >= 0xf0 {
                        continue;
                    }
                    let n = if ext {
                        if code == 0x40 || code == 0xd0 {
                            9
                        } else {
                            33
                        }
                    } else {
                        8
                    };
                    let mut wire = if next {
                        command(0x80, 8, depth, false, 0)
                    } else {
                        vec![]
                    };
                    wire.extend(command(code, n, depth, ext, 0xa5));
                    bitmap(
                        &mut c,
                        &format!("code-{code:02x}-next-{next}-extended-{ext}/{bpp}"),
                        8,
                        12,
                        bpp,
                        &wire,
                        &[0x77; 17],
                    );
                }
            }
        }
        for code in [
            0x00, 0x20, 0x40, 0x60, 0x80, 0xc0, 0xd0, 0xe0, 0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf6,
            0xf7, 0xf8,
        ] {
            bitmap(
                &mut c,
                &format!("cross-three-rows-{code:02x}/{bpp}"),
                3,
                12,
                bpp,
                &command(
                    code,
                    if code == 0x40 || code == 0xd0 { 8 } else { 9 },
                    depth,
                    false,
                    0x96,
                ),
                &[],
            );
        }
        for mask in 0..=255 {
            for n in 1..=8 {
                let wire = command(0x40, n, depth, true, mask);
                bitmap(
                    &mut c,
                    &format!("mask-{mask}-tail-{n}/{bpp}"),
                    3,
                    3,
                    bpp,
                    &wire,
                    &[],
                );
            }
        }
        for mask in 0..=255 {
            for n in 1..=8 {
                let mut wire = command(0x80, 3, depth, false, 0);
                wire.extend(command(0x40, n, depth, true, mask));
                bitmap(
                    &mut c,
                    &format!("next-mask-{mask}-tail-{n}/{bpp}"),
                    3,
                    4,
                    bpp,
                    &wire,
                    &[],
                );
            }
        }
        let mut persistent = command(0xc0, 3, depth, false, 0);
        persistent.extend(command(0x20, 6, depth, false, 0));
        bitmap(
            &mut c,
            &format!("set-fg-persists/{bpp}"),
            3,
            5,
            bpp,
            &persistent,
            &[],
        );
        for (name, wire, w, h) in [
            ("consecutive-bg", vec![2, 2, 2, 2], 3, 4),
            ("bg-exit-first", vec![3, 3, 3], 3, 4),
            ("empty-src", vec![], 3, 2),
            ("bad-order", vec![0xff], 3, 2),
            ("short-fg", vec![0x21], 1, 1),
            ("source-before-size", vec![0x63], 1, 1),
            ("size-error", command(0x60, 8, depth, false, 0), 1, 1),
            ("zero-mega", vec![0xf0, 0, 0], 2, 2),
            ("short-header", vec![0xf0, 1], 2, 2),
            ("empty-image-priority", vec![0xff], 0, 0),
            ("partial-then-error", vec![0xfd, 0xff], 3, 2),
        ] {
            bitmap(
                &mut c,
                &format!("{name}/{bpp}"),
                w,
                h,
                bpp,
                &wire,
                &[0x7b; 17],
            );
        }
    }
    bitmap(&mut c, "invalid-bpp-priority", 0, 0, 7, &[0xff], &[0x11]);
    // 完整 raw/RLE header 笛卡尔积，ARGB 忽略 CS，alpha 真实非 ff；奇数/最小几何。
    for w in [1usize, 3] {
        for h in [1usize, 3] {
            for cll in 0..=7u8 {
                for alpha in [false, true] {
                    for cs in [false, true] {
                        for compressed in [false, true] {
                            let header = cll
                                | if alpha { 0 } else { 0x20 }
                                | if cs { 8 } else { 0 }
                                | if compressed { 0x10 } else { 0 };
                            let sub = cll != 0 && cs;
                            let mut src = vec![header];
                            let mut dims = vec![];
                            if alpha {
                                dims.push((w, h));
                            }
                            dims.push((w, h));
                            for _ in 0..2 {
                                dims.push(if sub {
                                    (w.div_ceil(2), h.div_ceil(2))
                                } else {
                                    (w, h)
                                });
                            }
                            for (p, (pw, ph)) in dims.into_iter().enumerate() {
                                for row in 0..ph {
                                    if compressed {
                                        src.push((pw as u8) << 4);
                                    }
                                    src.extend(
                                        (0..pw).map(|x| (p * 73 + row * 41 + x * 29 + 17) as u8),
                                    );
                                }
                            }
                            if !compressed {
                                src.push(0x9b);
                            }
                            let name = format!("headers-{header:02x}-{w}x{h}");
                            planar(&mut c, &name, w, h, &src);
                            for end in 0..src.len() {
                                planar(
                                    &mut c,
                                    &format!("{name}-truncate-{end}"),
                                    w,
                                    h,
                                    &src[..end],
                                );
                            }
                            let mut extra = src.clone();
                            extra.extend([0xde, 0xad]);
                            planar(&mut c, &format!("{name}-extra"), w, h, &extra);
                        }
                    }
                }
            }
        }
    }
    for raw in 0..=15u8 {
        for low in [1, 2] {
            let width = if low == 1 { 16 } else { 32 } + usize::from(raw);
            let src = [0x30, (raw << 4) | low, (raw << 4) | low, (raw << 4) | low];
            planar(
                &mut c,
                &format!("special-repeat-{raw}-{low}"),
                width,
                1,
                &src,
            );
        }
    }
    planar(
        &mut c,
        "literal-repeat-row-reset",
        8,
        2,
        &[0x30, 0x17, 0x29, 0x08, 0x17, 0x43, 0x08, 0x17, 0x81, 0x08],
    );
    let row_reset = [0x17, 0xff, 0x04, 0x40, 0x01, 0x02, 0x03, 0x04];
    let mut reset_wire = vec![0x30];
    for _ in 0..3 {
        reset_wire.extend(row_reset);
    }
    planar(
        &mut c,
        "upstream-plane-row-reset-in-complete-wire",
        8,
        2,
        &reset_wire,
    );
    let plane = [
        255u8, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 254, 0, 253, 1,
    ];
    let mut expected = vec![0xa5, 0x17, 0x61];
    for value in plane {
        expected.extend([value; 3]);
    }
    assert_eq!(c.last().unwrap()["result"], "Ok(())");
    assert_eq!(c.last().unwrap()["output_or_partial"], json!(expected));
    for (name, w, h, src) in [
        (
            "plane-fragment-is-not-complete-bitmap",
            8,
            2,
            vec![0x17, 0xff, 0x04, 0x40, 0x01, 0x02, 0x03, 0x04],
        ),
        ("zero-control", 1, 1, vec![0x30, 0]),
        ("oversize-segment", 1, 1, vec![0x30, 0x20, 1, 2]),
        ("bad-alpha", 3, 3, vec![0x10, 0x40, 1]),
    ] {
        planar(&mut c, name, w, h, &src);
    }
    for (name, w, h) in [
        ("32x64_rgb_raw", 32, 64),
        ("64x24_argb_rle", 64, 24),
        ("64x64_aycocg_rle", 64, 64),
        ("64x64_ycocg_rle_ss", 64, 64),
        ("64x35_ycocg_rle_ss", 64, 35),
        ("64x64_ycocg_raw_ss", 64, 64),
    ] {
        let src = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/upstream")
                .join(format!("{name}.bin")),
        )
        .unwrap();
        planar(&mut c, name, w, h, &src);
    }
    for mode in [EntropyAlgorithm::Rlgr1, EntropyAlgorithm::Rlgr3] {
        for run in [0, 1, 7, 8, 63, 64, 4096] {
            let mut input = vec![0; run + 4];
            input[0] = 3;
            input[run + 1] = -7;
            input[run + 2] = 5;
            let mut wire = vec![0; 65536];
            let n = rlgr::encode(mode, &input, &mut wire).unwrap();
            wire.truncate(n);
            for size in [0, 1, 4095, 4096, 4097, input.len() + 17] {
                for end in 0..=wire.len() {
                    let mut out = vec![99; size];
                    let initial = format::i16_bytes(&out);
                    let result = rlgr::decode(mode, &wire[..end], &mut out);
                    c.push(entry(
                        &format!("rlgr-{mode:?}-run-{run}-size-{size}-end-{end}"),
                        "rlgr",
                        size,
                        1,
                        0,
                        &wire[..end],
                        &initial,
                        &format::i16_bytes(&out),
                        format!("{result:?}"),
                    ));
                }
            }
        }
        for wire in [&[0][..], &[0xc0][..], &[0xe0][..]] {
            for size in [2, 64] {
                let mut out = vec![99; size];
                let r = rlgr::decode(mode, wire, &mut out);
                c.push(entry(
                    &format!("rlgr-pad-{mode:?}-{wire:?}-size-{size}"),
                    "rlgr",
                    size,
                    1,
                    0,
                    wire,
                    &format::i16_bytes(&vec![99; size]),
                    &format::i16_bytes(&out),
                    format!("{r:?}"),
                ));
            }
        }
    }
    for mode in [EntropyAlgorithm::Rlgr1, EntropyAlgorithm::Rlgr3] {
        for wire in [
            vec![0xff; 8],
            vec![0xff; 32],
            vec![0xff; 64],
            vec![0, 0, 0xff],
            vec![0xc0],
            vec![0xe0],
        ] {
            for bits in 1..=8 {
                let mut prefix = wire.clone();
                let last = prefix.len() - 1;
                prefix[last] &= 0xff << (8 - bits);
                let mut output = vec![99; 4096];
                let result = rlgr::decode(mode, &prefix, &mut output);
                c.push(entry(
                    &format!(
                        "rlgr-key-bits-{mode:?}-{}-{bits}-{:02x}",
                        wire.len(),
                        wire[0]
                    ),
                    "rlgr",
                    4096,
                    1,
                    0,
                    &prefix,
                    &format::i16_bytes(&[99; 4096]),
                    &format::i16_bytes(&output),
                    format!("{result:?}"),
                ));
            }
        }
    }
    // 初始 RL 模式：1/0/sign + 16384 unary ones + 0/0，magnitude 超过 i16。
    // 有界约2 KiB真实 wire；没有复制decoder数值输出。
    for mode in [EntropyAlgorithm::Rlgr1, EntropyAlgorithm::Rlgr3] {
        for sign in [0, 1] {
            let mut bits = vec![1, 0, sign];
            bits.extend(vec![1; 16384]);
            bits.extend([0, 0]);
            let mut wire = vec![0u8; bits.len().div_ceil(8)];
            for (i, bit) in bits.into_iter().enumerate() {
                wire[i / 8] |= bit << (7 - i % 8);
            }
            let mut output = [99; 8];
            let result = rlgr::decode(mode, &wire, &mut output);
            assert!(matches!(
                result,
                Err(rlgr::RlgrError::InvalidIntegralConversion(_))
            ));
            c.push(entry(
                &format!("rlgr-integral-error-{mode:?}-sign-{sign}"),
                "rlgr",
                8,
                1,
                0,
                &wire,
                &format::i16_bytes(&[99; 8]),
                &format::i16_bytes(&output),
                format!("{result:?}"),
            ));
        }
    }
    let rect = InclusiveRectangle {
        left: 0,
        top: 0,
        right: 63,
        bottom: 63,
    };
    for end in [0, 1, 5, 11, 50, 63, 86, 100, WIRE.len() - 1, WIRE.len()] {
        let mut image = DecodedImage::new(PixelFormat::BgrX32, 64, 64);
        let initial = image.data().to_vec();
        let r = DecodingContext::default().decode(
            &mut image,
            &rect,
            &mut ReadCursor::new(&WIRE[..end]),
        );
        c.push(entry(
            &format!("session-end-{end}"),
            "session",
            64,
            64,
            32,
            &WIRE[..end],
            &initial,
            image.data(),
            format!("{r:?}"),
        ));
    }
    let mut cursor = ReadCursor::new(WIRE);
    let mut composition = b"FRDCMP01".to_vec();
    let mut tiles = 0;
    while !cursor.is_empty() {
        if let Block::CodecChannel(CodecChannel::TileSet(set)) = Block::decode(&mut cursor).unwrap()
        {
            for tile in set.tiles {
                tiles += 1;
                for (encoded, qi) in [
                    (tile.y_data, tile.y_quant_index),
                    (tile.cb_data, tile.cb_quant_index),
                    (tile.cr_data, tile.cr_quant_index),
                ] {
                    let q = &set.quants[usize::from(qi)];
                    let mut coeff = vec![0; 4096];
                    rlgr::decode(set.entropy_algorithm, encoded, &mut coeff).unwrap();
                    composition.extend(snapshots::qbytes(q));
                    composition.extend(format::i16_bytes(&coeff));
                    subband_reconstruction::decode(&mut coeff[4032..]);
                    quantization::decode(&mut coeff, q);
                    dwt::decode(&mut coeff, &mut [0; 4096]);
                    composition.extend(format::i16_bytes(&coeff));
                }
            }
        }
    }
    assert_eq!(tiles, 1);
    (
        json!({"schema":1,"oracle":"registry ironrdp graphics 0.9.0/session 0.11.0; public API debug result plus output/partial","entries":c}),
        composition,
    )
}
