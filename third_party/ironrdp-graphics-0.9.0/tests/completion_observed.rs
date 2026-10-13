use crate::packed::{RleDecodeProgress, RleError, RlePixelFormat, decompress, decompress_observed};
use crate::rdp6::bitmap_stream::BitmapStreamDecoder;

fn field<'a>(input: &mut &'a [u8]) -> &'a [u8] {
    let n = u32::from_le_bytes(input[..4].try_into().unwrap()) as usize;
    let value = &input[4..4 + n];
    *input = &input[4 + n..];
    value
}

#[test]
fn completion_packed_real_cursors_distinguish_underfill_from_resized_bytes() {
    assert_eq!(
        core::mem::size_of::<RleDecodeProgress>(),
        2 * core::mem::size_of::<usize>()
    );
    for (bpp, depth, format) in [
        (8, 1, RlePixelFormat::Rgb8),
        (15, 2, RlePixelFormat::Rgb15),
        (16, 2, RlePixelFormat::Rgb16),
        (24, 3, RlePixelFormat::Rgb24),
    ] {
        for initial in [vec![], vec![0x39; 6], vec![0; 6]] {
            for (wire, written, consumed) in [(&[][..], 0, 0), (&[0xfd][..], depth, 1)] {
                let mut dst = initial.clone();
                let result = decompress_observed(wire, &mut dst, 2, 1, bpp).unwrap();
                assert_eq!(result, (format, RleDecodeProgress { written, consumed }));
                let mut old = initial.clone();
                assert_eq!(decompress(wire, &mut old, 2, 1, bpp), Ok(format));
                assert_eq!(dst, old);
                assert_eq!(dst.len(), 2 * depth);
            }
        }
        // 全零输出同样是实际解码，不能从像素值猜测完成性。
        let mut dst = vec![0x39; 2 * depth];
        assert_eq!(
            decompress_observed(&[0x02], &mut dst, 2, 1, bpp).unwrap(),
            (
                format,
                RleDecodeProgress {
                    written: 2 * depth,
                    consumed: 1
                }
            )
        );
        assert_eq!(dst, vec![0; 2 * depth]);
        let wire: Vec<_> = core::iter::once(0x84).chain((0..4 * depth).map(|n| n as u8)).collect();
        let mut dst = vec![0x39; 20];
        assert_eq!(
            decompress_observed(&wire, &mut dst, 2, 2, bpp).unwrap(),
            (
                format,
                RleDecodeProgress {
                    written: 4 * depth,
                    consumed: wire.len()
                }
            )
        );
        assert_eq!(dst, wire[1..]);
        let mut shipped = vec![0x39; 20];
        let (shipped_format, progress) =
            ironrdp_graphics::rle::decompress_observed(&wire, &mut shipped, 2, 2, bpp).unwrap();
        assert_eq!(format!("{shipped_format:?}"), format!("{format:?}"));
        assert_eq!((progress.written, progress.consumed), (4 * depth, wire.len()));
        assert_eq!(shipped, dst);
    }
}

#[test]
fn completion_packed_priority_and_private_partial_prefix_are_unchanged() {
    let mut dst = vec![0x39, 0x67];
    assert_eq!(
        decompress_observed(&[], &mut dst, 0, 0, 7),
        Err(RleError::InvalidBpp { bpp: 7 })
    );
    assert_eq!(dst, [0x39, 0x67]);
    assert_eq!(
        decompress_observed(&[0xff], &mut dst, 0, 1, 8),
        Err(RleError::EmptyImage)
    );
    assert_eq!(dst, [0x39, 0x67]);
    // 第一 SPECIAL_WHITE 已写入，第二个 raw 同时源/目标不足；原源错误优先。
    assert_eq!(
        decompress_observed(&[0xfd, 0x82], &mut dst, 1, 1, 8),
        Err(RleError::NotEnoughBytes { expected: 2, actual: 0 })
    );
    assert_eq!(dst, [0xff]);
}

#[test]
fn completion_packed_frozen_original_records_preserve_old_and_observed_bytes_errors() {
    let mut cases = 0;
    for frozen in [
        include_bytes!("fixtures/bitmap-original-wire.bin").as_slice(),
        include_bytes!("fixtures/bitmap-original-malformed.bin").as_slice(),
    ] {
        assert_eq!(&frozen[..8], b"FRDBMP01");
        let count = u32::from_le_bytes(frozen[8..12].try_into().unwrap());
        let mut input = &frozen[12..];
        for _ in 0..count {
            let name = core::str::from_utf8(field(&mut input)).unwrap();
            let dims = field(&mut input);
            let size = |i| u32::from_le_bytes(dims[i..i + 4].try_into().unwrap()) as usize;
            let (w, h, bpp) = (size(0), size(4), size(8));
            let wire = field(&mut input);
            let initial = field(&mut input);
            let expected = field(&mut input);
            let result = core::str::from_utf8(field(&mut input)).unwrap();
            let mut old = initial.to_vec();
            assert_eq!(
                format!("{:?}", decompress(wire, &mut old, w, h, bpp)),
                result,
                "old {name}"
            );
            assert_eq!(old, expected, "old bytes {name}");
            let mut observed = initial.to_vec();
            let actual = decompress_observed(wire, &mut observed, w, h, bpp);
            if let Ok((_, progress)) = &actual {
                assert_eq!(progress.consumed, wire.len(), "cursor {name}");
                assert!(progress.written <= w * h * bpp.div_ceil(8), "written {name}");
            }
            assert_eq!(
                format!("{:?}", actual.map(|(format, _)| format)),
                result,
                "observed {name}"
            );
            assert_eq!(observed, expected, "observed bytes {name}");
            cases += 1;
        }
        assert!(input.is_empty());
    }
    assert_eq!(cases, 16721 + 7616);
}

#[test]
fn completion_planar_source_shape_append_tail_and_truncated_planes() {
    // 原版 header: raw ARGB 无 alpha、RLE ARGB 无 alpha、raw AYCoCg+CS。
    for (wire, w, h) in [
        (vec![0x20, 0x11, 0x22, 0x33, 0], 1, 1),
        (vec![0x30, 0x10, 0x11, 0x10, 0x22, 0x10, 0x33], 1, 1),
        (vec![0x00, 0x91, 0x11, 0x22, 0x33, 0], 1, 1),
        (vec![0x29, 30, 40, 50, 60, 70, 80, 0, 0, 0, 0, 0], 3, 2),
    ] {
        for tail in [vec![], vec![0x77, 0x88]] {
            let full: Vec<_> = wire.iter().chain(&tail).copied().collect();
            let mut dst = vec![0xa5, 0x39];
            let mut old = dst.clone();
            let mut decoder = BitmapStreamDecoder::default();
            assert_eq!(
                decoder
                    .decode_bitmap_stream_to_rgb24_observed(&full, &mut dst, w, h)
                    .unwrap(),
                Some((w, h))
            );
            BitmapStreamDecoder::default()
                .decode_bitmap_stream_to_rgb24(&full, &mut old, w, h)
                .unwrap();
            assert_eq!(dst, old);
            assert_eq!(&dst[..2], &[0xa5, 0x39]);
            assert_eq!(dst.len(), 2 + w * h * 3);
            let mut shipped = vec![0xa5, 0x39];
            assert_eq!(
                ironrdp_graphics::rdp6::BitmapStreamDecoder::default()
                    .decode_bitmap_stream_to_rgb24_observed(&full, &mut shipped, w, h)
                    .unwrap(),
                Some((w, h))
            );
            assert_eq!(dst, shipped);
        }
        for end in 0..wire.len() {
            let mut dst = vec![0xa5, 0x39];
            let mut old = dst.clone();
            let mut decoder = BitmapStreamDecoder::default();
            let actual = decoder
                .decode_bitmap_stream_to_rgb24_observed(&wire[..end], &mut dst, w, h)
                .map(|_| ());
            let expected = BitmapStreamDecoder::default().decode_bitmap_stream_to_rgb24(&wire[..end], &mut old, w, h);
            assert!(actual.is_err());
            assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
            assert_eq!(dst, old);
        }
    }
    // 成功零尺寸不提供形状证明；旧 decoder 的历史行为仍保留。
    for (w, h) in [(0, 1), (1, 0), (0, 0)] {
        let mut dst = vec![0xa5];
        assert_eq!(
            BitmapStreamDecoder::default()
                .decode_bitmap_stream_to_rgb24_observed(&[0x20, 0], &mut dst, w, h)
                .unwrap(),
            None
        );
        assert_eq!(dst, [0xa5]);
    }
}

#[test]
fn completion_planar_frozen_original_shape_pixels_append_errors_and_private_scratch() {
    let mut cases = 0;
    let mut panic_classifications = 0;
    for frozen in [
        include_bytes!("fixtures/planar-original-wire.bin").as_slice(),
        include_bytes!("fixtures/planar-original-supplemental.bin").as_slice(),
        include_bytes!("fixtures/planar-original-bmp.bin").as_slice(),
    ] {
        assert_eq!(&frozen[..8], b"FRDPLN02");
        let mut input = &frozen[12..];
        while !input.is_empty() {
            let name = core::str::from_utf8(field(&mut input)).unwrap();
            let dims = field(&mut input);
            let size = |i| u32::from_le_bytes(dims[i..i + 4].try_into().unwrap()) as usize;
            let (w, h, mode) = (size(0), size(4), size(8));
            let wire = field(&mut input);
            let initial = field(&mut input);
            let expected = field(&mut input);
            let result = core::str::from_utf8(field(&mut input)).unwrap();
            let scratch = field(&mut input);
            let consumed = field(&mut input);
            if mode == 1 {
                continue;
            }
            assert!(consumed.is_empty());
            if mode == 2 && usize::BITS != 64 {
                continue;
            }
            let (dst, actual, private) = crate::rdp6::bitmap_stream::decoder::observed_snapshot(wire, initial, w, h);
            assert_eq!(dst, expected, "bytes {name}");
            assert_eq!(actual.as_str(), result, "result {name}");
            if result.starts_with("Panic ") {
                panic_classifications += 1;
            }
            if frozen.len() == include_bytes!("fixtures/planar-original-supplemental.bin").len() {
                assert_eq!(private, scratch, "private scratch {name}");
            }
            cases += 1;
        }
    }
    assert_eq!(cases, 6906 + 12 + if usize::BITS == 64 { 4 } else { 0 });
    assert_eq!(panic_classifications, if usize::BITS == 64 { 4 } else { 0 });
}
