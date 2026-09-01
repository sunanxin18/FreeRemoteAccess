//! 已认证 HEVC RTP 包的有界重排与访问单元组装。

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use crate::hevc_rtp::{HevcRtpDepacketizer, HevcRtpError, HevcRtpLimits};

const MAX_REORDER_WINDOW: usize = 256;
const DEFAULT_REORDER_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_ACCESS_UNIT_BYTES: usize = 32 * 1024 * 1024;

/// 已完成、可直接交给长度前缀 HEVC 解码器的一个访问单元。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HevcAccessUnit {
    pub generation: u64,
    pub ssrc: u32,
    pub timestamp: u32,
    pub keyframe: bool,
    pub parameter_sets_prepended: bool,
    pub data: Vec<u8>,
}

/// 一个已完成认证并去除 RTP header 的视频包。
#[derive(Debug, Clone, Copy)]
pub struct HevcRtpPacket<'a> {
    pub generation: u64,
    pub ssrc: u32,
    pub sequence: u16,
    pub timestamp: u32,
    pub marker: bool,
    pub payload: &'a [u8],
}

/// AU 组装器的显式内存和重排上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HevcAccessUnitLimits {
    /// 包含当前期望序号槽位在内的最大重排窗口，最大为 256。
    pub max_reorder_packets: usize,
    pub max_reorder_bytes: usize,
    pub max_access_unit_bytes: usize,
    pub rtp: HevcRtpLimits,
}

impl Default for HevcAccessUnitLimits {
    fn default() -> Self {
        Self {
            max_reorder_packets: MAX_REORDER_WINDOW,
            max_reorder_bytes: DEFAULT_REORDER_BYTES,
            max_access_unit_bytes: DEFAULT_ACCESS_UNIT_BYTES,
            rtp: HevcRtpLimits::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HevcAccessUnitError {
    InvalidReorderWindow { actual: usize, maximum: usize },
    StaleGeneration { expected: u64, actual: u64 },
    SsrcChanged { previous: u32, actual: u32 },
    TimestampChangedBeforeMarker { previous: u32, actual: u32 },
    ReorderWindowExceeded { limit: usize },
    ReorderPacketBudgetExceeded { limit: usize },
    ReorderByteBudgetExceeded { limit: usize },
    AccessUnitBudgetExceeded { limit: usize },
    MarkerBeforeFuCompletion,
    Depacketize(HevcRtpError),
}

impl fmt::Display for HevcAccessUnitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidReorderWindow { actual, maximum } => write!(
                formatter,
                "HEVC RTP 重排窗口 {actual} 非法，允许范围为 1..={maximum}"
            ),
            Self::StaleGeneration { expected, actual } => write!(
                formatter,
                "HEVC AU generation 已过期：当前 {expected}，收到 {actual}"
            ),
            Self::SsrcChanged { previous, actual } => write!(
                formatter,
                "HEVC RTP SSRC 在 AU 边界前变化：{previous:#010x} -> {actual:#010x}"
            ),
            Self::TimestampChangedBeforeMarker { previous, actual } => write!(
                formatter,
                "HEVC RTP timestamp 在 marker 前变化：{previous} -> {actual}"
            ),
            Self::ReorderWindowExceeded { limit } => {
                write!(formatter, "HEVC RTP 序列缺口超过重排窗口 {limit}")
            }
            Self::ReorderPacketBudgetExceeded { limit } => {
                write!(formatter, "HEVC RTP 重排包数超过资源上限 {limit}")
            }
            Self::ReorderByteBudgetExceeded { limit } => {
                write!(formatter, "HEVC RTP 重排数据超过资源上限 {limit} 字节")
            }
            Self::AccessUnitBudgetExceeded { limit } => {
                write!(formatter, "HEVC 访问单元超过资源上限 {limit} 字节")
            }
            Self::MarkerBeforeFuCompletion => {
                formatter.write_str("HEVC RTP marker 到达时 FU 仍未完成")
            }
            Self::Depacketize(error) => write!(formatter, "HEVC RTP 解包失败：{error}"),
        }
    }
}

impl Error for HevcAccessUnitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Depacketize(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct BufferedPacket {
    ssrc: u32,
    sequence: u16,
    timestamp: u32,
    marker: bool,
    payload: Vec<u8>,
}

#[derive(Debug)]
pub struct HevcAccessUnitAssembler {
    generation: u64,
    limits: HevcAccessUnitLimits,
    depacketizer: HevcRtpDepacketizer,
    ssrc: Option<u32>,
    expected_sequence: Option<u16>,
    buffered: HashMap<u16, BufferedPacket>,
    buffered_bytes: usize,
    timestamp: Option<u32>,
    nals: Vec<Vec<u8>>,
    access_unit_bytes: usize,
    parameter_sets: [Option<Vec<u8>>; 3],
    dropping_until_marker: bool,
}

impl HevcAccessUnitAssembler {
    pub fn new(generation: u64, limits: HevcAccessUnitLimits) -> Result<Self, HevcAccessUnitError> {
        if limits.max_reorder_packets == 0 || limits.max_reorder_packets > MAX_REORDER_WINDOW {
            return Err(HevcAccessUnitError::InvalidReorderWindow {
                actual: limits.max_reorder_packets,
                maximum: MAX_REORDER_WINDOW,
            });
        }
        Ok(Self {
            generation,
            limits,
            depacketizer: HevcRtpDepacketizer::new(generation, limits.rtp),
            ssrc: None,
            expected_sequence: None,
            buffered: HashMap::new(),
            buffered_bytes: 0,
            timestamp: None,
            nals: Vec::new(),
            access_unit_bytes: 0,
            parameter_sets: [None, None, None],
            dropping_until_marker: false,
        })
    }

    /// 切换 display generation，并丢弃所有重排、FU、AU 和参数集状态。
    pub fn reset(&mut self, generation: u64) {
        self.generation = generation;
        self.depacketizer.reset(generation);
        self.ssrc = None;
        self.expected_sequence = None;
        self.buffered.clear();
        self.buffered_bytes = 0;
        self.timestamp = None;
        self.nals.clear();
        self.access_unit_bytes = 0;
        self.parameter_sets = [None, None, None];
        self.dropping_until_marker = false;
    }

    /// 接收一个已认证 RTP 包；同一次调用可能排空重排队列并发布多个 AU。
    pub fn push(
        &mut self,
        packet: HevcRtpPacket<'_>,
    ) -> Result<Vec<HevcAccessUnit>, HevcAccessUnitError> {
        if packet.generation != self.generation {
            return Err(HevcAccessUnitError::StaleGeneration {
                expected: self.generation,
                actual: packet.generation,
            });
        }

        if let Some(previous) = self.ssrc {
            if previous != packet.ssrc {
                self.discard_stream(packet.ssrc, packet.sequence, packet.marker);
                return Err(HevcAccessUnitError::SsrcChanged {
                    previous,
                    actual: packet.ssrc,
                });
            }
        } else {
            self.ssrc = Some(packet.ssrc);
        }

        if self.dropping_until_marker {
            if packet.marker {
                self.dropping_until_marker = false;
                self.expected_sequence = Some(packet.sequence.wrapping_add(1));
                self.clear_reorder();
            }
            return Ok(Vec::new());
        }

        let expected = *self.expected_sequence.get_or_insert(packet.sequence);
        let distance = packet.sequence.wrapping_sub(expected);
        if distance >= 0x8000 {
            return Ok(Vec::new());
        }
        if usize::from(distance) >= self.limits.max_reorder_packets {
            self.drop_at_packet_boundary(packet.sequence, packet.marker);
            return Err(HevcAccessUnitError::ReorderWindowExceeded {
                limit: self.limits.max_reorder_packets,
            });
        }
        if self.buffered.contains_key(&packet.sequence) {
            return Ok(Vec::new());
        }
        if self.buffered.len() == self.limits.max_reorder_packets {
            self.drop_at_packet_boundary(packet.sequence, packet.marker);
            return Err(HevcAccessUnitError::ReorderPacketBudgetExceeded {
                limit: self.limits.max_reorder_packets,
            });
        }
        let next_buffered_bytes = self
            .buffered_bytes
            .checked_add(packet.payload.len())
            .filter(|length| *length <= self.limits.max_reorder_bytes)
            .ok_or_else(|| {
                self.drop_at_packet_boundary(packet.sequence, packet.marker);
                HevcAccessUnitError::ReorderByteBudgetExceeded {
                    limit: self.limits.max_reorder_bytes,
                }
            })?;
        self.buffered_bytes = next_buffered_bytes;
        self.buffered.insert(
            packet.sequence,
            BufferedPacket {
                ssrc: packet.ssrc,
                sequence: packet.sequence,
                timestamp: packet.timestamp,
                marker: packet.marker,
                payload: packet.payload.to_vec(),
            },
        );

        let mut output = Vec::new();
        loop {
            let expected = self.expected_sequence.expect("expected sequence 已初始化");
            let Some(packet) = self.buffered.remove(&expected) else {
                break;
            };
            self.buffered_bytes -= packet.payload.len();
            self.expected_sequence = Some(expected.wrapping_add(1));
            match self.process_ordered(packet) {
                Ok(Some(access_unit)) => output.push(access_unit),
                Ok(None) => {}
                Err(error) => {
                    self.clear_reorder();
                    return Err(error);
                }
            }
        }
        Ok(output)
    }

    fn process_ordered(
        &mut self,
        packet: BufferedPacket,
    ) -> Result<Option<HevcAccessUnit>, HevcAccessUnitError> {
        if let Some(previous) = self.timestamp {
            if previous != packet.timestamp {
                self.discard_access_unit();
                self.dropping_until_marker = !packet.marker;
                return Err(HevcAccessUnitError::TimestampChangedBeforeMarker {
                    previous,
                    actual: packet.timestamp,
                });
            }
        } else {
            self.timestamp = Some(packet.timestamp);
        }

        let packet_nals =
            match self
                .depacketizer
                .push(self.generation, packet.sequence, &packet.payload)
            {
                Ok(nals) => nals,
                Err(error) => {
                    self.discard_access_unit();
                    self.dropping_until_marker = !packet.marker;
                    return Err(HevcAccessUnitError::Depacketize(error));
                }
            };
        if packet.marker && packet_nals.is_empty() {
            self.discard_access_unit();
            self.dropping_until_marker = false;
            return Err(HevcAccessUnitError::MarkerBeforeFuCompletion);
        }

        for nal in packet_nals {
            let bytes = nal.into_bytes();
            let encoded_length = 4usize.checked_add(bytes.len()).ok_or(
                HevcAccessUnitError::AccessUnitBudgetExceeded {
                    limit: self.limits.max_access_unit_bytes,
                },
            )?;
            let next_length = self
                .access_unit_bytes
                .checked_add(encoded_length)
                .filter(|length| *length <= self.limits.max_access_unit_bytes)
                .ok_or_else(|| {
                    self.discard_access_unit();
                    self.dropping_until_marker = !packet.marker;
                    HevcAccessUnitError::AccessUnitBudgetExceeded {
                        limit: self.limits.max_access_unit_bytes,
                    }
                })?;
            self.access_unit_bytes = next_length;
            self.nals.push(bytes);
        }

        if !packet.marker {
            return Ok(None);
        }
        self.finish_access_unit(packet.ssrc, packet.timestamp)
            .map(Some)
    }

    fn finish_access_unit(
        &mut self,
        ssrc: u32,
        timestamp: u32,
    ) -> Result<HevcAccessUnit, HevcAccessUnitError> {
        let mut next_parameter_sets = self.parameter_sets.clone();
        for nal in &self.nals {
            if let Some(index) = parameter_set_index(nal) {
                next_parameter_sets[index] = Some(nal.clone());
            }
        }
        let parameter_set_bytes = match next_parameter_sets
            .iter()
            .flatten()
            .try_fold(0usize, |total, nal| total.checked_add(4 + nal.len()))
        {
            Some(length) => length,
            None => return Err(self.fail_access_unit_budget()),
        };
        if parameter_set_bytes > self.limits.max_access_unit_bytes {
            return Err(self.fail_access_unit_budget());
        }

        let keyframe = self
            .nals
            .iter()
            .any(|nal| matches!(nal_type(nal), Some(16..=23)));
        let mut prefixes = Vec::new();
        if keyframe {
            for (index, parameter_set) in next_parameter_sets.iter().enumerate() {
                if self
                    .nals
                    .iter()
                    .all(|nal| parameter_set_index(nal) != Some(index))
                {
                    if let Some(parameter_set) = parameter_set {
                        prefixes.push(parameter_set.clone());
                    }
                }
            }
        }
        let prefix_bytes = match prefixes
            .iter()
            .try_fold(0usize, |total, nal| total.checked_add(4 + nal.len()))
        {
            Some(length) => length,
            None => return Err(self.fail_access_unit_budget()),
        };
        let output_length = match prefix_bytes
            .checked_add(self.access_unit_bytes)
            .filter(|length| *length <= self.limits.max_access_unit_bytes)
        {
            Some(length) => length,
            None => return Err(self.fail_access_unit_budget()),
        };
        let parameter_sets_prepended = !prefixes.is_empty();
        let mut data = Vec::with_capacity(output_length);
        for nal in prefixes.iter().chain(self.nals.iter()) {
            append_length_prefixed(&mut data, nal);
        }

        self.parameter_sets = next_parameter_sets;
        self.timestamp = None;
        self.nals.clear();
        self.access_unit_bytes = 0;
        self.depacketizer.reset(self.generation);
        Ok(HevcAccessUnit {
            generation: self.generation,
            ssrc,
            timestamp,
            keyframe,
            parameter_sets_prepended,
            data,
        })
    }

    fn drop_at_packet_boundary(&mut self, sequence: u16, marker: bool) {
        self.discard_access_unit();
        self.clear_reorder();
        self.expected_sequence = Some(sequence.wrapping_add(1));
        self.dropping_until_marker = !marker;
    }

    fn discard_stream(&mut self, ssrc: u32, sequence: u16, marker: bool) {
        self.discard_access_unit();
        self.clear_reorder();
        self.ssrc = Some(ssrc);
        self.expected_sequence = Some(sequence.wrapping_add(1));
        self.dropping_until_marker = !marker;
        self.parameter_sets = [None, None, None];
    }

    fn discard_access_unit(&mut self) {
        self.timestamp = None;
        self.nals.clear();
        self.access_unit_bytes = 0;
        self.depacketizer.reset(self.generation);
    }

    fn fail_access_unit_budget(&mut self) -> HevcAccessUnitError {
        self.discard_access_unit();
        self.dropping_until_marker = false;
        HevcAccessUnitError::AccessUnitBudgetExceeded {
            limit: self.limits.max_access_unit_bytes,
        }
    }

    fn clear_reorder(&mut self) {
        self.buffered.clear();
        self.buffered_bytes = 0;
    }
}

fn nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|first| (first >> 1) & 0x3f)
}

fn parameter_set_index(nal: &[u8]) -> Option<usize> {
    match nal_type(nal)? {
        32 => Some(0),
        33 => Some(1),
        34 => Some(2),
        _ => None,
    }
}

fn append_length_prefixed(output: &mut Vec<u8>, nal: &[u8]) {
    let length = u32::try_from(nal.len()).expect("HEVC NAL 资源上限小于 u32");
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(nal);
}

#[cfg(test)]
mod tests {
    use super::{
        HevcAccessUnitAssembler, HevcAccessUnitError, HevcAccessUnitLimits, HevcRtpPacket,
    };

    const GENERATION: u64 = 9;
    const SSRC: u32 = 0x1122_3344;

    fn packet(sequence: u16, timestamp: u32, marker: bool, payload: &[u8]) -> HevcRtpPacket<'_> {
        HevcRtpPacket {
            generation: GENERATION,
            ssrc: SSRC,
            sequence,
            timestamp,
            marker,
            payload,
        }
    }

    fn assembler() -> HevcAccessUnitAssembler {
        HevcAccessUnitAssembler::new(GENERATION, HevcAccessUnitLimits::default()).unwrap()
    }

    #[test]
    fn out_of_order_packets_publish_only_after_the_gap_is_filled() {
        let mut assembler = assembler();
        assert!(assembler
            .push(packet(10, 1000, false, &[0x02, 0x01, 0xaa]))
            .unwrap()
            .is_empty());
        assert!(assembler
            .push(packet(12, 1000, true, &[0x02, 0x01, 0xcc]))
            .unwrap()
            .is_empty());

        let output = assembler
            .push(packet(11, 1000, false, &[0x02, 0x01, 0xbb]))
            .unwrap();

        assert_eq!(output.len(), 1);
        assert_eq!(output[0].timestamp, 1000);
        assert_eq!(output[0].ssrc, SSRC);
        assert!(!output[0].keyframe);
        assert_eq!(
            output[0].data,
            [
                0, 0, 0, 3, 0x02, 0x01, 0xaa, 0, 0, 0, 3, 0x02, 0x01, 0xbb, 0, 0, 0, 3, 0x02, 0x01,
                0xcc,
            ]
        );
    }

    #[test]
    fn sequence_wrap_is_ordered_modulo_sixteen_bits() {
        let mut assembler = assembler();
        assert!(assembler
            .push(packet(u16::MAX, 2000, false, &[0x02, 0x01, 1]))
            .unwrap()
            .is_empty());
        let output = assembler
            .push(packet(0, 2000, true, &[0x02, 0x01, 2]))
            .unwrap();

        assert_eq!(output.len(), 1);
        assert_eq!(
            output[0].data,
            [0, 0, 0, 3, 0x02, 0x01, 1, 0, 0, 0, 3, 0x02, 0x01, 2]
        );
    }

    #[test]
    fn complete_ap_and_fu_form_one_keyframe_access_unit() {
        let mut assembler = assembler();
        assert!(assembler
            .push(packet(
                20,
                3000,
                false,
                &[
                    0x60, 0x01, // AP
                    0, 3, 0x40, 0x01, 0xaa, // VPS
                    0, 3, 0x42, 0x01, 0xbb, // SPS
                    0, 3, 0x44, 0x01, 0xcc, // PPS
                ],
            ))
            .unwrap()
            .is_empty());
        assert!(assembler
            .push(packet(21, 3000, false, &[0x62, 0x01, 0x93, 0xdd]))
            .unwrap()
            .is_empty());
        let output = assembler
            .push(packet(22, 3000, true, &[0x62, 0x01, 0x53, 0xee]))
            .unwrap();

        assert_eq!(output.len(), 1);
        assert!(output[0].keyframe);
        assert!(!output[0].parameter_sets_prepended);
        assert_eq!(
            output[0].data,
            [
                0, 0, 0, 3, 0x40, 0x01, 0xaa, 0, 0, 0, 3, 0x42, 0x01, 0xbb, 0, 0, 0, 3, 0x44, 0x01,
                0xcc, 0, 0, 0, 4, 0x26, 0x01, 0xdd, 0xee,
            ]
        );
    }

    #[test]
    fn cached_parameter_sets_are_prepended_to_a_later_keyframe() {
        let mut assembler = assembler();
        let config = [
            0x60, 0x01, 0, 3, 0x40, 0x01, 1, 0, 3, 0x42, 0x01, 2, 0, 3, 0x44, 0x01, 3,
        ];
        assert_eq!(
            assembler
                .push(packet(30, 4000, true, &config))
                .unwrap()
                .len(),
            1
        );

        let output = assembler
            .push(packet(31, 5000, true, &[0x26, 0x01, 0xfe]))
            .unwrap();

        assert_eq!(output.len(), 1);
        assert!(output[0].keyframe);
        assert!(output[0].parameter_sets_prepended);
        assert_eq!(
            output[0].data,
            [
                0, 0, 0, 3, 0x40, 0x01, 1, 0, 0, 0, 3, 0x42, 0x01, 2, 0, 0, 0, 3, 0x44, 0x01, 3, 0,
                0, 0, 3, 0x26, 0x01, 0xfe,
            ]
        );
    }

    #[test]
    fn gap_beyond_reorder_window_drops_the_access_unit() {
        let mut assembler = HevcAccessUnitAssembler::new(
            GENERATION,
            HevcAccessUnitLimits {
                max_reorder_packets: 2,
                ..HevcAccessUnitLimits::default()
            },
        )
        .unwrap();
        assembler
            .push(packet(40, 6000, false, &[0x02, 0x01, 1]))
            .unwrap();

        assert_eq!(
            assembler.push(packet(43, 6000, true, &[0x02, 0x01, 2])),
            Err(HevcAccessUnitError::ReorderWindowExceeded { limit: 2 })
        );
        let output = assembler
            .push(packet(44, 7000, true, &[0x02, 0x01, 3]))
            .unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].timestamp, 7000);
    }

    #[test]
    fn timestamp_change_before_marker_drops_both_sides_of_the_boundary() {
        let mut assembler = assembler();
        assembler
            .push(packet(50, 9000, false, &[0x02, 0x01, 1]))
            .unwrap();
        assert_eq!(
            assembler.push(packet(51, 10000, false, &[0x02, 0x01, 2])),
            Err(HevcAccessUnitError::TimestampChangedBeforeMarker {
                previous: 9000,
                actual: 10000,
            })
        );
        assert!(assembler
            .push(packet(52, 10000, true, &[0x02, 0x01, 3]))
            .unwrap()
            .is_empty());
        assert_eq!(
            assembler
                .push(packet(53, 11000, true, &[0x02, 0x01, 4]))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn ssrc_change_and_generation_reset_discard_stream_state() {
        let mut assembler = assembler();
        assembler
            .push(packet(60, 12000, false, &[0x02, 0x01, 1]))
            .unwrap();
        let changed = HevcRtpPacket {
            ssrc: SSRC + 1,
            sequence: 61,
            timestamp: 13000,
            marker: true,
            ..packet(61, 13000, true, &[0x02, 0x01, 2])
        };
        assert_eq!(
            assembler.push(changed),
            Err(HevcAccessUnitError::SsrcChanged {
                previous: SSRC,
                actual: SSRC + 1,
            })
        );

        assembler.reset(GENERATION + 1);
        assert_eq!(
            assembler.push(packet(62, 14000, true, &[0x02, 0x01, 3])),
            Err(HevcAccessUnitError::StaleGeneration {
                expected: GENERATION + 1,
                actual: GENERATION,
            })
        );
        let fresh = HevcRtpPacket {
            generation: GENERATION + 1,
            sequence: 1,
            timestamp: 1,
            marker: true,
            payload: &[0x02, 0x01, 4],
            ssrc: SSRC,
        };
        assert_eq!(assembler.push(fresh).unwrap().len(), 1);
    }

    #[test]
    fn malformed_fu_and_access_unit_budget_never_publish_partial_frames() {
        let mut assembler = assembler();
        assembler
            .push(packet(70, 15000, false, &[0x62, 0x01, 0x93, 1]))
            .unwrap();
        assert!(matches!(
            assembler.push(packet(71, 15000, true, &[0x62, 0x01, 0x54, 2])),
            Err(HevcAccessUnitError::Depacketize(_))
        ));
        assert_eq!(
            assembler
                .push(packet(72, 16000, true, &[0x02, 0x01, 3]))
                .unwrap()
                .len(),
            1
        );

        let mut bounded = HevcAccessUnitAssembler::new(
            GENERATION,
            HevcAccessUnitLimits {
                max_access_unit_bytes: 8,
                ..HevcAccessUnitLimits::default()
            },
        )
        .unwrap();
        bounded
            .push(packet(80, 17000, false, &[0x02, 0x01, 1]))
            .unwrap();
        assert_eq!(
            bounded.push(packet(81, 17000, true, &[0x02, 0x01, 2])),
            Err(HevcAccessUnitError::AccessUnitBudgetExceeded { limit: 8 })
        );
    }

    #[test]
    fn reorder_packet_and_byte_budgets_are_enforced() {
        let mut assembler = HevcAccessUnitAssembler::new(
            GENERATION,
            HevcAccessUnitLimits {
                max_reorder_packets: 2,
                max_reorder_bytes: 6,
                ..HevcAccessUnitLimits::default()
            },
        )
        .unwrap();
        assembler
            .push(packet(90, 18000, false, &[0x02, 0x01, 0]))
            .unwrap();
        assert!(assembler
            .push(packet(92, 18000, false, &[0x02, 0x01, 2]))
            .unwrap()
            .is_empty());
        assert_eq!(
            assembler.push(packet(91, 18000, false, &[0x02, 0x01, 1, 1])),
            Err(HevcAccessUnitError::ReorderByteBudgetExceeded { limit: 6 })
        );
    }

    #[test]
    fn cached_parameter_set_budget_failure_discards_the_completed_au() {
        let mut assembler = HevcAccessUnitAssembler::new(
            GENERATION,
            HevcAccessUnitLimits {
                max_access_unit_bytes: 18,
                ..HevcAccessUnitLimits::default()
            },
        )
        .unwrap();
        let config = [
            0x60, 0x01, 0, 2, 0x40, 0x01, 0, 2, 0x42, 0x01, 0, 2, 0x44, 0x01,
        ];
        assert_eq!(
            assembler
                .push(packet(100, 19000, true, &config))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            assembler.push(packet(101, 20000, true, &[0x26, 0x01, 1])),
            Err(HevcAccessUnitError::AccessUnitBudgetExceeded { limit: 18 })
        );

        let output = assembler
            .push(packet(102, 21000, true, &[0x02, 0x01, 2]))
            .unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].timestamp, 21000);
    }
}
