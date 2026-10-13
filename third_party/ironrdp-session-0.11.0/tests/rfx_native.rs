use super::*;
use core::error::Error as _;
use ironrdp_graphics::legacy_native::{KernelError, NativeRfxError};
const CATALOG: &[u8] = include_bytes!("fixtures/rfx-original-catalog.bin");
const EXTRA: &[u8] = include_bytes!("fixtures/rfx-original-supplemental.bin");
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
