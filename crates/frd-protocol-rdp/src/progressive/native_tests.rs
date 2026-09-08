use super::*;
use ironrdp::pdu::codecs::rfx::{
    progressive::{ComponentCodecQuant, TileSimple},
    EntropyAlgorithm,
};

fn quant(value: u8) -> ComponentCodecQuant {
    ComponentCodecQuant {
        ll3: value,
        hl3: value,
        lh3: value,
        hh3: value,
        hl2: value,
        lh2: value,
        hh2: value,
        hl1: value,
        lh1: value,
        hh1: value,
    }
}
fn encoded(coefficients: &[i16; 4096]) -> Vec<u8> {
    let mut data = vec![0; 65536];
    let size =
        ironrdp::graphics::rlgr::encode(EntropyAlgorithm::Rlgr1, coefficients, &mut data).unwrap();
    data.truncate(size);
    data
}
fn tile<'a>(data: &'a [u8], flags: u8) -> ProgressiveTile<'a> {
    ProgressiveTile::Simple(TileSimple {
        quant_idx_y: 0,
        quant_idx_cb: 0,
        quant_idx_cr: 0,
        x_idx: 0,
        y_idx: 0,
        flags,
        y_data: data,
        cb_data: data,
        cr_data: data,
        tail_data: &[],
    })
}
#[test]
fn real_rlgr_encoder_to_native_first_produces_neutral_bgra_for_both_layouts() {
    let backend = NativeBackend::new().unwrap();
    let data = encoded(&[0; 4096]);
    let tile = tile(&data, 0);
    for reduce_extrapolate in [false, true] {
        let parameters = TileParameters {
            base: [quant(6); 3],
            progressive: [quant(0); 3],
            subband_diffing: false,
            reduce_extrapolate,
            difference: false,
        };
        let (reference, state, pixels) = backend
            .decode_tile(
                None,
                None,
                TileRequest {
                    tile: &tile,
                    parameters: &parameters,
                    previous_parameters: None,
                },
            )
            .unwrap();
        assert!(reference.current.iter().flatten().all(|v| *v == 0));
        assert!(state.das.iter().flatten().all(|v| *v == 0));
        assert_eq!(pixels, [128, 128, 128, 255].repeat(4096));
    }
}
#[test]
fn truncated_first_does_not_mutate_existing_reference_or_das() {
    let backend = NativeBackend::new().unwrap();
    let parameters = TileParameters {
        base: [quant(6); 3],
        progressive: [quant(0); 3],
        subband_diffing: true,
        reduce_extrapolate: false,
        difference: true,
    };
    let reference = NativeReference {
        current: Box::new([[17; 4096]; 3]),
    };
    let state = NativeTileState {
        das: Box::new([[-1; 4096]; 3]),
    };
    let data = encoded(&[0; 4096]);
    let bad = tile(&data[..data.len() - 1], 1);
    assert!(backend
        .decode_tile(
            Some(&reference),
            Some(&state),
            TileRequest {
                tile: &bad,
                parameters: &parameters,
                previous_parameters: Some(&parameters)
            }
        )
        .is_err());
    assert!(reference.current.iter().flatten().all(|v| *v == 17));
    assert!(state.das.iter().flatten().all(|v| *v == -1));
}

#[test]
fn new_context_difference_preserves_surface_reference_and_resets_das() {
    let backend = NativeBackend::new().unwrap();
    let parameters = TileParameters {
        base: [quant(6); 3],
        progressive: [quant(0); 3],
        subband_diffing: true,
        reduce_extrapolate: true,
        difference: false,
    };
    let data = encoded(&[1; 4096]);
    let first = tile(&data, 0);
    let (reference, _, pixels) = backend
        .decode_tile(
            None,
            None,
            TileRequest {
                tile: &first,
                parameters: &parameters,
                previous_parameters: None,
            },
        )
        .unwrap();
    let zeros = encoded(&[0; 4096]);
    let difference = tile(&zeros, 1);
    let difference_parameters = TileParameters {
        difference: true,
        ..parameters
    };
    let (next, das, next_pixels) = backend
        .decode_tile(
            Some(&reference),
            None,
            TileRequest {
                tile: &difference,
                parameters: &difference_parameters,
                previous_parameters: None,
            },
        )
        .unwrap();
    assert_eq!(next.current, reference.current);
    assert_eq!(next_pixels, pixels);
    assert!(das.das.iter().flatten().all(|s| *s == 0));
}

#[test]
fn native_raw_upgrade_crosses_every_band_and_preserves_reference_on_truncation() {
    use ironrdp::pdu::codecs::rfx::progressive::TileUpgrade;
    let backend = NativeBackend::new().unwrap();
    let initial = TileParameters {
        base: [quant(7); 3],
        progressive: [quant(0); 3],
        subband_diffing: false,
        reduce_extrapolate: false,
        difference: false,
    };
    let refined = TileParameters {
        base: [quant(6); 3],
        ..initial.clone()
    };
    let data = encoded(&[1; 4096]);
    let first = tile(&data, 0);
    let (reference, das, _) = backend
        .decode_tile(
            None,
            None,
            TileRequest {
                tile: &first,
                parameters: &initial,
                previous_parameters: None,
            },
        )
        .unwrap();
    let raw = [255u8; 512];
    let wire = TileUpgrade {
        quant_idx_y: 0,
        quant_idx_cb: 0,
        quant_idx_cr: 0,
        x_idx: 0,
        y_idx: 0,
        quality: 255,
        y_srl_data: &[],
        y_raw_data: &raw,
        cb_srl_data: &[],
        cb_raw_data: &raw,
        cr_srl_data: &[],
        cr_raw_data: &raw,
    };
    let upgrade = ProgressiveTile::Upgrade(wire.clone());
    let (next, next_das, _) = backend
        .decode_tile(
            Some(&reference),
            Some(&das),
            TileRequest {
                tile: &upgrade,
                parameters: &refined,
                previous_parameters: Some(&initial),
            },
        )
        .unwrap();
    for (actual, previous) in next
        .current
        .iter()
        .flatten()
        .zip(reference.current.iter().flatten())
    {
        assert_eq!(*actual, previous.wrapping_add(32));
    }
    assert_eq!(next_das.das, das.das);
    let bad = ProgressiveTile::Upgrade(TileUpgrade {
        cr_raw_data: &raw[..511],
        ..wire
    });
    assert!(backend
        .decode_tile(
            Some(&reference),
            Some(&das),
            TileRequest {
                tile: &bad,
                parameters: &refined,
                previous_parameters: Some(&initial)
            }
        )
        .is_err());
    assert!(das.das.iter().flatten().all(|s| *s == 1));
}
