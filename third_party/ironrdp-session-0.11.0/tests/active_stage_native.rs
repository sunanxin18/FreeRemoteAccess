use super::*;
use core::error::Error as _;
use ironrdp_core::{Encode as _, WriteCursor};
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_pdu::bitmap::{BitmapData, BitmapUpdateData, Compression};
fn processor() -> fast_path::Processor {
    fast_path::ProcessorBuilder {
        io_channel_id: 1003,
        user_channel_id: 1001,
        share_id: 1,
        enable_server_pointer: false,
        pointer_software_rendering: false,
        bulk_decompressor: None,
    }
    .build()
}
fn stage() -> ActiveStage {
    ActiveStageBuilder {
        static_channels: StaticChannelSet::new(),
        user_channel_id: 1001,
        io_channel_id: 1003,
        message_channel_id: None,
        share_id: 1,
        compression_type: None,
        enable_server_pointer: false,
        pointer_software_rendering: false,
    }
    .build()
}
fn update(wire: &[u8], bpp: u16) -> BitmapUpdateData<'_> {
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
            bits_per_pixel: bpp,
            compression_flags: Compression::BITMAP_COMPRESSION | Compression::NO_BITMAP_COMPRESSION_HDR,
            compressed_data_header: None,
            bitmap_data: wire,
        }],
    }
}
fn slow(
    p: &mut fast_path::Processor,
    image: &mut DecodedImage,
    wire: &[u8],
    bpp: u16,
) -> SessionResult<Vec<UpdateKind>> {
    let bitmap = update(wire, bpp);
    let mut data = vec![0; bitmap.size()];
    bitmap.encode(&mut WriteCursor::new(&mut data)).unwrap();
    process_slow_path_graphics(p, image, &data)
}
fn packet(data: &[u8], code: ironrdp_pdu::fast_path::UpdateCode) -> Vec<u8> {
    use ironrdp_pdu::fast_path::{EncryptionFlags, FastPathHeader, FastPathUpdatePdu, Fragmentation};
    let update = FastPathUpdatePdu {
        fragmentation: Fragmentation::Single,
        update_code: code,
        compression_flags: None,
        compression_type: None,
        data,
    };
    let header = FastPathHeader::new(EncryptionFlags::empty(), update.size());
    let mut out = vec![0; header.size() + update.size()];
    let mut cursor = WriteCursor::new(&mut out);
    header.encode(&mut cursor).unwrap();
    update.encode(&mut cursor).unwrap();
    out
}
fn fast(
    p: &mut fast_path::Processor,
    image: &mut DecodedImage,
    wire: &[u8],
    bpp: u16,
) -> SessionResult<Vec<UpdateKind>> {
    let bitmap = update(wire, bpp);
    let mut data = vec![0; bitmap.size()];
    bitmap.encode(&mut WriteCursor::new(&mut data)).unwrap();
    p.process(
        image,
        &packet(&data, ironrdp_pdu::fast_path::UpdateCode::Bitmap),
        &mut WriteBuf::new(),
    )
}
fn rfx_packet(wire: &[u8]) -> Vec<u8> {
    use ironrdp_pdu::surface_commands::{ExtendedBitmapDataPdu, SurfaceBitsPdu, SurfaceCommand};
    let cmd = SurfaceCommand::SetSurfaceBits(SurfaceBitsPdu {
        destination: ironrdp_pdu::geometry::ExclusiveRectangle {
            left: 0,
            top: 0,
            right: 128,
            bottom: 64,
        },
        extended_bitmap_data: ExtendedBitmapDataPdu {
            bpp: 32,
            codec_id: 3,
            width: 128,
            height: 64,
            header: None,
            data: wire,
        },
    });
    let mut data = vec![0; cmd.size()];
    cmd.encode(&mut WriteCursor::new(&mut data)).unwrap();
    packet(&data, ironrdp_pdu::fast_path::UpdateCode::SurfaceCommands)
}
#[test]
fn shared_bitmap_native_success_and_error_no_fake_region() {
    for bpp in [8, 15, 16, 24, 32] {
        for route in [0, 1, 2] {
            let success: Vec<u8> = match bpp {
                8 => vec![0xfd],
                15 | 16 => vec![0xfd],
                24 => vec![0x61, 1, 2, 3],
                32 => vec![0x20, 1, 2, 3, 0],
                _ => unreachable!(),
            };
            let mut p = processor();
            p.set_native_legacy_kernels(NativeKernels::detect().unwrap());
            let mut native = DecodedImage::new(PixelFormat::RgbA32, 1, 1);
            let mut old = DecodedImage::new(PixelFormat::RgbA32, 1, 1);
            let old_updates = processor()
                .process_bitmap_update(&mut old, update(&success, bpp))
                .unwrap();
            let updates = match route {
                1 => slow(&mut p, &mut native, &success, bpp),
                2 => fast(&mut p, &mut native, &success, bpp),
                _ => p.process_bitmap_update(&mut native, update(&success, bpp)),
            }
            .unwrap();
            assert!(matches!(
                (&old_updates[0], &updates[0]),
                (UpdateKind::Region(_), UpdateKind::Region(_))
            ));
            assert_eq!(native.data(), old.data(), "bpp={bpp} route={route}");
            assert_ne!(native.data(), [0; 4]);
            let bad = if bpp == 32 { vec![0x30, 0xff] } else { vec![0xff] };
            let initial = native.data().to_vec();
            let err = match route {
                1 => slow(&mut p, &mut native, &bad, bpp),
                2 => fast(&mut p, &mut native, &bad, bpp),
                _ => p.process_bitmap_update(&mut native, update(&bad, bpp)),
            }
            .unwrap_err();
            assert!(err.source().is_some());
            assert_eq!(native.data(), initial);
            let old_updates = processor().process_bitmap_update(&mut old, update(&bad, bpp)).unwrap();
            assert!(matches!(&old_updates[0], UpdateKind::Region(_)));
        }
    }
}
#[test]
fn reactivation_replacement_keeps_selected_native_and_default_keeps_old_errors() {
    let mut selected = stage();
    selected.set_native_legacy_kernels(NativeKernels::detect().unwrap());
    let mut default = stage();
    for _ in 0..3 {
        selected.set_fastpath_processor(processor());
        default.set_fastpath_processor(processor());
        let mut image = DecodedImage::new(PixelFormat::RgbA32, 1, 1);
        assert!(
            selected
                .fast_path_processor
                .process_bitmap_update(&mut image, update(&[0xff], 24))
                .is_err()
        );
        assert!(
            default
                .fast_path_processor
                .process_bitmap_update(&mut image, update(&[0xff], 24))
                .is_ok()
        );
        assert_eq!(image.data(), [0; 4]);
        // RFX policy replacement is observable through a valid RFX frame with ColorOverflow.
        let extra = include_bytes!("fixtures/rfx-original-supplemental.bin");
        let mut input = &extra[12..];
        fn field<'a>(input: &mut &'a [u8]) -> &'a [u8] {
            let n = u32::from_le_bytes(input[..4].try_into().unwrap()) as usize;
            let v = &input[4..4 + n];
            *input = &input[4 + n..];
            v
        }
        let mut found = false;
        while !input.is_empty() {
            let name = field(&mut input);
            let _meta = field(&mut input);
            let wire = field(&mut input);
            for _ in 0..3 {
                field(&mut input);
            }
            if name == b"session-3-first-color" {
                // exercise the same RFX handler used by the Processor SurfaceCommands branch via private test accessor
                let e = selected
                    .fast_path_processor
                    .process(
                        &mut DecodedImage::new(PixelFormat::RgbA32, 128, 64),
                        &rfx_packet(wire),
                        &mut WriteBuf::new(),
                    )
                    .unwrap_err();
                assert!(
                    e.source()
                        .unwrap()
                        .downcast_ref::<ironrdp_graphics::legacy_native::KernelError>()
                        .is_some()
                );
                found = true;
            }
        }
        assert!(found);
    }
}
