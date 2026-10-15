//! 真实 RequiredNative 工厂 → 生产 build_active_stage → 编码 cursor/输入路径。
use ironrdp::core::encode_vec;
use ironrdp::graphics::image_processing::PixelFormat;
use ironrdp::pdu::fast_path::{
    EncryptionFlags, FastPathHeader, FastPathUpdatePdu, Fragmentation, UpdateCode,
};
use ironrdp::pdu::geometry::InclusiveRectangle;
use ironrdp::pdu::pointer::{
    ColorPointerAttribute, LargePointerAttribute, Point16, PointerAttribute,
};
use ironrdp::pdu::{
    bitmap::{BitmapData, BitmapUpdateData, Compression},
    Action,
};
use ironrdp::session::{
    fast_path::ProcessorBuilder, image::DecodedImage, ActiveStage, ActiveStageBuilder,
    ActiveStageOutput,
};
use ironrdp::svc::StaticChannelSet;
fn rect(l: u16, t: u16, r: u16, b: u16) -> InclusiveRectangle {
    InclusiveRectangle {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}
fn stage(native: bool, enabled: bool) -> ActiveStage {
    let selection = crate::factory::tests::resolved_session_selection(if native {
        crate::RdpLegacyDecodePolicy::RequiredNative
    } else {
        crate::RdpLegacyDecodePolicy::Compatibility
    });
    crate::active_session::build_active_stage(
        ActiveStageBuilder {
            static_channels: StaticChannelSet::new(),
            user_channel_id: 1001,
            io_channel_id: 1003,
            message_channel_id: None,
            share_id: 1,
            compression_type: None,
            enable_server_pointer: enabled,
            pointer_software_rendering: enabled,
        },
        selection,
    )
}
fn packet(code: UpdateCode, data: &[u8]) -> Vec<u8> {
    let body = encode_vec(&FastPathUpdatePdu {
        fragmentation: Fragmentation::Single,
        update_code: code,
        compression_flags: None,
        compression_type: None,
        data,
    })
    .unwrap();
    let mut result =
        encode_vec(&FastPathHeader::new(EncryptionFlags::empty(), body.len())).unwrap();
    result.extend(body);
    result
}
fn slow_pointer(kind: u16, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend(kind.to_le_bytes());
    body.extend([0, 0]);
    body.extend(data);
    let mut user_data = Vec::new();
    user_data.extend(u16::try_from(18 + body.len()).unwrap().to_le_bytes());
    user_data.extend(0x17u16.to_le_bytes());
    user_data.extend(1001u16.to_le_bytes());
    user_data.extend(1u32.to_le_bytes());
    user_data.extend([0, 2]);
    user_data.extend(u16::try_from(body.len()).unwrap().to_le_bytes());
    user_data.extend([0x1b, 0, 0, 0]);
    user_data.extend(body);
    encode_vec(&ironrdp::pdu::x224::X224(
        ironrdp::pdu::mcs::SendDataIndication {
            initiator_id: 1001,
            channel_id: 1003,
            user_data: user_data.into(),
        },
    ))
    .unwrap()
}
fn bitmap_data(color: [u8; 4]) -> Vec<u8> {
    encode_vec(&BitmapUpdateData {
        rectangles: vec![BitmapData {
            rectangle: rect(0, 0, 11, 7),
            width: 12,
            height: 8,
            bits_per_pixel: 32,
            compression_flags: Compression::empty(),
            compressed_data_header: None,
            bitmap_data: &color.repeat(96),
        }],
    })
    .unwrap()
}
fn process_cursor(
    stage: &mut ActiveStage,
    image: &mut DecodedImage,
    code: UpdateCode,
    slow_kind: u16,
    data: &[u8],
    slow: bool,
) {
    let wire = if slow {
        slow_pointer(slow_kind, data)
    } else {
        packet(code, data)
    };
    let result = stage
        .process_with_decoded_coverage(
            image,
            if slow { Action::X224 } else { Action::FastPath },
            &wire,
        )
        .unwrap();
    assert!(
        result
            .decoded_coverage
            .iter()
            .all(|r| r.rectangles.is_empty()),
        "cursor 不建立 content/baseline"
    );
}
#[test]
fn required_native_factory_stage_fast_slow_new_color_large_cache_input_and_replacement() {
    use ironrdp::pdu::input::{
        fast_path::FastPathInputEvent,
        mouse::{MousePdu, PointerFlags},
    };
    for slow in [false, true] {
        let mut stage = stage(true, true);
        let mut image = DecodedImage::new(PixelFormat::RgbA32, 12, 8);
        // RDP raw32 原版 BGRA→RGBA，故颜色均等并单独检查 byte3。
        let background = packet(UpdateCode::Bitmap, &bitmap_data([255, 255, 255, 17]));
        let content = stage
            .process_with_decoded_coverage(&mut image, Action::FastPath, &background)
            .unwrap();
        assert_eq!(
            content
                .decoded_coverage
                .iter()
                .flat_map(|r| &r.rectangles)
                .collect::<Vec<_>>(),
            vec![&rect(0, 0, 11, 7)]
        );
        let xor = [255, 255, 255, 1];
        let color = ColorPointerAttribute {
            width: 1,
            height: 1,
            hot_spot: Point16 { x: 0, y: 0 },
            cache_index: 1,
            xor_mask: &xor,
            and_mask: &[],
        };
        let new = encode_vec(&PointerAttribute {
            xor_bpp: 32,
            color_pointer: color,
        })
        .unwrap();
        process_cursor(
            &mut stage,
            &mut image,
            UpdateCode::NewPointer,
            8,
            &new,
            slow,
        );
        assert_eq!(&image.data()[..4], &[251, 251, 251, 255]);
        process_cursor(
            &mut stage,
            &mut image,
            UpdateCode::CachedPointer,
            7,
            &1u16.to_le_bytes(),
            slow,
        );
        let events = [(3, 2), (7, 5)].map(|(x, y)| {
            FastPathInputEvent::MouseEvent(MousePdu {
                flags: PointerFlags::MOVE,
                number_of_wheel_rotation_units: 0,
                x_position: x,
                y_position: y,
            })
        });
        let moved = stage
            .process_fastpath_input_with_decoded_coverage(&mut image, &events)
            .unwrap();
        assert!(moved
            .decoded_coverage
            .iter()
            .all(|r| r.rectangles.is_empty()));
        assert!(moved
            .outputs
            .iter()
            .any(|o| matches!(o, ActiveStageOutput::GraphicsUpdate(_))));
        assert_eq!(
            &image.data()[(2 * 12 + 3) * 4..(2 * 12 + 3) * 4 + 4],
            &[251, 251, 251, 255]
        );
        assert_eq!(
            &image.data()[(5 * 12 + 7) * 4..(5 * 12 + 7) * 4 + 4],
            &[255, 255, 255, 255],
            "只消费首个 mouse event"
        );
        stage.update_mouse_pos(5, 4);
        process_cursor(
            &mut stage,
            &mut image,
            UpdateCode::PositionPointer,
            3,
            &encode_vec(&Point16 { x: 6, y: 3 }).unwrap(),
            slow,
        );
        // replacement 不能丢失选择；新 cache 生命周期仍为空。
        stage.set_fastpath_processor(
            ProcessorBuilder {
                io_channel_id: 1003,
                user_channel_id: 1001,
                share_id: 1,
                enable_server_pointer: true,
                pointer_software_rendering: true,
                bulk_decompressor: None,
            }
            .build(),
        );
        let large = encode_vec(&LargePointerAttribute {
            width: 1,
            height: 1,
            xor_bpp: 32,
            hot_spot: Point16 { x: 0, y: 0 },
            cache_index: 2,
            xor_mask: &xor,
            and_mask: &[],
        })
        .unwrap();
        process_cursor(
            &mut stage,
            &mut image,
            UpdateCode::LargePointer,
            9,
            &large,
            slow,
        );
        assert_eq!(
            &image.data()[(3 * 12 + 6) * 4..(3 * 12 + 6) * 4 + 4],
            &[251, 251, 251, 255]
        );
        let color = encode_vec(&ColorPointerAttribute {
            xor_mask: &[5, 6, 7, 0],
            ..color
        })
        .unwrap();
        process_cursor(
            &mut stage,
            &mut image,
            UpdateCode::ColorPointer,
            6,
            &color,
            slow,
        );
        // 24bpp 不带 alpha，旧平方后的 RGBA byte 顺序。
        assert_eq!(
            &image.data()[(3 * 12 + 6) * 4..(3 * 12 + 6) * 4 + 4],
            &[0, 0, 0, 255]
        );
    }
}
#[test]
fn required_native_disabled_pointer_and_stopped_input_gate_leave_image() {
    use frd_core::{InputEvent, PixelPoint};
    let mut stage = stage(true, false);
    let mut image = DecodedImage::new(PixelFormat::RgbA32, 12, 8);
    let before = image.data().to_vec();
    let mut input = crate::input::RdpInputState::new();
    input.stop();
    assert_eq!(
        input.translate(InputEvent::PointerMove {
            remote: PixelPoint { x: 3, y: 2 }
        }),
        Err(crate::input::RdpInputError::Stopped)
    );
    input.resume();
    let events = input
        .translate(InputEvent::PointerMove {
            remote: PixelPoint { x: 3, y: 2 },
        })
        .unwrap();
    let result = stage
        .process_fastpath_input_with_decoded_coverage(&mut image, &events)
        .unwrap();
    assert!(result.decoded_coverage.is_empty());
    assert_eq!(image.data(), before);
}
