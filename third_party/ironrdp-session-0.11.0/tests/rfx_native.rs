use super::*;
use core::error::Error as _;
use ironrdp_graphics::legacy_native::{KernelError, NativeRfxError};
const CATALOG: &[u8] = include_bytes!("fixtures/rfx-original-catalog.bin");
const EXTRA: &[u8] = include_bytes!("fixtures/rfx-original-supplemental.bin");

// MS-RDPRFX 3.1.8.3.1 的 channel width/height 是编码帧尺寸；
// MS-RDPBCGR 2.2.9.2.2 的 destination 是主绘图表面坐标。
// 同一 64x64 编码帧平移后仍应贡献全部 4096 像素，不能用局部尺寸裁剪绝对坐标。
fn translated_local_channel_case(native: bool) {
    let mut catalog = &CATALOG[12..];
    let mut fixture = None;
    while !catalog.is_empty() {
        let name = core::str::from_utf8(field(&mut catalog)).unwrap();
        let _meta = field(&mut catalog);
        let wire = field(&mut catalog);
        let _initial = field(&mut catalog);
        let expected = field(&mut catalog);
        let result = core::str::from_utf8(field(&mut catalog)).unwrap();
        if name == "session-end-2970" {
            assert!(result.starts_with("Ok"));
            fixture = Some((wire, expected));
        }
    }
    let (wire, expected_tile) = fixture.expect("已有原版冻结 64x64 RFX 夹具");
    assert_eq!(expected_tile.len(), 64 * 64 * 4);
    for (image_width, image_height, destination) in [
        (
            96,
            96,
            InclusiveRectangle {
                left: 16,
                top: 24,
                right: 79,
                bottom: 87,
            },
        ),
        (
            96,
            96,
            InclusiveRectangle {
                left: 48,
                top: 40,
                right: 111,
                bottom: 103,
            },
        ),
        (
            96,
            96,
            InclusiveRectangle {
                left: 16,
                top: 24,
                right: 47,
                bottom: 55,
            },
        ),
        (
            96,
            96,
            InclusiveRectangle {
                left: 120,
                top: 128,
                right: 183,
                bottom: 191,
            },
        ),
        (
            65535,
            64,
            InclusiveRectangle {
                left: 65472,
                top: 0,
                right: 65535,
                bottom: 63,
            },
        ),
    ] {
        let mut context = DecodingContext::new();
        if native {
            context.set_native_legacy_kernels(NativeKernels::detect().unwrap());
        }
        let mut image = DecodedImage::new(PixelFormat::BgrX32, image_width, image_height);
        let mut coverage = CoverageCollector::enabled();
        let mut cursor = ReadCursor::new(wire);
        let (_, damage) = context
            .decode_region_collected(&mut image, &destination, &mut cursor, &mut coverage)
            .expect("合法的偏移 RFX surface");
        assert!(cursor.is_empty());
        // oracle 仅将既有冻结像素平移到主表面，不调用生产 clipping helper。
        let mut expected = vec![0; usize::from(image_width) * usize::from(image_height) * 4];
        let mut expected_coverage = vec![false; expected.len() / 4];
        for y in 0..64usize {
            for x in 0..64usize {
                let target_x = x + usize::from(destination.left);
                let target_y = y + usize::from(destination.top);
                if target_x >= usize::from(image_width)
                    || target_y >= usize::from(image_height)
                    || target_x > usize::from(destination.right)
                    || target_y > usize::from(destination.bottom)
                {
                    continue;
                }
                let target = target_y * usize::from(image_width) + target_x;
                let source = (y * 64 + x) * 4;
                expected[target * 4..target * 4 + 4].copy_from_slice(&expected_tile[source..source + 4]);
                expected_coverage[target] = true;
            }
        }
        let mismatched_pixels = image
            .data()
            .chunks_exact(4)
            .zip(expected.chunks_exact(4))
            .filter(|(actual, expected)| actual != expected)
            .count();
        assert_eq!(
            mismatched_pixels, 0,
            "native={native}, destination={destination:?}, damage={damage:?}: 平移、channel、destination及image裁剪必须保持精确像素"
        );
        let updates = damage
            .clone()
            .map(crate::fast_path::UpdateKind::Region)
            .into_iter()
            .collect::<Vec<_>>();
        coverage.finish_updates(&updates).unwrap();
        let mut actual_coverage = vec![false; expected_coverage.len()];
        for receipt in coverage.receipts {
            for rect in receipt.rectangles {
                assert!(rect.right < image_width && rect.bottom < image_height);
                for y in rect.top..=rect.bottom {
                    for x in rect.left..=rect.right {
                        actual_coverage[usize::from(y) * usize::from(image_width) + usize::from(x)] = true;
                    }
                }
            }
        }
        assert_eq!(
            actual_coverage
                .iter()
                .zip(&expected_coverage)
                .filter(|(a, b)| a != b)
                .count(),
            0,
            "native={native}: 实际内容收据必须与写入像素一致，空交集不能变成原点像素"
        );
    }
}

#[test]
fn translated_local_channel_rich_compat_preserves_complete_tile() {
    translated_local_channel_case(false);
}

#[test]
fn translated_local_channel_rich_native_preserves_complete_tile() {
    translated_local_channel_case(true);
}

#[test]
fn local_channel_clipping_precedes_translation_and_never_wraps_or_fills_gaps() {
    let destination = InclusiveRectangle {
        left: 16,
        top: 24,
        right: 95,
        bottom: 103,
    };
    let clips = [
        RfxRectangle {
            x: 60,
            y: 62,
            width: 20,
            height: 20,
        },
        RfxRectangle {
            x: 64,
            y: 0,
            width: 1,
            height: 1,
        },
        RfxRectangle {
            x: 0,
            y: 64,
            width: 1,
            height: 1,
        },
        RfxRectangle {
            x: 65535,
            y: 65535,
            width: 65535,
            height: 65535,
        },
        RfxRectangle {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
    ];
    let got = checked_clipping_rectangles(&clips, &destination, 64, 64, 128, 128, true).unwrap();
    assert_eq!(
        got.rectangles,
        vec![
            InclusiveRectangle {
                left: 16,
                top: 24,
                right: 16,
                bottom: 24
            },
            InclusiveRectangle {
                left: 76,
                top: 86,
                right: 79,
                bottom: 87
            },
        ]
    );
    let far = InclusiveRectangle {
        left: 65530,
        top: 65530,
        right: 65535,
        bottom: 65535,
    };
    let got = checked_clipping_rectangles(&clips[..4], &far, 64, 64, 65535, 65535, true).unwrap();
    assert!(got.rectangles.is_empty(), "大偏移+局部坐标不得回绕到原点");
    for (channel_w, channel_h, image_w, image_h) in
        [(0, 64, 128, 128), (64, 0, 128, 128), (64, 64, 0, 128), (64, 64, 128, 0)]
    {
        assert!(
            checked_clipping_rectangles(&clips, &destination, channel_w, channel_h, image_w, image_h, true).is_err()
        );
    }
    let zero = [RfxRectangle {
        x: 0,
        y: 0,
        width: 0,
        height: 1,
    }];
    assert!(checked_clipping_rectangles(&zero, &destination, 64, 64, 128, 128, true).is_err());
}

fn field<'a>(input: &mut &'a [u8]) -> &'a [u8] {
    let n = u32::from_le_bytes(input[..4].try_into().unwrap()) as usize;
    let v = &input[4..4 + n];
    *input = &input[4 + n..];
    v
}
#[test]
fn full_actual_session_wire_and_clipped_two_tiles_match_original_publication() {
    let k = NativeKernels::detect().unwrap();
    let mut n = 0;
    for frozen in [CATALOG, EXTRA] {
        let mut input = &frozen[12..];
        while !input.is_empty() {
            let name = core::str::from_utf8(field(&mut input)).unwrap();
            let meta = field(&mut input);
            let wire = field(&mut input);
            let initial = field(&mut input);
            let expected = field(&mut input);
            let result = core::str::from_utf8(field(&mut input)).unwrap();
            if ![0, 20].contains(&meta[0]) {
                continue;
            }
            n += 1;
            let width = if meta[0] == 0 { 64 } else { 128 };
            let rect = InclusiveRectangle {
                left: 0,
                top: 0,
                right: width - 1,
                bottom: 63,
            };
            if !result.starts_with("Panic") {
                let mut image = DecodedImage::new(PixelFormat::BgrX32, width, 64);
                assert_eq!(image.data(), initial);
                let r = DecodingContext::new().decode(&mut image, &rect, &mut ReadCursor::new(wire));
                assert_eq!(format!("{r:?}"), result, "default {name}");
                assert_eq!(image.data(), expected, "default {name}");
            }
            let mut context = DecodingContext::new();
            context.set_native_legacy_kernels(k);
            let mut image = DecodedImage::new(PixelFormat::BgrX32, width, 64);
            let r = context.decode(&mut image, &rect, &mut ReadCursor::new(wire));
            if result.starts_with("Ok") {
                assert_eq!(format!("{r:?}"), result, "native {name}");
            } else {
                let e = r.unwrap_err();
                assert!(e.source().is_some(), "source {name}");
                if name.contains("color") {
                    assert_eq!(
                        e.source().unwrap().downcast_ref::<KernelError>(),
                        Some(&KernelError::ColorOverflow)
                    );
                }
                if name.contains("empty") {
                    assert!(matches!(
                        e.source().unwrap().downcast_ref::<NativeRfxError>(),
                        Some(NativeRfxError::Rlgr(rlgr::RlgrError::EmptyTile))
                    ));
                }
            }
            assert_eq!(image.data(), expected, "native publication {name}");
            if name.contains("first-") {
                assert_eq!(image.data(), initial, "first failure no writes {name}");
            }
            if name.contains("second-") {
                assert_ne!(image.data(), initial, "successful first tile retained {name}");
                for row in image.data().chunks_exact(128 * 4) {
                    assert!(
                        row[64 * 4..].iter().all(|&v| v == 0),
                        "failed second tile untouched {name}"
                    );
                }
            }
            if name == "session-end-2970" {
                assert_eq!(wire.len(), 2970);
                assert_eq!(
                    image.data(),
                    include_bytes!("../../../crates/frd-rdp-legacy-kernels/tests/fixtures/decoded_image.bin")
                );
            }
        }
    }
    assert_eq!(n, 28);
}
fn q(v: u8) -> Quant {
    Quant {
        ll3: v,
        lh3: v,
        hl3: v,
        hh3: v,
        lh2: v,
        hl2: v,
        hh2: v,
        lh1: v,
        hl1: v,
        hh1: v,
    }
}
#[test]
fn tile_each_channel_rlgr_quant_and_color_fail_before_output_publication() {
    let k = NativeKernels::detect().unwrap();
    let mode = EntropyAlgorithm::Rlgr3;
    let mut bytes = vec![0; 10000];
    let n = rlgr::encode(mode, &[0; 4096], &mut bytes).unwrap();
    let zero = &bytes[..n];
    for channel in 0..3 {
        for invalid_quant in [false, true] {
            let mut tile = TileData {
                quants: [q(1), q(1), q(1)],
                data: [zero; 3],
            };
            if invalid_quant {
                tile.quants[channel] = q(17);
            } else {
                tile.data[channel] = &[];
            }
            let mut output = vec![0xa7; 16384];
            let mut planes = vec![vec![0; 4096]; 3];
            let mut temp = vec![0; 4096];
            let e = decode_tile(&tile, mode, &mut output, &mut planes, &mut temp, Some(&k)).unwrap_err();
            if invalid_quant {
                assert!(matches!(
                    e.source().unwrap().downcast_ref::<NativeRfxError>(),
                    Some(NativeRfxError::Kernel(KernelError::InvalidQuant))
                ));
            } else {
                assert!(matches!(
                    e.source().unwrap().downcast_ref::<NativeRfxError>(),
                    Some(NativeRfxError::Rlgr(rlgr::RlgrError::EmptyTile))
                ));
            }
            assert_eq!(output, vec![0xa7; 16384]);
        }
    }
    // Late color overflow: all three components have completed, color leaf still does not write output.
    let mut v = vec![0; 4096];
    v[4095] = 32767;
    let mut encoded = vec![0; 200000];
    let n = rlgr::encode(mode, &v, &mut encoded).unwrap();
    let tile = TileData {
        quants: [q(1), q(1), q(1)],
        data: [&encoded[..n]; 3],
    };
    let mut output = vec![0xa7; 16384];
    let mut planes = vec![vec![0; 4096]; 3];
    let mut temp = vec![0; 4096];
    let e = decode_tile(&tile, mode, &mut output, &mut planes, &mut temp, Some(&k)).unwrap_err();
    assert_eq!(
        e.source().unwrap().downcast_ref::<KernelError>(),
        Some(&KernelError::ColorOverflow)
    );
    assert_eq!(output, vec![0xa7; 16384]);
    assert!(planes.iter().all(|p| p.iter().any(|&x| x != 0)));
}
