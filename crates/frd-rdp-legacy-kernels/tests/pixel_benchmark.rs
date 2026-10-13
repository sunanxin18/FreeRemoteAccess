#[path = "common/pixel_bench.rs"]
mod bench;
use flate2::read::GzDecoder;
use frd_rdp_legacy_kernels::{NativeKernels, PackedDepth, Pixel32Format, PixelRowSource};
use std::{hint::black_box, io::Read};
const FORMATS: [Pixel32Format; 8] = [
    Pixel32Format::ARgb,
    Pixel32Format::XRgb,
    Pixel32Format::ABgr,
    Pixel32Format::XBgr,
    Pixel32Format::BgrA,
    Pixel32Format::BgrX,
    Pixel32Format::RgbA,
    Pixel32Format::RgbX,
];
#[test]
#[ignore = "须 root 批准的 quiet window；仅 row leaf 与原版单行apply边界"]
fn bounded_all_types_short_long_native_row_benchmark() {
    let k = NativeKernels::detect().unwrap();
    let mut raw = Vec::new();
    GzDecoder::new(&include_bytes!("pixel_original/original-pixel.bin.gz")[..])
        .read_to_end(&mut raw)
        .unwrap();
    println!(
        "pixel-native arch={} os={} backend={:?} warmup={} samples={} batch={} profile=release",
        std::env::consts::ARCH,
        std::env::consts::OS,
        k.backend(),
        bench::WARMUP,
        bench::SAMPLES,
        bench::BATCH
    );
    for (id, r) in bench::records(&raw).iter().enumerate() {
        let source_format = match r.kind {
            0 => PixelRowSource::Packed(PackedDepth::Index8),
            1 => PixelRowSource::Packed(PackedDepth::Rgb555),
            2 => PixelRowSource::Packed(PackedDepth::Rgb565),
            3 => PixelRowSource::Packed(PackedDepth::Bgr24),
            4 => PixelRowSource::Rgb24,
            5 => PixelRowSource::Raw32(FORMATS[r.sf]),
            6 => PixelRowSource::Region32(FORMATS[r.sf]),
            _ => unreachable!(),
        };
        for n in bench::LENGTHS {
            let (src, want, hash) = bench::input(r, n);
            let mut out = vec![0; n * 4];
            let timing = bench::measure(|| {
                k.convert_pixel_row(
                    black_box(source_format),
                    black_box(FORMATS[r.df]),
                    black_box(&src),
                    black_box(r.palette.as_ref()),
                    black_box(&mut out),
                )
                .unwrap();
                black_box(&out);
            });
            assert_eq!(out, want);
            bench::print_case(id, r, n, &hash, &out, timing);
        }
    }
}
