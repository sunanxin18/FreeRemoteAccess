use super::*;

#[test]
fn strict_bits_cross_bytes_and_atomic_truncation() {
    let mut r = BitReader::new(&[0b10110110, 0b01010000]);
    assert_eq!(r.read(3), Ok(5));
    assert_eq!(r.read(7), Ok(89));
    assert_eq!(r.read(2), Ok(1));
    let at = r.position();
    assert_eq!(r.read(5), Err(Error::Truncated));
    assert_eq!(r.position(), at);
    r.finish_zero_padding().unwrap();
    assert_eq!(
        BitReader::new(&[0]).finish_zero_padding(),
        Err(Error::TrailingData)
    );
}

#[test]
fn rlgr1_hand_encoded_runs_and_signed_values() {
    for (data, expected) in [
        (vec![0x80], vec![1]),
        (vec![0xa0], vec![-1]),
        (vec![0xc0], vec![0, 1]),
        (vec![0x40], vec![0, 0]),
        (vec![0x86], vec![1, 1]),
        (vec![0x83, 0], vec![1, 0, 1]),
    ] {
        let mut out = vec![77; expected.len()];
        decode_rlgr1(&data, &mut out).unwrap();
        assert_eq!(out, expected);
        for n in 0..data.len() {
            let mut out = vec![77; expected.len()];
            assert!(decode_rlgr1(&data[..n], &mut out).is_err());
            assert!(out.iter().all(|v| *v == 77));
        }
        let mut trailing = data.clone();
        trailing.push(0);
        assert!(decode_rlgr1(&trailing, &mut out).is_err());
    }
    assert_eq!(decode_rlgr1(&[0x40], &mut [0]), Err(Error::RunOverrun));
    assert!(decode_rlgr1(&[0x81], &mut [0]).is_err());
}

#[test]
fn srl_unary_sign_and_state_continue_across_bands() {
    // 初始KP8，escape1、K位零数1：先出一个0；下band直接读取正号0。
    let mut r = UpgradeReader::new(&[0xc0, 0], &[]);
    assert_eq!(
        r.read_band(&[0], 1, false).unwrap(),
        vec![Refinement::zero()]
    );
    assert_eq!(r.positions(), (2, 0));
    assert_eq!(
        r.read_band(&[0], 1, false).unwrap(),
        vec![Refinement {
            magnitude: 1,
            negative: false
        }]
    );
    assert_eq!(r.positions(), (3, 0));
    r.finish().unwrap();
    // +1随后-1，KP沿用而非每band复位。
    let mut r = UpgradeReader::new(&[0x98, 0], &[]);
    assert_eq!(
        r.read_band(&[0], 1, false).unwrap()[0],
        Refinement {
            magnitude: 1,
            negative: false
        }
    );
    assert_eq!(
        r.read_band(&[0], 1, false).unwrap()[0],
        Refinement {
            magnitude: 1,
            negative: true
        }
    );
    r.finish().unwrap();
}

#[test]
fn srl_max_magnitude_omits_unary_terminator() {
    let mut r = UpgradeReader::new(&[0x80, 0, 0], &[]);
    assert_eq!(
        r.read_band(&[0], 3, false).unwrap()[0],
        Refinement {
            magnitude: 7,
            negative: false
        }
    );
    assert_eq!(r.positions(), (9, 0));
    r.finish().unwrap();
    let mut short = UpgradeReader::new(&[0x80], &[]);
    assert_eq!(short.read_band(&[0], 3, false), Err(Error::Truncated));
    assert_eq!(short.positions(), (0, 0));
}

#[test]
fn raw_sign_ll_and_zero_bit_bands_have_exact_consumption() {
    let mut r = UpgradeReader::new(&[], &[0xa8]);
    assert_eq!(
        r.read_band(&[-1], 3, false).unwrap()[0],
        Refinement {
            magnitude: 5,
            negative: true
        }
    );
    assert_eq!(
        r.read_band(&[-1], 3, true).unwrap()[0],
        Refinement {
            magnitude: 2,
            negative: false
        }
    );
    assert_eq!(
        r.read_band(&[0; 3], 0, false).unwrap(),
        vec![Refinement::zero(); 3]
    );
    assert_eq!(r.positions(), (0, 6));
    r.finish().unwrap();
    let mut r = UpgradeReader::new(&[], &[0xab]);
    r.read_band(&[1, 1], 3, false).unwrap();
    assert_eq!(r.finish(), Err(Error::TrailingData));
}

#[test]
fn upgrade_rejects_overrun_trailing_and_invalid_contracts() {
    let mut r = UpgradeReader::new(&[0], &[]);
    r.read_band(&[0], 1, false).unwrap();
    assert_eq!(r.finish(), Err(Error::RunOverrun));
    assert_eq!(
        UpgradeReader::new(&[0, 0], &[]).finish(),
        Err(Error::TrailingData)
    );
    assert_eq!(
        UpgradeReader::new(&[1], &[]).finish(),
        Err(Error::TrailingData)
    );
    let mut r = UpgradeReader::new(&[], &[]);
    assert_eq!(r.read_band(&[2], 0, false), Err(Error::InvalidSign));
    assert_eq!(r.read_band(&[0], 23, false), Err(Error::InvalidBits));
    assert_eq!(
        r.read_band(&[0; 4097], 0, false),
        Err(Error::TooManyCoefficients)
    );
    assert_eq!(r.read_band(&[1], 1, false), Err(Error::Truncated));
    assert_eq!(r.positions(), (0, 0));
}

fn pack_bits(bits: &[u8]) -> Vec<u8> {
    let mut out = vec![0; bits.len().div_ceil(8)];
    for (i, &v) in bits.iter().enumerate() {
        out[i / 8] |= v << (7 - i % 8);
    }
    out
}

#[test]
fn high_bit_width_raw_preserves_zero_and_checks_signed_range() {
    for width in 16..=22 {
        for (value, sign, ok) in [
            (0, 1, true),
            (32767, 1, true),
            (32768, -1, true),
            (32768, 1, false),
            (32769, -1, false),
            (65536, 1, false),
        ] {
            if value >= 1u32 << width {
                continue;
            }
            let bits: Vec<u8> = (0..width).rev().map(|i| ((value >> i) & 1) as u8).collect();
            let data = pack_bits(&bits);
            let mut r = UpgradeReader::new(&[], &data);
            let result = r.read_band(&[sign], width, false);
            if ok {
                assert_eq!(
                    result.unwrap()[0],
                    Refinement {
                        magnitude: value as u16,
                        negative: sign < 0
                    }
                );
                r.finish().unwrap();
            } else {
                assert_eq!(result, Err(Error::CoefficientRange));
                assert_eq!(r.positions(), (0, 0));
            }
        }
    }
    let mut r = UpgradeReader::new(&[0], &[]);
    assert_eq!(
        r.read_band(&[0, 0], 22, false).unwrap(),
        vec![Refinement::zero(); 2]
    );
    r.finish().unwrap();
}

#[test]
fn high_bit_width_srl_is_bounded_and_accepts_negative_minimum() {
    for (negative, magnitude, ok) in [
        (false, 1, true),
        (true, 32768, true),
        (false, 32768, false),
        (true, 32769, false),
    ] {
        // 初始escape1/K零数0/sign，然后magnitude-1零和终止1。
        let mut bits = vec![1, 0, u8::from(negative)];
        bits.resize(bits.len() + magnitude - 1, 0);
        bits.push(1);
        let data = pack_bits(&bits);
        let mut r = UpgradeReader::new(&data, &[]);
        let result = r.read_band(&[0], 22, false);
        if ok {
            assert_eq!(
                result.unwrap()[0],
                Refinement {
                    magnitude: magnitude as u16,
                    negative
                }
            );
            r.finish().unwrap();
        } else {
            assert_eq!(result, Err(Error::CoefficientRange));
            assert_eq!(r.positions(), (0, 0));
        }
    }
}
