// 真实 shipping session public API 消费前门错误；无图像 kernel/parser 复制。
use core::error::Error as _;
use ironrdp_core::{DecodeError, DecodeErrorKind, WriteBuf};
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_graphics::legacy_native::NativeKernels;
use ironrdp_pdu::bitmap::{BitmapData, BitmapUpdateData, Compression};
use ironrdp_pdu::geometry::InclusiveRectangle;
use ironrdp_session::fast_path::{Processor, ProcessorBuilder};
use ironrdp_session::image::DecodedImage;

fn processor(native: bool) -> Processor {
    let mut p = ProcessorBuilder {
        io_channel_id: 1003,
        user_channel_id: 1001,
        share_id: 1,
        enable_server_pointer: true,
        pointer_software_rendering: true,
        bulk_decompressor: None,
    }
    .build();
    if native {
        p.set_native_legacy_kernels(NativeKernels::detect().unwrap());
    }
    p
}

#[test]
fn malformed_surface_front_door_same_error_both_policies_and_unchanged_image() {
    for native in [false, true] {
        for command in [1u8, 6] {
            for length in [u32::MAX - 24, u32::MAX - 23, u32::MAX] {
                for flags in [0u8, 1, 0x80, 0x81] {
                    for available in [0usize, 23, 24] {
                        let mut p = processor(native); // 无 pending mouse 的全新 processor。
                        let mut image = DecodedImage::new(PixelFormat::RgbA32, 1, 1);
                        // 通过既有 public Bitmap API 设置非零 sentinel，再开始 Surface 观察。
                        p.process_bitmap_update(
                            &mut image,
                            BitmapUpdateData {
                                rectangles: vec![BitmapData {
                                    rectangle: InclusiveRectangle {
                                        left: 0,
                                        top: 0,
                                        right: 0,
                                        bottom: 0,
                                    },
                                    width: 1,
                                    height: 1,
                                    bits_per_pixel: 32,
                                    compression_flags: Compression::empty(),
                                    compressed_data_header: None,
                                    bitmap_data: &[0x31, 0x42, 0x53, 0xff],
                                }],
                            },
                        )
                        .unwrap();
                        let before = format!("{image:?}");
                        let pixels = image.data().to_vec();
                        assert_ne!(pixels, [0; 4]);
                        #[cfg(surface_observed)]
                        crate::reset_observations();
                        let mut packet = vec![0, u8::try_from(27 + available).unwrap(), 4];
                        packet.extend(u16::try_from(22 + available).unwrap().to_le_bytes());
                        packet.extend([
                            command, 0, 0, 0, 0, 0, 1, 0, 1, 0, 32, flags, 0, 0, 1, 0, 1, 0,
                        ]);
                        packet.extend(length.to_le_bytes());
                        packet.extend(vec![0; available]);
                        let mut output = WriteBuf::new();
                        let error = p.process(&mut image, &packet, &mut output).unwrap_err();
                        let source = error
                            .source()
                            .unwrap()
                            .downcast_ref::<DecodeError>()
                            .unwrap();
                        let combined = usize::try_from(length)
                            .unwrap()
                            .checked_add(if flags & 1 != 0 { 24 } else { 0 });
                        match combined {
                            Some(expected) => assert!(
                                matches!(source.kind(), DecodeErrorKind::NotEnoughBytes { received, expected: e } if *received == available && *e == expected)
                            ),
                            None => assert!(
                                matches!(source.kind(), DecodeErrorKind::Other { description } if *description == "bitmap data length including header exceeds usize")
                            ),
                        }
                        assert!(output.filled().is_empty());
                        assert_eq!(image.data(), pixels);
                        assert_eq!(format!("{image:?}"), before);
                        #[cfg(surface_observed)]
                        assert_eq!(crate::observations(), (0, 0));
                        // Err 没有 UpdateKind vector；不能报告 Region 或忽略为 None。
                    }
                }
            }
        }
    }
}

// 显式 u32 arithmetic 注入真实 session source 的现有 decode-result dispatch。
// 此测试不宣称 native PDU parser 在64位上发生了usize溢出，也不是i686实跑。
#[cfg(surface_injected)]
#[test]
fn injected_u32_other_dispatch_propagates_both_policies() {
    for native in [false, true] {
        let mut p = processor(native);
        let mut image = DecodedImage::new(PixelFormat::RgbA32, 1, 1);
        let before = format!("{image:?}");
        let mut packet = vec![
            0, 51, 4, 46, 0, 1, 0, 0, 0, 0, 0, 1, 0, 1, 0, 32, 1, 0, 0, 1, 0, 1, 0,
        ];
        packet.extend(u32::MAX.to_le_bytes());
        packet.extend([0; 24]);
        let mut output = WriteBuf::new();
        crate::reset_observations();
        let error = p.process(&mut image, &packet, &mut output).unwrap_err();
        let source = error
            .source()
            .unwrap()
            .downcast_ref::<DecodeError>()
            .unwrap();
        assert!(
            matches!(source.kind(), DecodeErrorKind::Other { description } if *description == "bitmap data length including header exceeds usize")
        );
        assert!(output.filled().is_empty());
        assert_eq!(format!("{image:?}"), before);
        assert_eq!(crate::observations(), (0, 0));
    }
}
