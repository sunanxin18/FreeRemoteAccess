#![allow(dead_code)]
use flate2::read::GzDecoder;
use frd_rdp_legacy_kernels::{PackedDepth, Pixel32Format, PixelRowSource};
use sha2::{Digest, Sha256};
use std::io::Read;
pub const FORMATS: [Pixel32Format; 8] = [
    Pixel32Format::ARgb,
    Pixel32Format::XRgb,
    Pixel32Format::ABgr,
    Pixel32Format::XBgr,
    Pixel32Format::BgrA,
    Pixel32Format::BgrX,
    Pixel32Format::RgbA,
    Pixel32Format::RgbX,
];
pub const LENGTHS: [usize; 18] = [
    0, 1, 2, 3, 4, 7, 8, 15, 16, 17, 31, 32, 33, 63, 64, 65, 95, 96,
];
pub struct Record {
    pub source_format: PixelRowSource,
    pub destination_format: Pixel32Format,
    pub source: Vec<u8>,
    pub palette: Option<[[u8; 3]; 256]>,
    pub expected: Vec<u8>,
}
impl Record {
    pub fn width(&self) -> usize {
        match self.source_format {
            PixelRowSource::Packed(PackedDepth::Index8) => 1,
            PixelRowSource::Packed(PackedDepth::Rgb555 | PackedDepth::Rgb565) => 2,
            PixelRowSource::Packed(PackedDepth::Bgr24) | PixelRowSource::Rgb24 => 3,
            _ => 4,
        }
    }
}
pub fn records() -> Vec<Record> {
    let mut raw = Vec::new();
    GzDecoder::new(&include_bytes!("../pixel_original/original-pixel.bin.gz")[..])
        .read_to_end(&mut raw)
        .unwrap();
    let metadata = include_str!("../pixel_original/provenance.json");
    // fixture 只读取冻结bytes；不在此 process 执行原版或新实现生成expected。
    let hash = format!("{:x}", Sha256::digest(&raw));
    assert_eq!(
        hash,
        "ee69627eb9513f5e71a7b310dd60b41ececf12ef72aa2be45562fc7cd0910f26"
    );
    assert!(metadata.contains(&format!("\"expected_sha256\": \"{hash}\"")));
    assert_eq!(&raw[..8], b"FRDPIX01");
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
        let source_format = match kind {
            0 => PixelRowSource::Packed(PackedDepth::Index8),
            1 => PixelRowSource::Packed(PackedDepth::Rgb555),
            2 => PixelRowSource::Packed(PackedDepth::Rgb565),
            3 => PixelRowSource::Packed(PackedDepth::Bgr24),
            4 => PixelRowSource::Rgb24,
            5 => PixelRowSource::Raw32(FORMATS[sf]),
            6 => PixelRowSource::Region32(FORMATS[sf]),
            _ => panic!("fixture kind"),
        };
        let source = raw[p..p + sl].to_vec();
        p += sl;
        let expected = raw[p..p + dl].to_vec();
        p += dl;
        records.push(Record {
            source_format,
            destination_format: FORMATS[df],
            source,
            palette,
            expected,
        });
    }
    assert_eq!(records.len(), 176);
    records
}
