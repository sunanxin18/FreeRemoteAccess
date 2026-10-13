//! feature=qoi 的 stock RGBA helper 独立调用锚点；不解析 QOI wire。
#![allow(dead_code)]
pub use ironrdp_session::{custom_err, SessionResult};
#[path="source/image.rs"] mod image;
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_pdu::geometry::InclusiveRectangle;
use std::io::Write;
fn main(){let formats=[PixelFormat::ARgb32,PixelFormat::XRgb32,PixelFormat::ABgr32,PixelFormat::XBgr32,PixelFormat::BgrA32,PixelFormat::BgrX32,PixelFormat::RgbA32,PixelFormat::RgbX32];let source:Vec<u8>=[0,1,127,128,254,255].into_iter().flat_map(|a|[17,34,51,a]).collect();let rectangle=InclusiveRectangle{left:0,top:0,right:5,bottom:0};let mut out=std::fs::File::create(std::env::args().nth(1).unwrap()).unwrap();for format in formats{let mut image=image::DecodedImage::new(format,6,1);image.apply_rgba32(&source,&rectangle,false).unwrap();out.write_all(image.data()).unwrap();}}
