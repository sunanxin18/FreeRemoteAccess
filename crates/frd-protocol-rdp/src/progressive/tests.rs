use super::*;
use ironrdp::pdu::codecs::rfx::{progressive::*, RfxRectangle};

struct Oracle;
impl TileDecoder for Oracle {
    type ReferenceState = u8;
    type TileState = u8;
    const REFERENCE_BYTES: usize = 1;
    const STATE_BYTES: usize = 1;
    fn decode_tile(
        &self,
        reference: Option<&u8>,
        previous: Option<&u8>,
        request: TileRequest<'_>,
    ) -> Result<(u8, u8, Vec<u8>)> {
        let value = match request.tile {
            ProgressiveTile::Simple(t) => {
                t.y_data.first().copied().ok_or(Error::Backend("empty"))?
            }
            ProgressiveTile::First(t) => {
                t.y_data.first().copied().ok_or(Error::Backend("empty"))?
            }
            ProgressiveTile::Upgrade(t) => {
                previous.ok_or(Error::MissingTile)?;
                reference.copied().ok_or(Error::MissingTile)? + t.y_raw_data[0]
            }
        };
        if value == 255 {
            return Err(Error::Backend("injected failure"));
        }
        let value = if !matches!(request.tile, ProgressiveTile::Upgrade(_))
            && request.parameters.difference
        {
            reference.copied().ok_or(Error::MissingTile)? + value
        } else {
            value
        };
        Ok((value, value, vec![value; TILE_BYTES]))
    }
}
fn first(x: u16, y: u16, bytes: &'static [u8]) -> ProgressiveTile<'static> {
    ProgressiveTile::First(TileFirst {
        quant_idx_y: 0,
        quant_idx_cb: 0,
        quant_idx_cr: 0,
        x_idx: x,
        y_idx: y,
        flags: 0,
        quality: 0,
        y_data: bytes,
        cb_data: &[0],
        cr_data: &[0],
        tail_data: &[],
    })
}
fn upgrade() -> ProgressiveTile<'static> {
    ProgressiveTile::Upgrade(TileUpgrade {
        quant_idx_y: 0,
        quant_idx_cb: 0,
        quant_idx_cr: 0,
        x_idx: 0,
        y_idx: 0,
        quality: 255,
        y_srl_data: &[],
        y_raw_data: &[1],
        cb_srl_data: &[],
        cb_raw_data: &[],
        cr_srl_data: &[],
        cr_raw_data: &[],
    })
}
fn region(
    rect: (u16, u16, u16, u16),
    tiles: Vec<ProgressiveTile<'static>>,
) -> ProgressiveBlock<'static> {
    ProgressiveBlock::Region(ProgressiveRegion {
        tile_size: 64,
        rects: vec![RfxRectangle {
            x: rect.0,
            y: rect.1,
            width: rect.2,
            height: rect.3,
        }],
        quant_vals: vec![ComponentCodecQuant::LOSSLESS],
        quant_prog_vals: vec![ProgressiveCodecQuant {
            quality: 100,
            y_quant: ComponentCodecQuant {
                hl1: 1,
                ..ComponentCodecQuant::LOSSLESS
            },
            cb_quant: ComponentCodecQuant::LOSSLESS,
            cr_quant: ComponentCodecQuant::LOSSLESS,
        }],
        flags: 0,
        tiles,
    })
}
fn frame(regions: Vec<ProgressiveBlock<'static>>) -> Vec<ProgressiveBlock<'static>> {
    let mut result = vec![ProgressiveBlock::FrameBegin(ProgressiveFrameBeginPdu {
        frame_index: 0,
        region_count: regions.len() as u16,
    })];
    result.extend(regions);
    result.push(ProgressiveBlock::FrameEnd(ProgressiveFrameEndPdu));
    result
}
fn decoder() -> Decoder<Oracle> {
    let mut d = Decoder::new(Oracle, Limits::default());
    d.begin_frame(1).unwrap();
    d
}

#[test]
fn first_upgrade_and_late_failure_are_transactional() {
    let mut d = decoder();
    let f = frame(vec![region((0, 0, 64, 64), vec![first(0, 0, &[5])])]);
    assert_eq!(d.decode(1, 2, 128, 64, &f).unwrap()[0].bgra[0], 5);
    d.end_frame(1).unwrap();
    d.begin_frame(2).unwrap();
    let bad = frame(vec![region(
        (0, 0, 128, 64),
        vec![upgrade(), first(1, 0, &[255])],
    )]);
    assert!(d.decode(1, 2, 128, 64, &bad).is_err());
    assert_eq!(d.tile_count(), 1);
    let good = frame(vec![region((0, 0, 64, 64), vec![upgrade()])]);
    assert_eq!(d.decode(1, 2, 128, 64, &good).unwrap()[0].bgra[0], 6);
}
#[test]
fn upgrade_requires_exact_surface_and_context_and_lifetimes() {
    let mut d = decoder();
    let f = frame(vec![region((0, 0, 64, 64), vec![first(0, 0, &[5])])]);
    d.decode(1, 2, 64, 64, &f).unwrap();
    let up = frame(vec![region((0, 0, 64, 64), vec![upgrade()])]);
    assert_eq!(
        d.decode(2, 2, 64, 64, &up).unwrap_err(),
        Error::Invalid("upgrade missing context tile")
    );
    assert_eq!(
        d.decode(1, 3, 64, 64, &up).unwrap_err(),
        Error::Invalid("upgrade missing context tile")
    );
    d.delete_context(1, 3);
    assert_eq!(d.context_count(), 1);
    d.delete_context(1, 2);
    assert_eq!(d.tile_count(), 1);
    assert_eq!(
        d.decode(1, 2, 64, 64, &up).unwrap_err(),
        Error::Invalid("upgrade missing context tile")
    );
    d.decode(3, 2, 64, 64, &f).unwrap();
    d.delete_surface(3);
    assert_eq!(d.context_count(), 0);
    d.reset();
    assert!(d.decode(3, 2, 64, 64, &f).is_err());
}
#[test]
fn masks_share_only_current_egfx_frame_tiles_across_payloads() {
    let mut d = decoder();
    let left = frame(vec![region((0, 0, 32, 64), vec![first(0, 0, &[7])])]);
    let right = frame(vec![region((32, 0, 32, 64), vec![])]);
    let a = d.decode(1, 1, 64, 64, &left).unwrap();
    assert_eq!(
        a[0].clip,
        Rect {
            x: 0,
            y: 0,
            width: 32,
            height: 64
        }
    );
    let b = d.decode(1, 1, 64, 64, &right).unwrap();
    assert_eq!(
        b[0].clip,
        Rect {
            x: 32,
            y: 0,
            width: 32,
            height: 64
        }
    );
    assert_eq!(b[0].bgra[0], 7);
    assert!(d.decode(1, 1, 64, 64, &right).is_err());
    d.end_frame(1).unwrap();
    d.begin_frame(2).unwrap();
    assert!(d.decode(1, 1, 64, 64, &right).is_err());
}
#[test]
fn missing_coverage_does_not_commit_decoded_tiles() {
    let mut d = decoder();
    let f = frame(vec![region((0, 0, 128, 64), vec![first(0, 0, &[5])])]);
    assert!(d.decode(1, 1, 128, 64, &f).is_err());
    assert_eq!(d.tile_count(), 0);
    assert_eq!(d.context_count(), 0);
}
#[test]
fn dimensions_tile_bounds_and_all_budgets_fail_before_commit() {
    let f = frame(vec![region((0, 0, 64, 64), vec![first(0, 0, &[5])])]);
    for limits in [
        Limits {
            max_contexts: 0,
            ..Limits::default()
        },
        Limits {
            max_tiles: 0,
            ..Limits::default()
        },
        Limits {
            max_frame_tiles: 0,
            ..Limits::default()
        },
        Limits {
            max_frame_rects: 0,
            ..Limits::default()
        },
        Limits {
            max_updates_per_call: 0,
            ..Limits::default()
        },
        Limits {
            max_decode_tiles_per_call: 0,
            ..Limits::default()
        },
        Limits {
            max_bytes: 0,
            ..Limits::default()
        },
        Limits {
            max_surface_pixels: 0,
            ..Limits::default()
        },
    ] {
        let mut d = Decoder::new(Oracle, limits);
        d.begin_frame(1).unwrap();
        assert_eq!(
            d.decode(1, 1, 64, 64, &f).unwrap_err(),
            Error::ResourceLimit
        );
        assert_eq!(d.context_count(), 0);
    }
    let mut d = decoder();
    d.decode(1, 1, 64, 64, &f).unwrap();
    assert!(d.decode(1, 1, 128, 64, &f).is_err());
    let oob = frame(vec![region((0, 0, 64, 64), vec![first(1, 0, &[5])])]);
    assert!(d.decode(2, 1, 64, 64, &oob).is_err());
}
#[test]
fn context_after_begin_and_region_layout_are_independent() {
    let mut d = decoder();
    let mut f = frame(vec![region((0, 0, 64, 64), vec![first(0, 0, &[5])])]);
    f.insert(
        1,
        ProgressiveBlock::Context(ProgressiveContextPdu {
            context_id: 99,
            tile_size: 64,
            flags: 1,
        }),
    );
    d.decode(1, 1, 64, 64, &f).unwrap();
    d.end_frame(1).unwrap();
    d.begin_frame(2).unwrap();
    let mut up = frame(vec![region((0, 0, 64, 64), vec![upgrade()])]);
    if let ProgressiveBlock::Region(r) = &mut up[1] {
        r.flags = 1;
    }
    assert!(d.decode(1, 1, 64, 64, &up).is_err());
}
#[test]
fn quant_index_ff_and_missing_or_regressive_quant_are_strict() {
    let mut d = decoder();
    let mut bad = first(0, 0, &[5]);
    if let ProgressiveTile::First(t) = &mut bad {
        t.quality = 1;
    }
    assert!(d
        .decode(
            1,
            1,
            64,
            64,
            &frame(vec![region((0, 0, 64, 64), vec![bad])])
        )
        .is_err());
    let mut full = first(0, 0, &[5]);
    if let ProgressiveTile::First(t) = &mut full {
        t.quality = 255;
    }
    d.decode(
        1,
        1,
        64,
        64,
        &frame(vec![region((0, 0, 64, 64), vec![full])]),
    )
    .unwrap();
    d.end_frame(1).unwrap();
    d.begin_frame(2).unwrap();
    let mut regressive = upgrade();
    if let ProgressiveTile::Upgrade(t) = &mut regressive {
        t.quality = 0;
    }
    assert!(d
        .decode(
            1,
            1,
            64,
            64,
            &frame(vec![region((0, 0, 64, 64), vec![regressive])])
        )
        .is_err());
}
#[test]
fn malformed_frame_count_rolls_back_and_open_frame_blocks_egfx_end() {
    let mut d = decoder();
    let mut f = frame(vec![region((0, 0, 64, 64), vec![first(0, 0, &[5])])]);
    if let ProgressiveBlock::FrameBegin(b) = &mut f[0] {
        b.region_count = 2;
    }
    assert!(d.decode(1, 1, 64, 64, &f).is_err());
    assert_eq!(d.tile_count(), 0);
    if let ProgressiveBlock::FrameBegin(b) = &mut f[0] {
        b.region_count = 1;
    }
    let end = f.pop().unwrap();
    assert_eq!(d.decode(1, 1, 64, 64, &f).unwrap().len(), 1);
    assert!(d.end_frame(1).is_err());
    assert!(d.decode(1, 1, 64, 64, &[end]).unwrap().is_empty());
    d.end_frame(1).unwrap();
}
#[test]
fn control_only_payload_and_multiple_codec_frames_keep_egfx_coverage() {
    let mut d = decoder();
    let controls = [
        ProgressiveBlock::Sync(ProgressiveSyncPdu),
        ProgressiveBlock::Context(ProgressiveContextPdu {
            context_id: 0,
            tile_size: 64,
            flags: 0,
        }),
    ];
    assert!(d.decode(1, 1, 64, 64, &controls).unwrap().is_empty());
    let mut frames = frame(vec![region((0, 0, 32, 64), vec![first(0, 0, &[4])])]);
    frames.extend(frame(vec![region((32, 0, 32, 64), vec![])]));
    assert_eq!(d.decode(1, 1, 64, 64, &frames).unwrap().len(), 2);
    d.end_frame(1).unwrap();
}
#[test]
fn distinct_partially_overlapping_rectangles_are_not_inferred_invalid() {
    let mut d = decoder();
    let f = frame(vec![
        region((0, 0, 48, 64), vec![first(0, 0, &[4])]),
        region((16, 0, 48, 64), vec![]),
    ]);
    assert_eq!(d.decode(1, 1, 64, 64, &f).unwrap().len(), 2);
}
#[test]
fn difference_requires_reference_and_matching_layout() {
    let mut d = decoder();
    let mut diff = first(0, 0, &[4]);
    if let ProgressiveTile::First(t) = &mut diff {
        t.flags = 1;
    }
    let f = frame(vec![region((0, 0, 64, 64), vec![diff.clone()])]);
    assert_eq!(
        d.decode(1, 1, 64, 64, &f).unwrap_err(),
        Error::Invalid("difference missing surface reference")
    );
    let orig = frame(vec![region((0, 0, 64, 64), vec![first(0, 0, &[5])])]);
    d.decode(1, 1, 64, 64, &orig).unwrap();
    d.end_frame(1).unwrap();
    d.begin_frame(2).unwrap();

    let context = ProgressiveBlock::Context(ProgressiveContextPdu {
        context_id: 0,
        tile_size: 64,
        flags: 1,
    });
    d.decode(1, 1, 64, 64, &[context]).unwrap();
    let mut wrong = f.clone();
    if let ProgressiveBlock::Region(r) = &mut wrong[1] {
        r.flags = 1;
    }
    assert!(d.decode(1, 1, 64, 64, &wrong).is_err());
    assert_eq!(d.decode(1, 1, 64, 64, &f).unwrap().len(), 1);
}

#[test]
fn surface_reference_survives_context_delete_and_new_context_difference() {
    let mut d = decoder();
    d.decode(
        1,
        1,
        64,
        64,
        &frame(vec![region((0, 0, 64, 64), vec![first(0, 0, &[5])])]),
    )
    .unwrap();
    d.end_frame(1).unwrap();
    d.begin_frame(2).unwrap();
    d.delete_context(1, 1);
    assert_eq!(d.context_count(), 0);
    assert_eq!(d.tile_count(), 1);
    let mut tile = first(0, 0, &[4]);
    if let ProgressiveTile::First(t) = &mut tile {
        t.flags = 1;
    }
    let mut diff = frame(vec![region((0, 0, 64, 64), vec![tile])]);
    diff.insert(
        0,
        ProgressiveBlock::Context(ProgressiveContextPdu {
            context_id: 0,
            tile_size: 64,
            flags: 1,
        }),
    );
    assert_eq!(d.decode(1, 2, 64, 64, &diff).unwrap()[0].bgra[0], 9);
    d.delete_surface(1);
    assert_eq!(d.context_count(), 0);
    assert_eq!(d.tile_count(), 0);
    assert_eq!(
        d.decode(1, 3, 64, 64, &diff).unwrap_err(),
        Error::Invalid("difference missing surface reference")
    );
}
#[test]
fn new_context_shares_surface_reference_and_same_frame_coverage() {
    let mut d = decoder();
    d.decode(
        1,
        1,
        64,
        64,
        &frame(vec![region((0, 0, 32, 64), vec![first(0, 0, &[5])])]),
    )
    .unwrap();
    assert_eq!(
        d.decode(1, 2, 64, 64, &frame(vec![region((32, 0, 32, 64), vec![])]))
            .unwrap()[0]
            .bgra[0],
        5
    );
    d.end_frame(1).unwrap();
    d.begin_frame(2).unwrap();
    let mut tile = first(0, 0, &[4]);
    if let ProgressiveTile::First(t) = &mut tile {
        t.flags = 1;
    }
    let mut diff = frame(vec![region((0, 0, 64, 64), vec![tile])]);
    diff.insert(
        0,
        ProgressiveBlock::Context(ProgressiveContextPdu {
            context_id: 0,
            tile_size: 64,
            flags: 1,
        }),
    );
    assert_eq!(d.decode(1, 3, 64, 64, &diff).unwrap()[0].bgra[0], 9);
}
#[test]
fn legal_progressive_quant_limits_are_not_classic_rfx_limits() {
    let mut d = decoder();
    let mut f = frame(vec![region((0, 0, 64, 64), vec![first(0, 0, &[5])])]);
    if let ProgressiveBlock::Region(r) = &mut f[1] {
        r.quant_vals[0].hl1 = 15;
        r.quant_prog_vals[0].y_quant.hl1 = 8;
    }
    assert!(d.decode(1, 1, 64, 64, &f).is_ok());
    d.end_frame(1).unwrap();
    d.begin_frame(2).unwrap();
    if let ProgressiveBlock::Region(r) = &mut f[1] {
        r.quant_prog_vals[0].y_quant.hl1 = 9;
    }
    assert!(d.decode(1, 1, 64, 64, &f).is_err());
}

#[test]
fn upgrade_rejects_reference_layout_changed_by_another_context() {
    let mut decoder = Decoder::new(Oracle, Limits::default());
    decoder.begin_frame(1).unwrap();
    decoder
        .decode(
            1,
            1,
            64,
            64,
            &frame(vec![region((0, 0, 64, 64), vec![first(0, 0, &[1])])]),
        )
        .unwrap();
    decoder.end_frame(1).unwrap();
    decoder.begin_frame(2).unwrap();
    let mut changed = region((0, 0, 64, 64), vec![first(0, 0, &[2])]);
    if let ProgressiveBlock::Region(r) = &mut changed {
        r.flags = 1;
    }
    decoder.decode(1, 2, 64, 64, &frame(vec![changed])).unwrap();
    decoder.end_frame(2).unwrap();
    decoder.begin_frame(3).unwrap();
    assert!(decoder
        .decode(
            1,
            1,
            64,
            64,
            &frame(vec![region((0, 0, 64, 64), vec![upgrade()])])
        )
        .is_err());
}

#[test]
fn first_tile_reserved_flags_are_ignored_but_difference_and_simple_stay_strict() {
    for high in [2, 4, 8, 16, 32, 64, 128, 254] {
        let mut original = first(0, 0, &[4]);
        if let ProgressiveTile::First(t) = &mut original {
            t.flags = high;
        }
        let mut d = decoder();
        let update = d
            .decode(
                1,
                1,
                64,
                64,
                &frame(vec![region((0, 0, 64, 64), vec![original.clone()])]),
            )
            .unwrap();
        assert_eq!(update[0].bgra[0], 4);
        if let ProgressiveTile::First(t) = &mut original {
            t.flags |= 1;
        }
        let mut absent = decoder();
        assert_eq!(
            absent
                .decode(
                    1,
                    1,
                    64,
                    64,
                    &frame(vec![region((0, 0, 64, 64), vec![original.clone()])])
                )
                .unwrap_err(),
            Error::Invalid("difference missing surface reference")
        );
        d.end_frame(1).unwrap();
        d.begin_frame(2).unwrap();
        let mut diff = frame(vec![region((0, 0, 64, 64), vec![original])]);
        diff.insert(
            0,
            ProgressiveBlock::Context(ProgressiveContextPdu {
                context_id: 0,
                tile_size: 64,
                flags: 1,
            }),
        );
        assert_eq!(d.decode(1, 1, 64, 64, &diff).unwrap()[0].bgra[0], 8);
        let simple = ProgressiveTile::Simple(TileSimple {
            quant_idx_y: 0,
            quant_idx_cb: 0,
            quant_idx_cr: 0,
            x_idx: 0,
            y_idx: 0,
            flags: high,
            y_data: &[4],
            cb_data: &[],
            cr_data: &[],
            tail_data: &[],
        });
        let mut d = decoder();
        assert_eq!(
            d.decode(
                1,
                1,
                64,
                64,
                &frame(vec![region((0, 0, 64, 64), vec![simple])])
            )
            .unwrap_err(),
            Error::Invalid("tile flags")
        );
        assert_eq!(d.tile_count(), 0);
    }
}

#[test]
fn difference_without_context_reuses_only_real_surface_reference() {
    let mut d = decoder();
    d.decode(
        1,
        1,
        64,
        64,
        &frame(vec![region((0, 0, 64, 64), vec![first(0, 0, &[5])])]),
    )
    .unwrap();
    d.end_frame(1).unwrap();
    d.begin_frame(2).unwrap();
    d.delete_context(1, 1);
    let mut tile = first(0, 0, &[4]);
    if let ProgressiveTile::First(t) = &mut tile {
        t.flags = 1;
    }
    let diff = frame(vec![region((0, 0, 64, 64), vec![tile])]);
    assert_eq!(d.decode(1, 2, 64, 64, &diff).unwrap()[0].bgra[0], 9);
    d.end_frame(2).unwrap();
    d.begin_frame(3).unwrap();
    // Context metadata可稍后出现，不改变系数/DAS布局。
    d.decode(
        1,
        2,
        64,
        64,
        &[ProgressiveBlock::Context(ProgressiveContextPdu {
            context_id: 0,
            tile_size: 64,
            flags: 1,
        })],
    )
    .unwrap();
    assert_eq!(
        d.decode(
            1,
            2,
            64,
            64,
            &frame(vec![region((0, 0, 64, 64), vec![upgrade()])])
        )
        .unwrap()[0]
            .bgra[0],
        10
    );
    d.delete_surface(1);
    assert_eq!(
        d.decode(1, 3, 64, 64, &diff).unwrap_err(),
        Error::Invalid("difference missing surface reference")
    );
    d.reset();
    d.begin_frame(4).unwrap();
    assert_eq!(
        d.decode(1, 4, 64, 64, &diff).unwrap_err(),
        Error::Invalid("difference missing surface reference")
    );
}
