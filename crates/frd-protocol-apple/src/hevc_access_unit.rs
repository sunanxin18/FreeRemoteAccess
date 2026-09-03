//! 已认证 HEVC RTP 包的有界重排与访问单元组装。

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::time::Instant;

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
    pub local_ingress_at: Instant,
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
    MissingInitialParameterSets,
    MalformedLengthPrefixedAccessUnit,
    EmptyNalUnit,
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
            Self::MissingInitialParameterSets => {
                formatter.write_str("HEVC 初始访问单元缺少 VPS/SPS/PPS")
            }
            Self::MalformedLengthPrefixedAccessUnit => {
                formatter.write_str("HEVC 访问单元 length-prefix 越界")
            }
            Self::EmptyNalUnit => formatter.write_str("HEVC 访问单元包含空 NAL"),
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
    local_ingress_at: Instant,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitialConfigurationState {
    AwaitingConfigurationAu,
    AwaitingRecoveryIrap,
    Configured,
}

#[cfg(any(debug_assertions, test))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct HevcAssemblerDiagnostics {
    pub(crate) complete_configuration_access_units: u64,
    pub(crate) reorder_window_exceeded: u64,
    pub(crate) recovery_marker_resyncs: u64,
    pub(crate) waiting_for_recovery_irap_drops: u64,
    pub(crate) completed_access_units: u64,
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
    local_ingress_at: Option<Instant>,
    nals: Vec<Vec<u8>>,
    access_unit_bytes: usize,
    parameter_sets: [Option<Vec<u8>>; 3],
    initial_configuration: InitialConfigurationState,
    complete_initial_ap_parameter_sets: Option<[Vec<u8>; 3]>,
    dropping_until_marker: bool,
    recovery_sequence: Option<u16>,
    #[cfg(any(debug_assertions, test))]
    diagnostics: HevcAssemblerDiagnostics,
    #[cfg(any(debug_assertions, test))]
    access_unit_has_complete_configuration: bool,
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
            local_ingress_at: None,
            nals: Vec::new(),
            access_unit_bytes: 0,
            parameter_sets: [None, None, None],
            initial_configuration: InitialConfigurationState::AwaitingConfigurationAu,
            complete_initial_ap_parameter_sets: None,
            dropping_until_marker: false,
            recovery_sequence: None,
            #[cfg(any(debug_assertions, test))]
            diagnostics: HevcAssemblerDiagnostics::default(),
            #[cfg(any(debug_assertions, test))]
            access_unit_has_complete_configuration: false,
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
        self.local_ingress_at = None;
        self.nals.clear();
        self.access_unit_bytes = 0;
        self.parameter_sets = [None, None, None];
        self.initial_configuration = InitialConfigurationState::AwaitingConfigurationAu;
        self.complete_initial_ap_parameter_sets = None;
        self.dropping_until_marker = false;
        self.recovery_sequence = None;
        #[cfg(any(debug_assertions, test))]
        {
            self.diagnostics = HevcAssemblerDiagnostics::default();
            self.access_unit_has_complete_configuration = false;
        }
    }

    #[cfg(any(debug_assertions, test))]
    pub(crate) const fn diagnostics(&self) -> HevcAssemblerDiagnostics {
        self.diagnostics
    }

    /// 接收一个已认证 RTP 包；同一次调用可能排空重排队列并发布多个 AU。
    pub fn push(
        &mut self,
        packet: HevcRtpPacket<'_>,
    ) -> Result<Vec<HevcAccessUnit>, HevcAccessUnitError> {
        self.push_received_at(packet, Instant::now())
    }

    /// 接收一个带本机认证完成时刻的 RTP 包；用于保留 AU 的最早入站边界。
    pub fn push_received_at(
        &mut self,
        packet: HevcRtpPacket<'_>,
        local_ingress_at: Instant,
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
            let relation = self
                .recovery_sequence
                .map_or(SerialRelation::Forward, |cursor| {
                    serial_relation(packet.sequence, cursor)
                });
            if relation == SerialRelation::Forward {
                self.recovery_sequence = Some(packet.sequence);
            }
            if packet.marker && relation != SerialRelation::OlderOrAmbiguous {
                self.dropping_until_marker = false;
                self.expected_sequence = Some(packet.sequence.wrapping_add(1));
                self.recovery_sequence = None;
                self.clear_reorder();
                #[cfg(any(debug_assertions, test))]
                {
                    self.diagnostics.recovery_marker_resyncs =
                        self.diagnostics.recovery_marker_resyncs.saturating_add(1);
                }
            }
            return Ok(Vec::new());
        }

        let expected = *self.expected_sequence.get_or_insert(packet.sequence);
        let distance = packet.sequence.wrapping_sub(expected);
        if distance >= 0x8000 {
            return Ok(Vec::new());
        }
        if usize::from(distance) >= self.limits.max_reorder_packets {
            self.require_recovery_irap();
            self.drop_at_packet_boundary(packet.sequence, packet.marker);
            #[cfg(any(debug_assertions, test))]
            {
                if packet.marker {
                    self.diagnostics.recovery_marker_resyncs =
                        self.diagnostics.recovery_marker_resyncs.saturating_add(1);
                }
                self.diagnostics.reorder_window_exceeded =
                    self.diagnostics.reorder_window_exceeded.saturating_add(1);
            }
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
                local_ingress_at,
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
                self.recovery_sequence = (!packet.marker).then_some(packet.sequence);
                return Err(HevcAccessUnitError::TimestampChangedBeforeMarker {
                    previous,
                    actual: packet.timestamp,
                });
            }
        } else {
            self.timestamp = Some(packet.timestamp);
        }
        self.local_ingress_at = Some(
            self.local_ingress_at
                .map_or(packet.local_ingress_at, |earliest| {
                    earliest.min(packet.local_ingress_at)
                }),
        );

        let packet_nals =
            match self
                .depacketizer
                .push(self.generation, packet.sequence, &packet.payload)
            {
                Ok(nals) => nals,
                Err(error) => {
                    self.discard_access_unit();
                    self.dropping_until_marker = !packet.marker;
                    self.recovery_sequence = (!packet.marker).then_some(packet.sequence);
                    return Err(HevcAccessUnitError::Depacketize(error));
                }
            };
        #[cfg(any(debug_assertions, test))]
        let observe_complete_configuration = nal_type(&packet.payload) == Some(48);
        #[cfg(not(any(debug_assertions, test)))]
        let observe_complete_configuration = self.initial_configuration
            == InitialConfigurationState::AwaitingConfigurationAu
            && nal_type(&packet.payload) == Some(48);
        if observe_complete_configuration {
            let mut parameter_sets: [Option<Vec<u8>>; 3] = [None, None, None];
            for nal in &packet_nals {
                if let Some(index) = parameter_set_index(nal.as_bytes()) {
                    parameter_sets[index] = Some(nal.as_bytes().to_vec());
                }
            }
            if let [Some(vps), Some(sps), Some(pps)] = parameter_sets {
                #[cfg(any(debug_assertions, test))]
                {
                    self.access_unit_has_complete_configuration = true;
                }
                if self.initial_configuration == InitialConfigurationState::AwaitingConfigurationAu
                {
                    self.complete_initial_ap_parameter_sets = Some([vps, sps, pps]);
                }
            }
        }
        if packet.marker && packet_nals.is_empty() {
            self.discard_access_unit();
            self.dropping_until_marker = false;
            self.recovery_sequence = None;
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
                    self.recovery_sequence = (!packet.marker).then_some(packet.sequence);
                    HevcAccessUnitError::AccessUnitBudgetExceeded {
                        limit: self.limits.max_access_unit_bytes,
                    }
                })?;
            self.access_unit_bytes = next_length;
            self.nals.push(bytes);
        }

        if packet.marker
            && self.initial_configuration == InitialConfigurationState::AwaitingRecoveryIrap
            && !self
                .nals
                .iter()
                .any(|nal| matches!(nal_type(nal), Some(16..=23)))
        {
            self.discard_access_unit();
            #[cfg(any(debug_assertions, test))]
            {
                self.diagnostics.waiting_for_recovery_irap_drops = self
                    .diagnostics
                    .waiting_for_recovery_irap_drops
                    .saturating_add(1);
            }
            return Ok(None);
        }

        if !packet.marker {
            return Ok(None);
        }
        let access_unit = self.finish_access_unit(packet.ssrc, packet.timestamp)?;
        #[cfg(any(debug_assertions, test))]
        {
            if self.access_unit_has_complete_configuration {
                self.diagnostics.complete_configuration_access_units = self
                    .diagnostics
                    .complete_configuration_access_units
                    .saturating_add(1);
            }
            self.access_unit_has_complete_configuration = false;
            self.diagnostics.completed_access_units =
                self.diagnostics.completed_access_units.saturating_add(1);
        }
        Ok(Some(access_unit))
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
        if self.initial_configuration != InitialConfigurationState::Configured
            && next_parameter_sets.iter().any(Option::is_none)
        {
            self.discard_access_unit();
            return Err(HevcAccessUnitError::MissingInitialParameterSets);
        }

        let keyframe = self
            .nals
            .iter()
            .any(|nal| matches!(nal_type(nal), Some(16..=23)));
        let mut prefixes = Vec::new();
        if keyframe
            || self.initial_configuration == InitialConfigurationState::AwaitingConfigurationAu
        {
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
        self.initial_configuration = InitialConfigurationState::Configured;
        self.complete_initial_ap_parameter_sets = None;
        self.timestamp = None;
        let local_ingress_at = self
            .local_ingress_at
            .take()
            .expect("完成的 HEVC AU 至少包含一个已认证 RTP 包");
        self.nals.clear();
        self.access_unit_bytes = 0;
        self.depacketizer.reset(self.generation);
        Ok(HevcAccessUnit {
            generation: self.generation,
            ssrc,
            timestamp,
            keyframe,
            parameter_sets_prepended,
            local_ingress_at,
            data,
        })
    }

    fn drop_at_packet_boundary(&mut self, sequence: u16, marker: bool) {
        self.discard_access_unit();
        self.clear_reorder();
        self.expected_sequence = Some(sequence.wrapping_add(1));
        self.dropping_until_marker = !marker;
        self.recovery_sequence = (!marker).then_some(sequence);
    }

    fn discard_stream(&mut self, ssrc: u32, sequence: u16, marker: bool) {
        self.discard_access_unit();
        self.clear_reorder();
        self.ssrc = Some(ssrc);
        self.expected_sequence = Some(sequence.wrapping_add(1));
        self.dropping_until_marker = !marker;
        self.recovery_sequence = (!marker).then_some(sequence);
        self.parameter_sets = [None, None, None];
        self.initial_configuration = InitialConfigurationState::AwaitingConfigurationAu;
        self.complete_initial_ap_parameter_sets = None;
    }

    fn discard_access_unit(&mut self) {
        self.timestamp = None;
        self.local_ingress_at = None;
        self.nals.clear();
        self.access_unit_bytes = 0;
        self.depacketizer.reset(self.generation);
        self.complete_initial_ap_parameter_sets = None;
        #[cfg(any(debug_assertions, test))]
        {
            self.access_unit_has_complete_configuration = false;
        }
    }

    fn preserve_complete_initial_ap_parameter_sets(&mut self) {
        if self.initial_configuration != InitialConfigurationState::AwaitingConfigurationAu {
            return;
        }
        let Some([vps, sps, pps]) = self.complete_initial_ap_parameter_sets.take() else {
            return;
        };
        self.parameter_sets = [Some(vps), Some(sps), Some(pps)];
        self.initial_configuration = InitialConfigurationState::AwaitingRecoveryIrap;
    }

    fn require_recovery_irap(&mut self) {
        self.preserve_complete_initial_ap_parameter_sets();
        if self.initial_configuration == InitialConfigurationState::Configured {
            self.initial_configuration = InitialConfigurationState::AwaitingRecoveryIrap;
        }
    }

    fn fail_access_unit_budget(&mut self) -> HevcAccessUnitError {
        self.discard_access_unit();
        self.dropping_until_marker = false;
        self.recovery_sequence = None;
        HevcAccessUnitError::AccessUnitBudgetExceeded {
            limit: self.limits.max_access_unit_bytes,
        }
    }

    fn clear_reorder(&mut self) {
        self.buffered.clear();
        self.buffered_bytes = 0;
    }
}

impl HevcAccessUnit {
    /// 严格拆分组装器产出的 4 字节大端 length-prefixed NAL 序列。
    pub(crate) fn nal_units(&self) -> Result<Vec<&[u8]>, HevcAccessUnitError> {
        let mut cursor = 0usize;
        let mut nals = Vec::new();
        while cursor < self.data.len() {
            let prefix_end = cursor
                .checked_add(4)
                .ok_or(HevcAccessUnitError::MalformedLengthPrefixedAccessUnit)?;
            let prefix = self
                .data
                .get(cursor..prefix_end)
                .ok_or(HevcAccessUnitError::MalformedLengthPrefixedAccessUnit)?;
            let length = usize::try_from(u32::from_be_bytes(prefix.try_into().unwrap()))
                .map_err(|_| HevcAccessUnitError::MalformedLengthPrefixedAccessUnit)?;
            if length == 0 {
                return Err(HevcAccessUnitError::EmptyNalUnit);
            }
            let end = prefix_end
                .checked_add(length)
                .ok_or(HevcAccessUnitError::MalformedLengthPrefixedAccessUnit)?;
            let nal = self
                .data
                .get(prefix_end..end)
                .ok_or(HevcAccessUnitError::MalformedLengthPrefixedAccessUnit)?;
            nals.push(nal);
            cursor = end;
        }
        if nals.is_empty() {
            return Err(HevcAccessUnitError::EmptyNalUnit);
        }
        Ok(nals)
    }

    /// 将内部 length-prefixed AU 转成中立 decoder 契约使用的 Annex B。
    pub(crate) fn annex_b_bytes(&self) -> Result<Vec<u8>, HevcAccessUnitError> {
        let nals = self.nal_units()?;
        let mut output = Vec::with_capacity(self.data.len());
        for nal in nals {
            output.extend_from_slice(&[0, 0, 0, 1]);
            output.extend_from_slice(nal);
        }
        Ok(output)
    }
}

fn nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|first| (first >> 1) & 0x3f)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SerialRelation {
    Equal,
    Forward,
    OlderOrAmbiguous,
}

fn serial_relation(sequence: u16, cursor: u16) -> SerialRelation {
    match sequence.wrapping_sub(cursor) {
        0 => SerialRelation::Equal,
        1..=0x7fff => SerialRelation::Forward,
        _ => SerialRelation::OlderOrAmbiguous,
    }
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
    use std::time::{Duration, Instant};

    use super::{
        HevcAccessUnitAssembler, HevcAccessUnitError, HevcAccessUnitLimits, HevcRtpPacket,
        InitialConfigurationState,
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

    fn recovering_assembler() -> HevcAccessUnitAssembler {
        let mut assembler = assembler();
        assembler.parameter_sets = [
            Some(vec![0x40, 0x01, 1]),
            Some(vec![0x42, 0x01, 2]),
            Some(vec![0x44, 0x01, 3]),
        ];
        assembler.initial_configuration = InitialConfigurationState::Configured;
        assembler.dropping_until_marker = true;
        assembler
    }

    fn assume_configured(assembler: &mut HevcAccessUnitAssembler, next_sequence: u16) {
        assembler.ssrc = Some(SSRC);
        assembler.expected_sequence = Some(next_sequence);
        assembler.parameter_sets = [
            Some(vec![0x40, 0x01, 1]),
            Some(vec![0x42, 0x01, 2]),
            Some(vec![0x44, 0x01, 3]),
        ];
        assembler.initial_configuration = InitialConfigurationState::Configured;
        assembler.dropping_until_marker = false;
    }

    fn synchronize(assembler: &mut HevcAccessUnitAssembler, first_sequence: u16) {
        let configuration = [
            0x60, 0x01, 0, 3, 0x40, 0x01, 1, 0, 3, 0x42, 0x01, 2, 0, 3, 0x44, 0x01, 3,
        ];
        assert_eq!(
            assembler
                .push(packet(
                    first_sequence.wrapping_sub(1),
                    0,
                    true,
                    &configuration,
                ))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn initial_access_unit_without_parameter_sets_fails_closed() {
        let mut assembler = assembler();

        assert!(assembler
            .push(packet(10, 1000, false, &[0x02, 0x01, 0xaa]))
            .unwrap()
            .is_empty());
        assert_eq!(
            assembler.push(packet(11, 1000, true, &[0x02, 0x01, 0xbb])),
            Err(HevcAccessUnitError::MissingInitialParameterSets)
        );
    }

    #[test]
    fn late_old_marker_during_recovery_cannot_rewind_the_sequence_cursor() {
        let mut assembler = recovering_assembler();
        assert!(assembler
            .push(packet(9, 900, true, &[0x02, 0x01, 0]))
            .unwrap()
            .is_empty());
        assembler
            .push(packet(10, 1000, false, &[0x02, 0x01, 1]))
            .unwrap();
        assert_eq!(
            assembler.push(packet(11, 2000, false, &[0x02, 0x01, 2])),
            Err(HevcAccessUnitError::TimestampChangedBeforeMarker {
                previous: 1000,
                actual: 2000,
            })
        );

        assert!(assembler
            .push(packet(8, 800, true, &[0x02, 0x01, 3]))
            .unwrap()
            .is_empty());
        assert!(assembler
            .push(packet(12, 2000, true, &[0x02, 0x01, 4]))
            .unwrap()
            .is_empty());
        let output = assembler
            .push(packet(13, 3000, true, &[0x02, 0x01, 5]))
            .unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].timestamp, 3000);
    }

    #[test]
    fn forward_recovery_marker_is_accepted_across_sequence_wrap() {
        let mut assembler = recovering_assembler();
        assert!(assembler
            .push(packet(u16::MAX - 2, 900, true, &[0x02, 0x01, 0]))
            .unwrap()
            .is_empty());
        assembler
            .push(packet(u16::MAX - 1, 1000, false, &[0x02, 0x01, 1]))
            .unwrap();
        assert_eq!(
            assembler.push(packet(u16::MAX, 2000, false, &[0x02, 0x01, 2])),
            Err(HevcAccessUnitError::TimestampChangedBeforeMarker {
                previous: 1000,
                actual: 2000,
            })
        );

        assert!(assembler
            .push(packet(u16::MAX - 1, 800, true, &[0x02, 0x01, 3]))
            .unwrap()
            .is_empty());
        assert!(assembler
            .push(packet(0, 2000, true, &[0x02, 0x01, 4]))
            .unwrap()
            .is_empty());
        let output = assembler
            .push(packet(1, 3000, true, &[0x02, 0x01, 5]))
            .unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].timestamp, 3000);
    }

    #[test]
    fn recovery_observation_rejects_marker_older_than_forward_non_marker() {
        let mut assembler = recovering_assembler();

        assert!(assembler
            .push(packet(100, 1000, false, &[0x02, 0x01, 1]))
            .unwrap()
            .is_empty());
        assert!(assembler
            .push(packet(99, 900, true, &[0x02, 0x01, 2]))
            .unwrap()
            .is_empty());
        assert!(assembler
            .push(packet(100, 1000, true, &[0x02, 0x01, 3]))
            .unwrap()
            .is_empty());

        let output = assembler
            .push(packet(101, 2000, true, &[0x02, 0x01, 4]))
            .unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].timestamp, 2000);
    }

    #[test]
    fn recovery_cursor_tracks_multiple_forward_observations() {
        let mut assembler = recovering_assembler();

        for sequence in [100, 105] {
            assert!(assembler
                .push(packet(sequence, 1000, false, &[0x02, 0x01, 1]))
                .unwrap()
                .is_empty());
        }
        assert!(assembler
            .push(packet(103, 1000, true, &[0x02, 0x01, 2]))
            .unwrap()
            .is_empty());
        assert!(assembler
            .push(packet(104, 1000, true, &[0x02, 0x01, 3]))
            .unwrap()
            .is_empty());
        assert!(assembler
            .push(packet(106, 1000, true, &[0x02, 0x01, 4]))
            .unwrap()
            .is_empty());
        assert!(assembler.recovery_sequence.is_none());
        assert!(assembler.buffered.is_empty());
        assert_eq!(assembler.buffered_bytes, 0);

        assert_eq!(
            assembler
                .push(packet(107, 2000, true, &[0x02, 0x01, 5]))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn recovery_cursor_advances_exactly_across_u16_wrap() {
        let mut assembler = recovering_assembler();

        for sequence in [u16::MAX, 0] {
            assert!(assembler
                .push(packet(sequence, 1000, false, &[0x02, 0x01, 1]))
                .unwrap()
                .is_empty());
        }
        assert!(assembler
            .push(packet(u16::MAX, 1000, true, &[0x02, 0x01, 2]))
            .unwrap()
            .is_empty());
        assert!(assembler
            .push(packet(0, 1000, true, &[0x02, 0x01, 3]))
            .unwrap()
            .is_empty());

        assert_eq!(
            assembler
                .push(packet(1, 2000, true, &[0x02, 0x01, 4]))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn recovery_half_range_observation_is_ambiguous_and_fails_closed() {
        let mut assembler = recovering_assembler();

        assert!(assembler
            .push(packet(0, 1000, false, &[0x02, 0x01, 1]))
            .unwrap()
            .is_empty());
        assert!(assembler
            .push(packet(0x8000, 1000, true, &[0x02, 0x01, 2]))
            .unwrap()
            .is_empty());
        assert!(assembler
            .push(packet(1, 1000, true, &[0x02, 0x01, 3]))
            .unwrap()
            .is_empty());

        assert_eq!(
            assembler
                .push(packet(2, 2000, true, &[0x02, 0x01, 4]))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn out_of_order_packets_publish_only_after_the_gap_is_filled() {
        let mut assembler = assembler();
        synchronize(&mut assembler, 10);
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
    fn access_unit_keeps_the_earliest_authenticated_ingress_across_reordering() {
        let mut assembler = assembler();
        assume_configured(&mut assembler, 10);
        let base = Instant::now();

        assert!(assembler
            .push_received_at(
                packet(10, 1_000, false, &[0x02, 0x01, 0xaa]),
                base + Duration::from_millis(20),
            )
            .unwrap()
            .is_empty());
        assert!(assembler
            .push_received_at(
                packet(12, 1_000, true, &[0x02, 0x01, 0xcc]),
                base + Duration::from_millis(5),
            )
            .unwrap()
            .is_empty());
        let access_units = assembler
            .push_received_at(
                packet(11, 1_000, false, &[0x02, 0x01, 0xbb]),
                base + Duration::from_millis(30),
            )
            .unwrap();

        assert_eq!(access_units.len(), 1);
        assert_eq!(
            access_units[0].local_ingress_at,
            base + Duration::from_millis(5)
        );
    }

    #[test]
    fn sequence_wrap_is_ordered_modulo_sixteen_bits() {
        let mut assembler = assembler();
        synchronize(&mut assembler, u16::MAX);
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
        synchronize(&mut assembler, 20);
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
    fn diagnostics_count_complete_configuration_au_completed_au_and_generation_reset() {
        let mut assembler = assembler();
        let configuration = [
            0x60, 0x01, 0, 3, 0x40, 0x01, 0xaa, 0, 3, 0x42, 0x01, 0xbb, 0, 3, 0x44, 0x01, 0xcc,
        ];

        assert_eq!(
            assembler
                .push(packet(20, 3_000, true, &configuration))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            assembler.diagnostics().complete_configuration_access_units,
            1
        );
        assert_eq!(assembler.diagnostics().completed_access_units, 1);

        assembler.reset(GENERATION + 1);
        assert_eq!(
            assembler.diagnostics(),
            super::HevcAssemblerDiagnostics::default()
        );
    }

    #[test]
    fn diagnostics_count_reorder_recovery_resync_and_waiting_for_irap_drop() {
        let mut assembler = assembler();
        let configuration = [
            0x60, 0x01, 0, 3, 0x40, 0x01, 1, 0, 3, 0x42, 0x01, 2, 0, 3, 0x44, 0x01, 3,
        ];
        assert!(assembler
            .push(packet(10, 0, false, &configuration))
            .unwrap()
            .is_empty());
        assert!(matches!(
            assembler.push(packet(267, 0, true, &[0x02, 0x01, 4])),
            Err(HevcAccessUnitError::ReorderWindowExceeded { .. })
        ));
        assert!(assembler
            .push(packet(268, 3_000, true, &[0x02, 0x01, 5]))
            .unwrap()
            .is_empty());
        assert!(assembler
            .push(packet(269, 6_000, false, &[0x02, 0x01, 6]))
            .unwrap()
            .is_empty());
        assert!(matches!(
            assembler.push(packet(270, 9_000, false, &[0x02, 0x01, 7])),
            Err(HevcAccessUnitError::TimestampChangedBeforeMarker { .. })
        ));
        assert!(assembler
            .push(packet(271, 9_000, true, &[0x02, 0x01, 8]))
            .unwrap()
            .is_empty());

        let diagnostics = assembler.diagnostics();
        assert_eq!(diagnostics.reorder_window_exceeded, 1);
        assert_eq!(diagnostics.waiting_for_recovery_irap_drops, 1);
        assert_eq!(diagnostics.recovery_marker_resyncs, 2);
    }

    #[test]
    fn configuration_diagnostic_counts_only_one_successful_complete_configuration_au() {
        let configuration = [
            0x60, 0x01, 0, 3, 0x40, 0x01, 1, 0, 3, 0x42, 0x01, 2, 0, 3, 0x44, 0x01, 3,
        ];
        let mut completed = assembler();
        assert!(completed
            .push(packet(1, 0, false, &configuration))
            .unwrap()
            .is_empty());
        assert_eq!(
            completed.diagnostics().complete_configuration_access_units,
            0,
            "a marker-less configuration AP is not a completed AU"
        );
        assert_eq!(
            completed
                .push(packet(2, 0, true, &configuration))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            completed.diagnostics().complete_configuration_access_units,
            1,
            "multiple complete APs in one completed AU count once"
        );

        let mut discarded = assembler();
        assert!(discarded
            .push(packet(1, 0, false, &configuration))
            .unwrap()
            .is_empty());
        assert!(matches!(
            discarded.push(packet(2, 1, false, &[0x02, 0x01, 4])),
            Err(HevcAccessUnitError::TimestampChangedBeforeMarker { .. })
        ));
        assert_eq!(
            discarded.diagnostics().complete_configuration_access_units,
            0,
            "a discarded configuration AU is not counted"
        );
    }

    #[test]
    fn reorder_window_with_its_own_marker_counts_one_immediate_recovery_resync() {
        let mut assembler = assembler();
        assert!(assembler
            .push(packet(10, 0, false, &[0x02, 0x01, 1]))
            .unwrap()
            .is_empty());
        assert!(matches!(
            assembler.push(packet(267, 0, true, &[0x02, 0x01, 2])),
            Err(HevcAccessUnitError::ReorderWindowExceeded { .. })
        ));
        let diagnostics = assembler.diagnostics();
        assert_eq!(diagnostics.reorder_window_exceeded, 1);
        assert_eq!(diagnostics.recovery_marker_resyncs, 1);
    }

    #[test]
    fn reorder_window_waits_for_one_later_marker_before_counting_recovery_resync() {
        let mut assembler = assembler();
        assert!(assembler
            .push(packet(10, 0, false, &[0x02, 0x01, 1]))
            .unwrap()
            .is_empty());
        assert!(matches!(
            assembler.push(packet(267, 0, false, &[0x02, 0x01, 2])),
            Err(HevcAccessUnitError::ReorderWindowExceeded { .. })
        ));
        assert_eq!(assembler.diagnostics().recovery_marker_resyncs, 0);
        assert!(assembler
            .push(packet(268, 0, true, &[0x02, 0x01, 3]))
            .unwrap()
            .is_empty());
        assert_eq!(assembler.diagnostics().recovery_marker_resyncs, 1);
    }

    #[test]
    fn cached_parameter_sets_are_prepended_to_a_later_keyframe() {
        let mut assembler = assembler();
        synchronize(&mut assembler, 30);
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
    fn configured_reorder_loss_waits_for_irap_and_prepends_cached_parameter_sets() {
        let mut assembler = HevcAccessUnitAssembler::new(
            GENERATION,
            HevcAccessUnitLimits {
                max_reorder_packets: 2,
                ..HevcAccessUnitLimits::default()
            },
        )
        .unwrap();
        synchronize(&mut assembler, 40);
        let configuration = [
            0x60, 0x01, 0, 3, 0x40, 0x01, 1, 0, 3, 0x42, 0x01, 2, 0, 3, 0x44, 0x01, 3,
        ];
        assert_eq!(
            assembler
                .push(packet(40, 5_000, true, &configuration))
                .unwrap()
                .len(),
            1
        );
        assembler
            .push(packet(41, 6_000, false, &[0x02, 0x01, 1]))
            .unwrap();

        assert_eq!(
            assembler.push(packet(44, 6_000, true, &[0x02, 0x01, 2])),
            Err(HevcAccessUnitError::ReorderWindowExceeded { limit: 2 })
        );
        assert!(assembler
            .push(packet(45, 7_000, true, &[0x02, 0x01, 3]))
            .unwrap()
            .is_empty());
        let output = assembler
            .push(packet(46, 8_000, true, &[0x26, 0x01, 0xfe]))
            .unwrap();
        assert_eq!(output.len(), 1);
        assert!(output[0].keyframe);
        assert!(output[0].parameter_sets_prepended);
        assert_eq!(output[0].timestamp, 8_000);
        assert_eq!(
            output[0].data,
            [
                0, 0, 0, 3, 0x40, 0x01, 1, 0, 0, 0, 3, 0x42, 0x01, 2, 0, 0, 0, 3, 0x44, 0x01, 3, 0,
                0, 0, 3, 0x26, 0x01, 0xfe,
            ]
        );
    }

    #[test]
    fn initial_complete_parameter_set_ap_survives_only_reorder_window_loss() {
        let mut assembler = assembler();
        let configuration = [
            0x60, 0x01, 0, 3, 0x40, 0x01, 1, 0, 3, 0x42, 0x01, 2, 0, 3, 0x44, 0x01, 3,
        ];
        assert!(assembler
            .push(packet(10, 0, false, &configuration))
            .unwrap()
            .is_empty());
        assert_eq!(
            assembler.push(packet(267, 0, true, &[0x02, 0x01, 4])),
            Err(HevcAccessUnitError::ReorderWindowExceeded { limit: 256 })
        );

        assert!(assembler
            .push(packet(268, 3_000, true, &[0x02, 0x01, 5]))
            .unwrap()
            .is_empty());
        assert!(assembler
            .push(packet(269, 6_000, true, &[0x00, 0x01, 6]))
            .unwrap()
            .is_empty());

        let output = assembler
            .push(packet(270, 9_000, true, &[0x26, 0x01, 7]))
            .unwrap();
        assert_eq!(output.len(), 1);
        assert!(output[0].keyframe);
        assert!(output[0].parameter_sets_prepended);
        assert_eq!(
            output[0].data,
            [
                0, 0, 0, 3, 0x40, 0x01, 1, 0, 0, 0, 3, 0x42, 0x01, 2, 0, 0, 0, 3, 0x44, 0x01, 3, 0,
                0, 0, 3, 0x26, 0x01, 7,
            ]
        );
    }

    #[test]
    fn timestamp_change_before_marker_drops_both_sides_of_the_boundary() {
        let mut assembler = assembler();
        synchronize(&mut assembler, 50);
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
        synchronize(&mut assembler, 60);
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
        let boundary = HevcRtpPacket {
            generation: GENERATION + 1,
            sequence: 1,
            timestamp: 1,
            marker: true,
            payload: &[
                0x60, 0x01, 0, 3, 0x40, 0x01, 1, 0, 3, 0x42, 0x01, 2, 0, 3, 0x44, 0x01, 3,
            ],
            ssrc: SSRC,
        };
        assert_eq!(assembler.push(boundary).unwrap().len(), 1);
        let fresh = HevcRtpPacket {
            generation: GENERATION + 1,
            sequence: 2,
            timestamp: 2,
            marker: true,
            payload: &[0x02, 0x01, 5],
            ssrc: SSRC,
        };
        assert_eq!(assembler.push(fresh).unwrap().len(), 1);
    }

    #[test]
    fn malformed_fu_and_access_unit_budget_never_publish_partial_frames() {
        let mut assembler = assembler();
        synchronize(&mut assembler, 70);
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
        assume_configured(&mut bounded, 80);
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
        assume_configured(&mut assembler, 90);
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
        assume_configured(&mut assembler, 100);
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
