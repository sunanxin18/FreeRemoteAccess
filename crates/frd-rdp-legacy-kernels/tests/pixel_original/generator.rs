//! 独立 rustc process；原版完整 source 原样 include；无 K / vendor 依赖。
#![allow(dead_code)]
pub use ironrdp_session::{custom_err, SessionResult};
#[path = "source/image.rs"]
mod image;
#[path = "source/image_processing.rs"]
mod image_processing;
#[path = "source/color_conversion.rs"]
mod color_conversion;
use ironrdp_graphics::image_processing::PixelFormat as OriginalFormat;
use ironrdp_pdu::geometry::InclusiveRectangle;
use image_processing::{ImageRegion, ImageRegionMut, PixelFormat};
use std::io::Write;

const FORMATS: [PixelFormat; 8] = [PixelFormat::ARgb32, PixelFormat::XRgb32, PixelFormat::ABgr32, PixelFormat::XBgr32, PixelFormat::BgrA32, PixelFormat::BgrX32, PixelFormat::RgbA32, PixelFormat::RgbX32];
fn rect(n: usize) -> InclusiveRectangle { InclusiveRectangle { left: 0, top: 0, right: (n - 1) as u16, bottom: 0 } }
fn expected(kind: u8, src_format: PixelFormat, dst_format: PixelFormat, src: &[u8], palette: &[[u8;3];256]) -> Vec<u8> {
    let width = match kind { 0 => 1, 1|2 => 2, 3|4 => 3, _ => 4 };
    let mut output = Vec::new();
    // u16 stock image width：65536 word 分为两个独立 tight row。
    for row in src.chunks(32768 * width) {
        let n = row.len()/width;
        let region = rect(n);
        if kind == 6 {
            let mut out = vec![0; n*4];
            ImageRegion { region: region.clone(), step: 0, pixel_format: src_format, data: row }.copy_to(&mut ImageRegionMut { region, step: 0, pixel_format: dst_format, data: &mut out }).unwrap();
            output.extend(out);
        } else {
            let dst = OriginalFormat::try_from(dst_format as u32).unwrap();
            let mut image = image::DecodedImage::new(dst, n as u16, 1);
            match kind {
                0 => { image.apply_rgb8_with_palette(row, &region, palette).unwrap(); }
                1 => { image.apply_rgb15_bitmap(row, &region).unwrap(); }
                2 => { image.apply_rgb16_bitmap(row, &region).unwrap(); }
                3 => { image.apply_bgr24_bitmap(row, &region).unwrap(); }
                4 => { image.apply_rgb24(row, &region, false).unwrap(); }
                5 => { image.apply_rgb32_bitmap(row, OriginalFormat::try_from(src_format as u32).unwrap(), &region).unwrap(); }
                _ => unreachable!(),
            }
            output.extend_from_slice(image.data());
        }
    }
    output
}
fn main() {
    let mut out = std::fs::File::create(std::env::args().nth(1).unwrap()).unwrap();
    out.write_all(b"FRDPIX01").unwrap();
    for kind in 0..=6u8 {
        for sf in 0..if kind >= 5 {8} else {1} {
            for df in 0..8 {
                for p in 0..if kind == 0 {2} else {1} {
                    let palette = std::array::from_fn(|i| if p == 0 {[i as u8, (i as u8).wrapping_mul(73).wrapping_add(19), (i as u8).wrapping_mul(131).wrapping_add(7)]} else {[(i as u8).wrapping_mul(53).wrapping_add(91), 255-i as u8, (i as u8).wrapping_mul(97).wrapping_add(13)]});
                    let src: Vec<u8> = match kind {
                        0 => (0..256).map(|i| ((i*173+255)%256) as u8).collect(),
                        1|2 => (0..=65535u16).flat_map(u16::to_le_bytes).collect(),
                        3|4 => (0..257*3).map(|i| ((i*71+i/3*11+17)%256) as u8).collect(),
                        5|6 => (0..96).flat_map(|i| {let mut pixel=[0;4]; let [r,g,b,a]=FORMATS[sf].channel_offsets(); pixel[r]=(i*29+17) as u8;pixel[g]=(i*53+34) as u8;pixel[b]=(i*79+51) as u8;pixel[a]=[0,1,127,128,254,255][i%6];pixel}).collect(),
                        _=>unreachable!(),
                    };
                    let want = expected(kind, FORMATS[sf], FORMATS[df], &src, &palette);
                    out.write_all(&[kind, sf as u8, df as u8, p]).unwrap();
                    out.write_all(&(src.len() as u32).to_le_bytes()).unwrap();
                    out.write_all(&(want.len() as u32).to_le_bytes()).unwrap();
                    if kind==0 {for pixel in palette {out.write_all(&pixel).unwrap();}}
                    out.write_all(&src).unwrap();out.write_all(&want).unwrap();
                }
            }
        }
    }
}
