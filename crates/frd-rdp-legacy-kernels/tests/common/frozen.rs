//! 独立 registry oracle 冻结输出查表，绝不链接/运行原版 decoder。
#![allow(dead_code)]
#[path = "snapshot_format.rs"]
pub mod format;
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::{io::Read, sync::OnceLock};
fn records() -> &'static format::Records {
    static RECORDS: OnceLock<format::Records> = OnceLock::new();
    RECORDS.get_or_init(|| {
        let blob = include_bytes!("../fixtures/original-leaves.bin.gz");
        assert_eq!(
            format!("{:x}", Sha256::digest(blob)),
            include_str!("../fixtures/original-leaves.bin.sha256").trim()
        );
        let mut raw = Vec::new();
        GzDecoder::new(blob.as_slice())
            .read_to_end(&mut raw)
            .unwrap();
        format::decode(&raw)
    })
}
fn output(op: u8, parts: &[&[u8]]) -> &'static [u8] {
    records()
        .get(&format::key(op, parts))
        .expect("缺失冻结原版记录：请在独立 oracle 中扩充并审查")
        .as_slice()
}
#[derive(Clone)]
pub struct Quant {
    pub hl1: u8,
    pub lh1: u8,
    pub hh1: u8,
    pub hl2: u8,
    pub lh2: u8,
    pub hh2: u8,
    pub hl3: u8,
    pub lh3: u8,
    pub hh3: u8,
    pub ll3: u8,
}
pub mod quantization {
    use super::*;
    pub fn decode(data: &mut [i16], q: &Quant) {
        let bytes = format::i16_bytes(data);
        let q = [
            q.hl1, q.lh1, q.hh1, q.hl2, q.lh2, q.hh2, q.hl3, q.lh3, q.hh3, q.ll3,
        ];
        data.copy_from_slice(&format::i16s(output(1, &[&bytes, &q])));
    }
}
pub mod dwt {
    use super::*;
    pub fn decode(data: &mut [i16], _scratch: &mut [i16]) {
        let bytes = format::i16_bytes(data);
        data.copy_from_slice(&format::i16s(output(2, &[&bytes])));
    }
}
pub mod subband_reconstruction {
    use super::*;
    pub fn decode(data: &mut [i16]) {
        let bytes = format::i16_bytes(data);
        data.copy_from_slice(&format::i16s(output(3, &[&bytes])));
    }
}
pub mod color_conversion {
    use super::*;
    pub struct YCbCrBuffer<'a> {
        pub y: &'a [i16],
        pub cb: &'a [i16],
        pub cr: &'a [i16],
    }
    pub fn ycbcr_to_rgba(data: YCbCrBuffer<'_>, out: &mut [u8]) -> Result<(), ()> {
        out.copy_from_slice(output(
            4,
            &[
                &format::i16_bytes(data.y),
                &format::i16_bytes(data.cb),
                &format::i16_bytes(data.cr),
            ],
        ));
        Ok(())
    }
}
pub fn composition() -> Vec<([u8; 10], Vec<i16>, Vec<i16>)> {
    let blob = include_bytes!("../fixtures/original-composition.bin");
    assert_eq!(
        format!("{:x}", Sha256::digest(blob)),
        include_str!("../fixtures/original-composition.sha256").trim()
    );
    assert_eq!(&blob[..8], b"FRDCMP01");
    assert_eq!(blob.len(), 8 + 3 * (10 + 8192 + 8192));
    blob[8..]
        .chunks_exact(10 + 8192 + 8192)
        .map(|r| {
            (
                r[..10].try_into().unwrap(),
                format::i16s(&r[10..8202]),
                format::i16s(&r[8202..]),
            )
        })
        .collect()
}

pub fn rlgr_outputs() -> Vec<Vec<i16>> {
    let blob = include_bytes!("../fixtures/original-rlgr-replay.bin.gz");
    assert_eq!(
        format!("{:x}", Sha256::digest(blob)),
        include_str!("../fixtures/original-rlgr-replay.bin.sha256").trim()
    );
    let mut raw = Vec::new();
    GzDecoder::new(blob.as_slice())
        .read_to_end(&mut raw)
        .unwrap();
    assert_eq!(&raw[..8], b"FRDRLG01");
    let mut rest = &raw[8..];
    let mut outputs = Vec::new();
    let mut previous = None;
    while !rest.is_empty() {
        let index = u32::from_le_bytes(rest[..4].try_into().unwrap());
        if let Some(previous) = previous {
            assert!(index > previous);
        }
        previous = Some(index);
        let len = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        outputs.push(format::i16s(&rest[8..8 + len]));
        rest = &rest[8 + len..];
    }
    outputs
}
