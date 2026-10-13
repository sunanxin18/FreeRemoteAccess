//! 两个独立process共用的纯测试输入/计时格式；没有decoder/K引用。
#![allow(dead_code)]
use sha2::{Digest, Sha256};
use std::time::Instant;
pub const LENGTHS: [usize; 6] = [1, 3, 4, 65, 1024, 4096];
pub const WARMUP: usize = 32;
pub const SAMPLES: usize = 51;
pub const BATCH: usize = 16;
pub struct Record {
    pub kind: u8,
    pub sf: usize,
    pub df: usize,
    pub source: Vec<u8>,
    pub palette: Option<[[u8; 3]; 256]>,
    pub expected: Vec<u8>,
}
impl Record {
    pub fn width(&self) -> usize {
        match self.kind {
            0 => 1,
            1 | 2 => 2,
            3 | 4 => 3,
            _ => 4,
        }
    }
}
pub fn records(raw: &[u8]) -> Vec<Record> {
    assert_eq!(&raw[..8], b"FRDPIX01");
    assert_eq!(
        format!("{:x}", Sha256::digest(raw)),
        "ee69627eb9513f5e71a7b310dd60b41ececf12ef72aa2be45562fc7cd0910f26"
    );
    let mut p = 8;
    let mut records = Vec::new();
    while p < raw.len() {
        let kind = raw[p];
        let sf = raw[p + 1] as usize;
        let df = raw[p + 2] as usize;
        p += 4;
        let sl = u32::from_le_bytes(raw[p..p + 4].try_into().unwrap()) as usize;
        let dl = u32::from_le_bytes(raw[p + 4..p + 8].try_into().unwrap()) as usize;
        p += 8;
        let palette = if kind == 0 {
            let mut pal = [[0; 3]; 256];
            for pixel in &mut pal {
                pixel.copy_from_slice(&raw[p..p + 3]);
                p += 3;
            }
            Some(pal)
        } else {
            None
        };
        let source = raw[p..p + sl].to_vec();
        p += sl;
        let expected = raw[p..p + dl].to_vec();
        p += dl;
        records.push(Record {
            kind,
            sf,
            df,
            source,
            palette,
            expected,
        });
    }
    records
}
pub fn input(record: &Record, n: usize) -> (Vec<u8>, Vec<u8>, String) {
    let src: Vec<u8> = record
        .source
        .iter()
        .copied()
        .cycle()
        .take(n * record.width())
        .collect();
    let want: Vec<u8> = record
        .expected
        .iter()
        .copied()
        .cycle()
        .take(n * 4)
        .collect();
    let mut hash = Sha256::new();
    hash.update([record.kind, record.sf as u8, record.df as u8]);
    hash.update(&src);
    if let Some(pal) = &record.palette {
        for pixel in pal {
            hash.update(pixel);
        }
    }
    (src, want, format!("{:x}", hash.finalize()))
}
pub fn measure(mut run: impl FnMut()) -> (u64, u64) {
    for _ in 0..WARMUP {
        run();
    }
    let mut times = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        for _ in 0..BATCH {
            run();
        }
        times.push((start.elapsed().as_nanos() / BATCH as u128) as u64);
    }
    times.sort_unstable();
    (times[SAMPLES / 2], times[SAMPLES * 95 / 100])
}
pub fn print_case(
    id: usize,
    r: &Record,
    n: usize,
    input_hash: &str,
    output: &[u8],
    timing: (u64, u64),
) {
    println!(
        "pixel-case,{id},{},{},{},{n},{input_hash},{:x},{},{}",
        r.kind,
        r.sf,
        r.df,
        Sha256::digest(output),
        timing.0,
        timing.1
    );
}
