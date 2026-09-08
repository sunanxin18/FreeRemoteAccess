use super::*;
use ironrdp::pdu::codecs::clearcodec::FLAG_GLYPH_INDEX;

struct Scalar;
impl PixelKernel for Scalar {
    fn fill_bgra(&self, dst: &mut [u8], color: [u8; 4]) {
        for p in dst.chunks_exact_mut(4) {
            p.copy_from_slice(&color);
        }
    }
    fn expand_bgr24(&self, src: &[u8], dst: &mut [u8]) {
        assert_eq!(src.len() / 3 * 4, dst.len());
        for (s, d) in src.chunks_exact(3).zip(dst.chunks_exact_mut(4)) {
            d[..3].copy_from_slice(s);
            d[3] = 255;
        }
    }
    fn copy_bgra(&self, src: &[u8], dst: &mut [u8]) {
        dst.copy_from_slice(src);
    }
    fn scatter_column(&self, src: &[u8], dst: &mut [u8], stride: usize) {
        for (i, p) in src.chunks_exact(4).enumerate() {
            dst[i * stride..i * stride + 4].copy_from_slice(p);
        }
    }
}
fn decoder() -> Decoder<Scalar, UnavailableNsCodec> {
    Decoder::new(Scalar, UnavailableNsCodec, Limits::default())
}
fn stream(seq: u8, flags: u8, glyph: u16, res: &[u8], bands: &[u8], subs: &[u8]) -> Vec<u8> {
    let mut d = vec![flags, seq];
    if flags & FLAG_GLYPH_INDEX != 0 {
        d.extend(glyph.to_le_bytes());
    }
    for n in [res.len(), bands.len(), subs.len()] {
        d.extend((n as u32).to_le_bytes());
    }
    d.extend(res);
    d.extend(bands);
    d.extend(subs);
    d
}
fn solid(seq: u8, n: u8) -> Vec<u8> {
    stream(seq, 0, 0, &[1, 2, 3, n], &[], &[])
}
fn band(x: u16, xe: u16, y: u16, ye: u16, vbars: &[u8]) -> Vec<u8> {
    let mut d = Vec::new();
    for n in [x, xe, y, ye] {
        d.extend(n.to_le_bytes());
    }
    d.extend([10, 20, 30]);
    d.extend(vbars);
    d
}
fn sub(x: u16, y: u16, w: u16, h: u16, id: u8, p: &[u8]) -> Vec<u8> {
    let mut d = Vec::new();
    for n in [x, y, w, h] {
        d.extend(n.to_le_bytes());
    }
    d.extend((p.len() as u32).to_le_bytes());
    d.push(id);
    d.extend(p);
    d
}
fn pixels(bgr: &[[u8; 3]]) -> Vec<u8> {
    bgr.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect()
}

#[test]
fn residual_exact_and_variable_runs() {
    let mut d = decoder();
    assert_eq!(
        d.decode(&solid(0, 6), 3, 2).unwrap().unwrap(),
        [1, 2, 3, 255].repeat(6)
    );
    let data = stream(1, 0, 0, &[9, 8, 7, 255, 44, 1], &[], &[]);
    assert_eq!(
        d.decode(&data, 300, 1).unwrap().unwrap(),
        [9, 8, 7, 255].repeat(300)
    );
    let mut res = vec![1, 2, 3, 255, 255, 255];
    res.extend(70_000u32.to_le_bytes());
    assert!(d.decode(&stream(2, 0, 0, &res, &[], &[]), 350, 200).is_ok());
}
#[test]
fn reject_residual_tail_overrun_and_underrun() {
    for res in [
        &[1, 2, 3, 2][..],
        &[1, 2, 3, 0],
        &[1, 2, 3, 1, 99],
        &[1, 2, 3, 255],
        &[1, 2, 3, 255, 255, 255, 1],
    ] {
        let mut d = decoder();
        assert!(d.decode(&stream(0, 0, 0, res, &[], &[]), 1, 1).is_err());
        assert_eq!(d.next_sequence, 0);
    }
}
#[test]
fn sequences_are_exact_wrap_and_transactional() {
    let mut d = decoder();
    assert!(d.decode(&solid(1, 1), 1, 1).is_err());
    for seq in 0..=255 {
        d.decode(&solid(seq, 1), 1, 1).unwrap();
    }
    d.decode(&solid(0, 1), 1, 1).unwrap();
    assert_eq!(d.next_sequence, 1);
    d.reset();
    assert_eq!(d.next_sequence, 0);
}
#[test]
fn glyph_hit_validates_dimensions_and_area() {
    let mut d = decoder();
    let seed = stream(0, FLAG_GLYPH_INDEX, 42, &[1, 2, 3, 2], &[], &[]);
    let p = d.decode(&seed, 2, 1).unwrap();
    let hit = [3, 1, 42, 0];
    assert!(d.decode(&hit, 1, 2).is_err());
    assert_eq!(p, d.decode(&hit, 2, 1).unwrap());
    assert!(decoder()
        .decode(&stream(0, 1, 4000, &[1, 2, 3, 1], &[], &[]), 1, 1)
        .is_err());
    assert!(decoder()
        .decode(&stream(0, 1, 1, &[], &[], &[]), 1025, 1)
        .is_err());
}
#[test]
fn rejects_invalid_flags_missing_glyph_and_trailing_payload() {
    for data in [
        vec![2, 0],
        vec![3, 0, 0, 0],
        vec![8, 0],
        vec![0, 0],
        vec![3, 0, 0, 0, 99],
    ] {
        assert!(decoder().decode(&data, 1, 1).is_err());
    }
}
#[test]
fn raw_rows_are_placed_and_uncovered_pixels_rejected() {
    let s = sub(1, 0, 1, 2, 0, &[7, 8, 9, 4, 5, 6]);
    let data = stream(0, 0, 0, &[1, 2, 3, 4], &[], &s);
    assert_eq!(
        decoder().decode(&data, 2, 2).unwrap().unwrap(),
        pixels(&[[1, 2, 3], [7, 8, 9], [1, 2, 3], [4, 5, 6]])
    );
    assert!(decoder()
        .decode(&stream(0, 0, 0, &[], &[], &s), 2, 2)
        .is_err());
}
#[test]
fn raw_requires_exact_bytes_and_region_bounds() {
    for s in [
        sub(0, 0, 1, 1, 0, &[1, 2]),
        sub(0, 0, 1, 1, 0, &[1, 2, 3, 4]),
        sub(1, 0, 1, 1, 0, &[1, 2, 3]),
        sub(0, 1, 1, 1, 0, &[1, 2, 3]),
    ] {
        assert!(decoder()
            .decode(&stream(0, 0, 0, &[], &[], &s), 1, 1)
            .is_err());
    }
}
#[test]
fn subcodec_trailing_bytes_never_ignored() {
    let mut s = sub(0, 0, 1, 1, 0, &[1, 2, 3]);
    s.push(9);
    assert!(decoder()
        .decode(&stream(0, 0, 0, &[], &[], &s), 1, 1)
        .is_err());
}
#[test]
fn bands_use_official_little_endian_yon_yoff_and_same_packet_hits() {
    // 01 03: YOn=1, YOff=3；两色short列，上下各一行背景。
    let b = band(0, 2, 0, 3, &[1, 3, 1, 2, 3, 4, 5, 6, 0, 128, 0, 64, 0]);
    let out = decoder()
        .decode(&stream(0, 0, 0, &[], &b, &[]), 3, 4)
        .unwrap()
        .unwrap();
    assert_eq!(
        out,
        pixels(&[
            [10, 20, 30],
            [10, 20, 30],
            [1, 2, 3],
            [1, 2, 3],
            [1, 2, 3],
            [4, 5, 6],
            [4, 5, 6],
            [4, 5, 6],
            [10, 20, 30],
            [10, 20, 30],
            [10, 20, 30],
            [10, 20, 30]
        ])
    );
}
#[test]
fn empty_short_vbar_is_background_and_cacheable() {
    let b = band(0, 1, 0, 1, &[0, 0, 0, 64, 1]);
    let out = decoder()
        .decode(&stream(0, 0, 0, &[], &b, &[]), 2, 2)
        .unwrap()
        .unwrap();
    assert_eq!(out, [10, 20, 30, 255].repeat(4));
}
#[test]
fn rejects_missing_cache_bad_short_bounds_and_tail() {
    for b in [
        band(0, 0, 0, 0, &[0, 128]),
        band(0, 0, 0, 0, &[0, 64, 0]),
        band(0, 0, 0, 0, &[2, 1]),
        band(0, 0, 0, 0, &[0, 2, 1, 2, 3, 1, 2, 3]),
        vec![0],
    ] {
        assert!(decoder()
            .decode(&stream(0, 0, 0, &[], &b, &[]), 1, 1)
            .is_err());
    }
}
#[test]
fn later_layer_failure_rolls_back_both_cache_writes_and_reset() {
    let mut d = decoder();
    let b = band(0, 0, 0, 0, &[0, 1, 1, 2, 3]);
    d.decode(&stream(0, 0, 0, &[], &b, &[]), 1, 1).unwrap();
    assert_eq!(d.full_cursor, 1);
    let bad = stream(
        1,
        FLAG_CACHE_RESET | FLAG_GLYPH_INDEX,
        42,
        &[],
        &band(0, 0, 0, 0, &[0, 1, 7, 8, 9]),
        &[9],
    );
    assert!(d.decode(&bad, 1, 1).is_err());
    assert_eq!(d.full_cursor, 1);
    assert_eq!(d.short_cursor, 1);
    assert!(d.glyphs[42].is_none());
    let hit = band(0, 0, 0, 0, &[0, 128]);
    assert_eq!(
        d.decode(&stream(1, 0, 0, &[], &hit, &[]), 1, 1)
            .unwrap()
            .unwrap(),
        vec![1, 2, 3, 255]
    );
}
#[test]
fn cache_reset_only_rewinds_cursors_preserving_entries() {
    let mut d = decoder();
    d.decode(
        &stream(0, 0, 0, &[], &band(0, 0, 0, 0, &[0, 1, 1, 2, 3]), &[]),
        1,
        1,
    )
    .unwrap();
    assert_eq!(d.decode(&[4, 1], 0, 0).unwrap(), None);
    assert_eq!(d.full_cursor, 0);
    assert_eq!(
        d.decode(
            &stream(2, 0, 0, &[], &band(0, 0, 0, 0, &[0, 128]), &[]),
            1,
            1
        )
        .unwrap()
        .unwrap(),
        vec![1, 2, 3, 255]
    );
}
#[test]
fn full_and_short_cache_cursor_wrap() {
    let mut d = decoder();
    d.full_cursor = 32767;
    d.short_cursor = 16383;
    d.decode(
        &stream(
            0,
            0,
            0,
            &[],
            &band(0, 1, 0, 0, &[0, 1, 1, 2, 3, 0, 1, 7, 8, 9]),
            &[],
        ),
        2,
        1,
    )
    .unwrap();
    assert_eq!(d.full_cursor, 1);
    assert_eq!(d.short_cursor, 1);
    assert_eq!(d.full[0].as_ref().unwrap().as_ref(), &[7, 8, 9, 255][..]);
}
#[test]
fn rlex_run_and_inclusive_palette_suite() {
    let payload = [3, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 2];
    let s = sub(0, 0, 5, 1, 2, &payload);
    assert_eq!(
        decoder()
            .decode(&stream(0, 0, 0, &[], &[], &s), 5, 1)
            .unwrap()
            .unwrap(),
        pixels(&[[1, 2, 3], [1, 2, 3], [1, 2, 3], [4, 5, 6], [7, 8, 9]])
    );
}
#[test]
fn rlex_single_palette_still_consumes_packed_byte() {
    let payload = [1, 1, 2, 3, 0, 7];
    let s = sub(0, 0, 8, 1, 2, &payload);
    assert_eq!(
        decoder()
            .decode(&stream(0, 0, 0, &[], &[], &s), 8, 1)
            .unwrap()
            .unwrap(),
        [1, 2, 3, 255].repeat(8)
    );
}
#[test]
fn rlex_rejects_suite_underflow_palette_bounds_and_partial_fill() {
    for p in [
        vec![2, 1, 2, 3, 4, 5, 6, 4, 0],
        vec![3, 1, 2, 3, 4, 5, 6, 7, 8, 9, 3, 0],
        vec![1, 1, 2, 3, 0, 0],
        vec![1, 1, 2, 3, 1, 2],
        vec![1, 1, 2, 3, 0, 255],
    ] {
        assert!(decoder()
            .decode(&stream(0, 0, 0, &[], &[], &sub(0, 0, 8, 1, 2, &p)), 8, 1)
            .is_err());
    }
}
#[test]
fn nscodec_unavailable_is_failure_without_cache_commit() {
    let mut d = decoder();
    let data = stream(0, FLAG_GLYPH_INDEX, 2, &[], &[], &sub(0, 0, 1, 1, 1, &[0]));
    assert_eq!(d.decode(&data, 1, 1), Err(Error::NsCodecUnavailable));
    assert!(d.glyphs[2].is_none());
    assert_eq!(d.next_sequence, 0);
}
struct FakeNs(bool);
impl NsCodecProvider for FakeNs {
    fn decode_bgr24(&self, _: &[u8], _: u16, _: u16) -> Result<Vec<u8>> {
        Ok(if self.0 { vec![8, 7, 6] } else { vec![] })
    }
}
#[test]
fn nscodec_exact_output_contract_and_alpha() {
    let data = stream(0, 0, 0, &[], &[], &sub(0, 0, 1, 1, 1, &[0]));
    assert!(Decoder::new(Scalar, FakeNs(false), Limits::default())
        .decode(&data, 1, 1)
        .is_err());
    assert_eq!(
        Decoder::new(Scalar, FakeNs(true), Limits::default())
            .decode(&data, 1, 1)
            .unwrap()
            .unwrap(),
        vec![8, 7, 6, 255]
    );
}
#[test]
fn resource_limits_reject_before_state_commit() {
    let mut d = Decoder::new(
        Scalar,
        UnavailableNsCodec,
        Limits {
            max_pixels: 10,
            max_wire_bytes: 100,
            max_pixel_work: 1,
            ..Limits::default()
        },
    );
    assert!(d.decode(&solid(0, 2), 2, 1).is_err());
    assert!(d.decode(&solid(0, 11), 11, 1).is_err());
    assert_eq!(d.next_sequence, 0);
}
#[test]
fn truncation_at_every_boundary_is_error_without_cache_changes() {
    let s = stream(
        0,
        FLAG_GLYPH_INDEX,
        1,
        &[],
        &band(0, 0, 0, 1, &[0, 2, 1, 2, 3, 4, 5, 6]),
        &[],
    );
    for n in 0..s.len() {
        let mut d = decoder();
        assert!(d.decode(&s[..n], 1, 2).is_err(), "{n}");
        assert_eq!(d.next_sequence, 0);
        assert_eq!(d.full_cursor, 0);
        assert!(d.glyphs[1].is_none());
    }
}

#[test]
fn partial_residual_prefix_can_be_completed_by_subcodec() {
    let data = stream(0, 0, 0, &[1, 2, 3, 3], &[], &sub(1, 1, 1, 1, 0, &[7, 8, 9]));
    assert_eq!(
        decoder().decode(&data, 2, 2).unwrap().unwrap(),
        pixels(&[[1, 2, 3], [1, 2, 3], [1, 2, 3], [7, 8, 9]])
    );
}
#[test]
fn zero_area_subcodecs_are_rejected_even_after_full_residual() {
    for (w, h) in [(0, 1), (1, 0), (0, 65535)] {
        let data = stream(0, 0, 0, &[1, 2, 3, 1], &[], &sub(0, 0, w, h, 0, &[]));
        assert!(decoder().decode(&data, 1, 1).is_err());
    }
}

#[test]
fn coverage_metadata_has_independent_budget() {
    let mut d = Decoder::new(
        Scalar,
        UnavailableNsCodec,
        Limits {
            max_coverage_intervals: 1,
            ..Limits::default()
        },
    );
    let data = stream(0, 0, 0, &[], &band(0, 0, 0, 1, &[0, 0]), &[]);
    assert_eq!(d.decode(&data, 1, 2), Err(Error::ResourceLimit));
    assert_eq!(d.full_cursor, 0);
    assert_eq!(d.next_sequence, 0);
}
