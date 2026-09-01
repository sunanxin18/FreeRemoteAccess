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
const MAX_SHORT_TERM_REF_PIC_SETS: u32 = 64;
const MAX_DPB_SIZE_MINUS1: u32 = 15;
const MAX_LONG_TERM_REFS: u32 = 32;
const MAX_HRD_CPB_CNT_MINUS1: u32 = 31;
const MAX_DELTA_POC_MINUS1: u32 = (1 << 15) - 1;

#[cfg(test)]
pub(crate) const CAPTURED_MAIN444_8BIT_SPS: &[u8] = &[
    0x42, 0x01, 0x01, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0xbe, 0x08, 0x00, 0x00, 0x03, 0x00, 0x00,
    0x96, 0x90, 0x00, 0x78, 0x10, 0x02, 0x20, 0xf8, 0x9c, 0x40, 0x21, 0xdc, 0x8a, 0x41, 0x05, 0xcf,
    0xe2, 0x7e, 0x17, 0xf4, 0x37, 0xea, 0x1f, 0xf5, 0x41, 0x1f, 0xaa, 0x82, 0x7f, 0x55, 0x41, 0x5f,
    0xaa, 0xa8, 0x2f, 0xf5, 0x55, 0x41, 0x9f, 0xaa, 0xaa, 0x83, 0x7f, 0x55, 0x55, 0x41, 0xdf, 0xaa,
    0xaa, 0xa8, 0x3f, 0xf5, 0x55, 0x55, 0x40, 0x87, 0xea, 0xaa, 0xaa, 0xa5, 0x37, 0x0c, 0x0d, 0x01,
    0x00, 0x80,
];

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
    NalBudgetExceeded {
        limit: usize,
    },
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
    InvalidRange {
        field: &'static str,
        value: u32,
        maximum: u32,
    },
    UnsupportedSyntax(&'static str),
    InvalidTrailingBits,
    ExtraRbspData,
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
            Self::InvalidRange {
                field,
                value,
                maximum,
            } => write!(
                formatter,
                "HEVC SPS {field} 超出范围：{value}，最大允许 {maximum}"
            ),
            Self::UnsupportedSyntax(field) => {
                write!(formatter, "HEVC SPS 暂不支持 {field}")
            }
            Self::InvalidTrailingBits => formatter.write_str("HEVC SPS rbsp_trailing_bits 非法"),
            Self::ExtraRbspData => formatter.write_str("HEVC SPS 尾随位后存在额外数据"),
        }
    }
}

impl Error for HevcSpsError {}

/// 解析一个完整的 HEVC SPS NAL（包含 2 字节 NAL header）。
///
/// 只提取解码后端选择所需的能力字段；其余 SPS 语法仅做有界完整验证，
/// 不在此门禁中推断或解码图像内容。
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

    let sps_id = bits.read_ue()?;
    validate_max("sps_seq_parameter_set_id", sps_id, 15)?;
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

    parse_sps_suffix(
        &mut bits,
        max_sub_layers_minus1,
        8 + bit_depth_luma_minus8,
        8 + bit_depth_chroma_minus8,
        if separate_colour_plane_flag {
            0
        } else {
            chroma_format_idc
        },
    )?;

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

fn parse_sps_suffix(
    bits: &mut BitReader<'_>,
    max_sub_layers_minus1: u8,
    bit_depth_luma: u32,
    bit_depth_chroma: u32,
    chroma_array_type: u8,
) -> Result<(), HevcSpsError> {
    let log2_max_pic_order_cnt_lsb_minus4 = bits.read_ue()?;
    validate_max(
        "log2_max_pic_order_cnt_lsb_minus4",
        log2_max_pic_order_cnt_lsb_minus4,
        12,
    )?;

    let ordering_info_present = bits.read_bit()?;
    let first_layer = if ordering_info_present {
        0
    } else {
        max_sub_layers_minus1
    };
    let mut highest_max_dec_pic_buffering_minus1 = None;
    let mut previous_max_dec_pic_buffering_minus1 = None;
    let mut previous_max_num_reorder_pics = None;
    for layer in first_layer..=max_sub_layers_minus1 {
        let max_dec_pic_buffering_minus1 = bits.read_ue()?;
        validate_max(
            "sps_max_dec_pic_buffering_minus1",
            max_dec_pic_buffering_minus1,
            MAX_DPB_SIZE_MINUS1,
        )?;
        if previous_max_dec_pic_buffering_minus1
            .is_some_and(|previous| max_dec_pic_buffering_minus1 < previous)
        {
            return Err(HevcSpsError::InvalidRange {
                field: "sps_max_dec_pic_buffering_minus1 sub-layer ordering",
                value: max_dec_pic_buffering_minus1,
                maximum: previous_max_dec_pic_buffering_minus1.unwrap(),
            });
        }
        let max_num_reorder_pics = bits.read_ue()?;
        if max_num_reorder_pics > max_dec_pic_buffering_minus1 {
            return Err(HevcSpsError::InvalidRange {
                field: "sps_max_num_reorder_pics",
                value: max_num_reorder_pics,
                maximum: max_dec_pic_buffering_minus1,
            });
        }
        if previous_max_num_reorder_pics.is_some_and(|previous| max_num_reorder_pics < previous) {
            return Err(HevcSpsError::InvalidRange {
                field: "sps_max_num_reorder_pics sub-layer ordering",
                value: max_num_reorder_pics,
                maximum: previous_max_num_reorder_pics.unwrap(),
            });
        }
        validate_max(
            "sps_max_latency_increase_plus1",
            bits.read_ue()?,
            u32::MAX - 1,
        )?;
        previous_max_dec_pic_buffering_minus1 = Some(max_dec_pic_buffering_minus1);
        previous_max_num_reorder_pics = Some(max_num_reorder_pics);
        if layer == max_sub_layers_minus1 {
            highest_max_dec_pic_buffering_minus1 = Some(max_dec_pic_buffering_minus1);
        }
    }
    let max_dec_pic_buffering_minus1 =
        highest_max_dec_pic_buffering_minus1.expect("至少解析最高 temporal sub-layer");

    let log2_min_luma_coding_block_size_minus3 = bits.read_ue()?;
    validate_max(
        "log2_min_luma_coding_block_size_minus3",
        log2_min_luma_coding_block_size_minus3,
        3,
    )?;
    let log2_diff_max_min_luma_coding_block_size = bits.read_ue()?;
    validate_sum_max(
        "log2_diff_max_min_luma_coding_block_size",
        log2_min_luma_coding_block_size_minus3,
        log2_diff_max_min_luma_coding_block_size,
        3,
    )?;
    let min_cb_log2_size_y = log2_min_luma_coding_block_size_minus3 + 3;
    let ctb_log2_size_y = min_cb_log2_size_y + log2_diff_max_min_luma_coding_block_size;
    let log2_min_luma_transform_block_size_minus2 = bits.read_ue()?;
    validate_max(
        "log2_min_luma_transform_block_size_minus2",
        log2_min_luma_transform_block_size_minus2,
        3,
    )?;
    let log2_diff_max_min_luma_transform_block_size = bits.read_ue()?;
    let min_tb_log2_size_y = log2_min_luma_transform_block_size_minus2 + 2;
    if min_tb_log2_size_y >= min_cb_log2_size_y {
        return Err(HevcSpsError::InvalidRange {
            field: "MinTbLog2SizeY",
            value: min_tb_log2_size_y,
            maximum: min_cb_log2_size_y - 1,
        });
    }
    let max_tb_log2_size_y = min_tb_log2_size_y
        .checked_add(log2_diff_max_min_luma_transform_block_size)
        .ok_or(HevcSpsError::ExpGolombOverflow)?;
    validate_max("MaxTbLog2SizeY", max_tb_log2_size_y, ctb_log2_size_y.min(5))?;
    let max_transform_hierarchy_depth = ctb_log2_size_y - min_tb_log2_size_y;
    validate_max(
        "max_transform_hierarchy_depth_inter",
        bits.read_ue()?,
        max_transform_hierarchy_depth,
    )?;
    validate_max(
        "max_transform_hierarchy_depth_intra",
        bits.read_ue()?,
        max_transform_hierarchy_depth,
    )?;

    if bits.read_bit()? && bits.read_bit()? {
        skip_scaling_list_data(bits)?;
    }
    bits.read_bit()?; // amp_enabled_flag
    bits.read_bit()?; // sample_adaptive_offset_enabled_flag
    if bits.read_bit()? {
        let pcm_bit_depth_luma = bits.read_bits(4)? as u32 + 1;
        validate_max("PcmBitDepthY", pcm_bit_depth_luma, bit_depth_luma)?;
        let pcm_bit_depth_chroma = bits.read_bits(4)? as u32 + 1;
        if chroma_array_type != 0 {
            validate_max("PcmBitDepthC", pcm_bit_depth_chroma, bit_depth_chroma)?;
        }
        let log2_min_pcm_luma_coding_block_size_minus3 = bits.read_ue()?;
        let min_pcm_log2_size_y = log2_min_pcm_luma_coding_block_size_minus3
            .checked_add(3)
            .ok_or(HevcSpsError::ExpGolombOverflow)?;
        let minimum_pcm_log2_size_y = min_cb_log2_size_y.min(5);
        let maximum_pcm_log2_size_y = ctb_log2_size_y.min(5);
        if min_pcm_log2_size_y < minimum_pcm_log2_size_y {
            return Err(HevcSpsError::InvalidRange {
                field: "Log2MinIpcmCbSizeY",
                value: min_pcm_log2_size_y,
                maximum: minimum_pcm_log2_size_y,
            });
        }
        validate_max(
            "Log2MinIpcmCbSizeY",
            min_pcm_log2_size_y,
            maximum_pcm_log2_size_y,
        )?;
        let log2_diff_max_min_pcm_luma_coding_block_size = bits.read_ue()?;
        let max_pcm_log2_size_y = min_pcm_log2_size_y
            .checked_add(log2_diff_max_min_pcm_luma_coding_block_size)
            .ok_or(HevcSpsError::ExpGolombOverflow)?;
        validate_max(
            "Log2MaxIpcmCbSizeY",
            max_pcm_log2_size_y,
            maximum_pcm_log2_size_y,
        )?;
        bits.read_bit()?; // pcm_loop_filter_disabled_flag
    }

    let num_short_term_ref_pic_sets = bits.read_ue()?;
    validate_max(
        "num_short_term_ref_pic_sets",
        num_short_term_ref_pic_sets,
        MAX_SHORT_TERM_REF_PIC_SETS,
    )?;
    skip_short_term_ref_pic_sets(
        bits,
        num_short_term_ref_pic_sets,
        max_dec_pic_buffering_minus1,
    )?;

    if bits.read_bit()? {
        let num_long_term_ref_pics_sps = bits.read_ue()?;
        validate_max(
            "num_long_term_ref_pics_sps",
            num_long_term_ref_pics_sps,
            MAX_LONG_TERM_REFS,
        )?;
        let poc_width = usize::try_from(log2_max_pic_order_cnt_lsb_minus4 + 4)
            .map_err(|_| HevcSpsError::ExpGolombOverflow)?;
        for _ in 0..num_long_term_ref_pics_sps {
            bits.read_bits(poc_width)?;
            bits.read_bit()?;
        }
    }
    bits.read_bit()?; // sps_temporal_mvp_enabled_flag
    bits.read_bit()?; // strong_intra_smoothing_enabled_flag
    if bits.read_bit()? {
        skip_vui_parameters(bits, max_sub_layers_minus1)?;
    }

    if bits.read_bit()? {
        let range_extension = bits.read_bit()?;
        let multilayer_extension = bits.read_bit()?;
        let extension_3d = bits.read_bit()?;
        let scc_extension = bits.read_bit()?;
        let extension_4bits = bits.read_bits(4)?;
        if range_extension {
            bits.skip_bits(9)?;
        }
        if multilayer_extension || extension_3d || scc_extension || extension_4bits != 0 {
            return Err(HevcSpsError::UnsupportedSyntax("非 Range SPS extension"));
        }
    }

    bits.finish_rbsp()
}

fn skip_scaling_list_data(bits: &mut BitReader<'_>) -> Result<(), HevcSpsError> {
    for size_id in 0..4usize {
        let matrix_step = if size_id == 3 { 3 } else { 1 };
        for matrix_id in (0..6usize).step_by(matrix_step) {
            if !bits.read_bit()? {
                let delta = bits.read_ue()?;
                let maximum_delta = if size_id == 3 {
                    matrix_id as u32 / 3
                } else {
                    matrix_id as u32
                };
                validate_max("scaling_list_pred_matrix_id_delta", delta, maximum_delta)?;
                continue;
            }
            let mut next_coefficient = 8i32;
            if size_id > 1 {
                let dc_minus8 = bits.read_se()?;
                if !(-7..=247).contains(&dc_minus8) {
                    return Err(HevcSpsError::InvalidRange {
                        field: "scaling_list_dc_coef_minus8",
                        value: dc_minus8.unsigned_abs(),
                        maximum: 247,
                    });
                }
                next_coefficient = dc_minus8 + 8;
            }
            let coefficient_count = 64usize.min(1usize << (4 + (size_id << 1)));
            for _ in 0..coefficient_count {
                let delta = bits.read_se()?;
                if !(-128..=127).contains(&delta) {
                    return Err(HevcSpsError::InvalidRange {
                        field: "scaling_list_delta_coef",
                        value: delta.unsigned_abs(),
                        maximum: 128,
                    });
                }
                next_coefficient = (next_coefficient + delta + 256) % 256;
                if next_coefficient == 0 {
                    return Err(HevcSpsError::InvalidRange {
                        field: "ScalingList",
                        value: 0,
                        maximum: 255,
                    });
                }
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct ShortTermRefPicSet {
    negative: Vec<i32>,
    positive: Vec<i32>,
}

impl ShortTermRefPicSet {
    fn len(&self) -> usize {
        self.negative.len() + self.positive.len()
    }
}

fn skip_short_term_ref_pic_sets(
    bits: &mut BitReader<'_>,
    count: u32,
    max_dec_pic_buffering_minus1: u32,
) -> Result<(), HevcSpsError> {
    let mut reference_sets: Vec<ShortTermRefPicSet> = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        let inter_prediction = index != 0 && bits.read_bit()?;
        let reference_set = if inter_prediction {
            let delta_rps_sign = bits.read_bit()?;
            let abs_delta_rps_minus1 = bits.read_ue()?;
            validate_max(
                "abs_delta_rps_minus1",
                abs_delta_rps_minus1,
                MAX_DELTA_POC_MINUS1,
            )?;
            let delta_rps_magnitude = i32::try_from(abs_delta_rps_minus1 + 1)
                .map_err(|_| HevcSpsError::ExpGolombOverflow)?;
            let delta_rps = if delta_rps_sign {
                -delta_rps_magnitude
            } else {
                delta_rps_magnitude
            };
            let reference = &reference_sets[index - 1];
            let mut use_delta_flags = Vec::with_capacity(reference.len() + 1);
            for _ in 0..=reference.len() {
                let used_by_current = bits.read_bit()?;
                let use_delta = used_by_current || bits.read_bit()?;
                use_delta_flags.push(use_delta);
            }
            derive_inter_predicted_ref_pic_set(reference, delta_rps, &use_delta_flags)?
        } else {
            let negative = bits.read_ue()?;
            validate_max("num_negative_pics", negative, max_dec_pic_buffering_minus1)?;
            let positive = bits.read_ue()?;
            validate_max(
                "num_positive_pics",
                positive,
                max_dec_pic_buffering_minus1 - negative,
            )?;
            let mut reference_set = ShortTermRefPicSet {
                negative: Vec::with_capacity(negative as usize),
                positive: Vec::with_capacity(positive as usize),
            };
            let mut previous_delta_poc = 0i32;
            for _ in 0..negative {
                let delta_poc_minus1 = bits.read_ue()?;
                validate_max(
                    "delta_poc_s0_minus1",
                    delta_poc_minus1,
                    MAX_DELTA_POC_MINUS1,
                )?;
                let delta = i32::try_from(delta_poc_minus1 + 1)
                    .map_err(|_| HevcSpsError::ExpGolombOverflow)?;
                previous_delta_poc = previous_delta_poc
                    .checked_sub(delta)
                    .ok_or(HevcSpsError::ExpGolombOverflow)?;
                if previous_delta_poc >= 0 {
                    return Err(HevcSpsError::InvalidRange {
                        field: "DeltaPocS0",
                        value: previous_delta_poc.unsigned_abs(),
                        maximum: i32::MAX as u32,
                    });
                }
                reference_set.negative.push(previous_delta_poc);
                bits.read_bit()?;
            }
            previous_delta_poc = 0;
            for _ in 0..positive {
                let delta_poc_minus1 = bits.read_ue()?;
                validate_max(
                    "delta_poc_s1_minus1",
                    delta_poc_minus1,
                    MAX_DELTA_POC_MINUS1,
                )?;
                let delta = i32::try_from(delta_poc_minus1 + 1)
                    .map_err(|_| HevcSpsError::ExpGolombOverflow)?;
                previous_delta_poc = previous_delta_poc
                    .checked_add(delta)
                    .ok_or(HevcSpsError::ExpGolombOverflow)?;
                if previous_delta_poc <= 0 {
                    return Err(HevcSpsError::InvalidRange {
                        field: "DeltaPocS1",
                        value: previous_delta_poc.unsigned_abs(),
                        maximum: i32::MAX as u32,
                    });
                }
                reference_set.positive.push(previous_delta_poc);
                bits.read_bit()?;
            }
            reference_set
        };
        validate_max(
            "NumDeltaPocs",
            u32::try_from(reference_set.len()).map_err(|_| HevcSpsError::ExpGolombOverflow)?,
            max_dec_pic_buffering_minus1,
        )?;
        validate_delta_poc_order(&reference_set)?;
        reference_sets.push(reference_set);
    }
    Ok(())
}

fn derive_inter_predicted_ref_pic_set(
    reference: &ShortTermRefPicSet,
    delta_rps: i32,
    use_delta_flags: &[bool],
) -> Result<ShortTermRefPicSet, HevcSpsError> {
    if use_delta_flags.len() != reference.len() + 1 {
        return Err(HevcSpsError::UnsupportedSyntax(
            "short-term RPS reference flag count",
        ));
    }
    let mut derived = ShortTermRefPicSet::default();
    let negative_count = reference.negative.len();

    for index in (0..reference.positive.len()).rev() {
        let delta_poc = reference.positive[index]
            .checked_add(delta_rps)
            .ok_or(HevcSpsError::ExpGolombOverflow)?;
        if delta_poc < 0 && use_delta_flags[negative_count + index] {
            derived.negative.push(delta_poc);
        }
    }
    if delta_rps < 0 && use_delta_flags[reference.len()] {
        derived.negative.push(delta_rps);
    }
    for (index, delta_poc) in reference.negative.iter().enumerate() {
        let delta_poc = delta_poc
            .checked_add(delta_rps)
            .ok_or(HevcSpsError::ExpGolombOverflow)?;
        if delta_poc < 0 && use_delta_flags[index] {
            derived.negative.push(delta_poc);
        }
    }

    for index in (0..reference.negative.len()).rev() {
        let delta_poc = reference.negative[index]
            .checked_add(delta_rps)
            .ok_or(HevcSpsError::ExpGolombOverflow)?;
        if delta_poc > 0 && use_delta_flags[index] {
            derived.positive.push(delta_poc);
        }
    }
    if delta_rps > 0 && use_delta_flags[reference.len()] {
        derived.positive.push(delta_rps);
    }
    for (index, delta_poc) in reference.positive.iter().enumerate() {
        let delta_poc = delta_poc
            .checked_add(delta_rps)
            .ok_or(HevcSpsError::ExpGolombOverflow)?;
        if delta_poc > 0 && use_delta_flags[negative_count + index] {
            derived.positive.push(delta_poc);
        }
    }
    Ok(derived)
}

fn validate_delta_poc_order(reference: &ShortTermRefPicSet) -> Result<(), HevcSpsError> {
    if reference.negative.windows(2).any(|pair| pair[0] <= pair[1])
        || reference.positive.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(HevcSpsError::UnsupportedSyntax(
            "short-term RPS delta POC ordering",
        ));
    }
    Ok(())
}

fn skip_vui_parameters(
    bits: &mut BitReader<'_>,
    max_sub_layers_minus1: u8,
) -> Result<(), HevcSpsError> {
    if bits.read_bit()? {
        let aspect_ratio_idc = bits.read_bits(8)? as u32;
        if aspect_ratio_idc == 255 {
            let width = bits.read_bits(16)? as u32;
            let height = bits.read_bits(16)? as u32;
            if width == 0 || height == 0 {
                return Err(HevcSpsError::InvalidRange {
                    field: "sar_width/sar_height",
                    value: 0,
                    maximum: u16::MAX.into(),
                });
            }
        } else if aspect_ratio_idc > 16 {
            return Err(HevcSpsError::InvalidRange {
                field: "aspect_ratio_idc",
                value: aspect_ratio_idc,
                maximum: 16,
            });
        }
    }
    if bits.read_bit()? {
        bits.read_bit()?; // overscan_appropriate_flag
    }
    if bits.read_bit()? {
        let video_format = bits.read_bits(3)? as u32;
        validate_max("video_format", video_format, 5)?;
        bits.read_bit()?; // video_full_range_flag
        if bits.read_bit()? {
            bits.skip_bits(24)?;
        }
    }
    if bits.read_bit()? {
        validate_max("chroma_sample_loc_type_top_field", bits.read_ue()?, 5)?;
        validate_max("chroma_sample_loc_type_bottom_field", bits.read_ue()?, 5)?;
    }
    bits.read_bit()?; // neutral_chroma_indication_flag
    bits.read_bit()?; // field_seq_flag
    bits.read_bit()?; // frame_field_info_present_flag
    if bits.read_bit()? {
        for _ in 0..4 {
            bits.read_ue()?;
        }
    }
    if bits.read_bit()? {
        let num_units_in_tick = bits.read_bits(32)?;
        let time_scale = bits.read_bits(32)?;
        if num_units_in_tick == 0 || time_scale == 0 {
            return Err(HevcSpsError::InvalidRange {
                field: "vui timing",
                value: 0,
                maximum: u32::MAX,
            });
        }
        if bits.read_bit()? {
            bits.read_ue()?;
        }
        if bits.read_bit()? {
            skip_hrd_parameters(bits, max_sub_layers_minus1)?;
        }
    }
    if bits.read_bit()? {
        bits.read_bit()?; // tiles_fixed_structure_flag
        bits.read_bit()?; // motion_vectors_over_pic_boundaries_flag
        bits.read_bit()?; // restricted_ref_pic_lists_flag
        validate_max("min_spatial_segmentation_idc", bits.read_ue()?, 4095)?;
        validate_max("max_bytes_per_pic_denom", bits.read_ue()?, 16)?;
        validate_max("max_bits_per_min_cu_denom", bits.read_ue()?, 16)?;
        validate_max("log2_max_mv_length_horizontal", bits.read_ue()?, 15)?;
        validate_max("log2_max_mv_length_vertical", bits.read_ue()?, 15)?;
    }
    Ok(())
}

fn skip_hrd_parameters(
    bits: &mut BitReader<'_>,
    max_sub_layers_minus1: u8,
) -> Result<(), HevcSpsError> {
    let nal_hrd_parameters_present = bits.read_bit()?;
    let vcl_hrd_parameters_present = bits.read_bit()?;
    let mut sub_pic_hrd_params_present = false;
    let mut bit_rate_scale = 0u32;
    let mut cpb_size_scale = 0u32;
    let mut cpb_size_du_scale = 0u32;
    if nal_hrd_parameters_present || vcl_hrd_parameters_present {
        sub_pic_hrd_params_present = bits.read_bit()?;
        if sub_pic_hrd_params_present {
            bits.read_bits(8)?;
            bits.read_bits(5)?;
            bits.read_bit()?;
            bits.read_bits(5)?;
        }
        bit_rate_scale = bits.read_bits(4)? as u32;
        cpb_size_scale = bits.read_bits(4)? as u32;
        if sub_pic_hrd_params_present {
            cpb_size_du_scale = bits.read_bits(4)? as u32;
        }
        bits.read_bits(5)?;
        bits.read_bits(5)?;
        bits.read_bits(5)?;
    }

    for _ in 0..=max_sub_layers_minus1 {
        let fixed_pic_rate_general = bits.read_bit()?;
        let fixed_pic_rate_within_cvs = fixed_pic_rate_general || bits.read_bit()?;
        let low_delay_hrd = if fixed_pic_rate_within_cvs {
            validate_max("elemental_duration_in_tc_minus1", bits.read_ue()?, 2047)?;
            false
        } else {
            bits.read_bit()?
        };
        let cpb_cnt_minus1 = if low_delay_hrd { 0 } else { bits.read_ue()? };
        validate_max("cpb_cnt_minus1", cpb_cnt_minus1, MAX_HRD_CPB_CNT_MINUS1)?;
        if nal_hrd_parameters_present {
            skip_sub_layer_hrd_parameters(
                bits,
                cpb_cnt_minus1,
                sub_pic_hrd_params_present,
                bit_rate_scale,
                cpb_size_scale,
                cpb_size_du_scale,
            )?;
        }
        if vcl_hrd_parameters_present {
            skip_sub_layer_hrd_parameters(
                bits,
                cpb_cnt_minus1,
                sub_pic_hrd_params_present,
                bit_rate_scale,
                cpb_size_scale,
                cpb_size_du_scale,
            )?;
        }
    }
    Ok(())
}

fn skip_sub_layer_hrd_parameters(
    bits: &mut BitReader<'_>,
    cpb_cnt_minus1: u32,
    sub_pic_hrd_params_present: bool,
    bit_rate_scale: u32,
    cpb_size_scale: u32,
    cpb_size_du_scale: u32,
) -> Result<(), HevcSpsError> {
    let mut previous_bit_rate = None;
    let mut previous_cpb_size = None;
    let mut previous_cpb_size_du = None;
    let mut previous_bit_rate_du = None;
    for index in 0..=cpb_cnt_minus1 {
        let bit_rate_value_minus1 = bits.read_ue()?;
        validate_max("bit_rate_value_minus1", bit_rate_value_minus1, u32::MAX - 1)?;
        let bit_rate = scaled_hrd_value(bit_rate_value_minus1, 6, bit_rate_scale)?;
        if index != 0 && previous_bit_rate.is_some_and(|previous| bit_rate <= previous) {
            return Err(HevcSpsError::UnsupportedSyntax(
                "HRD bit_rate_value_minus1 ordering",
            ));
        }

        let cpb_size_value_minus1 = bits.read_ue()?;
        validate_max("cpb_size_value_minus1", cpb_size_value_minus1, u32::MAX - 1)?;
        let cpb_size = scaled_hrd_value(cpb_size_value_minus1, 4, cpb_size_scale)?;
        if let Some(previous) = previous_cpb_size {
            if cpb_size > previous {
                return Err(HevcSpsError::UnsupportedSyntax(
                    "HRD cpb_size_value_minus1 ordering",
                ));
            }
        }

        if sub_pic_hrd_params_present {
            let cpb_size_du_value_minus1 = bits.read_ue()?;
            validate_max(
                "cpb_size_du_value_minus1",
                cpb_size_du_value_minus1,
                u32::MAX - 1,
            )?;
            let cpb_size_du = scaled_hrd_value(cpb_size_du_value_minus1, 4, cpb_size_du_scale)?;
            if let Some(previous) = previous_cpb_size_du {
                if cpb_size_du > previous {
                    return Err(HevcSpsError::UnsupportedSyntax(
                        "HRD cpb_size_du_value_minus1 ordering",
                    ));
                }
            }

            let bit_rate_du_value_minus1 = bits.read_ue()?;
            validate_max(
                "bit_rate_du_value_minus1",
                bit_rate_du_value_minus1,
                u32::MAX - 1,
            )?;
            let bit_rate_du = scaled_hrd_value(bit_rate_du_value_minus1, 6, bit_rate_scale)?;
            if index != 0 && previous_bit_rate_du.is_some_and(|previous| bit_rate_du <= previous) {
                return Err(HevcSpsError::UnsupportedSyntax(
                    "HRD bit_rate_du_value_minus1 ordering",
                ));
            }
            previous_cpb_size_du = Some(cpb_size_du);
            previous_bit_rate_du = Some(bit_rate_du);
        }
        bits.read_bit()?;
        previous_bit_rate = Some(bit_rate);
        previous_cpb_size = Some(cpb_size);
    }
    Ok(())
}

fn scaled_hrd_value(value_minus1: u32, base_shift: u32, scale: u32) -> Result<u64, HevcSpsError> {
    let shift = base_shift
        .checked_add(scale)
        .ok_or(HevcSpsError::ExpGolombOverflow)?;
    (u64::from(value_minus1) + 1)
        .checked_shl(shift)
        .ok_or(HevcSpsError::ExpGolombOverflow)
}

fn validate_max(field: &'static str, value: u32, maximum: u32) -> Result<(), HevcSpsError> {
    if value > maximum {
        return Err(HevcSpsError::InvalidRange {
            field,
            value,
            maximum,
        });
    }
    Ok(())
}

fn validate_sum_max(
    field: &'static str,
    first: u32,
    second: u32,
    maximum: u32,
) -> Result<(), HevcSpsError> {
    let value = first
        .checked_add(second)
        .ok_or(HevcSpsError::ExpGolombOverflow)?;
    validate_max(field, value, maximum)
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

    fn read_se(&mut self) -> Result<i32, HevcSpsError> {
        let code_num = i64::from(self.read_ue()?);
        let value = if code_num & 1 == 0 {
            -(code_num / 2)
        } else {
            (code_num + 1) / 2
        };
        i32::try_from(value).map_err(|_| HevcSpsError::ExpGolombOverflow)
    }

    fn finish_rbsp(&mut self) -> Result<(), HevcSpsError> {
        if !self.read_bit()? {
            return Err(HevcSpsError::InvalidTrailingBits);
        }
        while self.bit_offset % 8 != 0 {
            if self.read_bit()? {
                return Err(HevcSpsError::InvalidTrailingBits);
            }
        }
        if self.bit_offset != self.bytes.len().saturating_mul(8) {
            return Err(HevcSpsError::ExtraRbspData);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_hevc_sps, skip_hrd_parameters, skip_scaling_list_data, skip_short_term_ref_pic_sets,
        BitReader, HevcSps, HevcSpsError, CAPTURED_MAIN444_8BIT_SPS, MAX_HEVC_SPS_NAL_BYTES,
    };

    #[derive(Default)]
    struct BitWriter {
        bytes: Vec<u8>,
        bit_len: usize,
    }

    impl BitWriter {
        fn bit(&mut self, value: bool) {
            if self.bit_len % 8 == 0 {
                self.bytes.push(0);
            }
            if value {
                let shift = 7 - self.bit_len % 8;
                *self.bytes.last_mut().unwrap() |= 1 << shift;
            }
            self.bit_len += 1;
        }

        fn bits(&mut self, value: u64, width: usize) {
            for shift in (0..width).rev() {
                self.bit((value >> shift) & 1 != 0);
            }
        }

        fn ue(&mut self, value: u32) {
            let code_num = u64::from(value) + 1;
            let width = (u64::BITS - code_num.leading_zeros()) as usize;
            for _ in 1..width {
                self.bit(false);
            }
            self.bits(code_num, width);
        }

        fn rbsp_trailing_bits(&mut self) {
            self.bit(true);
            while self.bit_len % 8 != 0 {
                self.bit(false);
            }
        }

        fn into_bytes(self) -> Vec<u8> {
            self.bytes
        }
    }

    #[derive(Clone, Copy)]
    struct SuffixFixture {
        max_dec_pic_buffering_minus1: u32,
        min_cb_minus3: u32,
        diff_cb: u32,
        min_tb_minus2: u32,
        diff_tb: u32,
        hierarchy_inter: u32,
        hierarchy_intra: u32,
        pcm_luma_minus1: Option<u8>,
        pcm_chroma_minus1: u8,
        min_pcm_minus3: u32,
        diff_pcm: u32,
        negative_refs: u32,
        negative_delta_minus1: u32,
    }

    impl Default for SuffixFixture {
        fn default() -> Self {
            Self {
                max_dec_pic_buffering_minus1: 15,
                min_cb_minus3: 0,
                diff_cb: 2,
                min_tb_minus2: 0,
                diff_tb: 3,
                hierarchy_inter: 3,
                hierarchy_intra: 3,
                pcm_luma_minus1: None,
                pcm_chroma_minus1: 7,
                min_pcm_minus3: 0,
                diff_pcm: 2,
                negative_refs: 0,
                negative_delta_minus1: 0,
            }
        }
    }

    fn write_suffix(writer: &mut BitWriter, fixture: SuffixFixture) {
        writer.ue(0); // log2_max_pic_order_cnt_lsb_minus4
        writer.bit(false); // ordering info only at the highest sub-layer
        writer.ue(fixture.max_dec_pic_buffering_minus1);
        writer.ue(0); // sps_max_num_reorder_pics
        writer.ue(0); // sps_max_latency_increase_plus1
        writer.ue(fixture.min_cb_minus3);
        writer.ue(fixture.diff_cb);
        writer.ue(fixture.min_tb_minus2);
        writer.ue(fixture.diff_tb);
        writer.ue(fixture.hierarchy_inter);
        writer.ue(fixture.hierarchy_intra);
        writer.bit(false); // scaling_list_enabled_flag
        writer.bit(false); // amp_enabled_flag
        writer.bit(false); // sample_adaptive_offset_enabled_flag
        writer.bit(fixture.pcm_luma_minus1.is_some());
        if let Some(pcm_luma_minus1) = fixture.pcm_luma_minus1 {
            writer.bits(u64::from(pcm_luma_minus1), 4);
            writer.bits(u64::from(fixture.pcm_chroma_minus1), 4);
            writer.ue(fixture.min_pcm_minus3);
            writer.ue(fixture.diff_pcm);
            writer.bit(false); // pcm_loop_filter_disabled_flag
        }
        writer.ue(u32::from(fixture.negative_refs != 0));
        if fixture.negative_refs != 0 {
            writer.ue(fixture.negative_refs);
            writer.ue(0);
            for _ in 0..fixture.negative_refs {
                writer.ue(fixture.negative_delta_minus1);
                writer.bit(false);
            }
        }
        writer.bit(false); // long_term_ref_pics_present_flag
        writer.bit(false); // sps_temporal_mvp_enabled_flag
        writer.bit(false); // strong_intra_smoothing_enabled_flag
        writer.bit(false); // vui_parameters_present_flag
        writer.bit(false); // sps_extension_present_flag
        writer.rbsp_trailing_bits();
    }

    fn fixture_sps(fixture: SuffixFixture) -> Vec<u8> {
        let mut writer = BitWriter::default();
        writer.bits(0, 4); // sps_video_parameter_set_id
        writer.bits(0, 3); // sps_max_sub_layers_minus1
        writer.bit(true); // sps_temporal_id_nesting_flag
        writer.bits(0, 2); // general_profile_space
        writer.bit(false); // general_tier_flag
        writer.bits(4, 5); // general_profile_idc
        writer.bits(0, 32); // general_profile_compatibility_flags
        writer.bits(0, 48); // general_constraint_indicator_flags
        writer.bits(120, 8); // general_level_idc
        writer.ue(0); // sps_seq_parameter_set_id
        writer.ue(3); // chroma_format_idc
        writer.bit(false); // separate_colour_plane_flag
        writer.ue(64);
        writer.ue(64);
        writer.bit(false); // conformance_window_flag
        writer.ue(0); // bit_depth_luma_minus8
        writer.ue(0); // bit_depth_chroma_minus8
        write_suffix(&mut writer, fixture);

        let rbsp = writer.into_bytes();
        let mut nal = vec![0x42, 0x01];
        let mut zero_count = 0usize;
        for byte in rbsp {
            if zero_count >= 2 && byte <= 3 {
                nal.push(3);
                zero_count = 0;
            }
            nal.push(byte);
            zero_count = if byte == 0 { zero_count + 1 } else { 0 };
        }
        nal
    }

    const MAIN_420_8BIT_1280X720_SPS: &[u8] = &[
        0x42, 0x01, 0x01, 0x01, 0x40, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x03, 0x00, 0x78, 0xa0, 0x02, 0x80, 0x80, 0x2d, 0x17, 0x7f, 0xc2, 0x08,
    ];

    const SUB_LAYER_PROFILE_SPS: &[u8] = &[
        0x42, 0x01, 0x03, 0x01, 0x40, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x03, 0x00, 0x78, 0x80, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0xa0, 0x88, 0x45, 0xdf, 0xf0, 0x82,
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
        for length in 2..CAPTURED_MAIN444_8BIT_SPS.len() {
            assert!(
                parse_hevc_sps(&CAPTURED_MAIN444_8BIT_SPS[..length]).is_err(),
                "truncated SPS prefix of {length} bytes must be rejected"
            );
        }
        assert_eq!(parse_hevc_sps(&[0x42, 0x01]), Err(HevcSpsError::Truncated));
    }

    #[test]
    fn rbsp_trailing_bits_and_extra_nonzero_data_are_rejected() {
        let mut missing_stop_bit = CAPTURED_MAIN444_8BIT_SPS.to_vec();
        *missing_stop_bit.last_mut().unwrap() = 0;
        assert!(parse_hevc_sps(&missing_stop_bit).is_err());

        let mut nonzero_after_stop_bit = CAPTURED_MAIN444_8BIT_SPS.to_vec();
        *nonzero_after_stop_bit.last_mut().unwrap() = 0x81;
        assert!(parse_hevc_sps(&nonzero_after_stop_bit).is_err());

        let mut extra_rbsp = CAPTURED_MAIN444_8BIT_SPS.to_vec();
        extra_rbsp.push(0x80);
        assert!(parse_hevc_sps(&extra_rbsp).is_err());
    }

    #[test]
    fn malformed_supported_suffix_ranges_are_rejected_table_driven() {
        let baseline = SuffixFixture::default();
        assert!(parse_hevc_sps(&fixture_sps(baseline)).is_ok());

        let cases = [
            (
                "decoded picture buffer exceeds the normative absolute maximum",
                SuffixFixture {
                    max_dec_pic_buffering_minus1: 16,
                    ..baseline
                },
            ),
            (
                "transform hierarchy exceeds CTB to minimum-TB depth",
                SuffixFixture {
                    hierarchy_inter: 4,
                    ..baseline
                },
            ),
            (
                "maximum transform block exceeds CTB",
                SuffixFixture {
                    diff_cb: 0,
                    diff_tb: 2,
                    hierarchy_inter: 1,
                    hierarchy_intra: 1,
                    ..baseline
                },
            ),
            (
                "PCM luma depth exceeds SPS bit depth",
                SuffixFixture {
                    pcm_luma_minus1: Some(8),
                    ..baseline
                },
            ),
            (
                "minimum PCM block is below minimum coding block",
                SuffixFixture {
                    min_cb_minus3: 2,
                    diff_cb: 0,
                    pcm_luma_minus1: Some(7),
                    ..baseline
                },
            ),
            (
                "short-term negative reference count exceeds DPB",
                SuffixFixture {
                    negative_refs: 16,
                    ..baseline
                },
            ),
            (
                "short-term delta_poc_minus1 exceeds 15 bits",
                SuffixFixture {
                    negative_refs: 1,
                    negative_delta_minus1: 1 << 15,
                    ..baseline
                },
            ),
        ];

        for (name, fixture) in cases {
            assert!(
                parse_hevc_sps(&fixture_sps(fixture)).is_err(),
                "{name} must be rejected"
            );
        }
    }

    #[test]
    fn scaling_list_size_three_uses_matrix_id_divided_by_three() {
        let mut writer = BitWriter::default();
        for size_id in 0..4usize {
            let step = if size_id == 3 { 3 } else { 1 };
            for matrix_id in (0..6usize).step_by(step) {
                writer.bit(false);
                writer.ue(if size_id == 3 && matrix_id == 3 { 2 } else { 0 });
            }
        }
        let bytes = writer.into_bytes();

        assert!(skip_scaling_list_data(&mut BitReader::new(&bytes)).is_err());
    }

    #[test]
    fn direct_short_term_delta_poc_range_is_bounded() {
        let mut writer = BitWriter::default();
        writer.ue(1); // num_negative_pics
        writer.ue(0); // num_positive_pics
        writer.ue(1 << 15); // delta_poc_s0_minus1
        writer.bit(false);
        let bytes = writer.into_bytes();

        assert!(skip_short_term_ref_pic_sets(&mut BitReader::new(&bytes), 1, 15).is_err());
    }

    #[test]
    fn inter_predicted_short_term_set_cannot_exceed_the_dpb() {
        let mut writer = BitWriter::default();
        writer.ue(15); // set zero: num_negative_pics
        writer.ue(0);
        for _ in 0..15 {
            writer.ue(0);
            writer.bit(false);
        }
        writer.bit(true); // set one: inter_ref_pic_set_prediction_flag
        writer.bit(true); // delta_rps_sign, so DeltaRps is -1
        writer.ue(0); // abs_delta_rps_minus1
        for _ in 0..=15 {
            writer.bit(true); // used_by_curr_pic_flag; selects all 16 derived entries
        }
        let bytes = writer.into_bytes();

        assert!(skip_short_term_ref_pic_sets(&mut BitReader::new(&bytes), 2, 15).is_err());
    }

    #[test]
    fn hrd_elemental_duration_and_sub_pic_ordering_are_bounded() {
        let mut invalid_duration = BitWriter::default();
        invalid_duration.bit(true); // nal_hrd_parameters_present_flag
        invalid_duration.bit(false); // vcl_hrd_parameters_present_flag
        invalid_duration.bit(false); // sub_pic_hrd_params_present_flag
        invalid_duration.bits(0, 4); // bit_rate_scale
        invalid_duration.bits(0, 4); // cpb_size_scale
        invalid_duration.bits(0, 5);
        invalid_duration.bits(0, 5);
        invalid_duration.bits(0, 5);
        invalid_duration.bit(true); // fixed_pic_rate_general_flag
        invalid_duration.ue(2048); // elemental_duration_in_tc_minus1
        invalid_duration.ue(0); // cpb_cnt_minus1
        invalid_duration.ue(0); // bit_rate_value_minus1
        invalid_duration.ue(0); // cpb_size_value_minus1
        invalid_duration.bit(false); // cbr_flag
        let invalid_duration = invalid_duration.into_bytes();
        assert!(
            skip_hrd_parameters(&mut BitReader::new(&invalid_duration), 0).is_err(),
            "elemental_duration_in_tc_minus1 above 2047 must be rejected"
        );

        let mut invalid_sub_pic_order = BitWriter::default();
        invalid_sub_pic_order.bit(true);
        invalid_sub_pic_order.bit(false);
        invalid_sub_pic_order.bit(true);
        invalid_sub_pic_order.bits(0, 8);
        invalid_sub_pic_order.bits(0, 5);
        invalid_sub_pic_order.bit(false);
        invalid_sub_pic_order.bits(0, 5);
        invalid_sub_pic_order.bits(0, 4); // bit_rate_scale
        invalid_sub_pic_order.bits(0, 4); // cpb_size_scale
        invalid_sub_pic_order.bits(0, 4); // cpb_size_du_scale
        invalid_sub_pic_order.bits(0, 5);
        invalid_sub_pic_order.bits(0, 5);
        invalid_sub_pic_order.bits(0, 5);
        invalid_sub_pic_order.bit(true);
        invalid_sub_pic_order.ue(0);
        invalid_sub_pic_order.ue(1); // cpb_cnt_minus1
        for cpb_size_du in [0, 1] {
            invalid_sub_pic_order.ue(0); // equal bit rates are invalid after entry zero
            invalid_sub_pic_order.ue(0);
            invalid_sub_pic_order.ue(cpb_size_du); // increasing CPB DU sizes are invalid
            invalid_sub_pic_order.ue(0); // equal DU bit rates are invalid after entry zero
            invalid_sub_pic_order.bit(false);
        }
        let invalid_sub_pic_order = invalid_sub_pic_order.into_bytes();
        assert!(
            skip_hrd_parameters(&mut BitReader::new(&invalid_sub_pic_order), 0).is_err(),
            "HRD and sub-picture value ordering must be validated"
        );
    }

    #[test]
    fn reserved_vui_video_format_is_rejected() {
        let mut reserved_video_format = CAPTURED_MAIN444_8BIT_SPS.to_vec();
        // RBSP bit 579..581 是当前 fixture 的 video_format；101(5) 改为 110(6)。
        reserved_video_format[76] = 0x3b;

        assert!(parse_hevc_sps(&reserved_video_format).is_err());
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
