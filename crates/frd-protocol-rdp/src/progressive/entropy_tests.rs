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

#[test]
fn freerdp_c_encoder_nonzero_complete_component() {
    let mut output = [0; 4096];
    decode_rlgr1(
        include_bytes!("fixtures/freerdp_rlgr1_synthetic.bin"),
        &mut output,
    )
    .unwrap();
    for (i, value) in output.iter().enumerate() {
        let expected = if i < 4000 {
            (i % 17) as i16 - 8
        } else if i == 4095 {
            1
        } else {
            0
        };
        assert_eq!(*value, expected, "coefficient {i}");
    }
}

#[test]
fn terminal_full_zero_run_consumes_one_complete_symbol_then_exact_padding() {
    // 从4096这个公开系数总数生成run，与任何会话字节无关。
    let mut bits = Vec::new();
    let mut zeros = 4096;
    let mut kp = 8;
    while zeros >= 1 << (kp / 8) {
        bits.push(0);
        zeros -= 1 << (kp / 8);
        kp = (kp + 4).min(80);
    }
    bits.push(1);
    for bit in (0..kp / 8).rev() {
        bits.push(u8::from(zeros & (1 << bit) != 0));
    }
    assert_eq!(bits.len(), 31);
    let short = pack_bits(&bits);
    let mut output = [17; 4096];
    decode_rlgr1(&short, &mut output).unwrap();
    assert_eq!(output, [0; 4096]);
    for negative in [0, 1] {
        let mut complete = bits.clone();
        complete.extend([negative, 0, 0]); // sign + GR(0), kr=1
        let encoded = pack_bits(&complete);
        decode_rlgr1(&encoded, &mut output).unwrap();
        assert_eq!(output, [0; 4096]);
        let mut extra = encoded.clone();
        extra.push(0);
        assert_eq!(decode_rlgr1(&extra, &mut output), Err(Error::TrailingData));
        let mut bad_padding = encoded.clone();
        *bad_padding.last_mut().unwrap() |= 1;
        assert_eq!(
            decode_rlgr1(&bad_padding, &mut output),
            Err(Error::TrailingData)
        );
    }
    // 终端符号虽不写入输出，仍须满足i16范围；不能绕过数值校验。
    for (negative, code, valid) in [(0, 32767u32, false), (1, 32767, true), (1, 32768, false)] {
        let mut terminal = bits.clone();
        terminal.push(negative);
        terminal.extend(std::iter::repeat_n(1, (code >> 1) as usize));
        terminal.extend([0, (code & 1) as u8]);
        output.fill(17);
        let result = decode_rlgr1(&pack_bits(&terminal), &mut output);
        if valid {
            assert_eq!(result, Ok(()));
            assert_eq!(output, [0; 4096]);
        } else {
            assert_eq!(result, Err(Error::CoefficientRange));
            assert_eq!(output, [17; 4096]);
        }
    }
    // 非零sign已到达最后bit，缺失GR，不能把缺失数据补零。
    let mut truncated = bits.clone();
    truncated.push(1);
    output.fill(17);
    assert_eq!(
        decode_rlgr1(&pack_bits(&truncated), &mut output),
        Err(Error::Truncated)
    );
    assert_eq!(output, [17; 4096]);
}

#[test]
fn terminal_escape_exact_coverage_stops_before_byte_padding() {
    // 初始k=1：两条escape各表达2零，已覆盖4项；余下6位是零对齐。
    let mut out = [17; 4];
    assert_eq!(decode_rlgr1(&[0], &mut out), Ok(()));
    assert_eq!(out, [0; 4]);
    // 同样2+2零后显式terminator=1、k=2 remainder=0仍走完整语法。
    assert_eq!(decode_rlgr1(&pack_bits(&[0, 0, 1, 0, 0]), &mut out), Ok(()));
    // 一个已编码的2零escape并不足以覆盖4096，EOF不得补输出。
    assert!(decode_rlgr1(&[0], &mut [17; 4096]).is_err());
    // 三项无法用两个2零escape精确覆盖，仍拒绝超出，不隐式裁剪。
    assert_eq!(decode_rlgr1(&[0], &mut [17; 3]), Err(Error::RunOverrun));
    // 不接受额外完整零字节或非零尾部，失败不修改输出。
    for data in [&[0, 0][..], &[1][..]] {
        out.fill(17);
        assert!(decode_rlgr1(data, &mut out).is_err());
        assert_eq!(out, [17; 4]);
    }
}
