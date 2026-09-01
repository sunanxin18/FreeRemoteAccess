//! HEVC RTP payload 解包。

use std::error::Error;
use std::fmt;

const HEVC_AP_NAL_TYPE: u8 = 48;
const HEVC_FU_NAL_TYPE: u8 = 49;
const DEFAULT_RESOURCE_BUDGET: usize = 32 * 1024 * 1024;

/// HEVC RTP 解包的显式资源上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HevcRtpLimits {
    pub max_nal_bytes: usize,
    pub max_nals_per_packet: usize,
    pub max_payload_bytes: usize,
}

impl Default for HevcRtpLimits {
    fn default() -> Self {
        Self {
            max_nal_bytes: DEFAULT_RESOURCE_BUDGET,
            max_nals_per_packet: 1024,
            max_payload_bytes: DEFAULT_RESOURCE_BUDGET,
        }
    }
}

/// 一个已去除 RTP payload 封装的完整 HEVC NAL 单元。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HevcNal {
    bytes: Vec<u8>,
}

impl HevcNal {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// 转换为 Apple 解码链使用的 4 字节大端长度前缀格式。
    pub fn to_length_prefixed(&self) -> Vec<u8> {
        let length = u32::try_from(self.bytes.len()).expect("HEVC NAL 资源上限小于 u32");
        let mut output = Vec::with_capacity(4 + self.bytes.len());
        output.extend_from_slice(&length.to_be_bytes());
        output.extend_from_slice(&self.bytes);
        output
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HevcRtpError {
    StaleGeneration { expected: u64, actual: u64 },
    Truncated,
    InvalidNalHeader,
    UnsupportedNalType(u8),
    ZeroLengthAggregateNal,
    AggregateNalCountExceeded { limit: usize },
    PayloadBudgetExceeded { limit: usize },
    NalBudgetExceeded { limit: usize },
    FuStartAndEnd,
    ReservedFuType(u8),
    FuContinuationWithoutStart,
    InterleavedPayload,
    FuSequenceGap { expected: u16, actual: u16 },
    FuHeaderChanged,
    FuTypeChanged { expected: u8, actual: u8 },
}

impl fmt::Display for HevcRtpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleGeneration { expected, actual } => write!(
                formatter,
                "HEVC RTP generation 已过期：当前 {expected}，收到 {actual}"
            ),
            Self::Truncated => formatter.write_str("HEVC RTP payload 被截断"),
            Self::InvalidNalHeader => formatter.write_str("HEVC NAL header 非法"),
            Self::UnsupportedNalType(nal_type) => {
                write!(formatter, "HEVC RTP 不支持 NAL 类型 {nal_type}")
            }
            Self::ZeroLengthAggregateNal => formatter.write_str("HEVC AP 包含零长度 NAL"),
            Self::AggregateNalCountExceeded { limit } => {
                write!(formatter, "HEVC AP 的 NAL 数量超过资源上限 {limit}")
            }
            Self::PayloadBudgetExceeded { limit } => {
                write!(formatter, "HEVC RTP payload 超过资源上限 {limit} 字节")
            }
            Self::NalBudgetExceeded { limit } => {
                write!(formatter, "HEVC NAL 超过资源上限 {limit} 字节")
            }
            Self::FuStartAndEnd => formatter.write_str("HEVC FU 同时设置开始和结束标志"),
            Self::ReservedFuType(nal_type) => {
                write!(formatter, "HEVC FU 使用了保留类型 {nal_type}")
            }
            Self::FuContinuationWithoutStart => {
                formatter.write_str("HEVC FU continuation 缺少开始片")
            }
            Self::InterleavedPayload => formatter.write_str("HEVC FU 尚未完成时收到交错 payload"),
            Self::FuSequenceGap { expected, actual } => write!(
                formatter,
                "HEVC FU 序列号不连续：预期 {expected}，收到 {actual}"
            ),
            Self::FuHeaderChanged => formatter.write_str("HEVC FU 分片 header 发生变化"),
            Self::FuTypeChanged { expected, actual } => write!(
                formatter,
                "HEVC FU 类型发生变化：预期 {expected}，收到 {actual}"
            ),
        }
    }
}

impl Error for HevcRtpError {}

#[derive(Debug)]
struct FuState {
    indicator_header: [u8; 2],
    nal_type: u8,
    next_sequence: u16,
    bytes: Vec<u8>,
}

/// 对单一 display generation 的 HEVC RTP payload 进行有状态解包。
///
/// RTP timestamp 与 marker 由上层访问单元组装器处理；本类型只在一个包
/// 中返回零个或多个完整 NAL，进行中的 FU 返回空列表。
#[derive(Debug)]
pub struct HevcRtpDepacketizer {
    generation: u64,
    limits: HevcRtpLimits,
    fu: Option<FuState>,
}

impl HevcRtpDepacketizer {
    pub fn new(generation: u64, limits: HevcRtpLimits) -> Self {
        Self {
            generation,
            limits,
            fu: None,
        }
    }

    pub fn reset(&mut self, generation: u64) {
        self.generation = generation;
        self.fu = None;
    }

    pub fn push(
        &mut self,
        generation: u64,
        sequence_number: u16,
        payload: &[u8],
    ) -> Result<Vec<HevcNal>, HevcRtpError> {
        if generation != self.generation {
            return Err(HevcRtpError::StaleGeneration {
                expected: self.generation,
                actual: generation,
            });
        }
        if payload.len() > self.limits.max_payload_bytes {
            self.fu = None;
            return Err(HevcRtpError::PayloadBudgetExceeded {
                limit: self.limits.max_payload_bytes,
            });
        }
        if payload.len() < 2 {
            self.fu = None;
            return Err(HevcRtpError::Truncated);
        }
        if !valid_nal_header(payload[0], payload[1]) {
            self.fu = None;
            return Err(HevcRtpError::InvalidNalHeader);
        }

        let nal_type = nal_type(payload[0]);
        match nal_type {
            HEVC_AP_NAL_TYPE => self.push_aggregation(payload),
            HEVC_FU_NAL_TYPE => self.push_fragment(sequence_number, payload),
            50..=63 => {
                self.fu = None;
                Err(HevcRtpError::UnsupportedNalType(nal_type))
            }
            _ => self.push_single(payload),
        }
    }

    fn push_single(&mut self, payload: &[u8]) -> Result<Vec<HevcNal>, HevcRtpError> {
        if self.fu.take().is_some() {
            return Err(HevcRtpError::InterleavedPayload);
        }
        self.check_nal_budget(payload.len())?;
        Ok(vec![HevcNal {
            bytes: payload.to_vec(),
        }])
    }

    fn push_aggregation(&mut self, payload: &[u8]) -> Result<Vec<HevcNal>, HevcRtpError> {
        if self.fu.take().is_some() {
            return Err(HevcRtpError::InterleavedPayload);
        }

        let mut cursor = 2;
        let mut output = Vec::new();
        while cursor < payload.len() {
            if payload.len() - cursor < 2 {
                return Err(HevcRtpError::Truncated);
            }
            let length = usize::from(u16::from_be_bytes([payload[cursor], payload[cursor + 1]]));
            cursor += 2;
            if length == 0 {
                return Err(HevcRtpError::ZeroLengthAggregateNal);
            }
            if output.len() == self.limits.max_nals_per_packet {
                return Err(HevcRtpError::AggregateNalCountExceeded {
                    limit: self.limits.max_nals_per_packet,
                });
            }
            self.check_nal_budget(length)?;
            let end = cursor.checked_add(length).ok_or(HevcRtpError::Truncated)?;
            let bytes = payload.get(cursor..end).ok_or(HevcRtpError::Truncated)?;
            if bytes.len() < 2 || !valid_nal_header(bytes[0], bytes[1]) {
                return Err(HevcRtpError::InvalidNalHeader);
            }
            let member_type = nal_type(bytes[0]);
            if member_type > 47 {
                return Err(HevcRtpError::UnsupportedNalType(member_type));
            }
            output.push(HevcNal {
                bytes: bytes.to_vec(),
            });
            cursor = end;
        }

        if output.is_empty() {
            return Err(HevcRtpError::Truncated);
        }
        Ok(output)
    }

    fn push_fragment(
        &mut self,
        sequence_number: u16,
        payload: &[u8],
    ) -> Result<Vec<HevcNal>, HevcRtpError> {
        if payload.len() < 4 {
            self.fu = None;
            return Err(HevcRtpError::Truncated);
        }

        let fu_header = payload[2];
        let start = fu_header & 0x80 != 0;
        let end = fu_header & 0x40 != 0;
        let fragment_type = fu_header & 0x3f;
        if start && end {
            self.fu = None;
            return Err(HevcRtpError::FuStartAndEnd);
        }
        if fragment_type > 47 {
            self.fu = None;
            return Err(HevcRtpError::ReservedFuType(fragment_type));
        }

        if start {
            if self.fu.take().is_some() {
                return Err(HevcRtpError::InterleavedPayload);
            }
            let initial_length = 2 + payload.len() - 3;
            self.check_nal_budget(initial_length)?;
            let reconstructed_header = (payload[0] & 0x81) | (fragment_type << 1);
            let mut bytes = Vec::with_capacity(initial_length);
            bytes.extend_from_slice(&[reconstructed_header, payload[1]]);
            bytes.extend_from_slice(&payload[3..]);
            self.fu = Some(FuState {
                indicator_header: [payload[0], payload[1]],
                nal_type: fragment_type,
                next_sequence: sequence_number.wrapping_add(1),
                bytes,
            });
            return Ok(Vec::new());
        }

        let mut state = self
            .fu
            .take()
            .ok_or(HevcRtpError::FuContinuationWithoutStart)?;
        if sequence_number != state.next_sequence {
            return Err(HevcRtpError::FuSequenceGap {
                expected: state.next_sequence,
                actual: sequence_number,
            });
        }
        if [payload[0], payload[1]] != state.indicator_header {
            return Err(HevcRtpError::FuHeaderChanged);
        }
        if fragment_type != state.nal_type {
            return Err(HevcRtpError::FuTypeChanged {
                expected: state.nal_type,
                actual: fragment_type,
            });
        }
        let next_length = state.bytes.len().checked_add(payload.len() - 3).ok_or(
            HevcRtpError::NalBudgetExceeded {
                limit: self.limits.max_nal_bytes,
            },
        )?;
        self.check_nal_budget(next_length)?;
        state.bytes.extend_from_slice(&payload[3..]);
        state.next_sequence = sequence_number.wrapping_add(1);

        if end {
            Ok(vec![HevcNal { bytes: state.bytes }])
        } else {
            self.fu = Some(state);
            Ok(Vec::new())
        }
    }

    fn check_nal_budget(&self, length: usize) -> Result<(), HevcRtpError> {
        if length > self.limits.max_nal_bytes {
            return Err(HevcRtpError::NalBudgetExceeded {
                limit: self.limits.max_nal_bytes,
            });
        }
        Ok(())
    }
}

fn nal_type(first_header_byte: u8) -> u8 {
    (first_header_byte >> 1) & 0x3f
}

fn valid_nal_header(first_header_byte: u8, second_header_byte: u8) -> bool {
    first_header_byte & 0x80 == 0 && second_header_byte & 0x07 != 0
}

#[cfg(test)]
mod tests {
    use super::{HevcNal, HevcRtpDepacketizer, HevcRtpError, HevcRtpLimits};

    const GENERATION: u64 = 7;

    fn decoder() -> HevcRtpDepacketizer {
        HevcRtpDepacketizer::new(GENERATION, HevcRtpLimits::default())
    }

    fn bytes(nal: &HevcNal) -> &[u8] {
        nal.as_bytes()
    }

    #[test]
    fn single_nal_is_published_without_the_rtp_payload_wrapper() {
        let mut decoder = decoder();
        let output = decoder.push(GENERATION, 10, &[0x40, 0x01, 0xaa]).unwrap();

        assert_eq!(output.len(), 1);
        assert_eq!(bytes(&output[0]), &[0x40, 0x01, 0xaa]);
        assert_eq!(
            output[0].to_length_prefixed(),
            [0, 0, 0, 3, 0x40, 0x01, 0xaa]
        );
    }

    #[test]
    fn aggregation_packet_publishes_each_length_delimited_nal() {
        let mut decoder = decoder();
        let output = decoder
            .push(
                GENERATION,
                11,
                &[
                    0x60, 0x01, // AP header (type 48)
                    0x00, 0x03, 0x40, 0x01, 0xaa, // first NAL
                    0x00, 0x04, 0x02, 0x01, 0xbb, 0xcc, // second NAL
                ],
            )
            .unwrap();

        assert_eq!(output.len(), 2);
        assert_eq!(bytes(&output[0]), &[0x40, 0x01, 0xaa]);
        assert_eq!(bytes(&output[1]), &[0x02, 0x01, 0xbb, 0xcc]);
    }

    #[test]
    fn single_nal_path_rejects_paci_and_reserved_or_unspecified_types() {
        for nal_type in 50..=63 {
            let mut decoder = decoder();
            let payload = [(nal_type << 1), 0x01, 0xaa];
            assert!(
                decoder.push(GENERATION, 12, &payload).is_err(),
                "single-NAL type {nal_type} must be rejected"
            );
        }
    }

    #[test]
    fn aggregation_rejects_every_packetization_or_reserved_member_transactionally() {
        for nal_type in 48..=63 {
            let mut decoder = decoder();
            let payload = [
                0x60,
                0x01,
                0x00,
                0x03,
                0x40,
                0x01,
                0xaa,
                0x00,
                0x03,
                nal_type << 1,
                0x01,
                0xbb,
            ];
            assert!(
                decoder.push(GENERATION, 13, &payload).is_err(),
                "AP member type {nal_type} must be rejected"
            );
            let output = decoder.push(GENERATION, 14, &[0x02, 0x01, 0xcc]).unwrap();
            assert_eq!(output.len(), 1);
            assert_eq!(bytes(&output[0]), &[0x02, 0x01, 0xcc]);
        }
    }

    #[test]
    fn fragmentation_rejects_every_packetization_or_reserved_reconstructed_type() {
        for nal_type in 48..=63 {
            let mut decoder = decoder();
            let payload = [0x62, 0x01, 0x80 | nal_type, 0xaa];
            assert_eq!(
                decoder.push(GENERATION, 15, &payload),
                Err(HevcRtpError::ReservedFuType(nal_type))
            );
        }
    }

    #[test]
    fn fragmentation_unit_reassembles_one_nal_across_sequence_wrap() {
        let mut decoder = decoder();

        assert!(decoder
            .push(GENERATION, u16::MAX, &[0x62, 0x01, 0xa0, 0xaa])
            .unwrap()
            .is_empty());
        let output = decoder
            .push(GENERATION, 0, &[0x62, 0x01, 0x60, 0xbb, 0xcc])
            .unwrap();

        assert_eq!(output.len(), 1);
        assert_eq!(bytes(&output[0]), &[0x40, 0x01, 0xaa, 0xbb, 0xcc]);
    }

    #[test]
    fn malformed_aggregation_packets_are_rejected_transactionally() {
        let cases = [
            (&[0x60, 0x01, 0x00][..], HevcRtpError::Truncated),
            (
                &[0x60, 0x01, 0x00, 0x00][..],
                HevcRtpError::ZeroLengthAggregateNal,
            ),
            (
                &[0x60, 0x01, 0x00, 0x04, 0x40, 0x01, 0xaa][..],
                HevcRtpError::Truncated,
            ),
        ];

        for (payload, expected) in cases {
            let mut decoder = decoder();
            assert_eq!(decoder.push(GENERATION, 12, payload), Err(expected));
        }
    }

    #[test]
    fn malformed_fu_headers_are_rejected() {
        let cases = [
            (&[0x62, 0x01][..], HevcRtpError::Truncated),
            (
                &[0xe2, 0x01, 0xa0, 0xaa][..],
                HevcRtpError::InvalidNalHeader,
            ),
            (
                &[0x62, 0x00, 0xa0, 0xaa][..],
                HevcRtpError::InvalidNalHeader,
            ),
            (&[0x62, 0x01, 0xe0, 0xaa][..], HevcRtpError::FuStartAndEnd),
            (
                &[0x62, 0x01, 0xb0, 0xaa][..],
                HevcRtpError::ReservedFuType(48),
            ),
            (
                &[0x62, 0x01, 0xb1, 0xaa][..],
                HevcRtpError::ReservedFuType(49),
            ),
        ];

        for (payload, expected) in cases {
            let mut decoder = decoder();
            assert_eq!(decoder.push(GENERATION, 13, payload), Err(expected));
        }
    }

    #[test]
    fn continuation_without_start_and_interleaved_payloads_are_rejected() {
        let mut decoder = decoder();
        assert_eq!(
            decoder.push(GENERATION, 20, &[0x62, 0x01, 0x20, 0xaa]),
            Err(HevcRtpError::FuContinuationWithoutStart)
        );

        decoder
            .push(GENERATION, 21, &[0x62, 0x01, 0xa0, 0xaa])
            .unwrap();
        assert_eq!(
            decoder.push(GENERATION, 22, &[0x62, 0x01, 0xa0, 0xbb]),
            Err(HevcRtpError::InterleavedPayload)
        );

        decoder
            .push(GENERATION, 23, &[0x62, 0x01, 0xa0, 0xaa])
            .unwrap();
        assert_eq!(
            decoder.push(GENERATION, 24, &[0x40, 0x01, 0xbb]),
            Err(HevcRtpError::InterleavedPayload)
        );
        assert_eq!(
            decoder.push(GENERATION, 25, &[0x62, 0x01, 0x60, 0xcc]),
            Err(HevcRtpError::FuContinuationWithoutStart)
        );
    }

    #[test]
    fn fu_sequence_gap_and_type_change_discard_the_partial_nal() {
        let mut decoder = decoder();
        decoder
            .push(GENERATION, 30, &[0x62, 0x01, 0xa0, 0xaa])
            .unwrap();
        assert_eq!(
            decoder.push(GENERATION, 32, &[0x62, 0x01, 0x60, 0xbb]),
            Err(HevcRtpError::FuSequenceGap {
                expected: 31,
                actual: 32,
            })
        );

        decoder
            .push(GENERATION, 33, &[0x62, 0x01, 0xa0, 0xaa])
            .unwrap();
        assert_eq!(
            decoder.push(GENERATION, 34, &[0x62, 0x01, 0x61, 0xbb]),
            Err(HevcRtpError::FuTypeChanged {
                expected: 32,
                actual: 33,
            })
        );
        assert_eq!(
            decoder.push(GENERATION, 35, &[0x62, 0x01, 0x60, 0xcc]),
            Err(HevcRtpError::FuContinuationWithoutStart)
        );
    }

    #[test]
    fn reset_changes_generation_and_discards_in_progress_fu() {
        let mut decoder = decoder();
        decoder
            .push(GENERATION, 40, &[0x62, 0x01, 0xa0, 0xaa])
            .unwrap();
        decoder.reset(GENERATION + 1);

        assert_eq!(
            decoder.push(GENERATION, 41, &[0x62, 0x01, 0x60, 0xbb]),
            Err(HevcRtpError::StaleGeneration {
                expected: GENERATION + 1,
                actual: GENERATION,
            })
        );
        assert_eq!(
            decoder.push(GENERATION + 1, 41, &[0x62, 0x01, 0x60, 0xbb]),
            Err(HevcRtpError::FuContinuationWithoutStart)
        );
    }

    #[test]
    fn nal_budget_rejects_single_aggregate_and_fragmented_nals() {
        let limits = HevcRtpLimits {
            max_nal_bytes: 4,
            max_nals_per_packet: 2,
            max_payload_bytes: 32,
        };

        let mut decoder = HevcRtpDepacketizer::new(GENERATION, limits);
        assert_eq!(
            decoder.push(GENERATION, 50, &[0x40, 0x01, 1, 2, 3]),
            Err(HevcRtpError::NalBudgetExceeded { limit: 4 })
        );

        assert_eq!(
            decoder.push(
                GENERATION,
                51,
                &[0x60, 0x01, 0x00, 0x05, 0x40, 0x01, 1, 2, 3],
            ),
            Err(HevcRtpError::NalBudgetExceeded { limit: 4 })
        );

        decoder
            .push(GENERATION, 52, &[0x62, 0x01, 0xa0, 1, 2])
            .unwrap();
        assert_eq!(
            decoder.push(GENERATION, 53, &[0x62, 0x01, 0x60, 3]),
            Err(HevcRtpError::NalBudgetExceeded { limit: 4 })
        );

        let mut decoder = HevcRtpDepacketizer::new(
            GENERATION,
            HevcRtpLimits {
                max_payload_bytes: 3,
                ..limits
            },
        );
        assert_eq!(
            decoder.push(GENERATION, 54, &[0x40, 0x01, 1, 2]),
            Err(HevcRtpError::PayloadBudgetExceeded { limit: 3 })
        );

        let mut decoder = HevcRtpDepacketizer::new(
            GENERATION,
            HevcRtpLimits {
                max_nals_per_packet: 1,
                ..limits
            },
        );
        assert_eq!(
            decoder.push(
                GENERATION,
                55,
                &[0x60, 0x01, 0x00, 0x02, 0x40, 0x01, 0x00, 0x02, 0x02, 0x01,],
            ),
            Err(HevcRtpError::AggregateNalCountExceeded { limit: 1 })
        );
    }
}
