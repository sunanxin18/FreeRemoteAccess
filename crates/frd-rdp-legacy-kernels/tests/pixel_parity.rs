#[path = "common/pixel.rs"]
mod frozen;
use frd_rdp_legacy_kernels::{
    KernelError, NativeKernels, PackedDepth, Pixel32Format, PixelRowSource,
};
use frozen::*;

#[test]
fn all_words_formats_alpha_branches_and_palette_entries_match_frozen_original() {
    let k = NativeKernels::detect().unwrap();
    for record in records() {
        let mut out = vec![0xcd; record.expected.len()];
        k.convert_pixel_row(
            record.source_format,
            record.destination_format,
            &record.source,
            record.palette.as_ref(),
            &mut out,
        )
        .unwrap();
        assert_eq!(
            out, record.expected,
            "{:?} -> {:?}",
            record.source_format, record.destination_format
        );
    }
}
#[test]
fn every_source_destination_short_tail_and_unaligned_row_preserves_canaries() {
    let k = NativeKernels::detect().unwrap();
    for r in records() {
        for n in LENGTHS {
            for source_offset in 0..16 {
                for destination_offset in 0..16 {
                    let mut input = vec![0xac; source_offset + n * r.width() + 17];
                    input[source_offset..source_offset + n * r.width()]
                        .copy_from_slice(&r.source[..n * r.width()]);
                    let mut out = vec![0xcd; destination_offset + n * 4 + 17];
                    k.convert_pixel_row(
                        r.source_format,
                        r.destination_format,
                        &input[source_offset..source_offset + n * r.width()],
                        r.palette.as_ref(),
                        &mut out[destination_offset..destination_offset + n * 4],
                    )
                    .unwrap();
                    assert_eq!(
                        &out[destination_offset..destination_offset + n * 4],
                        &r.expected[..n * 4],
                        "{:?} {:?} n{n} offsets{source_offset}/{destination_offset}",
                        r.source_format,
                        r.destination_format
                    );
                    assert!(out[..destination_offset]
                        .iter()
                        .chain(&out[destination_offset + n * 4..])
                        .all(|&b| b == 0xcd));
                    assert!(input[..source_offset]
                        .iter()
                        .chain(&input[source_offset + n * r.width()..])
                        .all(|&b| b == 0xac));
                }
            }
        }
    }
}
#[test]
fn malformed_rows_missing_palette_and_disabled_capability_never_write() {
    assert!(matches!(
        NativeKernels::detect_if(false),
        Err(KernelError::Unavailable)
    ));
    let k = NativeKernels::detect().unwrap();
    for r in records() {
        for destination in FORMATS {
            let mut out = [0x9a; 9];
            let before = out;
            assert_eq!(
                k.convert_pixel_row(
                    r.source_format,
                    destination,
                    &r.source[..r.width()],
                    None,
                    &mut out
                ),
                Err(KernelError::Length)
            );
            assert_eq!(out, before);
            if r.width() > 1 {
                assert_eq!(
                    k.convert_pixel_row(
                        r.source_format,
                        destination,
                        &r.source[..r.width() - 1],
                        r.palette.as_ref(),
                        &mut out[..4]
                    ),
                    Err(KernelError::Length)
                );
                assert_eq!(out, before);
            }
            if r.source_format == PixelRowSource::Packed(PackedDepth::Index8) {
                for n in [0, 1, 2] {
                    assert_eq!(
                        k.convert_pixel_row(
                            r.source_format,
                            destination,
                            &r.source[..n],
                            None,
                            &mut out[..n * 4]
                        ),
                        Err(KernelError::MissingPalette)
                    );
                    assert_eq!(out, before);
                }
            } else {
                let ignored = [[0xed; 3]; 256];
                let mut actual = [0; 4];
                k.convert_pixel_row(
                    r.source_format,
                    destination,
                    &r.source[..r.width()],
                    Some(&ignored),
                    &mut actual,
                )
                .unwrap();
                let mut baseline = [0; 4];
                k.convert_pixel_row(
                    r.source_format,
                    destination,
                    &r.source[..r.width()],
                    None,
                    &mut baseline,
                )
                .unwrap();
                assert_eq!(actual, baseline);
            }
        }
    }
}
#[test]
fn qoi_stock_helper_keeps_alpha_in_all_eight_destinations() {
    use sha2::{Digest, Sha256};
    let expected = include_bytes!("pixel_original/qoi-original.bin");
    assert_eq!(
        format!("{:x}", Sha256::digest(expected)),
        "e09f03d6dd10d3977a2730856d881dd0f7a4e55d17779d1161802d00add93bee"
    );
    let source: Vec<u8> = [0, 1, 127, 128, 254, 255]
        .into_iter()
        .flat_map(|a| [17, 34, 51, a])
        .collect();
    let kernels = NativeKernels::detect().unwrap();
    for (i, destination) in FORMATS.into_iter().enumerate() {
        let mut out = [0; 24];
        kernels
            .convert_pixel_row(
                PixelRowSource::Raw32(Pixel32Format::RgbA),
                destination,
                &source,
                None,
                &mut out,
            )
            .unwrap();
        assert_eq!(out, &expected[i * 24..i * 24 + 24]);
    }
}
#[test]
fn hand_computed_numeric_and_raw_region_alpha_anchors() {
    let k = NativeKernels::detect().unwrap();
    let checks = [
        (
            PixelRowSource::Packed(PackedDepth::Rgb555),
            Pixel32Format::RgbA,
            vec![0x43, 0x04],
            [8, 16, 25, 255],
        ),
        (
            PixelRowSource::Packed(PackedDepth::Rgb555),
            Pixel32Format::RgbA,
            vec![0x43, 0x84],
            [8, 16, 25, 255],
        ),
        (
            PixelRowSource::Packed(PackedDepth::Rgb565),
            Pixel32Format::RgbA,
            vec![0xe7, 0x09],
            [8, 61, 58, 255],
        ),
        (
            PixelRowSource::Raw32(Pixel32Format::RgbA),
            Pixel32Format::XRgb,
            vec![17, 34, 51, 68],
            [68, 17, 34, 51],
        ),
        (
            PixelRowSource::Region32(Pixel32Format::RgbA),
            Pixel32Format::XRgb,
            vec![17, 34, 51, 68],
            [255, 17, 34, 51],
        ),
        (
            PixelRowSource::Region32(Pixel32Format::RgbA),
            Pixel32Format::RgbX,
            vec![17, 34, 51, 68],
            [17, 34, 51, 68],
        ),
        (
            PixelRowSource::Raw32(Pixel32Format::RgbX),
            Pixel32Format::RgbA,
            vec![17, 34, 51, 68],
            [17, 34, 51, 255],
        ),
    ];
    for (src, dst, input, want) in checks {
        let mut out = [0; 4];
        k.convert_pixel_row(src, dst, &input, None, &mut out)
            .unwrap();
        assert_eq!(out, want);
    }
}
