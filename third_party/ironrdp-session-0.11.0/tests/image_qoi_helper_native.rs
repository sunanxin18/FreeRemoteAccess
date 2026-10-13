use super::*;
#[test]
fn original_qoi_rgba_helper_alpha_on_eight_native_targets() {
    let targets = [
        PixelFormat::ARgb32,
        PixelFormat::XRgb32,
        PixelFormat::ABgr32,
        PixelFormat::XBgr32,
        PixelFormat::BgrA32,
        PixelFormat::BgrX32,
        PixelFormat::RgbA32,
        PixelFormat::RgbX32,
    ];
    let expected = include_bytes!("../../../crates/frd-rdp-legacy-kernels/tests/pixel_original/qoi-original.bin");
    let source: Vec<u8> = [0u8, 1, 127, 128, 254, 255]
        .into_iter()
        .flat_map(|a| [0x11, 0x22, 0x33, a])
        .collect();
    let k = NativeKernels::detect().unwrap();
    for (index, format) in targets.into_iter().enumerate() {
        let mut image = DecodedImage::new(format, 9, 3);
        let rectangle = InclusiveRectangle {
            left: 1,
            top: 1,
            right: 6,
            bottom: 1,
        };
        let region = image
            .apply_bitmap_native(
                &source,
                PixelRowSource::Raw32(Pixel32Format::RgbA),
                6,
                1,
                24,
                false,
                &rectangle,
                None,
                &k,
            )
            .unwrap();
        assert_eq!(region, Some(rectangle));
        assert_eq!(&image.data()[40..64], &expected[index * 24..index * 24 + 24]);
        assert_eq!(&image.data()[..40], &[0; 40]);
        assert_eq!(&image.data()[64..], &[0; 44]);
    }
}
