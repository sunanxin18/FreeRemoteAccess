//! 原版完整source include，与K完全独立；预分配image，不含生成/分配成本。
#![allow(dead_code)]
pub use ironrdp_session::{custom_err,SessionResult};
#[path="source/image.rs"]mod image;
#[path="source/image_processing.rs"]mod image_processing;
#[path="source/color_conversion.rs"]mod color_conversion;
#[path="../common/pixel_bench.rs"]mod bench;
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_pdu::geometry::InclusiveRectangle;
use std::hint::black_box;
const FORMATS:[PixelFormat;8]=[PixelFormat::ARgb32,PixelFormat::XRgb32,PixelFormat::ABgr32,PixelFormat::XBgr32,PixelFormat::BgrA32,PixelFormat::BgrX32,PixelFormat::RgbA32,PixelFormat::RgbX32];
fn main(){let raw=std::fs::read(std::env::args().nth(1).unwrap()).unwrap();println!("pixel-original arch={} os={} warmup={} samples={} batch={} profile=release boundary=stock-one-row-apply-pointer-hidden",std::env::consts::ARCH,std::env::consts::OS,bench::WARMUP,bench::SAMPLES,bench::BATCH);for(id,r)in bench::records(&raw).iter().enumerate(){for n in bench::LENGTHS{let(src,want,hash)=bench::input(r,n);let region=InclusiveRectangle{left:0,top:0,right:(n-1)as u16,bottom:0};let mut image=image::DecodedImage::new(FORMATS[r.df],n as u16,1);let mut out=vec![0;n*4];let timing=bench::measure(||{match black_box(r.kind){0=>{image.apply_rgb8_with_palette(black_box(&src),black_box(&region),black_box(r.palette.as_ref().unwrap())).unwrap();},1=>{image.apply_rgb15_bitmap(black_box(&src),black_box(&region)).unwrap();},2=>{image.apply_rgb16_bitmap(black_box(&src),black_box(&region)).unwrap();},3=>{image.apply_bgr24_bitmap(black_box(&src),black_box(&region)).unwrap();},4=>{image.apply_rgb24(black_box(&src),black_box(&region),false).unwrap();},5=>{image.apply_rgb32_bitmap(black_box(&src),black_box(FORMATS[r.sf]),black_box(&region)).unwrap();},6=>{use image_processing::{ImageRegion,ImageRegionMut,PixelFormat as F};ImageRegion{region:region.clone(),step:0,pixel_format:F::try_from(FORMATS[r.sf]as u32).unwrap(),data:black_box(&src)}.copy_to(&mut ImageRegionMut{region:region.clone(),step:0,pixel_format:F::try_from(FORMATS[r.df]as u32).unwrap(),data:black_box(&mut out)}).unwrap();},_=>unreachable!()};if r.kind==6{black_box(&out);}else{black_box(image.data());}});let actual=if r.kind==6{&out}else{image.data()};assert_eq!(actual,&want);bench::print_case(id,r,n,&hash,actual,timing);}}}
