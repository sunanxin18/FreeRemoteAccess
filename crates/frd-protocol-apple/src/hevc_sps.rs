//! HEVC SPS 中用于选择解码后端的严格能力门禁。

use std::error::Error;
use std::fmt;

/// SPS 很小；拒绝异常大的 NAL，避免在去除 emulation-prevention 时无界分配。
pub const MAX_HEVC_SPS_NAL_BYTES: usize = 64 * 1024;

/// H.265 Annex A Table A.2 中 Main 4:4:4 行要求置位的 RExt constraint flags。
///
/// 在 48 位 `general_constraint_indicator_flags` 中依次对应
/// `max_12bit`、`max_10bit`、`max_8bit` 和 `lower_bit_rate`。
const MAIN444_8_REXT_CONSTRAINT_MASK: u64 =
    (1u64 << 43) | (1u64 << 42) | (1u64 << 41) | (1u64 << 35);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HevcSps {
    pub general_profile_space: u8,
    pub general_tier_flag: bool,
    pub general_profile_idc: u8,
    pub general_profile_compatibility_flags: u32,
    /// HEVC `general_constraint_indicator_flags` 的低 48 位。
    pub general_constraint_indicator_flags: u64,
    pub general_level_idc: u8,
    pub chroma_format_idc: u8,
    pub separate_colour_plane_flag: bool,
    pub coded_width: u32,
    pub coded_height: u32,
    /// 按 chroma subsampling 单位换算后的 luma sample 裁剪量。
    pub crop_left: u32,
    pub crop_right: u32,
    pub crop_top: u32,
    pub crop_bottom: u32,
    pub visible_width: u32,
    pub visible_height: u32,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
}

impl HevcSps {
    /// 当前 Windows HP 解码路径所需的 Main 4:4:4、8-bit 能力门禁。
    pub fn is_main444_8bit(&self) -> bool {
        let profile_is_main444 = self.general_profile_idc == 4
            || self.general_profile_compatibility_flags & (1 << (31 - 4)) != 0;
        let has_main444_8_constraints = self.general_constraint_indicator_flags
            & MAIN444_8_REXT_CONSTRAINT_MASK
            == MAIN444_8_REXT_CONSTRAINT_MASK;
        self.general_profile_space == 0
            && profile_is_main444
            && has_main444_8_constraints
            && self.chroma_format_idc == 3
            && !self.separate_colour_plane_flag
            && self.bit_depth_luma == 8
            && self.bit_depth_chroma == 8
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HevcSpsError {
    NalBudgetExceeded { limit: usize },
    Truncated,
    InvalidNalHeader,
    InvalidNalType(u8),
    InvalidEmulationPrevention,
    InvalidReservedBits,
    ExpGolombOverflow,
    InvalidChromaFormat(u32),
    InvalidDimensions,
    InvalidConformanceWindow,
    InvalidBitDepth(u32),
}

impl fmt::Display for HevcSpsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NalBudgetExceeded { limit } => {
                write!(formatter, "HEVC SPS NAL 超过资源上限 {limit} 字节")
            }
            Self::Truncated => formatter.write_str("HEVC SPS 被截断"),
            Self::InvalidNalHeader => formatter.write_str("HEVC SPS NAL header 非法"),
            Self::InvalidNalType(nal_type) => {
                write!(formatter, "预期 HEVC SPS NAL type 33，收到 {nal_type}")
            }
            Self::InvalidEmulationPrevention => {
                formatter.write_str("HEVC SPS emulation-prevention 序列非法")
            }
            Self::InvalidReservedBits => formatter.write_str("HEVC SPS 保留位非零"),
            Self::ExpGolombOverflow => formatter.write_str("HEVC SPS Exp-Golomb 值溢出"),
            Self::InvalidChromaFormat(value) => {
                write!(formatter, "HEVC SPS chroma_format_idc 非法：{value}")
            }
            Self::InvalidDimensions => formatter.write_str("HEVC SPS coded 尺寸非法"),
            Self::InvalidConformanceWindow => {
                formatter.write_str("HEVC SPS conformance window 非法")
            }
            Self::InvalidBitDepth(value) => {
                write!(formatter, "HEVC SPS bit_depth_minus8 非法：{value}")
            }
        }
    }
}

impl Error for HevcSpsError {}

/// 解析一个完整的 HEVC SPS NAL（包含 2 字节 NAL header）。
///
/// 只读取解码后端选择所需的 SPS 头字段；其余语法不在此能力门禁中推断。
pub fn parse_hevc_sps(nal: &[u8]) -> Result<HevcSps, HevcSpsError> {
    if nal.len() > MAX_HEVC_SPS_NAL_BYTES {
        return Err(HevcSpsError::NalBudgetExceeded {
            limit: MAX_HEVC_SPS_NAL_BYTES,
        });
    }
    if nal.len() < 3 {
        return Err(HevcSpsError::Truncated);
    }
    if nal[0] & 0x80 != 0 || nal[1] & 0x07 == 0 {
        return Err(HevcSpsError::InvalidNalHeader);
    }
    let nal_type = (nal[0] >> 1) & 0x3f;
    if nal_type != 33 {
        return Err(HevcSpsError::InvalidNalType(nal_type));
    }

    let rbsp = remove_emulation_prevention(&nal[2..])?;
    let mut bits = BitReader::new(&rbsp);

    bits.read_bits(4)?; // sps_video_parameter_set_id
    let max_sub_layers_minus1 = bits.read_bits(3)? as u8;
    if max_sub_layers_minus1 > 6 {
        return Err(HevcSpsError::InvalidReservedBits);
    }
    bits.read_bit()?; // sps_temporal_id_nesting_flag

    let general_profile_space = bits.read_bits(2)? as u8;
    let general_tier_flag = bits.read_bit()?;
    let general_profile_idc = bits.read_bits(5)? as u8;
    let general_profile_compatibility_flags = bits.read_bits(32)? as u32;
    let general_constraint_indicator_flags = bits.read_bits(48)?;
    let general_level_idc = bits.read_bits(8)? as u8;

    let mut sub_layer_profile_present = [false; 7];
    let mut sub_layer_level_present = [false; 7];
    for index in 0..usize::from(max_sub_layers_minus1) {
        sub_layer_profile_present[index] = bits.read_bit()?;
        sub_layer_level_present[index] = bits.read_bit()?;
    }
    if max_sub_layers_minus1 > 0 {
        for _ in max_sub_layers_minus1..8 {
            if bits.read_bits(2)? != 0 {
                return Err(HevcSpsError::InvalidReservedBits);
            }
        }
    }
    for index in 0..usize::from(max_sub_layers_minus1) {
        if sub_layer_profile_present[index] {
            bits.skip_bits(88)?;
        }
        if sub_layer_level_present[index] {
            bits.skip_bits(8)?;
        }
    }

    bits.read_ue()?; // sps_seq_parameter_set_id
    let chroma_format = bits.read_ue()?;
    if chroma_format > 3 {
        return Err(HevcSpsError::InvalidChromaFormat(chroma_format));
    }
    let chroma_format_idc = chroma_format as u8;
    let separate_colour_plane_flag = chroma_format_idc == 3 && bits.read_bit()?;
    let coded_width = bits.read_ue()?;
    let coded_height = bits.read_ue()?;
    if coded_width == 0 || coded_height == 0 {
        return Err(HevcSpsError::InvalidDimensions);
    }

    let (left_offset, right_offset, top_offset, bottom_offset) = if bits.read_bit()? {
        (
            bits.read_ue()?,
            bits.read_ue()?,
            bits.read_ue()?,
            bits.read_ue()?,
        )
    } else {
        (0, 0, 0, 0)
    };
    let (sub_width, sub_height) = crop_units(chroma_format_idc, separate_colour_plane_flag);
    let crop_left = left_offset
        .checked_mul(sub_width)
        .ok_or(HevcSpsError::InvalidConformanceWindow)?;
    let crop_right = right_offset
        .checked_mul(sub_width)
        .ok_or(HevcSpsError::InvalidConformanceWindow)?;
    let crop_top = top_offset
        .checked_mul(sub_height)
        .ok_or(HevcSpsError::InvalidConformanceWindow)?;
    let crop_bottom = bottom_offset
        .checked_mul(sub_height)
        .ok_or(HevcSpsError::InvalidConformanceWindow)?;
    let visible_width = coded_width
        .checked_sub(
            crop_left
                .checked_add(crop_right)
                .ok_or(HevcSpsError::InvalidConformanceWindow)?,
        )
        .filter(|value| *value != 0)
        .ok_or(HevcSpsError::InvalidConformanceWindow)?;
    let visible_height = coded_height
        .checked_sub(
            crop_top
                .checked_add(crop_bottom)
                .ok_or(HevcSpsError::InvalidConformanceWindow)?,
        )
        .filter(|value| *value != 0)
        .ok_or(HevcSpsError::InvalidConformanceWindow)?;

    let bit_depth_luma_minus8 = bits.read_ue()?;
    let bit_depth_chroma_minus8 = bits.read_ue()?;
    if bit_depth_luma_minus8 > 8 {
        return Err(HevcSpsError::InvalidBitDepth(bit_depth_luma_minus8));
    }
    if bit_depth_chroma_minus8 > 8 {
        return Err(HevcSpsError::InvalidBitDepth(bit_depth_chroma_minus8));
    }

    Ok(HevcSps {
        general_profile_space,
        general_tier_flag,
        general_profile_idc,
        general_profile_compatibility_flags,
        general_constraint_indicator_flags,
        general_level_idc,
        chroma_format_idc,
        separate_colour_plane_flag,
        coded_width,
        coded_height,
        crop_left,
        crop_right,
        crop_top,
        crop_bottom,
        visible_width,
        visible_height,
        bit_depth_luma: 8 + bit_depth_luma_minus8 as u8,
        bit_depth_chroma: 8 + bit_depth_chroma_minus8 as u8,
    })
}

fn crop_units(chroma_format_idc: u8, separate_colour_plane_flag: bool) -> (u32, u32) {
    if separate_colour_plane_flag {
        return (1, 1);
    }
    match chroma_format_idc {
        0 => (1, 1),
        1 => (2, 2),
        2 => (2, 1),
        3 => (1, 1),
        _ => unreachable!("chroma_format_idc 已验证"),
    }
}

fn remove_emulation_prevention(ebsp: &[u8]) -> Result<Vec<u8>, HevcSpsError> {
    let mut rbsp = Vec::with_capacity(ebsp.len());
    let mut zero_count = 0usize;
    let mut index = 0usize;
    while index < ebsp.len() {
        let byte = ebsp[index];
        if zero_count >= 2 && byte == 0x03 {
            let next = *ebsp
                .get(index + 1)
                .ok_or(HevcSpsError::InvalidEmulationPrevention)?;
            if next > 0x03 {
                return Err(HevcSpsError::InvalidEmulationPrevention);
            }
            zero_count = 0;
            index += 1;
            continue;
        }
        if zero_count >= 2 && byte <= 0x02 {
            return Err(HevcSpsError::InvalidEmulationPrevention);
        }
        rbsp.push(byte);
        zero_count = if byte == 0 { zero_count + 1 } else { 0 };
        index += 1;
    }
    Ok(rbsp)
}

struct BitReader<'a> {
    bytes: &'a [u8],
    bit_offset: usize,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_offset: 0,
        }
    }

    fn read_bit(&mut self) -> Result<bool, HevcSpsError> {
        Ok(self.read_bits(1)? != 0)
    }

    fn read_bits(&mut self, width: usize) -> Result<u64, HevcSpsError> {
        if width > 64 {
            return Err(HevcSpsError::ExpGolombOverflow);
        }
        let end = self
            .bit_offset
            .checked_add(width)
            .filter(|end| *end <= self.bytes.len().saturating_mul(8))
            .ok_or(HevcSpsError::Truncated)?;
        let mut value = 0u64;
        while self.bit_offset < end {
            let byte = self.bytes[self.bit_offset / 8];
            let shift = 7 - (self.bit_offset % 8);
            value = (value << 1) | u64::from((byte >> shift) & 1);
            self.bit_offset += 1;
        }
        Ok(value)
    }

    fn skip_bits(&mut self, width: usize) -> Result<(), HevcSpsError> {
        self.bit_offset = self
            .bit_offset
            .checked_add(width)
            .filter(|end| *end <= self.bytes.len().saturating_mul(8))
            .ok_or(HevcSpsError::Truncated)?;
        Ok(())
    }

    fn read_ue(&mut self) -> Result<u32, HevcSpsError> {
        let mut leading_zero_bits = 0usize;
        while !self.read_bit()? {
            leading_zero_bits += 1;
            if leading_zero_bits > 31 {
                return Err(HevcSpsError::ExpGolombOverflow);
            }
        }
        if leading_zero_bits == 0 {
            return Ok(0);
        }
        let suffix = self.read_bits(leading_zero_bits)? as u32;
        Ok(((1u32 << leading_zero_bits) - 1) + suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_hevc_sps, HevcSps, HevcSpsError, MAX_HEVC_SPS_NAL_BYTES};

    const CAPTURED_MAIN444_8BIT_SPS: &[u8] = &[
        0x42, 0x01, 0x01, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0xbe, 0x08, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x96, 0x90, 0x00, 0x78, 0x10, 0x02, 0x20, 0xf8, 0x9c, 0x40, 0x21, 0xdc, 0x8a, 0x41,
        0x05, 0xcf, 0xe2, 0x7e, 0x17, 0xf4, 0x37, 0xea, 0x1f, 0xf5, 0x41, 0x1f, 0xaa, 0x82, 0x7f,
        0x55, 0x41, 0x5f, 0xaa, 0xa8, 0x2f, 0xf5, 0x55, 0x41, 0x9f, 0xaa, 0xaa, 0x83, 0x7f, 0x55,
        0x55, 0x41, 0xdf, 0xaa, 0xaa, 0xa8, 0x3f, 0xf5, 0x55, 0x55, 0x40, 0x87, 0xea, 0xaa, 0xaa,
        0xa5, 0x37, 0x0c, 0x0d, 0x01, 0x00, 0x80,
    ];

    const MAIN_420_8BIT_1280X720_SPS: &[u8] = &[
        0x42, 0x01, 0x01, 0x01, 0x40, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x03, 0x00, 0x78, 0xa0, 0x02, 0x80, 0x80, 0x2d, 0x17,
    ];

    const SUB_LAYER_PROFILE_SPS: &[u8] = &[
        0x42, 0x01, 0x03, 0x01, 0x40, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x03, 0x00, 0x78, 0x80, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0xa0, 0x88, 0x45, 0xc0,
    ];

    #[test]
    fn captured_sps_reports_main444_8bit_and_exact_visible_geometry() {
        let actual = parse_hevc_sps(CAPTURED_MAIN444_8BIT_SPS).unwrap();

        assert_eq!(
            actual,
            HevcSps {
                general_profile_space: 0,
                general_tier_flag: false,
                general_profile_idc: 4,
                general_profile_compatibility_flags: 0x0800_0000,
                general_constraint_indicator_flags: 0xbe08_0000_0000,
                general_level_idc: 150,
                chroma_format_idc: 3,
                separate_colour_plane_flag: false,
                coded_width: 1920,
                coded_height: 1088,
                crop_left: 0,
                crop_right: 0,
                crop_top: 0,
                crop_bottom: 8,
                visible_width: 1920,
                visible_height: 1080,
                bit_depth_luma: 8,
                bit_depth_chroma: 8,
            }
        );
        assert!(actual.is_main444_8bit());
    }

    #[test]
    fn missing_required_rext_constraint_never_passes_the_main444_8bit_gate() {
        let captured = parse_hevc_sps(CAPTURED_MAIN444_8BIT_SPS).unwrap();

        for required_bit in [43, 42, 41, 35] {
            let mut without_required_constraint = captured;
            without_required_constraint.general_constraint_indicator_flags &= !(1 << required_bit);

            assert!(
                !without_required_constraint.is_main444_8bit(),
                "constraint bit {required_bit} must be required"
            );
        }
    }

    #[test]
    fn unsupported_ptl_header_values_never_pass_the_main444_8bit_gate() {
        let mut nonstandard_profile_space = CAPTURED_MAIN444_8BIT_SPS.to_vec();
        nonstandard_profile_space[3] |= 1 << 6;

        let actual = parse_hevc_sps(&nonstandard_profile_space).unwrap();
        assert_eq!(actual.general_profile_space, 1);
        assert!(!actual.is_main444_8bit());

        let mut reserved_max_sub_layers = CAPTURED_MAIN444_8BIT_SPS.to_vec();
        reserved_max_sub_layers[2] |= 0x0e;
        assert_eq!(
            parse_hevc_sps(&reserved_max_sub_layers),
            Err(HevcSpsError::InvalidReservedBits)
        );
    }

    #[test]
    fn main_420_sps_is_parsed_but_does_not_pass_the_main444_gate() {
        let actual = parse_hevc_sps(MAIN_420_8BIT_1280X720_SPS).unwrap();

        assert_eq!(actual.general_profile_idc, 1);
        assert_eq!(actual.general_profile_compatibility_flags, 0x4000_0000);
        assert_eq!(actual.chroma_format_idc, 1);
        assert_eq!((actual.visible_width, actual.visible_height), (1280, 720));
        assert_eq!((actual.bit_depth_luma, actual.bit_depth_chroma), (8, 8));
        assert!(!actual.is_main444_8bit());
    }

    #[test]
    fn present_sub_layer_profile_is_skipped_without_relaxing_the_gate() {
        let actual = parse_hevc_sps(SUB_LAYER_PROFILE_SPS).unwrap();

        assert_eq!((actual.coded_width, actual.coded_height), (16, 16));
        assert_eq!(actual.chroma_format_idc, 1);
        assert!(!actual.is_main444_8bit());
    }

    #[test]
    fn truncated_sps_never_publishes_partial_capabilities() {
        assert_eq!(
            parse_hevc_sps(&CAPTURED_MAIN444_8BIT_SPS[..17]),
            Err(HevcSpsError::Truncated)
        );
        assert_eq!(parse_hevc_sps(&[0x42, 0x01]), Err(HevcSpsError::Truncated));
    }

    #[test]
    fn invalid_emulation_prevention_is_rejected_before_bit_parsing() {
        assert_eq!(
            parse_hevc_sps(&[0x42, 0x01, 0x00, 0x00, 0x03, 0x04]),
            Err(HevcSpsError::InvalidEmulationPrevention)
        );
        assert_eq!(
            parse_hevc_sps(&[0x42, 0x01, 0x00, 0x00, 0x03]),
            Err(HevcSpsError::InvalidEmulationPrevention)
        );
        assert_eq!(
            parse_hevc_sps(&[0x42, 0x01, 0x00, 0x00, 0x01]),
            Err(HevcSpsError::InvalidEmulationPrevention)
        );
    }

    #[test]
    fn exp_golomb_and_nal_size_budgets_fail_closed() {
        const UE_OVERFLOW_SPS: &[u8] = &[
            0x42, 0x01, 0x01, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0xbe, 0x08, 0x00, 0x00, 0x03,
            0x00, 0x00, 0x96, 0x00, 0x00, 0x03, 0x00, 0x00, 0x80,
        ];
        assert_eq!(
            parse_hevc_sps(UE_OVERFLOW_SPS),
            Err(HevcSpsError::ExpGolombOverflow)
        );

        let oversized = vec![0; MAX_HEVC_SPS_NAL_BYTES + 1];
        assert_eq!(
            parse_hevc_sps(&oversized),
            Err(HevcSpsError::NalBudgetExceeded {
                limit: MAX_HEVC_SPS_NAL_BYTES
            })
        );
    }
}
