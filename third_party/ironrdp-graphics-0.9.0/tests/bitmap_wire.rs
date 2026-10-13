use super::{decompress, decompress_native};
use frd_rdp_legacy_kernels::NativeKernels;

// 仅提取 A2.3 的原版 catalog；期望值不来自生产副本。
const FROZEN: &[u8] = include_bytes!("fixtures/bitmap-original-wire.bin");

fn field<'a>(input: &mut &'a [u8]) -> &'a [u8] {
    let n = u32::from_le_bytes(input[..4].try_into().unwrap()) as usize;
    let value = &input[4..4 + n];
    *input = &input[4 + n..];
    value
}

#[test]
fn frozen_original_wire_old_and_native_match_all_commands_depths_and_errors() {
    assert_eq!(&FROZEN[..8], b"FRDBMP01");
    let count = u32::from_le_bytes(FROZEN[8..12].try_into().unwrap());
    assert_eq!(count, 16721);
    let kernels = NativeKernels::detect().unwrap();
    let mut input = &FROZEN[12..];
    for _ in 0..count {
        let name = core::str::from_utf8(field(&mut input)).unwrap();
        let dims = field(&mut input);
        let size = |i| u32::from_le_bytes(dims[i..i + 4].try_into().unwrap()) as usize;
        let (width, height, bpp) = (size(0), size(4), size(8));
        let wire = field(&mut input);
        let initial = field(&mut input);
        let expected = field(&mut input);
        let result = core::str::from_utf8(field(&mut input)).unwrap();
        let mut old = initial.to_vec();
        let old_result = decompress(wire, &mut old, width, height, bpp);
        assert_eq!(format!("{old_result:?}"), result, "old result {name}");
        assert_eq!(old, expected, "old bytes {name}");
        let mut native = initial.to_vec();
        let native_result = decompress_native(wire, &mut native, width, height, bpp, &kernels)
            .map_err(|error| match error {
                super::NativeRleError::Rle(error) => error,
                super::NativeRleError::Kernel(error) => panic!("kernel {name}: {error}"),
            });
        assert_eq!(format!("{native_result:?}"), result, "native result {name}");
        assert_eq!(native, expected, "native bytes {name}");
    }
    assert!(input.is_empty());
}

fn trace<Mode: super::DepthMode>(
    wire: &[u8],
    initial: &[u8],
    width: usize,
    height: usize,
    kernels: &NativeKernels,
) -> (Result<(), super::NativeRleError>, Vec<u8>, super::LeafCalls) {
    let mut dst = initial.to_vec();
    dst.resize(width * height * Mode::COLOR_DEPTH, 0);
    let mut native = super::NativeMaterializer::new(kernels);
    let result =
        super::decompress_with::<Mode, _>(wire, &mut dst, width * Mode::COLOR_DEPTH, &mut native);
    (result, dst, native.calls)
}

#[test]
fn every_command_depth_dispatches_actual_native_leaves_during_parsing() {
    let kernels = NativeKernels::detect().unwrap();
    let mut input = &FROZEN[12..];
    let mut families = std::collections::BTreeSet::new();
    let mut variants = 0;
    while !input.is_empty() {
        let name = core::str::from_utf8(field(&mut input)).unwrap();
        let dims = field(&mut input);
        let size = |i| u32::from_le_bytes(dims[i..i + 4].try_into().unwrap()) as usize;
        let (width, height, bpp) = (size(0), size(4), size(8));
        let wire = field(&mut input);
        let initial = field(&mut input);
        let expected = field(&mut input);
        let expected_result = core::str::from_utf8(field(&mut input)).unwrap();
        if !name.starts_with("code-") {
            continue;
        }
        let (result, bytes, calls) = match bpp {
            8 => trace::<super::Mode8Bpp>(wire, initial, width, height, &kernels),
            15 => trace::<super::Mode15Bpp>(wire, initial, width, height, &kernels),
            16 => trace::<super::Mode16Bpp>(wire, initial, width, height, &kernels),
            24 => trace::<super::Mode24Bpp>(wire, initial, width, height, &kernels),
            _ => unreachable!(),
        };
        assert!(result.is_ok(), "{name}: {result:?}");
        assert!(expected_result.starts_with("Ok("), "{name}");
        assert_eq!(bytes, expected, "{name}");
        let code = u8::from_str_radix(&name[5..7], 16).unwrap();
        let next = name.contains("next-true");
        match code {
            0x00 | 0xf0 if next => assert!(calls.copy >= 2, "BG copy {name}: {calls:?}"),
            0x00 | 0xf0 => assert!(calls.fill > 0, "BG fill {name}: {calls:?}"),
            0x20 | 0xc0 | 0xf1 | 0xf6 if next => assert!(calls.xor > 0, "FG XOR {name}: {calls:?}"),
            0x20 | 0xc0 | 0xf1 | 0xf6 => assert!(calls.fill > 0, "FG fill {name}: {calls:?}"),
            0x40 | 0xd0 | 0xf2 | 0xf7 | 0xf9 | 0xfa => {
                assert!(calls.mask > 0, "mask {name}: {calls:?}")
            }
            0xe0 | 0xf8 => assert!(calls.dither > 0, "dither {name}: {calls:?}"),
            0x60 | 0xf3 | 0xfd | 0xfe => assert!(calls.fill > 0, "color {name}: {calls:?}"),
            0x80 | 0xf4 => assert!(calls.copy > 0, "raw {name}: {calls:?}"),
            _ => panic!("unclassified {name}"),
        }
        families.insert((code, bpp));
        variants += 1;
    }
    assert_eq!(families.len(), 20 * 4);
    assert_eq!(variants, 224);
}

#[test]
fn native_checked_dimensions_error_before_destination_resize_and_bpp_has_priority() {
    use super::{NativeRleError, RleError};
    use frd_rdp_legacy_kernels::KernelError;
    let kernels = NativeKernels::detect().unwrap();
    for bpp in [8, 15, 16, 24] {
        for (width, height) in [(usize::MAX, 2), (2, usize::MAX), (isize::MAX as usize, 2)] {
            let mut dst = vec![0xa5, 0x39];
            assert_eq!(
                decompress_native(&[0xff], &mut dst, width, height, bpp, &kernels),
                Err(NativeRleError::Kernel(KernelError::InvalidDimensions))
            );
            assert_eq!(dst, [0xa5, 0x39]);
        }
        let mut dst = vec![0xa5];
        assert_eq!(
            decompress_native(&[0xff], &mut dst, 0, usize::MAX, bpp, &kernels),
            Err(NativeRleError::Rle(RleError::EmptyImage))
        );
        assert_eq!(dst, [0xa5]);
    }
    let mut dst = vec![0x39];
    assert_eq!(
        decompress_native(&[0xff], &mut dst, usize::MAX, usize::MAX, 7, &kernels),
        Err(NativeRleError::Rle(RleError::InvalidBpp { bpp: 7 }))
    );
    assert_eq!(dst, [0x39]);
}

const MALFORMED: &[u8] = include_bytes!("fixtures/bitmap-original-malformed.bin");

fn guarded<Mode: super::DepthMode>(
    wire: &[u8],
    initial: &[u8],
    geometry: (usize, usize),
    expected: &[u8],
    expected_result: &str,
    name: &str,
    kernels: &NativeKernels,
) {
    let (width, height) = geometry;
    let len = width * height * Mode::COLOR_DEPTH;
    let mut resized = initial.to_vec();
    resized.resize(len, 0);
    for offset in 0..16 {
        let start = 16 + offset;
        let mut allocation = vec![0xa7; start + len + 17];
        allocation[start..start + len].copy_from_slice(&resized);
        let result = super::decompress_with::<Mode, _>(
            wire,
            &mut allocation[start..start + len],
            width * Mode::COLOR_DEPTH,
            &mut super::NativeMaterializer::new(kernels),
        );
        let normalized = result
            .map(|()| Mode::PIXEL_FORMAT)
            .map_err(|error| match error {
                super::NativeRleError::Rle(error) => error,
                super::NativeRleError::Kernel(error) => panic!("kernel {name}: {error}"),
            });
        assert_eq!(
            format!("{normalized:?}"),
            expected_result,
            "canary result {name} offset={offset}"
        );
        assert_eq!(
            &allocation[start..start + len],
            expected,
            "canary bytes {name} offset={offset}"
        );
        assert!(
            allocation[..start]
                .iter()
                .chain(&allocation[start + len..])
                .all(|&v| v == 0xa7),
            "canary damaged {name} offset={offset}"
        );
    }
}

#[test]
fn independent_original_truncations_preserve_error_priority_partial_and_canaries() {
    assert_eq!(&MALFORMED[..8], b"FRDBMP01");
    let count = u32::from_le_bytes(MALFORMED[8..12].try_into().unwrap());
    assert_eq!(count, 7616);
    let kernels = NativeKernels::detect().unwrap();
    let mut input = &MALFORMED[12..];
    let mut errors = std::collections::BTreeSet::new();
    for _ in 0..count {
        let name = core::str::from_utf8(field(&mut input)).unwrap();
        let dims = field(&mut input);
        let size = |i| u32::from_le_bytes(dims[i..i + 4].try_into().unwrap()) as usize;
        let (width, height, bpp) = (size(0), size(4), size(8));
        let wire = field(&mut input);
        let initial = field(&mut input);
        let expected = field(&mut input);
        let expected_result = core::str::from_utf8(field(&mut input)).unwrap();
        let mut old = initial.to_vec();
        let result = decompress(wire, &mut old, width, height, bpp);
        assert_eq!(format!("{result:?}"), expected_result, "old {name}");
        assert_eq!(old, expected, "old partial {name}");
        let mut native = initial.to_vec();
        let result =
            decompress_native(wire, &mut native, width, height, bpp, &kernels).map_err(|error| {
                match error {
                    super::NativeRleError::Rle(error) => error,
                    super::NativeRleError::Kernel(error) => panic!("kernel {name}: {error}"),
                }
            });
        assert_eq!(format!("{result:?}"), expected_result, "native {name}");
        assert_eq!(native, expected, "native partial {name}");
        if let Err(error) = result {
            errors.insert(
                format!("{error:?}")
                    .split([' ', '{'])
                    .next()
                    .unwrap()
                    .to_owned(),
            );
        }
        match bpp {
            8 => guarded::<super::Mode8Bpp>(
                wire,
                initial,
                (width, height),
                expected,
                expected_result,
                name,
                &kernels,
            ),
            15 => guarded::<super::Mode15Bpp>(
                wire,
                initial,
                (width, height),
                expected,
                expected_result,
                name,
                &kernels,
            ),
            16 => guarded::<super::Mode16Bpp>(
                wire,
                initial,
                (width, height),
                expected,
                expected_result,
                name,
                &kernels,
            ),
            24 => guarded::<super::Mode24Bpp>(
                wire,
                initial,
                (width, height),
                expected,
                expected_result,
                name,
                &kernels,
            ),
            _ => unreachable!(),
        }
    }
    assert!(input.is_empty());
    assert_eq!(
        errors,
        [
            "BadOrderCode",
            "InvalidImageSize",
            "NotEnoughBytes",
            "UnexpectedZeroLength"
        ]
        .map(str::to_owned)
        .into_iter()
        .collect()
    );
}
