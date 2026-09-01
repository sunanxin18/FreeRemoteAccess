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
const MAX_REFS_PER_SHORT_TERM_SET: u32 = 64;
const MAX_LONG_TERM_REFS: u32 = 32;
const MAX_HRD_CPB_CNT_MINUS1: u32 = 31;

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

    parse_sps_suffix(&mut bits, max_sub_layers_minus1)?;

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
    for _ in first_layer..=max_sub_layers_minus1 {
        let max_dec_pic_buffering_minus1 = bits.read_ue()?;
        validate_max(
            "sps_max_dec_pic_buffering_minus1",
            max_dec_pic_buffering_minus1,
            MAX_REFS_PER_SHORT_TERM_SET,
        )?;
        let max_num_reorder_pics = bits.read_ue()?;
        if max_num_reorder_pics > max_dec_pic_buffering_minus1 {
            return Err(HevcSpsError::InvalidRange {
                field: "sps_max_num_reorder_pics",
                value: max_num_reorder_pics,
                maximum: max_dec_pic_buffering_minus1,
            });
        }
        bits.read_ue()?; // sps_max_latency_increase_plus1
    }

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
    let log2_min_luma_transform_block_size_minus2 = bits.read_ue()?;
    validate_max(
        "log2_min_luma_transform_block_size_minus2",
        log2_min_luma_transform_block_size_minus2,
        3,
    )?;
    let log2_diff_max_min_luma_transform_block_size = bits.read_ue()?;
    validate_sum_max(
        "log2_diff_max_min_luma_transform_block_size",
        log2_min_luma_transform_block_size_minus2,
        log2_diff_max_min_luma_transform_block_size,
        3,
    )?;
    validate_max("max_transform_hierarchy_depth_inter", bits.read_ue()?, 32)?;
    validate_max("max_transform_hierarchy_depth_intra", bits.read_ue()?, 32)?;

    if bits.read_bit()? && bits.read_bit()? {
        skip_scaling_list_data(bits)?;
    }
    bits.read_bit()?; // amp_enabled_flag
    bits.read_bit()?; // sample_adaptive_offset_enabled_flag
    if bits.read_bit()? {
        bits.read_bits(4)?; // pcm_sample_bit_depth_luma_minus1
        bits.read_bits(4)?; // pcm_sample_bit_depth_chroma_minus1
        let log2_min_pcm_luma_coding_block_size_minus3 = bits.read_ue()?;
        validate_max(
            "log2_min_pcm_luma_coding_block_size_minus3",
            log2_min_pcm_luma_coding_block_size_minus3,
            2,
        )?;
        let log2_diff_max_min_pcm_luma_coding_block_size = bits.read_ue()?;
        validate_sum_max(
            "log2_diff_max_min_pcm_luma_coding_block_size",
            log2_min_pcm_luma_coding_block_size_minus3,
            log2_diff_max_min_pcm_luma_coding_block_size,
            2,
        )?;
        bits.read_bit()?; // pcm_loop_filter_disabled_flag
    }

    let num_short_term_ref_pic_sets = bits.read_ue()?;
    validate_max(
        "num_short_term_ref_pic_sets",
        num_short_term_ref_pic_sets,
        MAX_SHORT_TERM_REF_PIC_SETS,
    )?;
    skip_short_term_ref_pic_sets(bits, num_short_term_ref_pic_sets)?;

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
                validate_max("scaling_list_pred_matrix_id_delta", delta, matrix_id as u32)?;
                continue;
            }
            if size_id > 1 {
                let dc_minus8 = bits.read_se()?;
                if !(-7..=247).contains(&dc_minus8) {
                    return Err(HevcSpsError::InvalidRange {
                        field: "scaling_list_dc_coef_minus8",
                        value: dc_minus8.unsigned_abs(),
                        maximum: 247,
                    });
                }
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
            }
        }
    }
    Ok(())
}

fn skip_short_term_ref_pic_sets(bits: &mut BitReader<'_>, count: u32) -> Result<(), HevcSpsError> {
    let mut delta_poc_counts = [0u32; MAX_SHORT_TERM_REF_PIC_SETS as usize];
    for index in 0..count as usize {
        let inter_prediction = index != 0 && bits.read_bit()?;
        let delta_poc_count = if inter_prediction {
            bits.read_bit()?; // delta_rps_sign
            bits.read_ue()?; // abs_delta_rps_minus1
            let reference_count = delta_poc_counts[index - 1];
            let mut used_count = 0u32;
            for _ in 0..=reference_count {
                let used_by_current = bits.read_bit()?;
                let use_delta = used_by_current || bits.read_bit()?;
                if use_delta {
                    used_count += 1;
                }
            }
            used_count
        } else {
            let negative = bits.read_ue()?;
            let positive = bits.read_ue()?;
            let total = negative
                .checked_add(positive)
                .ok_or(HevcSpsError::ExpGolombOverflow)?;
            validate_max("NumDeltaPocs", total, MAX_REFS_PER_SHORT_TERM_SET)?;
            for _ in 0..negative {
                bits.read_ue()?;
                bits.read_bit()?;
            }
            for _ in 0..positive {
                bits.read_ue()?;
                bits.read_bit()?;
            }
            total
        };
        validate_max("NumDeltaPocs", delta_poc_count, MAX_REFS_PER_SHORT_TERM_SET)?;
        delta_poc_counts[index] = delta_poc_count;
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
    if nal_hrd_parameters_present || vcl_hrd_parameters_present {
        sub_pic_hrd_params_present = bits.read_bit()?;
        if sub_pic_hrd_params_present {
            bits.read_bits(8)?;
            bits.read_bits(5)?;
            bits.read_bit()?;
            bits.read_bits(5)?;
        }
        bits.read_bits(4)?;
        bits.read_bits(4)?;
        if sub_pic_hrd_params_present {
            bits.read_bits(4)?;
        }
        bits.read_bits(5)?;
        bits.read_bits(5)?;
        bits.read_bits(5)?;
    }

    for _ in 0..=max_sub_layers_minus1 {
        let fixed_pic_rate_general = bits.read_bit()?;
        let fixed_pic_rate_within_cvs = fixed_pic_rate_general || bits.read_bit()?;
        let low_delay_hrd = if fixed_pic_rate_within_cvs {
            bits.read_ue()?;
            false
        } else {
            bits.read_bit()?
        };
        let cpb_cnt_minus1 = if low_delay_hrd { 0 } else { bits.read_ue()? };
        validate_max("cpb_cnt_minus1", cpb_cnt_minus1, MAX_HRD_CPB_CNT_MINUS1)?;
        if nal_hrd_parameters_present {
            skip_sub_layer_hrd_parameters(bits, cpb_cnt_minus1, sub_pic_hrd_params_present)?;
        }
        if vcl_hrd_parameters_present {
            skip_sub_layer_hrd_parameters(bits, cpb_cnt_minus1, sub_pic_hrd_params_present)?;
        }
    }
    Ok(())
}

fn skip_sub_layer_hrd_parameters(
    bits: &mut BitReader<'_>,
    cpb_cnt_minus1: u32,
    sub_pic_hrd_params_present: bool,
) -> Result<(), HevcSpsError> {
    for _ in 0..=cpb_cnt_minus1 {
        bits.read_ue()?;
        bits.read_ue()?;
        if sub_pic_hrd_params_present {
            bits.read_ue()?;
            bits.read_ue()?;
        }
        bits.read_bit()?;
    }
    Ok(())
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
