//! Apple MVS bounded bitstream primitives.

use anyhow::{bail, Context, Result};

/// 有界、MSB-first 的 Apple MVS 位读取器。
pub struct BitReader<'a> {
    bytes: &'a [u8],
    bit_position: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_position: 0,
        }
    }

    pub fn read_bits(&mut self, count: u8) -> Result<u32> {
        if count > u32::BITS as u8 {
            bail!(
                "MVS 位读取长度超过 u32: {count}（bit offset {}）",
                self.bit_position
            );
        }
        if count == 0 {
            return Ok(0);
        }

        let count = usize::from(count);
        let total_bits = self
            .bytes
            .len()
            .checked_mul(u8::BITS as usize)
            .context("MVS 位流总长度溢出")?;
        let end = self
            .bit_position
            .checked_add(count)
            .context("MVS 位游标溢出")?;
        if end > total_bits {
            bail!(
                "MVS 位流耗尽: bit offset {} 请求 {count} 位，仅余 {} 位",
                self.bit_position,
                total_bits.saturating_sub(self.bit_position)
            );
        }

        let mut value = 0u32;
        for absolute_bit in self.bit_position..end {
            let byte = *self
                .bytes
                .get(absolute_bit / u8::BITS as usize)
                .context("MVS 位流边界检查与字节索引不一致")?;
            let shift = u32::try_from(7 - (absolute_bit % u8::BITS as usize))?;
            let bit = u32::from(byte)
                .checked_shr(shift)
                .context("MVS 位移超出字节宽度")?
                & 1;
            value = value.checked_shl(1).context("MVS 位读取累积移位溢出")? | bit;
        }
        self.bit_position = end;
        Ok(value)
    }

    pub fn read_u8(&mut self) -> Result<u8> {
        u8::try_from(self.read_bits(u8::BITS as u8)?).context("MVS 8 位读取结果超出 u8")
    }

    pub fn bit_position(&self) -> usize {
        self.bit_position
    }

    pub fn remaining_bits(&self) -> usize {
        self.bytes
            .len()
            .checked_mul(u8::BITS as usize)
            .and_then(|total| total.checked_sub(self.bit_position))
            .unwrap_or(0)
    }
}

/// `_GetRepeatCount_at_1c894dc84` 的逐分支转译。
pub fn decode_repeat_count(reader: &mut BitReader<'_>) -> Result<usize> {
    if reader.read_bits(1)? == 0 {
        return Ok(0);
    }

    let direct = reader.read_bits(4)?;
    if direct != 0x0f {
        return usize::try_from(direct)?
            .checked_add(1)
            .context("MVS repeat 直接计数溢出");
    }

    let low_group = reader.read_u8()?;
    if low_group & 0x80 == 0 {
        return usize::from(low_group)
            .checked_add(0x10)
            .context("MVS repeat 13 位分支溢出");
    }

    let middle_group = reader.read_u8()?;
    if middle_group & 0x80 == 0 {
        let middle = usize::from(middle_group & 0x7f)
            .checked_shl(7)
            .context("MVS repeat 21 位中组移位溢出")?;
        return middle
            .checked_add(usize::from(low_group & 0x7f))
            .and_then(|value| value.checked_add(0x10))
            .context("MVS repeat 21 位分支溢出");
    }

    let high_group = usize::from(reader.read_u8()?)
        .checked_shl(14)
        .context("MVS repeat 29 位高组移位溢出")?;
    let middle = usize::from(middle_group & 0x7f)
        .checked_shl(7)
        .context("MVS repeat 29 位中组移位溢出")?;
    high_group
        .checked_add(middle)
        .and_then(|value| value.checked_add(usize::from(low_group & 0x7f)))
        .and_then(|value| value.checked_add(0x10))
        .context("MVS repeat 29 位分支溢出")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_msb_first_across_byte_boundary() {
        let mut bits = BitReader::new(&[0b1011_0010, 0b0110_0001]);
        assert_eq!(bits.read_bits(3).unwrap(), 0b101);
        assert_eq!(bits.read_bits(6).unwrap(), 0b100100);
        assert_eq!(bits.read_bits(7).unwrap(), 0b1100001);
    }

    #[test]
    fn exhaustion_is_an_error_without_advancing() {
        let mut bits = BitReader::new(&[0x80]);
        assert!(bits.read_bits(9).is_err());
        assert_eq!(bits.bit_position(), 0);
        assert_eq!(bits.read_bits(1).unwrap(), 1);
    }

    #[test]
    fn rejects_bit_counts_above_word_width_without_advancing() {
        let mut bits = BitReader::new(&[0xff; 8]);
        assert!(bits.read_bits(33).is_err());
        assert_eq!(bits.bit_position(), 0);
        assert_eq!(bits.read_u8().unwrap(), 0xff);
    }

    #[test]
    fn zero_bits_do_not_move_the_cursor() {
        let mut bits = BitReader::new(&[0x80]);
        assert_eq!(bits.read_bits(0).unwrap(), 0);
        assert_eq!(bits.bit_position(), 0);
        assert_eq!(bits.remaining_bits(), 8);
        assert_eq!(bits.read_bits(1).unwrap(), 1);
    }

    #[test]
    fn consumes_the_exact_last_bit() {
        let mut bits = BitReader::new(&[0x01]);
        assert_eq!(bits.read_bits(7).unwrap(), 0);
        assert_eq!(bits.read_bits(1).unwrap(), 1);
        assert_eq!(bits.bit_position(), 8);
        assert_eq!(bits.remaining_bits(), 0);
        assert!(bits.read_bits(1).is_err());
        assert_eq!(bits.bit_position(), 8);
    }

    #[test]
    fn read_u8_crosses_a_non_byte_aligned_cursor() {
        let mut bits = BitReader::new(&[0b1110_1010, 0b0101_1100]);
        assert_eq!(bits.read_bits(3).unwrap(), 0b111);
        assert_eq!(bits.read_u8().unwrap(), 0x52);
        assert_eq!(bits.bit_position(), 11);
        assert_eq!(bits.remaining_bits(), 5);
    }

    #[test]
    fn repeat_count_matches_each_recovered_prefix_branch_and_maximum() {
        struct Case {
            name: &'static str,
            bytes: &'static [u8],
            expected: usize,
            consumed_bits: usize,
        }

        let cases = [
            Case {
                name: "0",
                bytes: &[0x00],
                expected: 0,
                consumed_bits: 1,
            },
            Case {
                name: "1_0000",
                bytes: &[0x80],
                expected: 1,
                consumed_bits: 5,
            },
            Case {
                name: "1_1110",
                bytes: &[0xf0],
                expected: 15,
                consumed_bits: 5,
            },
            Case {
                name: "1_1111_00000000",
                bytes: &[0xf8, 0x00],
                expected: 16,
                consumed_bits: 13,
            },
            Case {
                name: "1_1111_01111111",
                bytes: &[0xfb, 0xf8],
                expected: 143,
                consumed_bits: 13,
            },
            Case {
                name: "1_1111_10000000_00000000",
                bytes: &[0xfc, 0x00, 0x00],
                expected: 16,
                consumed_bits: 21,
            },
            Case {
                name: "1_1111_11111111_01111111",
                bytes: &[0xff, 0xfb, 0xf8],
                expected: 16_399,
                consumed_bits: 21,
            },
            Case {
                name: "1_1111_10000000_10000000_00000000",
                bytes: &[0xfc, 0x04, 0x00, 0x00],
                expected: 16,
                consumed_bits: 29,
            },
            Case {
                name: "1_1111_11111111_11111111_11111111",
                bytes: &[0xff, 0xff, 0xff, 0xf8],
                expected: 4_194_319,
                consumed_bits: 29,
            },
        ];

        for case in cases {
            let mut bits = BitReader::new(case.bytes);
            assert_eq!(
                decode_repeat_count(&mut bits).unwrap(),
                case.expected,
                "{}",
                case.name
            );
            assert_eq!(bits.bit_position(), case.consumed_bits, "{}", case.name);
        }
    }

    #[test]
    fn repeat_count_preserves_asymmetric_21_bit_group_weights() {
        let mut bits = BitReader::new(&[0xfc, 0x08, 0x10]);
        assert_eq!(decode_repeat_count(&mut bits).unwrap(), 273);
        assert_eq!(bits.bit_position(), 21);
    }

    #[test]
    fn repeat_count_preserves_asymmetric_29_bit_group_weights() {
        let mut bits = BitReader::new(&[0xfc, 0x0c, 0x10, 0x18]);
        assert_eq!(decode_repeat_count(&mut bits).unwrap(), 49_425);
        assert_eq!(bits.bit_position(), 29);
    }

    #[test]
    fn repeat_count_rejects_each_truncated_prefix_without_overread() {
        struct Case {
            name: &'static str,
            bytes: &'static [u8],
            skip_bits: u8,
            stopped_at: usize,
            remaining_bits: usize,
        }

        let cases = [
            Case {
                name: "zero_of_one_bits",
                bytes: &[],
                skip_bits: 0,
                stopped_at: 0,
                remaining_bits: 0,
            },
            Case {
                name: "four_of_five_bits",
                bytes: &[0x0f],
                skip_bits: 4,
                stopped_at: 5,
                remaining_bits: 3,
            },
            Case {
                name: "twelve_of_thirteen_bits",
                bytes: &[0x0f, 0x80],
                skip_bits: 4,
                stopped_at: 9,
                remaining_bits: 7,
            },
            Case {
                name: "twenty_of_twenty_one_bits",
                bytes: &[0x0f, 0xc0, 0x00],
                skip_bits: 4,
                stopped_at: 17,
                remaining_bits: 7,
            },
            Case {
                name: "twenty_eight_of_twenty_nine_bits",
                bytes: &[0x0f, 0xc0, 0x40, 0x00],
                skip_bits: 4,
                stopped_at: 25,
                remaining_bits: 7,
            },
        ];

        for case in cases {
            let mut bits = BitReader::new(case.bytes);
            bits.read_bits(case.skip_bits).unwrap();
            assert!(decode_repeat_count(&mut bits).is_err(), "{}", case.name);
            assert_eq!(bits.bit_position(), case.stopped_at, "{}", case.name);
            assert_eq!(bits.remaining_bits(), case.remaining_bits, "{}", case.name);
        }
    }
}
