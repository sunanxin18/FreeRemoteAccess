use super::*;
use crate::fast_path::{Processor, ProcessorBuilder, UpdateKind};
use crate::{ActiveStage, ActiveStageBuilder, ActiveStageOutput};
use core::error::Error as _;
use ironrdp_core::{ReadCursor, WriteBuf, decode_cursor, encode_vec};
use ironrdp_pdu::fast_path::{EncryptionFlags, FastPathHeader, FastPathUpdatePdu, Fragmentation, UpdateCode};
use ironrdp_pdu::geometry::ExclusiveRectangle;
use ironrdp_pdu::surface_commands::{ExtendedBitmapDataPdu, SurfaceBitsPdu, SurfaceCommand};
use ironrdp_pdu::{
    Action,
    bitmap::{BitmapData, BitmapUpdateData, Compression},
};
use ironrdp_svc::StaticChannelSet;

const FORMATS: [PixelFormat; 8] = [
    PixelFormat::ARgb32,
    PixelFormat::XRgb32,
    PixelFormat::ABgr32,
    PixelFormat::XBgr32,
    PixelFormat::BgrA32,
    PixelFormat::BgrX32,
    PixelFormat::RgbA32,
    PixelFormat::RgbX32,
];
fn rect(left: u16, top: u16, right: u16, bottom: u16) -> InclusiveRectangle {
    InclusiveRectangle {
        left,
        top,
        right,
        bottom,
    }
}
fn processor(native: bool, pointer: bool) -> Processor {
    let mut p = ProcessorBuilder {
        io_channel_id: 1003,
        user_channel_id: 1001,
        share_id: 1,
        enable_server_pointer: pointer,
        pointer_software_rendering: pointer,
        bulk_decompressor: None,
    }
    .build();
    if native {
        p.set_native_legacy_kernels(NativeKernels::detect().unwrap());
    }
    p
}
fn stage(native: bool, pointer: bool) -> ActiveStage {
    let mut s = ActiveStageBuilder {
        static_channels: StaticChannelSet::new(),
        user_channel_id: 1001,
        io_channel_id: 1003,
        message_channel_id: None,
        share_id: 1,
        compression_type: None,
        enable_server_pointer: pointer,
        pointer_software_rendering: pointer,
    }
    .build();
    if native {
        s.set_native_legacy_kernels(NativeKernels::detect().unwrap());
    }
    s
}
fn bitmap(wire: &[u8], bpp: u16, sw: u16, sh: u16, r: InclusiveRectangle, compressed: bool) -> BitmapData<'_> {
    BitmapData {
        rectangle: r,
        width: sw,
        height: sh,
        bits_per_pixel: bpp,
        compression_flags: if compressed {
            Compression::BITMAP_COMPRESSION | Compression::NO_BITMAP_COMPRESSION_HDR
        } else {
            Compression::empty()
        },
        compressed_data_header: None,
        bitmap_data: wire,
    }
}
fn packet(updates: &[(UpdateCode, Vec<u8>)]) -> Vec<u8> {
    let mut payload = Vec::new();
    for (code, data) in updates {
        payload.extend(
            encode_vec(&FastPathUpdatePdu {
                fragmentation: Fragmentation::Single,
                update_code: *code,
                compression_flags: None,
                compression_type: None,
                data,
            })
            .unwrap(),
        );
    }
    let h = FastPathHeader::new(EncryptionFlags::empty(), payload.len());
    let mut out = encode_vec(&h).unwrap();
    out.extend(payload);
    out
}
fn bmp_frame(v: Vec<BitmapData<'_>>) -> Vec<u8> {
    packet(&[(
        UpdateCode::Bitmap,
        encode_vec(&BitmapUpdateData { rectangles: v }).unwrap(),
    )])
}
fn slow_frame(v: Vec<BitmapData<'_>>) -> Vec<u8> {
    // 上游未提供 Update encode；测试构造18字节 ShareControl/Data header，
    // 再调用真实 MCS/X224 encoder 和完整 ActiveStage parser。
    let data = encode_vec(&BitmapUpdateData { rectangles: v }).unwrap();
    let mut user_data = Vec::new();
    user_data.extend(u16::try_from(18 + data.len()).unwrap().to_le_bytes());
    user_data.extend(0x17u16.to_le_bytes());
    user_data.extend(1001u16.to_le_bytes());
    user_data.extend(1u32.to_le_bytes());
    user_data.extend([0, 2]);
    user_data.extend(u16::try_from(data.len()).unwrap().to_le_bytes());
    user_data.extend([2, 0, 0, 0]);
    user_data.extend(data);
    encode_vec(&ironrdp_pdu::x224::X224(ironrdp_pdu::mcs::SendDataIndication {
        initiator_id: 1001,
        channel_id: 1003,
        user_data: user_data.into(),
    }))
    .unwrap()
}

fn surface(set: bool, codec: u8, bpp: u8, sw: u16, sh: u16, d: ExclusiveRectangle, data: &[u8]) -> Vec<u8> {
    let bits = SurfaceBitsPdu {
        destination: d,
        extended_bitmap_data: ExtendedBitmapDataPdu {
            bpp,
            codec_id: codec,
            width: sw,
            height: sh,
            header: None,
            data,
        },
    };
    let c = if set {
        SurfaceCommand::SetSurfaceBits(bits)
    } else {
        SurfaceCommand::StreamSurfaceBits(bits)
    };
    packet(&[(UpdateCode::SurfaceCommands, encode_vec(&c).unwrap())])
}
fn exact(result: &ActiveStageProcessResult, expected: &[Vec<InclusiveRectangle>]) {
    let indices: Vec<_> = result
        .outputs
        .iter()
        .enumerate()
        .filter_map(|(i, o)| matches!(o, ActiveStageOutput::GraphicsUpdate(_)).then_some(i))
        .collect();
    assert_eq!(result.decoded_coverage.len(), indices.len());
    assert_eq!(indices.len(), expected.len());
    for ((receipt, index), rects) in result.decoded_coverage.iter().zip(indices).zip(expected) {
        assert_eq!(receipt.graphics_output_index, index);
        assert_eq!(&receipt.rectangles, rects);
    }
}
#[test]
fn encoded_active_fast_slow_default_native_endpoint_gap_and_later_middle() {
    for native in [false, true] {
        for slow in [false, true] {
            for compressed in [false, true] {
                for bpp in [24, 32] {
                    let a = if compressed {
                        if bpp == 24 {
                            vec![0x61, 1, 2, 3]
                        } else {
                            vec![0x20, 3, 2, 1, 0]
                        }
                    } else {
                        vec![1, 2, 3, 0]
                    };
                    let b = if compressed {
                        if bpp == 24 {
                            vec![0x61, 4, 5, 6]
                        } else {
                            vec![0x20, 6, 5, 4, 0]
                        }
                    } else {
                        vec![4, 5, 6, 0]
                    };
                    let r0 = rect(0, 0, 0, 0);
                    let r2 = rect(2, 0, 2, 0);
                    let mut image = DecodedImage::new(PixelFormat::RgbA32, 3, 1);
                    image.data.fill(0x55);
                    let mut s = stage(native, false);
                    let updates = vec![
                        bitmap(&a, bpp, 1, 1, r0.clone(), compressed),
                        bitmap(&b, bpp, 1, 1, r2.clone(), compressed),
                    ];
                    let f = if slow { slow_frame(updates) } else { bmp_frame(updates) };
                    let result = s
                        .process_with_decoded_coverage(
                            &mut image,
                            if slow { Action::X224 } else { Action::FastPath },
                            &f,
                        )
                        .unwrap();
                    exact(&result, &[vec![r0, r2]]);
                    assert!(
                        result
                            .outputs
                            .iter()
                            .any(|o| matches!(o,ActiveStageOutput::GraphicsUpdate(r) if r==&rect(0,0,2,0)))
                    );
                    assert_eq!(&image.data()[4..8], &[0x55; 4]);
                    let mid = rect(1, 0, 1, 0);
                    let result = s
                        .process_with_decoded_coverage(
                            &mut image,
                            Action::FastPath,
                            &bmp_frame(vec![bitmap(&a, bpp, 1, 1, mid.clone(), compressed)]),
                        )
                        .unwrap();
                    exact(&result, &[vec![mid]]);
                    assert_eq!(&image.data()[4..7], &[3, 2, 1]);
                    // 公开 Processor 共用同一个 parser；内部 receipt 索引暂为 update 序号。
                    let mut c = CoverageCollector::enabled();
                    let updates =
                        processor(native, false).process_collected(&mut image, &f, &mut WriteBuf::new(), &mut c);
                    if !slow {
                        assert!(updates.is_ok());
                        assert_eq!(c.receipts[0].rectangles, vec![rect(0, 0, 0, 0), rect(2, 0, 2, 0)]);
                    }
                }
            }
        }
    }
}
fn raw(bpp: u16, sw: usize, sh: usize) -> Vec<u8> {
    let bytes = usize::from(bpp).div_ceil(8);
    let tight = sw * bytes;
    let stride = (tight + 3) & !3;
    let mut out = Vec::new();
    for y in 0..sh {
        for x in 0..sw {
            for b in 0..bytes {
                out.push((7 + y * 43 + x * 11 + b * 19) as u8);
            }
        }
        out.extend(vec![0xeb; stride - tight]);
    }
    out
}
fn planar(sw: usize, sh: usize, alpha: bool, chroma: bool) -> Vec<u8> {
    let mut out = vec![if chroma {
        0x09 | if alpha { 0 } else { 0x20 }
    } else {
        if alpha { 0 } else { 0x20 }
    }];
    if alpha {
        out.extend(vec![37; sw * sh]);
    }
    out.extend((0..sw * sh).map(|n| (30 + n * 3) as u8));
    let n = if chroma {
        sw.div_ceil(2) * sh.div_ceil(2)
    } else {
        sw * sh
    };
    out.extend(vec![50; n]);
    out.extend(vec![70; n]);
    out.push(0);
    out
}
#[test]
fn complete_source_raw_packed_rdp6_crop_matches_full_same_shape_reference_all_targets() {
    for format in FORMATS {
        for bpp in [8, 15, 16, 24, 32] {
            for compressed in [false, true] {
                let sw = 3;
                let sh = 3;
                let source = raw(bpp, sw, sh);
                let bytes = usize::from(bpp).div_ceil(8);
                let stride = (sw * bytes + 3) & !3;
                let mut wire = Vec::new();
                if compressed {
                    if bpp == 32 {
                        wire = planar(sw, sh, false, false);
                    } else {
                        wire.push(0x89);
                        for row in source.chunks_exact(stride) {
                            wire.extend_from_slice(&row[..sw * bytes]);
                        }
                    }
                } else {
                    wire = source;
                }
                let mut oracle = DecodedImage::new(format, 3, 3);
                processor(false, false)
                    .process_bitmap_update(
                        &mut oracle,
                        BitmapUpdateData {
                            rectangles: vec![bitmap(&wire, bpp, 3, 3, rect(0, 0, 2, 2), compressed)],
                        },
                    )
                    .unwrap();
                for native in [false, true] {
                    let mut im = DecodedImage::new(format, 2, 2);
                    let result = stage(native, false)
                        .process_with_decoded_coverage(
                            &mut im,
                            Action::FastPath,
                            &bmp_frame(vec![bitmap(&wire, bpp, 3, 3, rect(0, 0, 1, 1), compressed)]),
                        )
                        .unwrap();
                    exact(&result, &[vec![rect(0, 0, 1, 1)]]);
                    let expected: Vec<_> = (0..2)
                        .flat_map(|y| oracle.data()[y * 12..y * 12 + 8].iter().copied())
                        .collect();
                    assert_eq!(
                        im.data(),
                        expected,
                        "{format:?} bpp={bpp} compressed={compressed} native={native}"
                    );
                }
            }
        }
    }
    for alpha in [false, true] {
        for chroma in [false, true] {
            let wire = planar(3, 3, alpha, chroma);
            let mut full = DecodedImage::new(PixelFormat::RgbA32, 3, 3);
            processor(false, false)
                .process_bitmap_update(
                    &mut full,
                    BitmapUpdateData {
                        rectangles: vec![bitmap(&wire, 32, 3, 3, rect(0, 0, 2, 2), true)],
                    },
                )
                .unwrap();
            for native in [false, true] {
                let mut im = DecodedImage::new(PixelFormat::RgbA32, 2, 2);
                stage(native, false)
                    .process_with_decoded_coverage(
                        &mut im,
                        Action::FastPath,
                        &bmp_frame(vec![bitmap(&wire, 32, 3, 3, rect(0, 0, 1, 1), true)]),
                    )
                    .unwrap();
                assert_eq!(
                    im.data(),
                    (0..2)
                        .flat_map(|y| full.data()[y * 12..y * 12 + 8].iter().copied())
                        .collect::<Vec<_>>()
                );
            }
        }
    }
}
#[test]
fn underfill_and_empty_progress_fail_before_prefilled_apply_decoder_error_has_empty_receipt() {
    for bpp in [8, 15, 16, 24] {
        for wire in [&[][..], &[0xfd][..], &[0xfe][..]] {
            let mut im = DecodedImage::new(PixelFormat::RgbA32, 2, 2);
            im.data.fill(71);
            let before = im.data().to_vec();
            let e = stage(false, false)
                .process_with_decoded_coverage(
                    &mut im,
                    Action::FastPath,
                    &bmp_frame(vec![bitmap(wire, bpp, 2, 2, rect(0, 0, 1, 1), true)]),
                )
                .unwrap_err();
            assert!(matches!(
                e.source()
                    .unwrap()
                    .downcast_ref::<ironrdp_graphics::rle::CompleteRleError>(),
                Some(ironrdp_graphics::rle::CompleteRleError::Incomplete { .. })
            ));
            assert_eq!(im.data(), before);
            // 老 API 仍允许 portable underfill；不执行 rich 验证或元数据分配。
            let mut old = DecodedImage::new(PixelFormat::RgbA32, 2, 2);
            assert!(
                stage(false, false)
                    .process(
                        &mut old,
                        Action::FastPath,
                        &bmp_frame(vec![bitmap(wire, bpp, 2, 2, rect(0, 0, 1, 1), true)])
                    )
                    .is_ok()
            );
        }
    }
    for bpp in [24, 32] {
        let mut im = DecodedImage::new(PixelFormat::RgbA32, 2, 2);
        im.data.fill(81);
        let before = im.data().to_vec();
        let result = stage(false, false)
            .process_with_decoded_coverage(
                &mut im,
                Action::FastPath,
                &bmp_frame(vec![bitmap(&[0xff], bpp, 2, 2, rect(0, 0, 1, 1), true)]),
            )
            .unwrap();
        exact(&result, &[vec![]]);
        assert_eq!(im.data(), before);
        assert!(
            stage(true, false)
                .process_with_decoded_coverage(
                    &mut im,
                    Action::FastPath,
                    &bmp_frame(vec![bitmap(&[0xff], bpp, 2, 2, rect(0, 0, 1, 1), true)])
                )
                .is_err()
        );
    }
}
#[test]
fn raw_final_padding_geometry_resource_and_combined_invalid_order() {
    for native in [false, true] {
        for bpp in [8, 15, 16, 24, 32] {
            let source = raw(bpp, 3, 3);
            let mut im = DecodedImage::new(PixelFormat::RgbA32, 2, 2);
            im.data.fill(72);
            let before = im.data().to_vec();
            let e = stage(native, false)
                .process_with_decoded_coverage(
                    &mut im,
                    Action::FastPath,
                    &bmp_frame(vec![bitmap(
                        &source[..source.len() - 1],
                        bpp,
                        3,
                        3,
                        rect(0, 0, 1, 1),
                        false,
                    )]),
                )
                .unwrap_err();
            assert!(e.source().is_some());
            assert_eq!(im.data(), before);
        }
        // 几何检查先于源长度和 cursor begin；同时图外且几何畸形也必须被拒绝。
        for (sw, sh, r) in [
            (0, 1, rect(0, 0, 0, 0)),
            (1, 0, rect(0, 0, 0, 0)),
            (1, 1, rect(2, 0, 1, 0)),
            (1, 1, rect(0, 0, 1, 1)),
            (65535, 65535, rect(0, 0, 0, 0)),
        ] {
            let mut im = DecodedImage::new(PixelFormat::RgbA32, 2, 2);
            let e = stage(native, false)
                .process_with_decoded_coverage(
                    &mut im,
                    Action::FastPath,
                    &bmp_frame(vec![bitmap(&[], 24, sw, sh, r, false)]),
                )
                .unwrap_err();
            assert_eq!(
                e.source().unwrap().downcast_ref::<KernelError>(),
                Some(&KernelError::InvalidDimensions)
            );
            assert_eq!(im.data(), [0; 16]);
        }
    }
}
#[test]
fn set_ignores_incoming_bounds_stream_crops_and_none_layout_stays_bottom_up_tight() {
    let tight = [1, 2, 3, 4, 5, 6, 7, 8, 9, 11, 12, 13, 14, 15, 16, 17, 18, 19];
    for native in [false, true] {
        let mut im = DecodedImage::new(PixelFormat::RgbA32, 4, 3);
        let d = ExclusiveRectangle {
            left: 1,
            top: 1,
            right: 0,
            bottom: 0,
        };
        let result = stage(native, false)
            .process_with_decoded_coverage(&mut im, Action::FastPath, &surface(true, 0, 24, 3, 2, d, &tight))
            .unwrap();
        exact(&result, &[vec![rect(1, 1, 3, 2)]]);
        assert_eq!(&im.data()[20..24], &[13, 12, 11, 255]);
        let mut im = DecodedImage::new(PixelFormat::RgbA32, 2, 1);
        let d = ExclusiveRectangle {
            left: 0,
            top: 0,
            right: 2,
            bottom: 1,
        };
        let result = stage(native, false)
            .process_with_decoded_coverage(&mut im, Action::FastPath, &surface(false, 0, 24, 3, 2, d, &tight))
            .unwrap();
        exact(&result, &[vec![rect(0, 0, 1, 0)]]);
        assert_eq!(im.data(), &[13, 12, 11, 255, 16, 15, 14, 255]);
        for d in [
            ExclusiveRectangle {
                left: 1,
                top: 0,
                right: 0,
                bottom: 1,
            },
            ExclusiveRectangle {
                left: 0,
                top: 0,
                right: 4,
                bottom: 1,
            },
        ] {
            assert!(
                stage(native, false)
                    .process_with_decoded_coverage(&mut im, Action::FastPath, &surface(false, 0, 24, 3, 2, d, &tight))
                    .is_err()
            );
        }
    }
}
fn frozen_rfx(name: &str) -> &'static [u8] {
    let f = include_bytes!("fixtures/rfx-original-supplemental.bin");
    let mut input = &f[12..];
    while !input.is_empty() {
        let fields: [&[u8]; 6] = std::array::from_fn(|_| {
            let n = u32::from_le_bytes(input[..4].try_into().unwrap()) as usize;
            let value = &input[4..4 + n];
            input = &input[4 + n..];
            value
        });
        if fields[0] == name.as_bytes() {
            return fields[2];
        }
    }
    panic!("missing fixture {name}")
}
fn rfx_wire(clips: &[(u16, u16, u16, u16)], tiles: usize) -> Vec<u8> {
    use ironrdp_pdu::codecs::rfx::{Block, CodecChannel, RfxRectangle};
    let mut input = ReadCursor::new(frozen_rfx("session-1-two-success"));
    let mut out = Vec::new();
    while !input.is_empty() {
        let mut block: Block<'_> = decode_cursor(&mut input).unwrap();
        match &mut block {
            Block::CodecChannel(CodecChannel::Region(r)) => {
                r.rectangles = clips
                    .iter()
                    .map(|&(x, y, width, height)| RfxRectangle { x, y, width, height })
                    .collect()
            }
            Block::CodecChannel(CodecChannel::TileSet(t)) => {
                for (i, tile) in t.tiles.iter_mut().enumerate() {
                    tile.x = u16::try_from(i).unwrap();
                    tile.y = 0;
                }
                t.tiles.truncate(tiles);
            }
            Block::Channels(c) => {
                c.0[0].width = 128;
                c.0[0].height = 64;
            }
            _ => {}
        }
        out.extend(encode_vec(&block).unwrap());
    }
    out
}
#[test]
fn rfx_successful_tile_intersections_match_independent_pixel_membership_oracle() {
    // 小范围离散 clip 跨越 tile 边界；独立枚举每个屏幕像素的成员关系。
    for format in FORMATS {
        for native in [false, true] {
            for (clips, tiles) in [
                (vec![(0, 0, 1, 1)], 2),
                (vec![(1, 1, 2, 2), (63, 2, 3, 2), (100, 1, 2, 1)], 2),
                (vec![(63, 2, 3, 2)], 1),
                (vec![], 1),
                (vec![], 0),
                (vec![(80, 2, 2, 2)], 1),
                (vec![(500, 2, 2, 2)], 2),
                (vec![(0, 0, 1, 2), (2, 0, 1, 4), (0, 1, 1, 2), (2, 1, 1, 4)], 2),
                (
                    vec![(0, 0, 1, 2), (2, 0, 1, 4), (0, 1, 1, 2), (2, 1, 1, 4), (2, 0, 1, 4)],
                    2,
                ),
            ] {
                let wire = rfx_wire(&clips, tiles);
                let mut im = DecodedImage::new(format, 128, 64);
                let result = stage(native, false)
                    .process_with_decoded_coverage(
                        &mut im,
                        Action::FastPath,
                        &surface(
                            true,
                            3,
                            32,
                            128,
                            64,
                            ExclusiveRectangle {
                                left: 0,
                                top: 0,
                                right: 128,
                                bottom: 64,
                            },
                            &wire,
                        ),
                    )
                    .unwrap();
                let actual: Vec<_> = result
                    .decoded_coverage
                    .iter()
                    .flat_map(|r| r.rectangles.iter())
                    .collect();
                for y in 0..64 {
                    for x in 0..128 {
                        let in_clip = clips.is_empty()
                            || clips
                                .iter()
                                .any(|&(cx, cy, w, h)| x >= cx && x < cx + w && y >= cy && y < cy + h);
                        let arrived = x < 64 * tiles as u16;
                        let expected = in_clip && arrived;
                        let found = actual
                            .iter()
                            .any(|r| x >= r.left && x <= r.right && y >= r.top && y <= r.bottom);
                        let written = im.data()[(usize::from(y) * 128 + usize::from(x)) * 4..][..4] != [0; 4];
                        assert_eq!(
                            written, expected,
                            "actual image x={x} y={y} clips={clips:?} native={native}"
                        );
                        assert_eq!(
                            found, expected,
                            "x={x} y={y} tiles={tiles} clips={clips:?} native={native}"
                        );
                    }
                }
                for receipt in &result.decoded_coverage {
                    assert!(matches!(
                        result.outputs[receipt.graphics_output_index],
                        ActiveStageOutput::GraphicsUpdate(_)
                    ));
                }
                if clips == vec![(0, 0, 1, 1)] {
                    assert_eq!(actual, vec![&rect(0, 0, 0, 0)]);
                }
            }
        }
    }
}

fn pointer_frame(code: UpdateCode, data: &[u8]) -> Vec<u8> {
    packet(&[(code, data.to_vec())])
}
fn pointer_image() -> DecodedImage {
    let mut im = DecodedImage::new(PixelFormat::RgbA32, 8, 6);
    im.move_pointer(1, 1).unwrap();
    im.update_pointer(Arc::new(DecodedPointer {
        width: 2,
        height: 2,
        hotspot_x: 0,
        hotspot_y: 0,
        bitmap_data: vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255],
    }))
    .unwrap();
    im
}
#[test]
fn cursor_damage_is_never_content_pending_local_input_cache_failures_and_content() {
    use ironrdp_pdu::input::fast_path::FastPathInputEvent;
    use ironrdp_pdu::input::mouse::{MousePdu, PointerFlags};
    for native in [false, true] {
        let mut s = stage(native, true);
        let mut im = pointer_image();
        let events = [FastPathInputEvent::MouseEvent(MousePdu {
            flags: PointerFlags::MOVE,
            number_of_wheel_rotation_units: 0,
            x_position: 5,
            y_position: 3,
        })];
        let result = s
            .process_fastpath_input_with_decoded_coverage(&mut im, &events)
            .unwrap();
        exact(&result, &[vec![]]);
        assert_eq!((im.pointer_x, im.pointer_y), (5, 3));
        s.update_mouse_pos(2, 2);
        let f = bmp_frame(vec![bitmap(&[3, 4, 5, 0], 24, 1, 1, rect(2, 2, 2, 2), false)]);
        let result = s.process_with_decoded_coverage(&mut im, Action::FastPath, &f).unwrap();
        assert_eq!(
            result
                .decoded_coverage
                .iter()
                .flat_map(|r| &r.rectangles)
                .cloned()
                .collect::<Vec<_>>(),
            vec![rect(2, 2, 2, 2)]
        );
        assert_eq!((im.pointer_x, im.pointer_y), (2, 2));
        assert!(
            result
                .outputs
                .iter()
                .any(|o| matches!(o,ActiveStageOutput::GraphicsUpdate(r) if r!=&rect(2,2,2,2)))
        );
        let result = s
            .process_with_decoded_coverage(
                &mut im,
                Action::FastPath,
                &pointer_frame(UpdateCode::HiddenPointer, &[]),
            )
            .unwrap();
        assert!(result.decoded_coverage.iter().all(|r| r.rectangles.is_empty()));
        let result = s
            .process_with_decoded_coverage(
                &mut im,
                Action::FastPath,
                &pointer_frame(UpdateCode::DefaultPointer, &[]),
            )
            .unwrap();
        assert!(result.decoded_coverage.iter().all(|r| r.rectangles.is_empty()));
    }
    // 显式 native 在首个失败或 cache miss 时保留待处理鼠标位置。
    let mut s = stage(true, true);
    let mut im = pointer_image();
    s.update_mouse_pos(4, 3);
    let initial = format!("{im:?}");
    let data = im.data().to_vec();
    assert!(
        s.process_with_decoded_coverage(
            &mut im,
            Action::FastPath,
            &bmp_frame(vec![bitmap(&[], 24, 1, 1, rect(0, 0, 0, 0), true)])
        )
        .is_err()
    );
    assert_eq!(format!("{im:?}"), initial);
    assert_eq!(im.data(), data);
    let result = s
        .process_with_decoded_coverage(
            &mut im,
            Action::FastPath,
            &pointer_frame(UpdateCode::CachedPointer, &91u16.to_le_bytes()),
        )
        .unwrap();
    exact(&result, &[]);
    assert_eq!((im.pointer_x, im.pointer_y), (1, 1));
    assert!(
        s.process_with_decoded_coverage(&mut im, Action::FastPath, &pointer_frame(UpdateCode::NewPointer, &[0]))
            .is_err()
    );
    assert_eq!((im.pointer_x, im.pointer_y), (1, 1));
    let result = s
        .process_with_decoded_coverage(&mut im, Action::FastPath, &packet(&[]))
        .unwrap();
    exact(&result, &[vec![]]);
    assert_eq!((im.pointer_x, im.pointer_y), (4, 3));
}
#[test]
fn response_control_indices_explicit_empty_origin_and_old_enums_exhaustive() {
    let mut s = stage(false, false);
    let mut im = DecodedImage::new(PixelFormat::RgbA32, 3, 1);
    let data = encode_vec(&BitmapUpdateData {
        rectangles: vec![bitmap(&[1, 2, 3, 0], 24, 1, 1, rect(0, 0, 0, 0), false)],
    })
    .unwrap();
    let marker = encode_vec(&SurfaceCommand::FrameMarker(
        ironrdp_pdu::surface_commands::FrameMarkerPdu {
            frame_action: ironrdp_pdu::surface_commands::FrameAction::End,
            frame_id: Some(37),
        },
    ))
    .unwrap();
    let result = s
        .process_with_decoded_coverage(
            &mut im,
            Action::FastPath,
            &packet(&[
                (UpdateCode::Bitmap, data.clone()),
                (UpdateCode::SurfaceCommands, marker),
                (UpdateCode::Bitmap, data),
            ]),
        )
        .unwrap();
    exact(&result, &[vec![rect(0, 0, 0, 0)], vec![], vec![rect(0, 0, 0, 0)]]);
    assert!(matches!(result.outputs[0], ActiveStageOutput::ResponseFrame(_)));
    let _: fn(UpdateKind) = |u| match u {
        UpdateKind::None
        | UpdateKind::Region(_)
        | UpdateKind::PointerDefault
        | UpdateKind::PointerHidden
        | UpdateKind::PointerPosition { .. }
        | UpdateKind::PointerBitmap(_) => {}
    };
    let _: fn(ActiveStageOutput) = |u| match u {
        ActiveStageOutput::ResponseFrame(_)
        | ActiveStageOutput::GraphicsUpdate(_)
        | ActiveStageOutput::PointerDefault
        | ActiveStageOutput::PointerHidden
        | ActiveStageOutput::PointerPosition { .. }
        | ActiveStageOutput::PointerBitmap(_)
        | ActiveStageOutput::Terminate(_)
        | ActiveStageOutput::DeactivateAll
        | ActiveStageOutput::MultitransportRequest(_)
        | ActiveStageOutput::AutoDetect(_) => {}
    };
}
#[test]
fn batch_budget_65536_65537_retains_prior_valid_pixels_but_returns_no_metadata() {
    let mut p = processor(false, false);
    let mut im = DecodedImage::new(PixelFormat::RgbA32, 1, 1);
    let mut c = CoverageCollector::enabled();
    // 上限贯穿同一次调用中的多个 update，不可在每次 Bitmap 更新时重置。
    for count in [32768, 32768] {
        let updates = p
            .process_bitmap_update_collected(
                &mut im,
                BitmapUpdateData {
                    rectangles: (0..count)
                        .map(|_| bitmap(&[1, 2, 3, 0], 24, 1, 1, rect(0, 0, 0, 0), false))
                        .collect(),
                },
                &mut c,
            )
            .unwrap();
        c.finish_updates(&updates).unwrap();
    }
    assert_eq!(c.total_rectangles, 65536);
    assert_eq!(c.receipts.iter().map(|r| r.rectangles.len()).sum::<usize>(), 65536);
    let before = im.data().to_vec();
    let e = p
        .process_bitmap_update_collected(
            &mut im,
            BitmapUpdateData {
                rectangles: vec![bitmap(&[11, 12, 13, 0], 24, 1, 1, rect(0, 0, 0, 0), false)],
            },
            &mut c,
        )
        .unwrap_err();
    assert!(matches!(
        e.source().unwrap().downcast_ref::<CoverageError>(),
        Some(CoverageError::Budget)
    ));
    assert_eq!(im.data(), before);
    let mut entries = CoverageCollector::enabled();
    for _ in 0..65536 {
        entries.finish_updates(&[UpdateKind::Region(rect(0, 0, 0, 0))]).unwrap();
    }
    assert!(entries.reserve_entry().is_err());
    let mut s = stage(false, false);
    let f = bmp_frame(vec![
        bitmap(&[1, 2, 3, 0], 24, 1, 1, rect(0, 0, 0, 0), false),
        bitmap(&[], 24, 1, 1, rect(0, 0, 0, 0), true),
    ]);
    let e = s
        .process_with_decoded_coverage(&mut im, Action::FastPath, &f)
        .unwrap_err();
    assert!(e.source().is_some());
    assert_eq!(im.data(), [3, 2, 1, 255]);
    // 禁用 collector 即使合成计数超过限额也不执行 rich 门。
    let mut off = CoverageCollector {
        total_rectangles: usize::MAX,
        ..CoverageCollector::default()
    };
    off.reserve_rectangles(usize::MAX).unwrap();
    off.reserve_entry().unwrap();
    assert!(off.receipts.is_empty());
    assert_eq!(off.pending.capacity(), 0);
}
#[test]
fn fallible_receipt_and_rectangle_reserve_fail_before_content_or_cursor_begin() {
    for size in [
        core::mem::size_of::<DecodedCoverageReceipt>(),
        core::mem::size_of::<InclusiveRectangle>(),
    ] {
        let mut p = processor(false, true);
        let mut im = pointer_image();
        let before = im.data().to_vec();
        let state = format!("{im:?}");
        let mut c = CoverageCollector::enabled();
        if size == core::mem::size_of::<InclusiveRectangle>() {
            c.reserve_entry().unwrap();
        }
        let update = BitmapUpdateData {
            rectangles: vec![bitmap(&[31, 41, 51, 0], 24, 1, 1, rect(1, 1, 1, 1), false)],
        };
        crate::coverage_fail(size);
        let result = p.process_bitmap_update_collected(&mut im, update, &mut c);
        crate::coverage_fail(0);
        let e = result.unwrap_err();
        let source = e.source().unwrap().downcast_ref::<CoverageError>().unwrap();
        assert!(matches!(source, CoverageError::Allocation(_)));
        assert!(source.source().is_some());
        assert_eq!(im.data(), before);
        assert_eq!(format!("{im:?}"), state);
        assert!(c.pending.is_empty());
    }
}
#[test]
fn fragmented_actual_stage_call_has_shared_rectangle_ceiling_and_error_drops_batch_receipts() {
    fn fragments(s: &mut ActiveStage, im: &mut DecodedImage, data: &[u8]) -> Vec<u8> {
        let chunks: Vec<_> = data.chunks(8192).collect();
        for (i, chunk) in chunks[..chunks.len() - 1].iter().enumerate() {
            let u = FastPathUpdatePdu {
                fragmentation: if i == 0 {
                    Fragmentation::First
                } else {
                    Fragmentation::Next
                },
                update_code: UpdateCode::Bitmap,
                compression_flags: None,
                compression_type: None,
                data: chunk,
            };
            let mut f = encode_vec(&FastPathHeader::new(
                EncryptionFlags::empty(),
                encode_vec(&u).unwrap().len(),
            ))
            .unwrap();
            f.extend(encode_vec(&u).unwrap());
            let result = s.process_with_decoded_coverage(im, Action::FastPath, &f).unwrap();
            exact(&result, &[]);
        }
        encode_vec(&FastPathUpdatePdu {
            fragmentation: Fragmentation::Last,
            update_code: UpdateCode::Bitmap,
            compression_flags: None,
            compression_type: None,
            data: chunks.last().unwrap(),
        })
        .unwrap()
    }
    for additional in [1, 2] {
        let mut s = stage(false, false);
        let mut im = DecodedImage::new(PixelFormat::RgbA32, 1, 1);
        let many = encode_vec(&BitmapUpdateData {
            rectangles: (0..65535)
                .map(|_| bitmap(&[1, 2, 3, 0], 24, 1, 1, rect(0, 0, 0, 0), false))
                .collect(),
        })
        .unwrap();
        let mut payload = fragments(&mut s, &mut im, &many);
        for n in 0..additional {
            let data = encode_vec(&BitmapUpdateData {
                rectangles: vec![bitmap(
                    if n == 0 { &[11, 12, 13, 0] } else { &[21, 22, 23, 0] },
                    24,
                    1,
                    1,
                    rect(0, 0, 0, 0),
                    false,
                )],
            })
            .unwrap();
            payload.extend(
                encode_vec(&FastPathUpdatePdu {
                    fragmentation: Fragmentation::Single,
                    update_code: UpdateCode::Bitmap,
                    compression_flags: None,
                    compression_type: None,
                    data: &data,
                })
                .unwrap(),
            );
        }
        let mut f = encode_vec(&FastPathHeader::new(EncryptionFlags::empty(), payload.len())).unwrap();
        f.extend(payload);
        let result = s.process_with_decoded_coverage(&mut im, Action::FastPath, &f);
        if additional == 1 {
            let result = result.unwrap();
            assert_eq!(result.decoded_coverage.len(), 2);
            assert_eq!(
                result
                    .decoded_coverage
                    .iter()
                    .map(|r| r.rectangles.len())
                    .sum::<usize>(),
                65536
            );
        } else {
            let error = result.unwrap_err();
            assert!(matches!(
                error.source().unwrap().downcast_ref::<CoverageError>(),
                Some(CoverageError::Budget)
            ));
        }
        assert_eq!(im.data(), [13, 12, 11, 255]);
    }
}
#[test]
fn rich_default_crop_and_palette_match_immutable_independent_original_fixture() {
    let frozen = include_bytes!("fixtures/image-consumers-original.bin");
    assert_eq!(&frozen[..8], b"FRDIMG01");
    assert_eq!(u32::from_le_bytes(frozen[8..12].try_into().unwrap()), 2624);
    let mut input = &frozen[12..];
    let mut checked = 0;
    let palette: [[u8; 3]; 256] =
        std::array::from_fn(|i| [i as u8, (i as u8).wrapping_mul(37), (i as u8).wrapping_mul(73)]);
    while !input.is_empty() {
        let fields: [&[u8]; 6] = std::array::from_fn(|_| {
            let n = u32::from_le_bytes(input[..4].try_into().unwrap()) as usize;
            let value = &input[4..4 + n];
            input = &input[4 + n..];
            value
        });
        let [name, meta, wire, initial, expected, _] = fields;
        let kind = meta[0];
        if kind > 5 || (kind == 5 && meta[2] != 5) {
            continue;
        }
        let dim = |i| usize::from(u16::from_le_bytes(meta[i..i + 2].try_into().unwrap()));
        let (sw, sh, dw, dh) = (dim(4), dim(6), dim(8), dim(10));
        let format = match kind {
            0 => PixelRowSource::Packed(PackedDepth::Index8),
            1 => PixelRowSource::Packed(PackedDepth::Rgb555),
            2 => PixelRowSource::Packed(PackedDepth::Rgb565),
            3 => PixelRowSource::Packed(PackedDepth::Bgr24),
            4 => PixelRowSource::Rgb24,
            _ => PixelRowSource::Raw32(Pixel32Format::BgrX),
        };
        let mut im = DecodedImage::new(
            FORMATS[usize::from(meta[1])],
            u16::try_from(dw).unwrap() + 3,
            u16::try_from(dh).unwrap() + 3,
        );
        im.data.copy_from_slice(initial);
        let r = rect(1, 1, u16::try_from(dw).unwrap(), u16::try_from(dh).unwrap());
        assert_eq!(
            im.apply_bitmap_compat(
                wire,
                format,
                sw,
                sh,
                sw * native_source_bytes(format),
                true,
                &r,
                &palette
            )
            .unwrap(),
            Some(r)
        );
        assert_eq!(im.data(), expected, "frozen {}", String::from_utf8_lossy(name));
        checked += 1;
    }
    assert_eq!(checked, 632);
}
#[test]
fn rdp6_insufficient_planes_keep_original_parser_order_and_empty_default_metadata() {
    for wire in [
        &[0x20, 1, 2, 0][..],
        &[0, 1, 2, 3, 0][..],
        &[0x29, 1, 2, 0][..],
        &[0x30, 0xff][..],
    ] {
        let mut im = DecodedImage::new(PixelFormat::RgbA32, 3, 3);
        im.data.fill(91);
        let before = im.data().to_vec();
        let mut original = ironrdp_graphics::rdp6::BitmapStreamDecoder::default();
        let original_error = original
            .decode_bitmap_stream_to_rgb24(wire, &mut Vec::new(), 3, 3)
            .unwrap_err();
        let result = stage(false, false)
            .process_with_decoded_coverage(
                &mut im,
                Action::FastPath,
                &bmp_frame(vec![bitmap(wire, 32, 3, 3, rect(0, 0, 1, 1), true)]),
            )
            .unwrap();
        exact(&result, &[vec![]]);
        assert_eq!(im.data(), before);
        let native_error = stage(true, false)
            .process_with_decoded_coverage(
                &mut im,
                Action::FastPath,
                &bmp_frame(vec![bitmap(wire, 32, 3, 3, rect(0, 0, 1, 1), true)]),
            )
            .unwrap_err();
        match native_error
            .source()
            .unwrap()
            .downcast_ref::<ironrdp_graphics::rdp6::NativeBitmapError>()
            .unwrap()
        {
            ironrdp_graphics::rdp6::NativeBitmapError::Bitmap(e) => {
                assert_eq!(format!("{e:?}"), format!("{original_error:?}"))
            }
            _ => panic!("valid shape must preserve parser order"),
        }
        assert_eq!(im.data(), before);
    }
}
#[cfg(all(feature = "qoi", feature = "qoiz"))]
#[test]
fn optional_rich_default_and_native_exact_crops_all_targets_and_checked_header_failures() {
    for channels in [3, 4] {
        let source: Vec<u8> = (0..3 * 3 * channels).map(|n| (7 + n * 5) as u8).collect();
        let qoi = qoi::encode_to_vec(&source, 3, 3).unwrap();
        let mut z = vec![0; zstd_safe::compress_bound(qoi.len())];
        let size = zstd_safe::compress(&mut z, qoi.as_slice(), 1).unwrap();
        z.truncate(size);
        for (codec, data) in [(10, &qoi), (11, &z)] {
            for format in FORMATS {
                for native in [false, true] {
                    let mut im = DecodedImage::new(format, 2, 2);
                    let d = ExclusiveRectangle {
                        left: 0,
                        top: 0,
                        right: 2,
                        bottom: 2,
                    };
                    let result = stage(native, false)
                        .process_with_decoded_coverage(
                            &mut im,
                            Action::FastPath,
                            &surface(false, codec, 32, 3, 3, d, data),
                        )
                        .unwrap();
                    exact(&result, &[vec![rect(0, 0, 1, 1)]]);
                    let [r, g, b, a] = format.channel_offsets();
                    for y in 0..2 {
                        for x in 0..2 {
                            let from = (y * 3 + x) * channels;
                            let to = (y * 2 + x) * 4;
                            assert_eq!(im.data()[to + r], source[from]);
                            assert_eq!(im.data()[to + g], source[from + 1]);
                            assert_eq!(im.data()[to + b], source[from + 2]);
                            assert_eq!(im.data()[to + a], if channels == 4 { source[from + 3] } else { 255 });
                        }
                    }
                    let before = im.data().to_vec();
                    let d = ExclusiveRectangle {
                        left: 0,
                        top: 0,
                        right: 1,
                        bottom: 1,
                    };
                    let e = stage(native, false)
                        .process_with_decoded_coverage(
                            &mut im,
                            Action::FastPath,
                            &surface(false, codec, 32, 1, 9, d, data),
                        )
                        .unwrap_err();
                    assert!(e.source().is_some());
                    assert_eq!(im.data(), before);
                }
            }
        }
    }
}
#[test]
fn real_pointer_shape_cache_position_hide_redraw_and_palette_never_count_as_content() {
    use ironrdp_pdu::pointer::{ColorPointerAttribute, Point16, PointerAttribute};
    for native in [false, true] {
        let mut s = stage(native, true);
        let mut im = DecodedImage::new(PixelFormat::RgbA32, 8, 6);
        let pointer = PointerAttribute {
            xor_bpp: 32,
            color_pointer: ColorPointerAttribute {
                cache_index: 1,
                hot_spot: Point16 { x: 0, y: 0 },
                width: 1,
                height: 1,
                xor_mask: &[3, 2, 1, 255],
                and_mask: &[0, 0],
            },
        };
        let shape = encode_vec(&pointer).unwrap();
        let position = encode_vec(&Point16 { x: 4, y: 3 }).unwrap();
        for (code, data) in [
            (UpdateCode::NewPointer, shape),
            (UpdateCode::CachedPointer, 1u16.to_le_bytes().to_vec()),
            (UpdateCode::PositionPointer, position),
            (UpdateCode::HiddenPointer, vec![]),
            (UpdateCode::DefaultPointer, vec![]),
            (UpdateCode::CachedPointer, 1u16.to_le_bytes().to_vec()),
        ] {
            let result = s
                .process_with_decoded_coverage(&mut im, Action::FastPath, &pointer_frame(code, &data))
                .unwrap();
            let count = result
                .outputs
                .iter()
                .filter(|o| matches!(o, ActiveStageOutput::GraphicsUpdate(_)))
                .count();
            exact(&result, &vec![vec![]; count]);
            if code == UpdateCode::PositionPointer {
                assert_eq!((im.pointer_x, im.pointer_y), (4, 3));
            }
        }
        let result = s
            .process_with_decoded_coverage(
                &mut im,
                Action::FastPath,
                &bmp_frame(vec![bitmap(&[11, 12, 13, 0], 24, 1, 1, rect(4, 3, 4, 3), false)]),
            )
            .unwrap();
        exact(&result, &[vec![rect(4, 3, 4, 3)]]);
        let result = s
            .process_with_decoded_coverage(
                &mut im,
                Action::FastPath,
                &pointer_frame(UpdateCode::HiddenPointer, &[]),
            )
            .unwrap();
        assert!(result.decoded_coverage.iter().all(|r| r.rectangles.is_empty()));
        assert_eq!(&im.data()[(3 * 8 + 4) * 4..][..4], &[13, 12, 11, 255]);
        let mut colors = vec![0, 0];
        colors.extend(256u32.to_le_bytes());
        for i in 0u16..256 {
            let i = i as u8;
            colors.extend([i.wrapping_mul(3), i.wrapping_mul(2), i, 0]);
        }
        let result = s
            .process_with_decoded_coverage(&mut im, Action::FastPath, &packet(&[(UpdateCode::Palette, colors)]))
            .unwrap();
        exact(&result, &[]);
        let result = s
            .process_with_decoded_coverage(
                &mut im,
                Action::FastPath,
                &bmp_frame(vec![bitmap(&[7, 0, 0, 0], 8, 1, 1, rect(0, 0, 0, 0), false)]),
            )
            .unwrap();
        exact(&result, &[vec![rect(0, 0, 0, 0)]]);
        assert_eq!(&im.data()[..4], &[7, 14, 21, 255]);
    }
}
