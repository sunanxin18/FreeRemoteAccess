//! 显式离线生成器：只调用未修改的 registry IronRDP decoder，不调用 NativeKernels。
use ironrdp_graphics::rdp6::BitmapStreamDecoder;
use std::{fs, path::PathBuf};
fn emit(blob: &mut Vec<u8>, width: usize, height: usize, header: u8, wire: &[u8]) {
    let mut expected = Vec::new();
    BitmapStreamDecoder::default()
        .decode_bitmap_stream_to_rgb24(wire, &mut expected, width, height)
        .unwrap();
    for value in [
        width,
        height,
        usize::from(header),
        wire.len(),
        expected.len(),
    ] {
        blob.extend_from_slice(&u32::try_from(value).unwrap().to_le_bytes());
    }
    blob.extend_from_slice(wire);
    blob.extend_from_slice(&expected);
}
fn raw(blob: &mut Vec<u8>, w: usize, h: usize, cll: u8, alpha: bool, sub: bool) {
    let header = cll | if alpha { 0 } else { 0x20 } | if sub { 8 } else { 0 };
    let mut wire = vec![header];
    if alpha {
        wire.extend((0..w * h).map(|i| (i * 59 + 17) as u8));
    }
    // 384 个不同 Y/Co/Cg 组合，sub 模式每两个 Y 共享一个 chroma cell。
    let ys = [0, 1, 127, 128, 254, 255];
    let cs = [0, 1, 63, 64, 127, 128, 129, 255];
    let cells = if sub {
        w.div_ceil(2) * h.div_ceil(2)
    } else {
        w * h
    };
    wire.extend((0..w * h).map(|i| ys[(if sub { i / 2 } else { i }) % 6]));
    wire.extend((0..cells).map(|i| cs[(i / 6) % 8]));
    wire.extend((0..cells).map(|i| cs[(i / 48) % 8]));
    wire.push(0);
    emit(blob, w, h, header, &wire);
}
fn rle_row(wire: &mut Vec<u8>, row: &[u8]) {
    for chunk in row.chunks(15) {
        wire.push((chunk.len() as u8) << 4);
        wire.extend_from_slice(chunk);
    }
}
fn main() {
    let mut blob = b"FRDPLN01".to_vec();
    for cll in 1..=7 {
        for alpha in [false, true] {
            for sub in [false, true] {
                raw(&mut blob, if sub { 768 } else { 384 }, 1, cll, alpha, sub);
                // 每个 chroma cell 不同，包含最后单列、单行。
                let header = cll | if alpha { 0 } else { 0x20 } | if sub { 8 } else { 0 };
                let mut wire = vec![header];
                if alpha {
                    wire.extend([17; 9]);
                }
                wire.extend([0, 1, 127, 128, 254, 255, 34, 89, 167]);
                let count = if sub { 4 } else { 9 };
                wire.extend([1, 63, 128, 255, 64, 127, 129, 0, 17][..count].iter());
                wire.extend([255, 129, 64, 1, 128, 0, 63, 127, 17][..count].iter());
                wire.push(0);
                emit(&mut blob, 3, 3, header, &wire);
            }
        }
    }
    for alpha in [false, true] {
        let header = if alpha { 0 } else { 0x20 };
        let mut wire = vec![header];
        if alpha {
            wire.extend([19; 33]);
        }
        for channel in 0..3 {
            wire.extend((0..33).map(|i| (i * 37 + channel * 71) as u8));
        }
        wire.push(0);
        emit(&mut blob, 33, 1, header, &wire);
    }
    for p in [0, 1, 127, 128, 254, 255] {
        let mut wire = vec![0x30]; // RLE、无 alpha、ARGB
        rle_row(&mut wire, &[p; 256]);
        rle_row(&mut wire, &(0..=255).collect::<Vec<u8>>());
        for _ in 0..4 {
            rle_row(&mut wire, &[0; 256]);
        }
        emit(&mut blob, 256, 2, 0x30, &wire);
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/planar-original.bin");
    fs::write(&path, &blob).unwrap();
    println!("records=64 bytes={} output={}", blob.len(), path.display());
}
