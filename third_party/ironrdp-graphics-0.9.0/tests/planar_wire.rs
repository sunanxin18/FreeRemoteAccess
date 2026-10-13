use super::*;
use frd_rdp_legacy_kernels::{KernelError, NativeKernels};

const FROZEN: &[u8] = include_bytes!("fixtures/planar-original-wire.bin");
const BMP: &[u8] = include_bytes!("fixtures/planar-original-bmp.bin");
const SUPPLEMENTAL: &[u8] = include_bytes!("fixtures/planar-original-supplemental.bin");

fn field<'a>(input: &mut &'a [u8]) -> &'a [u8] {
    let n = u32::from_le_bytes(input[..4].try_into().unwrap()) as usize;
    let value = &input[4..4 + n];
    *input = &input[4 + n..];
    value
}

fn bitmap_result(result: Result<(), NativeBitmapError>) -> String {
    match result {
        Ok(()) => "Ok(())".to_owned(),
        Err(NativeBitmapError::Bitmap(error)) => format!("Err({error:?})"),
        Err(NativeBitmapError::Kernel(error)) => panic!("unexpected kernel: {error}"),
    }
}

#[test]
fn original_catalog_and_supplemental_bitmap_wire_append_error_scratch_parity() {
    let kernels = NativeKernels::detect().unwrap();
    let mut cases = 0;
    let mut host_width_classifications = 0;
    for frozen in [FROZEN, SUPPLEMENTAL, BMP] {
        assert_eq!(&frozen[..8], b"FRDPLN02");
        let count = u32::from_le_bytes(frozen[8..12].try_into().unwrap());
        assert_eq!(
            count,
            if frozen.as_ptr() == FROZEN.as_ptr() {
                4972
            } else if frozen.as_ptr() == SUPPLEMENTAL.as_ptr() {
                4181
            } else {
                6
            }
        );
        let mut input = &frozen[12..];
        for _ in 0..count {
            let name = core::str::from_utf8(field(&mut input)).unwrap();
            let dims = field(&mut input);
            let size = |i| u32::from_le_bytes(dims[i..i + 4].try_into().unwrap()) as usize;
            let (width, height, mode) = (size(0), size(4), size(8));
            let wire = field(&mut input);
            let initial = field(&mut input);
            let expected = field(&mut input);
            let result = core::str::from_utf8(field(&mut input)).unwrap();
            let scratch = field(&mut input);
            let consumed = field(&mut input);
            if mode == 1 {
                continue;
            }
            if mode == 2 && usize::BITS != 64 {
                // 这四项标记的是64bit原版panic；32bit仅检查native安全拒绝，不伪称原panic parity。
                assert_eq!(result, "Panic geometry");
                let mut dst = initial.to_vec();
                let mut decoder = BitmapStreamDecoder::default();
                let error = decoder
                    .decode_bitmap_stream_to_rgb24_native(wire, &mut dst, width, height, &kernels)
                    .unwrap_err();
                assert!(matches!(
                    error,
                    NativeBitmapError::Kernel(KernelError::InvalidDimensions)
                ));
                assert_eq!(dst, initial);
                assert!(decoder.planes_buffer.is_empty());
                host_width_classifications += 1;
                continue;
            }
            assert!(consumed.is_empty(), "bitmap API has no measured consumption");
            let mut old = initial.to_vec();
            let mut old_decoder = BitmapStreamDecoder::default();
            let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                old_decoder.decode_bitmap_stream_to_rgb24(wire, &mut old, width, height)
            }));
            let actual = match observed {
                Ok(r) => format!("{r:?}"),
                Err(_) => "Panic geometry".to_owned(),
            };
            assert_eq!(actual, result, "old result {name}");
            assert_eq!(old, expected, "old bytes {name}");
            let mut native = initial.to_vec();
            let mut decoder = BitmapStreamDecoder::default();
            let actual = decoder.decode_bitmap_stream_to_rgb24_native(wire, &mut native, width, height, &kernels);
            if width == 0 || height == 0 || result.starts_with("Panic ") {
                assert!(matches!(
                    actual,
                    Err(NativeBitmapError::Kernel(KernelError::InvalidDimensions))
                ));
                assert_eq!(native, initial);
                continue;
            }
            assert_eq!(bitmap_result(actual), result, "native result {name}");
            assert_eq!(native, expected, "native append {name}");
            // 真正 Cargo 编译的 shipping library 公有入口也逐字节比对冻结输出。
            let mut shipped = initial.to_vec();
            let actual = ironrdp_graphics::rdp6::BitmapStreamDecoder::default().decode_bitmap_stream_to_rgb24_native(
                wire,
                &mut shipped,
                width,
                height,
                &kernels,
            );
            let actual = match actual {
                Ok(()) => "Ok(())".to_owned(),
                Err(ironrdp_graphics::rdp6::NativeBitmapError::Bitmap(error)) => {
                    format!("Err({error:?})")
                }
                Err(error) => panic!("shipping kernel {name}: {error}"),
            };
            assert_eq!(actual, result, "shipping result {name}");
            assert_eq!(shipped, expected, "shipping bytes {name}");
            if frozen.as_ptr() == SUPPLEMENTAL.as_ptr() {
                assert_eq!(decoder.planes_buffer, scratch, "native private scratch {name}");
                assert_eq!(old_decoder.planes_buffer, scratch, "old private scratch {name}");
            }
            cases += 1;
        }
        assert!(input.is_empty());
    }
    assert_eq!(cases, 6906);
    assert_eq!(host_width_classifications, if usize::BITS == 64 { 0 } else { 4 });
}

#[test]
fn checked_geometry_and_error_source_leave_prefix_and_scratch_untouched() {
    use std::error::Error as _;
    let kernels = NativeKernels::detect().unwrap();
    for (width, height) in [
        (0, 1),
        (1, 0),
        (usize::MAX, 2),
        (2, usize::MAX),
        (isize::MAX as usize, 2),
    ] {
        let mut decoder = BitmapStreamDecoder {
            planes_buffer: vec![0x67, 0x92],
        };
        let mut dst = vec![0xa5, 0x39];
        let error = decoder
            .decode_bitmap_stream_to_rgb24_native(&[], &mut dst, width, height, &kernels)
            .unwrap_err();
        assert!(matches!(
            error,
            NativeBitmapError::Kernel(KernelError::InvalidDimensions)
        ));
        assert!(error.source().is_some());
        assert_eq!(dst, [0xa5, 0x39]);
        assert_eq!(decoder.planes_buffer, [0x67, 0x92]);
    }
    let mut dst = vec![0xa5];
    let error = BitmapStreamDecoder::default()
        .decode_bitmap_stream_to_rgb24_native(&[], &mut dst, 1, 1, &kernels)
        .unwrap_err();
    assert!(matches!(error, NativeBitmapError::Bitmap(BitmapDecodeError::Decode(_))));
    assert!(error.source().unwrap().source().is_some());
    assert_eq!(dst, [0xa5]);
}

#[test]
fn original_private_plane_consumption_partial_scratch_and_offset_canaries() {
    use crate::rdp6::rle::{NativePlanes, decompress_8bpp_plane, decompress_8bpp_plane_with};
    let kernels = NativeKernels::detect().unwrap();
    let mut input = &SUPPLEMENTAL[12..];
    let mut cases = 0;
    let mut errors = std::collections::BTreeSet::new();
    while !input.is_empty() {
        let name = core::str::from_utf8(field(&mut input)).unwrap();
        let dims = field(&mut input);
        let size = |i| u32::from_le_bytes(dims[i..i + 4].try_into().unwrap()) as usize;
        let (width, height, mode) = (size(0), size(4), size(8));
        let wire = field(&mut input);
        let initial = field(&mut input);
        let _output = field(&mut input);
        let result = core::str::from_utf8(field(&mut input)).unwrap();
        let expected = field(&mut input);
        let consumed = core::str::from_utf8(field(&mut input)).unwrap();
        if mode != 1 {
            continue;
        }
        let original = if initial.is_empty() {
            vec![0xa7; width * height]
        } else {
            initial.to_vec()
        };
        let mut scalar = original.clone();
        let actual = decompress_8bpp_plane(wire, &mut scalar, width, height);
        assert_eq!(format!("{actual:?}"), result, "old result {name}");
        assert_eq!(scalar, expected, "old scratch {name}");
        for offset in 0..16 {
            let start = 16 + offset;
            let mut allocation = vec![0xe9; start + original.len() + 17];
            allocation[start..start + original.len()].copy_from_slice(&original);
            let mut native = NativePlanes::new(&kernels);
            let actual = decompress_8bpp_plane_with(
                wire,
                &mut allocation[start..start + original.len()],
                width,
                height,
                &mut native,
            )
            .map_err(|error| match error {
                NativeBitmapError::Bitmap(BitmapDecodeError::Rle(error)) => error,
                _ => panic!("unexpected {error:?}"),
            });
            assert_eq!(format!("{actual:?}"), result, "native result {name}");
            assert_eq!(
                &allocation[start..start + original.len()],
                expected,
                "native scratch {name}"
            );
            assert!(
                allocation[..start]
                    .iter()
                    .chain(allocation[start + original.len()..].iter())
                    .all(|&v| v == 0xe9),
                "canary {name}"
            );
            match actual {
                Ok(n) => assert_eq!(n.to_string(), consumed, "consumption {name}"),
                Err(error) => {
                    assert!(consumed.is_empty());
                    errors.insert(format!("{error:?}").split('(').next().unwrap().to_owned());
                }
            }
        }
        cases += 1;
    }
    assert_eq!(cases, 2237);
    for variant in [
        "ReadCompressedData",
        "WriteDecompressedData",
        "InvalidSegmentHeader",
        "SegmentDoNotFitScanline",
    ] {
        assert!(errors.contains(variant), "missing error {variant}");
    }
}

#[test]
fn real_bitmap_parser_dispatches_copy_fill_delta_and_both_color_leaves_for_all_headers() {
    use crate::rdp6::rle::NativePlanes;
    let kernels = NativeKernels::detect().unwrap();
    let mut input = &FROZEN[12..];
    let mut headers = std::collections::BTreeSet::new();
    let mut saw = [false; 5];
    while !input.is_empty() {
        let name = core::str::from_utf8(field(&mut input)).unwrap();
        let dims = field(&mut input);
        let size = |i| u32::from_le_bytes(dims[i..i + 4].try_into().unwrap()) as usize;
        let wire = field(&mut input);
        let initial = field(&mut input);
        let expected = field(&mut input);
        let result = core::str::from_utf8(field(&mut input)).unwrap();
        let _scratch = field(&mut input);
        let _consumed = field(&mut input);
        if result != "Ok(())" {
            continue;
        }
        let bitmap = decode::<BitmapStreamPdu<'_>>(wire).unwrap();
        let decoder = BitmapStreamDecoderImpl::init_checked(bitmap, size(0), size(4)).unwrap();
        let mut native = NativePlanes::new(&kernels);
        let mut aux = Vec::new();
        let mut output = initial.to_vec();
        decoder.decode_native(&mut output, &mut aux, &mut native).unwrap();
        assert_eq!(output, expected, "trace bytes {name}");
        let calls = &native.calls;
        let rle = wire[0] & 16 != 0;
        let argb = wire[0] & 7 == 0;
        if rle {
            assert!(calls.copy > 0 && calls.fill > 0, "RLE leaves {name}: {calls:?}");
            if size(4) > 1 {
                assert!(calls.delta > 0, "delta {name}");
            }
        }
        if argb {
            assert_eq!(calls.rgb, 1);
            assert_eq!(calls.ycocg, 0);
        } else {
            assert_eq!(calls.ycocg, 1);
            assert_eq!(calls.rgb, 0);
        }
        for (seen, value) in saw
            .iter_mut()
            .zip([calls.copy, calls.fill, calls.delta, calls.rgb, calls.ycocg])
        {
            *seen |= value > 0;
        }
        if name.starts_with("headers-") {
            headers.insert(wire[0] & 63);
        }
    }
    assert_eq!(headers.len(), 64);
    assert!(saw.into_iter().all(|v| v));
}

#[test]
fn header_specific_geometry_gate_precedes_scratch_and_leaf_failure_rolls_back_append() {
    let kernels = NativeKernels::detect().unwrap();
    let mut decoder = BitmapStreamDecoder {
        planes_buffer: vec![0x67, 0x92],
    };
    let mut dst = vec![0xa5, 0x39];
    // RGB 输出乘积可表示，带 alpha 的完整 planes 总长超过 isize；PDU先解析但scratch尚未触碰。
    let width = isize::MAX as usize / 3 - 1;
    let error = decoder
        .decode_bitmap_stream_to_rgb24_native(&[0, 0], &mut dst, width, 1, &kernels)
        .unwrap_err();
    assert!(matches!(
        error,
        NativeBitmapError::Kernel(KernelError::InvalidDimensions)
    ));
    assert_eq!(dst, [0xa5, 0x39]);
    assert_eq!(decoder.planes_buffer, [0x67, 0x92]);
    let error = decoder
        .decode_bitmap_stream_to_rgb24_native(&[], &mut dst, width, 1, &kernels)
        .unwrap_err();
    assert!(matches!(error, NativeBitmapError::Bitmap(BitmapDecodeError::Decode(_))));
    assert_eq!(dst, [0xa5, 0x39]);
    assert_eq!(decoder.planes_buffer, [0x67, 0x92]);

    // 私有 controller 注入非法 CLL 仅用于检查真实 leaf error 路由与append回滚；wire parser不接受此值。
    let bitmap = decode::<BitmapStreamPdu<'_>>(&[0x21, 17, 90, 163, 0]).unwrap();
    let mut decoder = BitmapStreamDecoderImpl::init_checked(bitmap, 1, 1).unwrap();
    if let ColorPlaneDefinition::AYCoCg { color_loss_level, .. } = &mut decoder.bitmap.header.color_plane_definition {
        *color_loss_level = 0;
    } else {
        panic!("expected YCoCg");
    }
    let mut native = crate::rdp6::rle::NativePlanes::new(&kernels);
    let mut scratch = vec![0x67, 0x92];
    let error = decoder.decode_native(&mut dst, &mut scratch, &mut native).unwrap_err();
    assert!(matches!(
        error,
        NativeBitmapError::Kernel(KernelError::InvalidColorLossLevel)
    ));
    assert_eq!(native.calls.ycocg, 1);
    assert_eq!(native.calls.rgb, 0);
    assert_eq!(dst, [0xa5, 0x39]);
    assert_eq!(scratch, [0x67, 0x92]);
}
