// 独立原版 API fixture producer，不包含任何 parser 或数值公式。
use ironrdp_core::{Decode as _, Encode as _, ReadCursor, WriteCursor};
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_graphics::{dwt, quantization, rlgr, subband_reconstruction};
use ironrdp_pdu::codecs::rfx::{self, EntropyAlgorithm, Quant};
use ironrdp_pdu::geometry::InclusiveRectangle;
use ironrdp_session::{image::DecodedImage, rfx::DecodingContext};
const WIRE: &[u8] = include_bytes!("../../../crates/frd-rdp-legacy-kernels/tests/fixtures/encoded_messages.bin");
fn field(out: &mut Vec<u8>, data: &[u8]) {
    out.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
    out.extend(data);
}
fn record(out: &mut Vec<u8>, name: &str, meta: &[u8], wire: &[u8], initial: &[u8], expected: &[u8], result: &str) {
    for v in [name.as_bytes(), meta, wire, initial, expected, result.as_bytes()] {
        field(out, v);
    }
}
fn bytes(v: &[i16]) -> Vec<u8> {
    v.iter().flat_map(|v| v.to_le_bytes()).collect()
}
fn quant(q: [u8; 10]) -> Quant {
    Quant {
        hl1: q[0],
        lh1: q[1],
        hh1: q[2],
        hl2: q[3],
        lh2: q[4],
        hh2: q[5],
        hl3: q[6],
        lh3: q[7],
        hh3: q[8],
        ll3: q[9],
    }
}
fn encoded(mode: EntropyAlgorithm, v: &[i16]) -> Vec<u8> {
    let mut out = vec![0; 200000];
    let n = rlgr::encode(mode, v, &mut out).unwrap();
    out.truncate(n);
    out
}
fn wire_with(case: &str, mode: EntropyAlgorithm) -> Vec<u8> {
    let mut cursor = ReadCursor::new(WIRE);
    let mut out = Vec::new();
    while !cursor.is_empty() {
        let mut block = rfx::Block::decode(&mut cursor).unwrap();
        match &mut block {
            rfx::Block::Channels(c) => c.0[0].width = 128,
            rfx::Block::CodecChannel(rfx::CodecChannel::Context(c)) => c.entropy_algorithm = mode,
            rfx::Block::CodecChannel(rfx::CodecChannel::Region(r)) => {
                r.rectangles = vec![rfx::RfxRectangle {
                    x: 3,
                    y: 4,
                    width: 122,
                    height: 56,
                }]
            }
            rfx::Block::CodecChannel(rfx::CodecChannel::TileSet(t)) => {
                t.entropy_algorithm = mode;
                // Captured streams are RLGR3; mode conversion consumes original coefficients.
                let source_mode = EntropyAlgorithm::Rlgr3;
                let mut planes = Vec::new();
                for data in [t.tiles[0].y_data, t.tiles[0].cb_data, t.tiles[0].cr_data] {
                    let mut v = vec![0; 4096];
                    rlgr::decode(source_mode, data, &mut v).unwrap();
                    planes.push(encoded(mode, &v));
                }
                let mut first = t.tiles[0].clone();
                first.y_data = Box::leak(planes[0].clone().into_boxed_slice());
                first.cb_data = Box::leak(planes[1].clone().into_boxed_slice());
                first.cr_data = Box::leak(planes[2].clone().into_boxed_slice());
                let mut second = first.clone();
                second.x = 1;
                let channel = if case.contains("cb") {
                    1
                } else if case.contains("cr") {
                    2
                } else {
                    0
                };
                let target = if case.contains("second") {
                    &mut second
                } else {
                    &mut first
                };
                if case.contains("empty") {
                    match channel {
                        0 => target.y_data = &[],
                        1 => target.cb_data = &[],
                        _ => target.cr_data = &[],
                    }
                }
                if case.contains("color") {
                    // Strong LL3 values are legal RLGR, but outside checked color's intermediate domain.
                    let mut v = vec![0; 4096];
                    v[4032] = 32767;
                    let payload = Box::leak(encoded(mode, &v).into_boxed_slice());
                    target.y_data = payload;
                    target.cb_data = payload;
                    target.cr_data = payload;
                    t.quants = vec![quant([1; 10])];
                }
                t.tiles = vec![first, second];
            }
            _ => (),
        }
        let mut data = vec![0; block.size()];
        block.encode(&mut WriteCursor::new(&mut data)).unwrap();
        out.extend(data);
    }
    out
}
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let mut out = b"FRDRFX01".to_vec();
    out.extend(0u32.to_le_bytes());
    let mut count = 0u32;
    let quant_only = std::env::args().nth(2).as_deref() == Some("quant-order");
    for mode in [EntropyAlgorithm::Rlgr1, EntropyAlgorithm::Rlgr3] {
        let tag = if mode == EntropyAlgorithm::Rlgr1 { 1 } else { 3 };
        if !quant_only {
            for run in [0, 1, 7, 8, 63, 64, 4096] {
                let mut values = vec![0; run + 3];
                values[run] = 17;
                values[run + 1] = -13;
                values[run + 2] = 5;
                let encoded = encoded(mode, &values);
                for size in [0, 1, 7, 8, 63, 64, 4095, 4096, 4097] {
                    for end in 0..=encoded.len() {
                        let mut v = vec![99; size];
                        let initial = bytes(&v);
                        let r = rlgr::decode(mode, &encoded[..end], &mut v);
                        let mut meta = vec![tag];
                        meta.extend(u32::try_from(size).unwrap().to_le_bytes());
                        record(
                            &mut out,
                            &format!("leading-run-{tag}-{run}-{size}-{end}"),
                            &meta,
                            &encoded[..end],
                            &initial,
                            &bytes(&v),
                            &format!("{r:?}"),
                        );
                        count += 1;
                    }
                }
            }
        }
        let quants = if quant_only {
            vec![[2, 3, 4, 5, 6, 7, 8, 9, 10, 11]]
        } else {
            vec![[1; 10], [0, 1, 2, 3, 4, 5, 6, 7, 8, 9], [16; 10], [17; 10]]
        };
        for q in quants {
            for pattern in 0..3 {
                let mut coeff = vec![0; 4096];
                match pattern {
                    0 => {
                        coeff[4031] = 21;
                        coeff[4032] = 32767;
                        coeff[4033] = 1;
                        coeff[4095] = -32767;
                    }
                    1 => {
                        for (i, v) in coeff.iter_mut().enumerate() {
                            *v = if i % 31 == 0 { 3 } else { 0 };
                        }
                    }
                    _ => {
                        coeff[4032] = 1;
                        coeff[4095] = -1;
                    }
                }
                let wire = encoded(mode, &coeff);
                let mut v = vec![99; 4096];
                let mut temp = vec![71; 4096];
                let initial = bytes(&v);
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    rlgr::decode(mode, &wire, &mut v).unwrap();
                    subband_reconstruction::decode(&mut v[4032..]);
                    quantization::decode(&mut v, &quant(q));
                    dwt::decode(&mut v, &mut temp);
                }));
                let mut meta = vec![tag + 10];
                meta.extend(q);
                record(
                    &mut out,
                    &format!("component-{tag}-{q:?}-{pattern}"),
                    &meta,
                    &wire,
                    &initial,
                    &bytes(&v),
                    if r.is_ok() { "Ok(())" } else { "Panic arithmetic" },
                );
                count += 1;
            }
        }
        if quant_only {
            continue;
        }
        for case in [
            "two-success",
            "first-empty-y",
            "first-empty-cb",
            "first-empty-cr",
            "second-empty-y",
            "second-empty-cb",
            "second-empty-cr",
            "first-color",
            "second-color",
        ] {
            let wire = wire_with(case, mode);
            let mut image = DecodedImage::new(PixelFormat::BgrX32, 128, 64);
            let initial = image.data().to_vec();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                DecodingContext::default().decode(
                    &mut image,
                    &InclusiveRectangle {
                        left: 0,
                        top: 0,
                        right: 127,
                        bottom: 63,
                    },
                    &mut ReadCursor::new(&wire),
                )
            }));
            let result = match r {
                Ok(r) => format!("{r:?}"),
                Err(_) => "Panic arithmetic".to_owned(),
            };
            record(
                &mut out,
                &format!("session-{tag}-{case}"),
                &[20],
                &wire,
                &initial,
                image.data(),
                &result,
            );
            count += 1;
        }
    }
    out[8..12].copy_from_slice(&count.to_le_bytes());
    std::fs::write(std::env::args().nth(1).unwrap(), out).unwrap();
    println!("supplemental {count} records");
}
