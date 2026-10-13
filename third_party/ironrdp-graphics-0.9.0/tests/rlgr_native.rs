use super::*;
use frd_rdp_legacy_kernels::NativeKernels;
fn field<'a>(input: &mut &'a [u8]) -> &'a [u8] {
    let n = u32::from_le_bytes(input[..4].try_into().unwrap()) as usize;
    let v = &input[4..4 + n];
    *input = &input[4 + n..];
    v
}
fn coeff(v: &[u8]) -> Vec<i16> {
    v.chunks_exact(2)
        .map(|v| i16::from_le_bytes(v.try_into().unwrap()))
        .collect()
}
#[test]
fn original_rlgr_catalog_and_supplemental_all_outputs_and_errors() {
    let k = NativeKernels::detect().unwrap();
    let mut total = 0;
    for frozen in [
        include_bytes!("../../ironrdp-session-0.11.0/tests/fixtures/rfx-original-catalog.bin").as_slice(),
        include_bytes!("../../ironrdp-session-0.11.0/tests/fixtures/rfx-original-supplemental.bin").as_slice(),
    ] {
        assert_eq!(&frozen[..8], b"FRDRFX01");
        let mut input = &frozen[12..];
        while !input.is_empty() {
            let name = core::str::from_utf8(field(&mut input)).unwrap();
            let meta = field(&mut input);
            let wire = field(&mut input);
            let initial = field(&mut input);
            let expected = field(&mut input);
            let result = core::str::from_utf8(field(&mut input)).unwrap();
            if ![1, 3].contains(&meta[0]) {
                continue;
            }
            total += 1;
            let mode = if meta[0] == 1 {
                EntropyAlgorithm::Rlgr1
            } else {
                EntropyAlgorithm::Rlgr3
            };
            let mut old = coeff(initial);
            assert_eq!(format!("{:?}", decode(mode, wire, &mut old)), result, "old {name}");
            assert_eq!(old, coeff(expected), "old {name}");
            let mut out = coeff(initial);
            assert_eq!(
                format!(
                    "{:?}",
                    crate::legacy_native::decode_rlgr_native(mode, wire, &mut out, &k)
                ),
                result,
                "native {name}"
            );
            assert_eq!(out, coeff(expected), "native {name}");
        }
    }
    assert!(total > 634);
}
#[test]
fn parser_run_and_terminal_padding_invoke_real_zero_calls() {
    let k = NativeKernels::detect().unwrap();
    for mode in [EntropyAlgorithm::Rlgr1, EntropyAlgorithm::Rlgr3] {
        for run in [0, 1, 7, 8, 63, 64, 4096] {
            let mut values = vec![0; run + 2];
            values[run] = 17;
            values[run + 1] = -13;
            let mut wire = vec![0; 200000];
            let n = encode(mode, &values, &mut wire).unwrap();
            let mut out = vec![99; run + 9];
            let mut materializer = NativeZero::new(&k);
            decode_with(mode, &wire[..n], &mut out, &mut materializer).unwrap();
            assert!(
                materializer
                    .calls
                    .iter()
                    .any(|&(site, len)| site == ZeroSite::Run && len == run),
                "run={run} trace={:?}",
                materializer.calls
            );
            assert!(
                materializer
                    .calls
                    .iter()
                    .any(|&(site, len)| site == ZeroSite::Padding && len > 0)
            );
            assert_eq!(&out[..run + 2], values);
        }
        let mut out = [99; 64];
        let mut materializer = NativeZero::new(&k);
        decode_with(mode, &[0], &mut out, &mut materializer).unwrap();
        assert_eq!(materializer.calls, vec![(ZeroSite::Padding, 64)]);
        assert_eq!(out, [0; 64]);
        let mut out = [];
        assert!(matches!(
            crate::legacy_native::decode_rlgr_native(mode, &[], &mut out, &k),
            Err(RlgrError::EmptyTile)
        ));
    }
}
#[test]
fn checked_components_quant_order_ll3_carry_and_atomic_length_precheck() {
    use crate::legacy_native::{KernelError, NativeRfxError, decode_component_native};
    use ironrdp_pdu::codecs::rfx::Quant;
    let k = NativeKernels::detect().unwrap();
    let mut count = 0;
    for extra in [
        include_bytes!("../../ironrdp-session-0.11.0/tests/fixtures/rfx-original-supplemental.bin").as_slice(),
        include_bytes!("../../ironrdp-session-0.11.0/tests/fixtures/rfx-original-quant-order.bin").as_slice(),
    ] {
        let mut input = &extra[12..];
        while !input.is_empty() {
            let name = core::str::from_utf8(field(&mut input)).unwrap();
            let meta = field(&mut input);
            let wire = field(&mut input);
            let initial = field(&mut input);
            let expected = field(&mut input);
            let result = field(&mut input);
            if ![11, 13].contains(&meta[0]) {
                continue;
            }
            count += 1;
            let mode = if meta[0] == 11 {
                EntropyAlgorithm::Rlgr1
            } else {
                EntropyAlgorithm::Rlgr3
            };
            let q = &meta[1..];
            let quant = Quant {
                hl1: q[0],
                lh1: q[1],
                hh1: q[2],
                hl2: q[3],
                lh2: q[4],
                hh2: q[5],
                hl3: q[6],
                lh3: q[7],
                hh3: q[8],
                ll3: q[9],
            };
            let mut output = coeff(initial);
            let mut temp = vec![71; 4096];
            crate::legacy_native::COMPONENT_CALLS.with(|v| v.borrow_mut().clear());
            let r = decode_component_native(&quant, mode, wire, &mut output, &mut temp, &k);
            if result == b"Panic arithmetic" {
                assert!(
                    matches!(r, Err(NativeRfxError::Kernel(KernelError::InvalidQuant))),
                    "{name}"
                );
            } else {
                r.unwrap();
                assert_eq!(output, coeff(expected), "quant/LL3/DWT {name}");
                crate::legacy_native::COMPONENT_CALLS.with(|v| {
                    assert_eq!(
                        *v.borrow(),
                        vec![
                            ("rlgr", 4096),
                            ("differential", 64),
                            ("dequantize", 4096),
                            ("inverse_dwt", 4096)
                        ]
                    )
                });
            }
        }
    }
    assert_eq!(count, 30);
    for len in [0, 1, 4095, 4097] {
        for scratch in [false, true] {
            let mut output = vec![91; if scratch { 4096 } else { len }];
            let mut temp = vec![37; if scratch { len } else { 4096 }];
            let r = decode_component_native(
                &Quant::default(),
                EntropyAlgorithm::Rlgr3,
                &[],
                &mut output,
                &mut temp,
                &k,
            );
            assert!(matches!(r, Err(NativeRfxError::Kernel(KernelError::Length))));
            assert!(output.iter().all(|&v| v == 91));
            assert!(temp.iter().all(|&v| v == 37));
        }
    }
}
