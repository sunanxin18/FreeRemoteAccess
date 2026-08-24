//! FRDMVS02 cold MVS capture container.
//!
//! This module deliberately keeps FRDMVS01 parsing in `hpss` unchanged.

use anyhow::{bail, ensure, Context, Result};
use std::io::Read;

use crate::framebuffer::validate_framebuffer_geometry;
use crate::vnc::mvs::MAX_MVS_DECODE_PIXELS;
use crate::vnc::mvs_stream::{MvsRecord, MvsRect, MAX_MVS_RECORD_PAYLOAD};

pub const MVS_CAPTURE_V2_MAGIC: [u8; 8] = *b"FRDMVS02";
pub const MVS_CAPTURE_V2_HEADER_BYTES: usize = 32;
pub const MVS_CAPTURE_V2_MAX_PAYLOAD: usize = 0x0100_0000;
pub const MVS_CAPTURE_V2_MAX_EVENT_BODY: usize = 0x0100_001c;
pub const MVS_CAPTURE_V2_MAX_EVENT_BYTES: usize = 0x0100_003c;
pub const MVS_CAPTURE_V2_MAX_RECORDS: usize = 4096;
pub const MVS_CAPTURE_V2_MAX_EVENTS: usize = 4102;
pub const MVS_CAPTURE_V2_MAX_CUMULATIVE_PAYLOAD: usize = 0x2000_0000;
pub const MVS_CAPTURE_V2_MAX_DURATION_MS: u32 = 30_000;

const EVENT_PREFIX_BYTES: usize = 32;
const EVENT_CREATED: u16 = 0x0001;
const EVENT_ARMED: u16 = 0x0002;
const EVENT_TRIGGERED: u16 = 0x0003;
const EVENT_RECORDING: u16 = 0x0004;
const EVENT_SURFACE: u16 = 0x0010;
const EVENT_RECORD: u16 = 0x0020;
const EVENT_GAP: u16 = 0x0021;
const EVENT_CLEAN: u16 = 0x00fe;
const EVENT_ABORTED: u16 = 0x00ff;

#[derive(Clone, Copy, Eq, PartialEq)]
enum StructuralPhase {
    Start,
    Created,
    Armed,
    Triggered,
    Recording,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MvsCaptureV2Provenance {
    DiagnosticOnly,
    HistoricalUnproven,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MvsCaptureV2Geometry {
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MvsCaptureV2TerminalKind {
    Clean,
    Aborted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MvsCaptureV2TerminalReason {
    Deadline,
    RecordLimit,
    ConnectFailure,
    CredentialOrAuthenticationFailure,
    ArmOutputFailure,
    TriggerFailure,
    ReadFailure,
    AssemblerFailure,
    PendingAtDeadline,
    OutputFailure,
    OperatorCancellation,
    InvalidGeometry,
    PreTriggerDeadline,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsCaptureV2Record {
    pub generation: u64,
    pub first_source_frame_ordinal: u64,
    pub last_source_frame_ordinal: u64,
    pub record: MvsRecord,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsCaptureV2Surface {
    pub generation: u64,
    pub geometry: MvsCaptureV2Geometry,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsCaptureV2Gap {
    pub reason: u16,
    pub stage: u16,
    pub first_source_frame_ordinal: u64,
    pub last_source_frame_ordinal: u64,
    pub declared_total: u32,
    pub accumulated_bytes: u32,
    pub rect: MvsRect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsCaptureV2Terminal {
    pub kind: MvsCaptureV2TerminalKind,
    pub reason: MvsCaptureV2TerminalReason,
    pub timestamp_us: u64,
    pub gap_count: u32,
    pub source_mvs_frame_count: u64,
    pub record_count: u64,
    pub type0_count: u64,
    pub type1_count: u64,
    pub type2_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuralMvsCaptureV2 {
    pub provenance: MvsCaptureV2Provenance,
    pub committed_surface: Option<MvsCaptureV2Geometry>,
    pub requested_surface: Option<MvsCaptureV2Geometry>,
    pub records: Vec<MvsCaptureV2Record>,
    pub surfaces: Vec<MvsCaptureV2Surface>,
    pub gaps: Vec<MvsCaptureV2Gap>,
    pub terminal: MvsCaptureV2Terminal,
    pub deadline_ms: Option<u32>,
    pub record_limit: Option<u32>,
    event_types: Vec<u16>,
    event_generations: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StrictColdMvsCaptureV2 {
    pub committed_surface: MvsCaptureV2Geometry,
    pub requested_surface: MvsCaptureV2Geometry,
    pub records: Vec<MvsCaptureV2Record>,
    pub deadline_ms: u32,
    pub record_limit: u32,
    pub terminal: MvsCaptureV2Terminal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoricalMvsCaptureV1 {
    pub provenance: MvsCaptureV2Provenance,
    pub records: Vec<MvsRecord>,
}

/// Preserves the unchanged V1 decoder behind explicit non-cold provenance.
pub fn read_mvs_capture_v1_historical(data: &[u8]) -> Result<HistoricalMvsCaptureV1> {
    Ok(HistoricalMvsCaptureV1 {
        provenance: MvsCaptureV2Provenance::HistoricalUnproven,
        records: crate::vnc::hpss::read_mvs_capture(data)?,
    })
}

#[derive(Default)]
pub(crate) struct CaptureCounters {
    events: usize,
    payload: usize,
}

impl CaptureCounters {
    pub(crate) fn reserve_event(&mut self) -> Result<()> {
        let next = self.events.checked_add(1).context("V2 event 计数溢出")?;
        ensure!(next <= MVS_CAPTURE_V2_MAX_EVENTS, "V2 event 数量超过上限");
        self.events = next;
        Ok(())
    }

    pub(crate) fn reserve_payload(&mut self, bytes: usize) -> Result<()> {
        let next = self
            .payload
            .checked_add(bytes)
            .context("V2 payload 累计溢出")?;
        ensure!(
            next <= MVS_CAPTURE_V2_MAX_CUMULATIVE_PAYLOAD,
            "V2 payload 累计超过上限"
        );
        self.payload = next;
        Ok(())
    }
}

pub(crate) fn validate_event_length(length: usize) -> Result<()> {
    ensure!(
        (EVENT_PREFIX_BYTES..=MVS_CAPTURE_V2_MAX_EVENT_BYTES).contains(&length),
        "V2 event 长度无效"
    );
    ensure!(
        length - EVENT_PREFIX_BYTES <= MVS_CAPTURE_V2_MAX_EVENT_BODY,
        "V2 event body 超过上限"
    );
    Ok(())
}

fn validate_record_payload_length(declared: usize, available_body: usize) -> Result<()> {
    let expected_body = 28usize
        .checked_add(declared)
        .context("Record payload length 算术溢出")?;
    ensure!(
        (1..=MVS_CAPTURE_V2_MAX_PAYLOAD).contains(&declared),
        "Record payload length 超出范围"
    );
    ensure!(
        available_body == expected_body,
        "Record payload length 与 body 不匹配"
    );
    Ok(())
}

/// Reads the fixed prefix. Task 2 reuses this codec helper for the writer tests.
pub(crate) fn read_prefix(data: &[u8]) -> Result<(usize, u16, u64, u64, u64)> {
    ensure!(data.len() == EVENT_PREFIX_BYTES, "V2 event prefix 长度无效");
    let length = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
    validate_event_length(length)?;
    let kind = u16::from_be_bytes(data[4..6].try_into().unwrap());
    ensure!(
        u16::from_be_bytes(data[6..8].try_into().unwrap()) == 0,
        "V2 event flags 非零"
    );
    Ok((
        length,
        kind,
        u64::from_be_bytes(data[8..16].try_into().unwrap()),
        u64::from_be_bytes(data[16..24].try_into().unwrap()),
        u64::from_be_bytes(data[24..32].try_into().unwrap()),
    ))
}

pub(crate) fn encode_event(
    kind: u16,
    ordinal: u64,
    generation: u64,
    timestamp_us: u64,
    body: &[u8],
) -> Result<Vec<u8>> {
    let length = EVENT_PREFIX_BYTES
        .checked_add(body.len())
        .context("V2 event 长度溢出")?;
    validate_event_length(length)?;
    let mut event = Vec::with_capacity(length);
    event.extend_from_slice(&(length as u32).to_be_bytes());
    event.extend_from_slice(&kind.to_be_bytes());
    event.extend_from_slice(&0u16.to_be_bytes());
    event.extend_from_slice(&ordinal.to_be_bytes());
    event.extend_from_slice(&generation.to_be_bytes());
    event.extend_from_slice(&timestamp_us.to_be_bytes());
    event.extend_from_slice(body);
    Ok(event)
}

pub fn read_mvs_capture_v2_structural<R: Read>(reader: &mut R) -> Result<StructuralMvsCaptureV2> {
    let mut header = [0u8; MVS_CAPTURE_V2_HEADER_BYTES];
    read_exact(reader, &mut header, "FRDMVS02 header 截断")?;
    if header[..8] == *b"FRDMVS01" {
        bail!("FRDMVS01 仅为 HistoricalUnproven，不能作为 V2 读取");
    }
    validate_header(&header)?;
    let mut counters = CaptureCounters::default();
    let mut event_types = Vec::new();
    let mut event_generations = Vec::new();
    let mut records = Vec::new();
    let mut surfaces = Vec::new();
    let mut gaps = Vec::new();
    let mut committed_surface = None;
    let mut requested_surface = None;
    let mut deadline_ms = None;
    let mut record_limit = None;
    let mut phase = StructuralPhase::Start;
    let mut current_generation = 0u64;
    let mut last_timestamp = None;
    let mut expected_ordinal = 0u64;
    let mut pending_gap_terminal = None;
    let terminal;

    loop {
        let mut prefix = [0u8; EVENT_PREFIX_BYTES];
        read_exact(reader, &mut prefix, "V2 event prefix 截断")?;
        let (length, kind, ordinal, generation, timestamp_us) = read_prefix(&prefix)?;
        ensure!(ordinal == expected_ordinal, "V2 event ordinal 不连续");
        expected_ordinal = expected_ordinal
            .checked_add(1)
            .context("V2 event ordinal 溢出")?;
        if let Some(last) = last_timestamp {
            ensure!(timestamp_us >= last, "V2 timestamp 倒退");
        }
        last_timestamp = Some(timestamp_us);
        counters.reserve_event()?;
        let mut body = vec![0u8; length - EVENT_PREFIX_BYTES];
        read_exact(reader, &mut body, "V2 event body 截断")?;
        if let Some(reason) = pending_gap_terminal {
            ensure!(kind == EVENT_ABORTED, "Gap 后必须紧随 Aborted");
            ensure!(
                generation == current_generation,
                "Gap terminal generation 无效"
            );
            let parsed = parse_terminal(kind, &body, timestamp_us)?;
            validate_terminal_phase(&parsed, phase)?;
            ensure!(
                parsed.reason == terminal_reason(gap_terminal_code(reason)?)?,
                "Gap terminal reason 不匹配"
            );
            terminal = parsed;
            event_types.push(kind);
            event_generations.push(generation);
            break;
        }
        match kind {
            EVENT_CREATED => {
                ensure!(
                    phase == StructuralPhase::Start && generation == 0 && timestamp_us == 0,
                    "Created 位置或前缀无效"
                );
                ensure!(body.len() == 16, "Created body 长度无效");
                let deadline = u32_be(&body[0..4]);
                ensure!(
                    [5000, 10000, 15000, 20000, 30000].contains(&deadline),
                    "Created deadline 无效"
                );
                let limit = u32_be(&body[4..8]);
                ensure!(
                    (1..=MVS_CAPTURE_V2_MAX_RECORDS as u32).contains(&limit),
                    "Created record limit 无效"
                );
                ensure!(
                    u16_be(&body[8..10]) == 1
                        && u16_be(&body[10..12]) == 1
                        && u32_be(&body[12..16]) == 0,
                    "Created 字段无效"
                );
                deadline_ms = Some(deadline);
                record_limit = Some(limit);
                phase = StructuralPhase::Created;
            }
            EVENT_ARMED => {
                ensure!(
                    generation == current_generation
                        && phase == StructuralPhase::Created
                        && committed_surface.is_none(),
                    "Armed 状态无效"
                );
                ensure!(body.len() == 24, "Armed body 长度无效");
                let committed = geometry(u16_be(&body[0..2]), u16_be(&body[2..4]))?;
                let requested = geometry(u16_be(&body[4..6]), u16_be(&body[6..8]))?;
                ensure!(
                    u16_be(&body[8..10]) == 1
                        && u16_be(&body[10..12]) == 1
                        && u32_be(&body[12..16]) == 3
                        && u64_be(&body[16..24]) == 0,
                    "Armed 字段无效"
                );
                committed_surface = Some(committed);
                requested_surface = Some(requested);
                phase = StructuralPhase::Armed;
            }
            EVENT_TRIGGERED => {
                ensure!(
                    generation == current_generation && phase == StructuralPhase::Armed,
                    "Triggered 状态无效"
                );
                ensure!(body.len() == 16, "Triggered body 长度无效");
                let requested = requested_surface.unwrap();
                ensure!(
                    u16_be(&body[0..2]) == requested.width
                        && u16_be(&body[2..4]) == requested.height
                        && u32_be(&body[4..8]) == 7
                        && u64_be(&body[8..16]) == 0,
                    "Triggered 字段无效"
                );
                phase = StructuralPhase::Triggered;
            }
            EVENT_RECORDING => {
                ensure!(
                    generation == current_generation && phase == StructuralPhase::Triggered,
                    "Recording 状态无效"
                );
                ensure!(body == 0u64.to_be_bytes(), "Recording body 无效");
                phase = StructuralPhase::Recording;
            }
            EVENT_SURFACE => {
                ensure!(
                    generation
                        == current_generation
                            .checked_add(1)
                            .context("generation 溢出")?,
                    "Surface generation 无效"
                );
                ensure!(
                    phase == StructuralPhase::Recording
                        && committed_surface.is_some()
                        && body.len() == 8,
                    "Surface 状态或 body 无效"
                );
                ensure!(
                    u16_be(&body[4..6]) == 1 && u16_be(&body[6..8]) == 0,
                    "Surface 字段无效"
                );
                let surface = geometry(u16_be(&body[0..2]), u16_be(&body[2..4]))?;
                current_generation = generation;
                committed_surface = Some(surface);
                surfaces.push(MvsCaptureV2Surface {
                    generation,
                    geometry: surface,
                });
            }
            EVENT_RECORD => {
                ensure!(
                    generation == current_generation && phase == StructuralPhase::Recording,
                    "Record generation/state 无效"
                );
                let surface = committed_surface.context("Record 缺少 active surface")?;
                let record = parse_record(&body, generation, surface, &mut counters)?;
                ensure!(
                    records.len() < MVS_CAPTURE_V2_MAX_RECORDS,
                    "V2 record 数量超过上限"
                );
                records.push(record);
            }
            EVENT_GAP => {
                ensure!(
                    generation == current_generation
                        && phase == StructuralPhase::Recording
                        && gaps.is_empty(),
                    "Gap generation 或数量无效"
                );
                let gap = parse_gap(&body, counters.payload)?;
                pending_gap_terminal = Some(gap.reason);
                gaps.push(gap);
            }
            EVENT_CLEAN | EVENT_ABORTED => {
                ensure!(generation == current_generation, "terminal generation 无效");
                ensure!(
                    kind != EVENT_CLEAN || phase == StructuralPhase::Recording,
                    "Clean 必须在 Recording 后"
                );
                ensure!(
                    kind != EVENT_ABORTED || phase != StructuralPhase::Start,
                    "Aborted 必须在 Created 后"
                );
                terminal = parse_terminal(kind, &body, timestamp_us)?;
                validate_terminal_phase(&terminal, phase)?;
                event_types.push(kind);
                event_generations.push(generation);
                break;
            }
            _ => bail!("未知 V2 event type"),
        }
        event_types.push(kind);
        event_generations.push(generation);
    }
    let mut probe = [0u8; 1];
    ensure!(
        reader.read(&mut probe)? == 0,
        "V2 terminal 后存在 trailing bytes"
    );
    validate_terminal(&terminal, &records, &gaps, deadline_ms, record_limit)?;
    Ok(StructuralMvsCaptureV2 {
        provenance: MvsCaptureV2Provenance::DiagnosticOnly,
        committed_surface,
        requested_surface,
        records,
        surfaces,
        gaps,
        terminal,
        deadline_ms,
        record_limit,
        event_types,
        event_generations,
    })
}

pub fn read_mvs_capture_v2_strict_cold<R: Read>(reader: &mut R) -> Result<StrictColdMvsCaptureV2> {
    let structural = read_mvs_capture_v2_structural(reader)?;
    ensure!(
        structural.event_types.len() >= 5,
        "严格 cold capture 缺少生命周期事件"
    );
    ensure!(
        structural.event_types[..4]
            == [EVENT_CREATED, EVENT_ARMED, EVENT_TRIGGERED, EVENT_RECORDING],
        "严格 cold 生命周期顺序无效"
    );
    ensure!(
        structural.event_types[4..structural.event_types.len() - 1]
            .iter()
            .all(|kind| *kind == EVENT_RECORD),
        "严格 cold 拒绝重复生命周期或未知中间 event"
    );
    ensure!(
        *structural.event_types.last().unwrap() == EVENT_CLEAN,
        "严格 cold 要求最后 event 为 Clean"
    );
    ensure!(
        structural
            .event_generations
            .iter()
            .all(|generation| *generation == 0),
        "严格 cold generation 必须为零"
    );
    ensure!(
        structural.surfaces.is_empty() && structural.gaps.is_empty(),
        "严格 cold 拒绝诊断事件"
    );
    ensure!(
        structural.terminal.kind == MvsCaptureV2TerminalKind::Clean,
        "严格 cold 要求 Clean terminal"
    );
    let committed_surface = structural
        .committed_surface
        .context("严格 cold 缺少 committed surface")?;
    let requested_surface = structural
        .requested_surface
        .context("严格 cold 缺少 requested surface")?;
    Ok(StrictColdMvsCaptureV2 {
        committed_surface,
        requested_surface,
        records: structural.records,
        deadline_ms: structural.deadline_ms.context("严格 cold 缺少 deadline")?,
        record_limit: structural
            .record_limit
            .context("严格 cold 缺少 record limit")?,
        terminal: structural.terminal,
    })
}

fn validate_header(header: &[u8]) -> Result<()> {
    ensure!(
        header[0..8] == MVS_CAPTURE_V2_MAGIC,
        "不是 FRDMVS02 capture"
    );
    ensure!(
        u16_be(&header[8..10]) == MVS_CAPTURE_V2_HEADER_BYTES as u16,
        "V2 header length 无效"
    );
    ensure!(
        header[10] == 2 && header[11] == 0 && header[12] == 1 && header[13] == 0,
        "V2 version/endian/checksum 无效"
    );
    ensure!(u16_be(&header[14..16]) == 1, "V2 header flags 无效");
    ensure!(
        u32_be(&header[16..20]) as usize == MVS_CAPTURE_V2_MAX_PAYLOAD,
        "V2 payload limit 无效"
    );
    ensure!(
        u32_be(&header[20..24]) as usize == MVS_CAPTURE_V2_MAX_EVENT_BODY,
        "V2 event body limit 无效"
    );
    ensure!(
        u32_be(&header[24..28]) as usize == MVS_CAPTURE_V2_MAX_RECORDS,
        "V2 record limit 无效"
    );
    ensure!(
        u32_be(&header[28..32]) == MVS_CAPTURE_V2_MAX_DURATION_MS,
        "V2 duration limit 无效"
    );
    ensure!(
        MVS_CAPTURE_V2_MAX_PAYLOAD == MAX_MVS_RECORD_PAYLOAD,
        "V2 MVS payload limit 与 transport 不一致"
    );
    Ok(())
}

fn geometry(width: u16, height: u16) -> Result<MvsCaptureV2Geometry> {
    ensure!(width != 0 && height != 0, "surface geometry 为零");
    validate_framebuffer_geometry(width.into(), height.into())?;
    Ok(MvsCaptureV2Geometry { width, height })
}

fn parse_record(
    body: &[u8],
    generation: u64,
    surface: MvsCaptureV2Geometry,
    counters: &mut CaptureCounters,
) -> Result<MvsCaptureV2Record> {
    ensure!(body.len() >= 29, "Record body 太短");
    let first = u64_be(&body[0..8]);
    let last = u64_be(&body[8..16]);
    ensure!(
        first <= last && last != u64::MAX,
        "Record source range 无效"
    );
    let rect = MvsRect {
        x: u16_be(&body[16..18]),
        y: u16_be(&body[18..20]),
        width: u16_be(&body[20..22]),
        height: u16_be(&body[22..24]),
    };
    let payload_len = u32_be(&body[24..28]) as usize;
    validate_record_payload_length(payload_len, body.len())?;
    let payload = &body[28..];
    match payload[0] {
        2 => ensure!(
            rect == MvsRect {
                x: 0,
                y: 0,
                width: 0,
                height: 0
            },
            "type-2 rectangle 必须为零"
        ),
        0 | 1 => validate_record_rect(rect, surface)?,
        _ => bail!("未知 MVS payload tag"),
    }
    counters.reserve_payload(payload_len)?;
    Ok(MvsCaptureV2Record {
        generation,
        first_source_frame_ordinal: first,
        last_source_frame_ordinal: last,
        record: MvsRecord {
            rect,
            payload: payload.to_vec(),
        },
    })
}

fn validate_record_rect(rect: MvsRect, surface: MvsCaptureV2Geometry) -> Result<()> {
    #[cfg(any(feature = "media", test))]
    crate::vnc::mvs_stream::validate_mvs_rect_against_surface(rect, surface.width, surface.height)?;
    ensure!(
        rect.width != 0 && rect.height != 0,
        "MVS record rectangle 为零"
    );
    let right = rect
        .x
        .checked_add(rect.width)
        .context("MVS record rectangle right 溢出")?;
    let bottom = rect
        .y
        .checked_add(rect.height)
        .context("MVS record rectangle bottom 溢出")?;
    ensure!(
        right <= surface.width && bottom <= surface.height,
        "MVS record rectangle 超出 committed surface"
    );
    let pixels = usize::from(rect.width)
        .checked_mul(usize::from(rect.height))
        .context("MVS record rectangle pixel 溢出")?;
    ensure!(
        pixels <= MAX_MVS_DECODE_PIXELS,
        "MVS record rectangle 超过像素预算"
    );
    Ok(())
}

fn parse_gap(body: &[u8], cumulative_before: usize) -> Result<MvsCaptureV2Gap> {
    ensure!(body.len() == 40, "Gap body 长度无效");
    let reason = u16_be(&body[0..2]);
    let stage = u16_be(&body[2..4]);
    ensure!(
        gap_stage(reason)? == stage && u32_be(&body[4..8]) == 1,
        "Gap reason/stage/flags 无效"
    );
    let first = u64_be(&body[8..16]);
    let last = u64_be(&body[16..24]);
    ensure!(
        first <= last && first != u64::MAX && last != u64::MAX,
        "Gap frame range 无效"
    );
    let declared_total = u32_be(&body[24..28]);
    let accumulated_bytes = u32_be(&body[28..32]);
    let rect = MvsRect {
        x: u16_be(&body[32..34]),
        y: u16_be(&body[34..36]),
        width: u16_be(&body[36..38]),
        height: u16_be(&body[38..40]),
    };
    match reason {
        1 | 9 => ensure!(
            declared_total == 0 && accumulated_bytes == 0 && first == last,
            "Gap immediate diagnostic 字段无效"
        ),
        2 => ensure!(
            (declared_total == 0 || declared_total as usize > MVS_CAPTURE_V2_MAX_PAYLOAD)
                && accumulated_bytes == 0
                && first == last,
            "Gap reason 2 total 无效"
        ),
        3 => ensure!(
            (1..=MVS_CAPTURE_V2_MAX_PAYLOAD).contains(&(declared_total as usize))
                && accumulated_bytes == 0
                && first == last,
            "Gap reason 3 字段无效"
        ),
        4 | 10 => ensure!(
            (1..=MVS_CAPTURE_V2_MAX_PAYLOAD).contains(&(declared_total as usize))
                && accumulated_bytes < declared_total
                && first < last,
            "Gap pending/offending 字段无效"
        ),
        11 => ensure!(
            (1..=MVS_CAPTURE_V2_MAX_PAYLOAD).contains(&(declared_total as usize))
                && accumulated_bytes == 0
                && first == last
                && cumulative_before
                    .checked_add(declared_total as usize)
                    .map(|total| total > MVS_CAPTURE_V2_MAX_CUMULATIVE_PAYLOAD)
                    .unwrap_or(true),
            "Gap reason 11 字段无效"
        ),
        _ => ensure!(
            (1..=MVS_CAPTURE_V2_MAX_PAYLOAD).contains(&(declared_total as usize))
                && accumulated_bytes < declared_total,
            "Gap pending 字段无效"
        ),
    }
    if reason == 9 {
        ensure!(
            rect == MvsRect {
                x: 0,
                y: 0,
                width: 0,
                height: 0
            },
            "Gap reason 9 rectangle 无效"
        );
    }
    Ok(MvsCaptureV2Gap {
        reason,
        stage,
        first_source_frame_ordinal: first,
        last_source_frame_ordinal: last,
        declared_total,
        accumulated_bytes,
        rect,
    })
}

fn parse_terminal(kind: u16, body: &[u8], timestamp_us: u64) -> Result<MvsCaptureV2Terminal> {
    ensure!(body.len() == 48, "terminal body 长度无效");
    ensure!(u16_be(&body[2..4]) == 0, "terminal reserved 非零");
    let raw_reason = u16_be(&body[0..2]);
    let reason = terminal_reason(raw_reason)?;
    let terminal_kind = match kind {
        EVENT_CLEAN => MvsCaptureV2TerminalKind::Clean,
        EVENT_ABORTED => MvsCaptureV2TerminalKind::Aborted,
        _ => bail!("terminal type 无效"),
    };
    match terminal_kind {
        MvsCaptureV2TerminalKind::Clean => {
            ensure!((1..=2).contains(&raw_reason), "Clean reason/type 无效")
        }
        MvsCaptureV2TerminalKind::Aborted => ensure!(
            (257..=267).contains(&raw_reason),
            "Aborted reason/type 无效"
        ),
    }
    Ok(MvsCaptureV2Terminal {
        kind: terminal_kind,
        reason,
        timestamp_us,
        gap_count: u32_be(&body[4..8]),
        source_mvs_frame_count: u64_be(&body[8..16]),
        record_count: u64_be(&body[16..24]),
        type0_count: u64_be(&body[24..32]),
        type1_count: u64_be(&body[32..40]),
        type2_count: u64_be(&body[40..48]),
    })
}

fn validate_terminal_phase(terminal: &MvsCaptureV2Terminal, phase: StructuralPhase) -> Result<()> {
    use MvsCaptureV2TerminalReason::*;
    let valid = match terminal.reason {
        ConnectFailure | CredentialOrAuthenticationFailure => phase == StructuralPhase::Created,
        ArmOutputFailure => matches!(phase, StructuralPhase::Created | StructuralPhase::Armed),
        TriggerFailure => matches!(
            phase,
            StructuralPhase::Armed | StructuralPhase::Triggered | StructuralPhase::Recording
        ),
        ReadFailure | AssemblerFailure | PendingAtDeadline => phase == StructuralPhase::Recording,
        OperatorCancellation => phase != StructuralPhase::Start,
        OutputFailure => phase != StructuralPhase::Start,
        InvalidGeometry => matches!(phase, StructuralPhase::Created | StructuralPhase::Recording),
        PreTriggerDeadline => matches!(phase, StructuralPhase::Created | StructuralPhase::Armed),
        Deadline | RecordLimit => {
            terminal.kind == MvsCaptureV2TerminalKind::Clean && phase == StructuralPhase::Recording
        }
    };
    ensure!(valid, "terminal reason 与已持久化 phase 不兼容");
    Ok(())
}

fn validate_terminal(
    terminal: &MvsCaptureV2Terminal,
    records: &[MvsCaptureV2Record],
    gaps: &[MvsCaptureV2Gap],
    deadline_ms: Option<u32>,
    record_limit: Option<u32>,
) -> Result<()> {
    ensure!(
        terminal.gap_count as usize == gaps.len(),
        "terminal gap count 不匹配"
    );
    match terminal.kind {
        MvsCaptureV2TerminalKind::Clean => ensure!(gaps.is_empty(), "Clean 的 gap count 必须为零"),
        MvsCaptureV2TerminalKind::Aborted => ensure!(gaps.len() <= 1, "Aborted gap count 无效"),
    }
    match terminal.reason {
        MvsCaptureV2TerminalReason::AssemblerFailure => {
            let gap = gaps.first().context("Aborted 262 必须有 Gap")?;
            ensure!(
                gap_terminal_code(gap.reason)? == 262,
                "Aborted 262 Gap 映射无效"
            );
        }
        MvsCaptureV2TerminalReason::PendingAtDeadline => {
            ensure!(
                gaps.len() == 1 && gaps[0].reason == 6,
                "Aborted 263 必须有 Gap6"
            );
        }
        _ => {}
    }
    let mut expected_source = 0u64;
    let mut type0 = 0u64;
    let mut type1 = 0u64;
    let mut type2 = 0u64;
    for record in records {
        ensure!(
            record.first_source_frame_ordinal == expected_source,
            "Record source range 不连续"
        );
        expected_source = record
            .last_source_frame_ordinal
            .checked_add(1)
            .context("Record source range 溢出")?;
        match record.record.payload[0] {
            0 => type0 += 1,
            1 => type1 += 1,
            2 => type2 += 1,
            _ => bail!("未知 MVS payload tag"),
        }
    }
    if let Some(gap) = gaps.first() {
        ensure!(
            terminal.kind == MvsCaptureV2TerminalKind::Aborted,
            "Gap 必须以 Aborted 终止"
        );
        ensure!(
            gap.first_source_frame_ordinal == expected_source,
            "Gap source range 不连续"
        );
        expected_source = gap
            .last_source_frame_ordinal
            .checked_add(1)
            .context("Gap source range 溢出")?;
    }
    ensure!(
        terminal.source_mvs_frame_count == expected_source,
        "terminal source frame count 不匹配"
    );
    ensure!(
        terminal.record_count == records.len() as u64,
        "terminal record count 不匹配"
    );
    if let Some(limit) = record_limit {
        ensure!(
            terminal.record_count <= u64::from(limit),
            "terminal record count 超过 Created limit"
        );
    }
    ensure!(
        terminal.type0_count == type0
            && terminal.type1_count == type1
            && terminal.type2_count == type2,
        "terminal tag count 不匹配"
    );
    ensure!(
        type0
            .checked_add(type1)
            .and_then(|count| count.checked_add(type2))
            == Some(terminal.record_count),
        "terminal tag count 总和不匹配"
    );
    match terminal.kind {
        MvsCaptureV2TerminalKind::Clean => {
            let deadline_us = u64::from(deadline_ms.context("Clean 缺少 Created")?)
                .checked_mul(1000)
                .context("deadline 微秒溢出")?;
            let limit = u64::from(record_limit.context("Clean 缺少 Created record limit")?);
            match terminal.reason {
                MvsCaptureV2TerminalReason::Deadline => ensure!(
                    terminal.timestamp_us >= deadline_us && terminal.record_count <= limit,
                    "Clean deadline 规则无效"
                ),
                MvsCaptureV2TerminalReason::RecordLimit => ensure!(
                    terminal.timestamp_us < deadline_us && terminal.record_count == limit,
                    "Clean record limit 规则无效"
                ),
                _ => bail!("Clean reason 无效"),
            }
        }
        MvsCaptureV2TerminalKind::Aborted => {
            ensure!(terminal.gap_count <= 1, "Aborted gap count 无效")
        }
    }
    Ok(())
}

fn gap_stage(reason: u16) -> Result<u16> {
    Ok(match reason {
        1 | 2 | 3 | 10 => 1,
        4 | 9 => 2,
        5 => 3,
        6 => 4,
        7 => 5,
        8 => 6,
        12 => 7,
        11 => 8,
        _ => bail!("未知 Gap reason"),
    })
}

fn gap_terminal_code(reason: u16) -> Result<u16> {
    Ok(match reason {
        1 | 2 | 3 | 4 | 5 | 9 | 10 | 11 => 262,
        6 => 263,
        7 => 261,
        8 => 266,
        12 => 265,
        _ => bail!("未知 Gap reason"),
    })
}

fn terminal_reason(reason: u16) -> Result<MvsCaptureV2TerminalReason> {
    Ok(match reason {
        1 => MvsCaptureV2TerminalReason::Deadline,
        2 => MvsCaptureV2TerminalReason::RecordLimit,
        257 => MvsCaptureV2TerminalReason::ConnectFailure,
        258 => MvsCaptureV2TerminalReason::CredentialOrAuthenticationFailure,
        259 => MvsCaptureV2TerminalReason::ArmOutputFailure,
        260 => MvsCaptureV2TerminalReason::TriggerFailure,
        261 => MvsCaptureV2TerminalReason::ReadFailure,
        262 => MvsCaptureV2TerminalReason::AssemblerFailure,
        263 => MvsCaptureV2TerminalReason::PendingAtDeadline,
        264 => MvsCaptureV2TerminalReason::OutputFailure,
        265 => MvsCaptureV2TerminalReason::OperatorCancellation,
        266 => MvsCaptureV2TerminalReason::InvalidGeometry,
        267 => MvsCaptureV2TerminalReason::PreTriggerDeadline,
        _ => bail!("未知 terminal reason"),
    })
}

fn read_exact<R: Read>(reader: &mut R, bytes: &mut [u8], field: &'static str) -> Result<()> {
    reader.read_exact(bytes).with_context(|| field)
}

fn u16_be(bytes: &[u8]) -> u16 {
    u16::from_be_bytes(bytes.try_into().unwrap())
}
fn u32_be(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().unwrap())
}
fn u64_be(bytes: &[u8]) -> u64 {
    u64::from_be_bytes(bytes.try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};

    fn read_structural(data: &[u8]) -> anyhow::Result<StructuralMvsCaptureV2> {
        let mut reader = Cursor::new(data);
        read_mvs_capture_v2_structural(&mut reader)
    }

    fn read_strict(data: &[u8]) -> anyhow::Result<StrictColdMvsCaptureV2> {
        let mut reader = Cursor::new(data);
        read_mvs_capture_v2_strict_cold(&mut reader)
    }

    #[test]
    fn structural_reader_accepts_early_aborted_after_created_and_reads_chunks() {
        let bytes = capture_with_abort();
        let mut reader = ShortReader::new(bytes, 3);
        assert!(read_mvs_capture_v2_structural(&mut reader).is_ok());
    }

    #[test]
    fn structural_reader_accepts_early_aborted_at_each_pre_recording_phase() {
        for phase in 0..=3u64 {
            let mut bytes = header();
            bytes.extend(event(1, 0, 0, 0, &created_body()));
            if phase >= 1 {
                bytes.extend(event(2, 1, 0, 1, &armed_body((640, 480), (640, 480))));
            }
            if phase >= 2 {
                bytes.extend(event(
                    3,
                    2,
                    0,
                    2,
                    &join(&[
                        &640u16.to_be_bytes(),
                        &480u16.to_be_bytes(),
                        &7u32.to_be_bytes(),
                        &0u64.to_be_bytes(),
                    ]),
                ));
            }
            if phase >= 3 {
                bytes.extend(event(4, 3, 0, 3, &0u64.to_be_bytes()));
            }
            let ordinal = phase + 1;
            let reason = [257, 267, 260, 260][phase as usize];
            bytes.extend(event(
                0xff,
                ordinal,
                0,
                ordinal,
                &terminal_body(reason, 0, 0, 0, 0, 0),
            ));
            assert!(read_structural(&bytes).is_ok(), "phase {phase}");
        }
    }

    #[test]
    fn terminal_reason_must_match_type_and_persisted_phase() {
        let mut created_trigger_failure = header();
        created_trigger_failure.extend(event(1, 0, 0, 0, &created_body()));
        created_trigger_failure.extend(event(0xff, 1, 0, 1, &terminal_body(260, 0, 0, 0, 0, 0)));
        assert!(read_structural(&created_trigger_failure).is_err());

        let mut created_clean = header();
        created_clean.extend(event(1, 0, 0, 0, &created_body()));
        created_clean.extend(event(
            0xfe,
            1,
            0,
            30_000_000,
            &terminal_body(257, 0, 0, 0, 0, 0),
        ));
        assert!(read_structural(&created_clean).is_err());

        let mut created_deadline_abort = header();
        created_deadline_abort.extend(event(1, 0, 0, 0, &created_body()));
        created_deadline_abort.extend(event(0xff, 1, 0, 1, &terminal_body(1, 0, 0, 0, 0, 0)));
        assert!(read_structural(&created_deadline_abort).is_err());
    }

    #[test]
    fn cancellation_is_valid_at_every_persisted_phase_but_262_263_need_their_gap() {
        for phase in 0..=3u64 {
            let bytes = abort_at_phase(phase, 265);
            assert!(read_structural(&bytes).is_ok(), "cancel phase {phase}");
        }
        assert!(read_structural(&abort_at_phase(3, 262)).is_err());
        assert!(read_structural(&abort_at_phase(3, 263)).is_err());
    }

    #[test]
    fn structural_phase_rejects_record_before_recording() {
        let mut bytes = header();
        bytes.extend(event(1, 0, 0, 0, &created_body()));
        bytes.extend(event(2, 1, 0, 1, &armed_body((640, 480), (640, 480))));
        bytes.extend(event(
            0x20,
            2,
            0,
            2,
            &record_body(0, 0, rect(0, 0, 1, 1), &[0]),
        ));
        bytes.extend(event(0xff, 3, 0, 3, &terminal_body(260, 0, 1, 1, 1, 0)));
        let mut reader = Cursor::new(bytes);
        assert!(read_mvs_capture_v2_structural(&mut reader).is_err());
    }

    #[test]
    fn counters_do_not_commit_failed_reservations() {
        let mut counters = CaptureCounters {
            events: MVS_CAPTURE_V2_MAX_EVENTS - 1,
            payload: MVS_CAPTURE_V2_MAX_CUMULATIVE_PAYLOAD - 1,
        };
        assert!(counters.reserve_event().is_ok());
        assert!(counters.reserve_event().is_err());
        assert_eq!(counters.events, MVS_CAPTURE_V2_MAX_EVENTS);
        assert!(counters.reserve_payload(1).is_ok());
        assert!(counters.reserve_payload(1).is_err());
        assert_eq!(counters.payload, MVS_CAPTURE_V2_MAX_CUMULATIVE_PAYLOAD);
    }

    #[test]
    fn structural_reader_accepts_4102_events_and_rejects_4103rd_before_terminal() {
        assert!(read_structural(&event_cap_fixture(false)).is_ok());
        assert!(read_structural(&event_cap_fixture(true)).is_err());
    }

    #[test]
    fn stream_rejects_oversized_event_before_reading_its_body() {
        let mut bytes = header();
        bytes.extend_from_slice(&((MVS_CAPTURE_V2_MAX_EVENT_BYTES + 1) as u32).to_be_bytes());
        bytes.extend_from_slice(&[0; EVENT_PREFIX_BYTES - 4]);
        let mut reader = ShortReader::new(bytes, 1);
        assert!(read_mvs_capture_v2_structural(&mut reader).is_err());
        assert_eq!(
            reader.consumed(),
            MVS_CAPTURE_V2_HEADER_BYTES + EVENT_PREFIX_BYTES
        );
    }

    #[test]
    fn literal_header_and_event_sizes_are_frozen() {
        assert_eq!(MVS_CAPTURE_V2_MAGIC, *b"FRDMVS02");
        assert_eq!(MVS_CAPTURE_V2_HEADER_BYTES, 32);
        assert_eq!(MVS_CAPTURE_V2_MAX_PAYLOAD, 0x0100_0000);
        assert_eq!(MVS_CAPTURE_V2_MAX_EVENT_BODY, 0x0100_001c);
        assert_eq!(MVS_CAPTURE_V2_MAX_EVENT_BYTES, 0x0100_003c);
        assert_eq!(MVS_CAPTURE_V2_MAX_RECORDS, 4096);
        assert_eq!(MVS_CAPTURE_V2_MAX_EVENTS, 4102);
        assert_eq!(MVS_CAPTURE_V2_MAX_CUMULATIVE_PAYLOAD, 0x2000_0000);
        assert_eq!(MVS_CAPTURE_V2_MAX_DURATION_MS, 30_000);
    }

    #[test]
    fn literal_event_bodies_prefixes_and_reserved_fields_are_exact() {
        let created = created_body();
        assert_eq!(
            created,
            [
                0x00, 0x00, 0x75, 0x30, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00,
                0x00, 0x00,
            ]
        );
        let armed = armed_body((0x0123, 0x0456), (0x0789, 0x0abc));
        assert_eq!(
            armed,
            [
                0x01, 0x23, 0x04, 0x56, 0x07, 0x89, 0x0a, 0xbc, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00,
                0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ]
        );
        let triggered = join(&[
            &0x0789u16.to_be_bytes(),
            &0x0abcu16.to_be_bytes(),
            &7u32.to_be_bytes(),
            &0u64.to_be_bytes(),
        ]);
        assert_eq!(
            triggered,
            [
                0x07, 0x89, 0x0a, 0xbc, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00,
            ]
        );
        let recording = 0u64.to_be_bytes();
        assert_eq!(recording, [0; 8]);
        let surface = surface_body(0x0280, 0x01e0);
        assert_eq!(surface, [0x02, 0x80, 0x01, 0xe0, 0x00, 0x01, 0x00, 0x00]);
        let record = record_body(
            0x0102_0304_0506_0708,
            0x1112_1314_1516_1718,
            rect(0x2122, 0x3132, 0x4142, 0x5152),
            &[0x00, 0xaa, 0x55],
        );
        assert_eq!(
            record,
            [
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
                0x17, 0x18, 0x21, 0x22, 0x31, 0x32, 0x41, 0x42, 0x51, 0x52, 0x00, 0x00, 0x00, 0x03,
                0x00, 0xaa, 0x55,
            ]
        );
        let gap = gap_body(
            7,
            5,
            1,
            0x0102_0304_0506_0708,
            0x1112_1314_1516_1718,
            0x0012_3456,
            0x0001_0203,
            rect(0x2122, 0x3132, 0x4142, 0x5152),
        );
        assert_eq!(
            gap,
            [
                0x00, 0x07, 0x00, 0x05, 0x00, 0x00, 0x00, 0x01, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06,
                0x07, 0x08, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x00, 0x12, 0x34, 0x56,
                0x00, 0x01, 0x02, 0x03, 0x21, 0x22, 0x31, 0x32, 0x41, 0x42, 0x51, 0x52,
            ]
        );
        let clean = terminal_body_full(1, 0, 3, 3, 1, 1, 1);
        assert_eq!(
            clean,
            [
                0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
            ]
        );
        let aborted = terminal_body_full(257, 0, 0, 0, 0, 0, 0);
        assert_eq!(
            aborted,
            [
                0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ]
        );

        for (kind, body, total) in [
            (EVENT_CREATED, created.as_slice(), 48),
            (EVENT_ARMED, armed.as_slice(), 56),
            (EVENT_TRIGGERED, triggered.as_slice(), 48),
            (EVENT_RECORDING, recording.as_slice(), 40),
            (EVENT_SURFACE, surface.as_slice(), 40),
            (EVENT_RECORD, record.as_slice(), 63),
            (EVENT_GAP, gap.as_slice(), 72),
            (EVENT_CLEAN, clean.as_slice(), 80),
            (EVENT_ABORTED, aborted.as_slice(), 80),
        ] {
            assert_eq!(encode_event(kind, 1, 2, 3, body).unwrap().len(), total);
        }
        let encoded_record = encode_event(
            EVENT_RECORD,
            0x0102_0304_0506_0708,
            0x1112_1314_1516_1718,
            0x2122_2324_2526_2728,
            &record,
        )
        .unwrap();
        assert_eq!(
            &encoded_record[..32],
            &[
                0x00, 0x00, 0x00, 0x3f, 0x00, 0x20, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06,
                0x07, 0x08, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x21, 0x22, 0x23, 0x24,
                0x25, 0x26, 0x27, 0x28,
            ]
        );
        assert_eq!(&encoded_record[32..], record.as_slice());

        let clean_capture = clean_fixture();
        let reserved_mutations = [
            rewrite_byte(&clean_capture, 32 + 32 + 12, 1),
            rewrite_byte(&clean_capture, 80 + 32 + 16, 1),
            rewrite_byte(&clean_capture, clean_capture.len() - 48 + 2, 1),
        ];
        for mutation in reserved_mutations {
            assert!(read_structural(&mutation).is_err());
        }
        let surface_capture = surface_transition_fixture(1, (1, 2), None);
        assert!(read_structural(&rewrite_byte(&surface_capture, 224 + 32 + 6, 1,)).is_err());
        let aborted_capture = capture_with_abort();
        assert!(read_structural(&rewrite_byte(
            &aborted_capture,
            aborted_capture.len() - 48 + 2,
            1,
        ))
        .is_err());
    }

    #[test]
    fn structural_reader_rejects_nonzero_triggered_trailing_u64_in_complete_lifecycle() {
        let mut capture = clean_fixture();
        let triggered_body_offset = MVS_CAPTURE_V2_HEADER_BYTES + 48 + 56 + EVENT_PREFIX_BYTES;
        capture[triggered_body_offset + 8..triggered_body_offset + 16]
            .copy_from_slice(&1u64.to_be_bytes());

        assert!(read_structural(&capture).is_err());
    }

    #[test]
    fn structural_reader_rejects_nonzero_recording_u64_in_complete_lifecycle() {
        let mut capture = clean_fixture();
        let recording_body_offset = MVS_CAPTURE_V2_HEADER_BYTES + 48 + 56 + 48 + EVENT_PREFIX_BYTES;
        capture[recording_body_offset..recording_body_offset + 8]
            .copy_from_slice(&1u64.to_be_bytes());

        assert!(read_structural(&capture).is_err());
    }

    #[test]
    fn record_payload_length_boundaries_are_allocation_safe() {
        assert!(validate_record_payload_length(0, 28).is_err());
        assert!(validate_record_payload_length(
            MVS_CAPTURE_V2_MAX_PAYLOAD,
            28 + MVS_CAPTURE_V2_MAX_PAYLOAD,
        )
        .is_ok());
        assert!(validate_record_payload_length(
            MVS_CAPTURE_V2_MAX_PAYLOAD + 1,
            29 + MVS_CAPTURE_V2_MAX_PAYLOAD,
        )
        .is_err());
        assert!(validate_record_payload_length(usize::MAX, usize::MAX).is_err());
        assert!(validate_record_payload_length(1, 28).is_err());
        assert!(validate_record_payload_length(1, 30).is_err());
    }

    #[test]
    fn surface_generation_budget_and_committed_geometry_matrix_is_enforced() {
        assert!(read_structural(&surface_transition_fixture(1, (1, 2), None)).is_ok());
        assert!(read_structural(&surface_transition_fixture(2, (1, 2), None)).is_err());
        assert!(read_structural(&surface_transition_fixture(0, (1, 2), None)).is_err());
        assert!(
            read_structural(&surface_transition_fixture(1, (u16::MAX, u16::MAX), None,)).is_err()
        );
        assert!(read_structural(&surface_transition_fixture(
            1,
            (1, 2),
            Some(rect(1, 0, 1, 1)),
        ))
        .is_err());
        assert!(read_structural(&surface_transition_fixture(
            1,
            (1, 2),
            Some(rect(0, 1, 1, 1)),
        ))
        .is_ok());
    }

    #[test]
    fn strict_reader_accepts_hand_derived_clean_capture() {
        let capture = clean_fixture();
        let parsed = read_strict(&capture).unwrap();
        assert_eq!(parsed.records.len(), 3);
        assert_eq!(parsed.committed_surface.width, 640);
        assert_eq!(parsed.requested_surface.width, 1920);
        assert_eq!(parsed.terminal.reason, MvsCaptureV2TerminalReason::Deadline);
    }

    #[test]
    fn structural_reader_labels_valid_diagnostic_capture_only() {
        let mut capture = clean_fixture();
        insert_before_terminal(
            &mut capture,
            event(0x0010, 7, 1, 10, &surface_body(800, 600)),
        );
        replace_terminal_ordinal(&mut capture, 8);
        replace_terminal_generation(&mut capture, 1);
        let parsed = read_structural(&capture).unwrap();
        assert_eq!(parsed.provenance, MvsCaptureV2Provenance::DiagnosticOnly);
        assert_eq!(parsed.surfaces.len(), 1);
        assert!(read_strict(&capture).is_err());
    }

    #[test]
    fn strict_rejects_diagnostic_events_and_v1() {
        assert!(read_strict(&capture_with_surface()).is_err());
        assert!(read_strict(&capture_with_gap_then_abort()).is_err());
        assert!(read_strict(&capture_with_abort()).is_err());
        assert!(read_strict(b"FRDMVS01").is_err());
    }

    #[test]
    fn strict_rejects_duplicate_lifecycle_event_after_recording() {
        let mut bytes = clean_fixture();
        insert_before_terminal(&mut bytes, event(4, 7, 0, 7, &0u64.to_be_bytes()));
        replace_terminal_ordinal(&mut bytes, 8);
        assert!(read_strict(&bytes).is_err());
    }

    #[test]
    fn requested_geometry_never_expands_active_surface() {
        let bytes = clean_capture_with_geometry((640, 480), (1920, 1080), rect(0, 0, 641, 1));
        assert!(read_structural(&bytes).is_err());
    }

    #[test]
    fn format_two_gap_always_has_present_non_sentinel_range() {
        assert!(parse_gap_fixture(1, 0, 0).is_ok());
        for (flags, first, last) in [(0, 0, 0), (2, 0, 0), (1, u64::MAX, 0), (1, 0, u64::MAX)] {
            assert!(parse_gap_fixture(flags, first, last).is_err());
        }
    }

    #[test]
    fn all_gap_rows_reach_parser_and_reject_each_fixed_field_mutation() {
        for case in gap_cases() {
            let valid = gap_body_for_case(case);
            let parsed = parse_gap(&valid, case.cumulative_before).unwrap();
            assert_eq!(parsed.reason, case.reason);
            assert_eq!(parsed.stage, case.stage);
            assert_eq!(parsed.first_source_frame_ordinal, case.first);
            assert_eq!(parsed.last_source_frame_ordinal, case.last);
            assert_eq!(parsed.declared_total, case.declared);
            assert_eq!(parsed.accumulated_bytes, case.accumulated);
            assert_eq!(parsed.rect, rect_from_tuple(case.rectangle));
            assert_eq!(gap_terminal_code(case.reason).unwrap(), case.terminal);

            let mut wrong_stage = case;
            wrong_stage.stage = case.stage + 1;
            assert!(parse_gap(&gap_body_for_case(wrong_stage), case.cumulative_before).is_err());
            let mut wrong_flags = valid.clone();
            wrong_flags[4..8].copy_from_slice(&0u32.to_be_bytes());
            assert!(parse_gap(&wrong_flags, case.cumulative_before).is_err());
            let mut wrong_first = case;
            wrong_first.first = u64::MAX;
            assert!(parse_gap(&gap_body_for_case(wrong_first), case.cumulative_before).is_err());
            let mut wrong_last = case;
            wrong_last.last = u64::MAX;
            assert!(parse_gap(&gap_body_for_case(wrong_last), case.cumulative_before).is_err());
            let mut wrong_declared = case;
            wrong_declared.declared = invalid_declared_for(case);
            assert!(parse_gap(&gap_body_for_case(wrong_declared), case.cumulative_before).is_err());
            let mut wrong_accumulated = case;
            wrong_accumulated.accumulated = invalid_accumulated_for(case);
            assert!(parse_gap(
                &gap_body_for_case(wrong_accumulated),
                case.cumulative_before
            )
            .is_err());
            if case.reason == 9 {
                let mut wrong_rectangle = case;
                wrong_rectangle.rectangle = (0, 0, 1, 1);
                assert!(
                    parse_gap(&gap_body_for_case(wrong_rectangle), case.cumulative_before,)
                        .is_err()
                );
            }
        }
    }

    #[test]
    fn every_structurally_representable_gap_row_requires_immediate_mapped_abort() {
        for case in gap_cases() {
            if case.reason == 11 {
                assert!(parse_gap(&gap_body_for_case(case), case.cumulative_before).is_ok());
                assert_eq!(gap_terminal_code(case.reason).unwrap(), case.terminal);
                continue;
            }
            let valid = gap_fixture_for_case(case, case.terminal);
            assert!(read_structural(&valid).is_ok(), "reason {}", case.reason);

            let wrong_terminal = if case.terminal == 262 { 261 } else { 262 };
            assert!(read_structural(&gap_fixture_for_case(case, wrong_terminal)).is_err());

            let missing_terminal = &valid[..valid.len() - 80];
            assert!(read_structural(missing_terminal).is_err());

            let mut intervening = valid.clone();
            let terminal = intervening.split_off(intervening.len() - 80);
            intervening.extend(event(
                EVENT_RECORD,
                5,
                0,
                5,
                &record_body(1, 1, rect(0, 0, 1, 1), &[0]),
            ));
            intervening.extend(terminal);
            assert!(read_structural(&intervening).is_err());
        }
    }

    #[test]
    fn every_aborted_reason_has_exact_positive_and_negative_phase_gap_compatibility() {
        let cases: [(u16, &[u64], Option<u16>); 11] = [
            (257, &[0], None),
            (258, &[0], None),
            (259, &[0, 1], None),
            (260, &[1, 2, 3], None),
            (261, &[3], Some(7)),
            (262, &[], Some(9)),
            (263, &[], Some(6)),
            (264, &[0, 1, 2, 3], None),
            (265, &[0, 1, 2, 3], Some(12)),
            (266, &[0, 3], Some(8)),
            (267, &[0, 1], None),
        ];
        for (reason, zero_gap_phases, mapped_gap) in cases {
            for phase in 0..=3 {
                assert_eq!(
                    read_structural(&abort_at_phase(phase, reason)).is_ok(),
                    zero_gap_phases.contains(&phase),
                    "Aborted {reason} zero-gap phase {phase}"
                );
            }
            if let Some(gap_reason) = mapped_gap {
                let case = gap_case(gap_reason);
                assert!(read_structural(&gap_fixture_for_case(case, reason)).is_ok());
            }
            let wrong_gap_reason = match mapped_gap {
                Some(9) => 7,
                _ => 9,
            };
            let wrong_gap = gap_case(wrong_gap_reason);
            assert!(
                read_structural(&gap_fixture_for_case(wrong_gap, reason)).is_err(),
                "Aborted {reason} accepted wrong Gap"
            );
        }

        let mut before_created = header();
        before_created.extend(event(
            EVENT_ABORTED,
            0,
            0,
            0,
            &terminal_body(264, 0, 0, 0, 0, 0),
        ));
        assert!(read_structural(&before_created).is_err());
    }

    #[test]
    fn reader_rejects_header_prefix_and_body_mutations() {
        let clean = clean_fixture();
        for mutation in [
            mutate(&clean, 0, 0),
            mutate(&clean, 10, 3),
            mutate(&clean, 12, 2),
            mutate(&clean, 13, 1),
            mutate(&clean, 15, 2),
            mutate(&clean, 35, 0),
            mutate(&clean, 36, 0xff),
            mutate(&clean, 38, 1),
        ] {
            assert!(read_structural(&mutation).is_err());
        }
    }

    #[test]
    fn reader_rejects_truncation_at_every_header_and_event_boundary() {
        let clean = clean_fixture();
        for cut in 0..clean.len() {
            assert!(read_structural(&clean[..cut]).is_err(), "cut {cut}");
        }
    }

    #[test]
    fn reader_rejects_ordinal_timestamp_generation_and_terminal_violations() {
        let clean = clean_fixture();
        for mutation in [
            rewrite_prefix_u64(&clean, 80, 0, 7),
            rewrite_prefix_u64(&clean, 80, 16, 1),
            rewrite_prefix_u64(&clean, 136, 24, 0),
            append_byte(&clean, 0),
        ] {
            assert!(read_structural(&mutation).is_err());
        }
    }

    #[test]
    fn record_and_counter_rules_are_recomputed() {
        let clean = clean_fixture();
        for mutation in [
            rewrite_u64_from_end(&clean, 8, 4),
            rewrite_u64_from_end(&clean, 16, 2),
            rewrite_u64_from_end(&clean, 24, 2),
            rewrite_u64_from_end(&clean, 32, 2),
            rewrite_u64_from_end(&clean, 40, 2),
        ] {
            assert!(read_structural(&mutation).is_err());
        }
    }

    #[test]
    fn limits_are_checked_without_large_allocations() {
        assert!(validate_event_length(MVS_CAPTURE_V2_MAX_EVENT_BYTES).is_ok());
        assert!(validate_event_length(MVS_CAPTURE_V2_MAX_EVENT_BYTES + 1).is_err());
        let mut counters = CaptureCounters::default();
        counters.reserve_event().unwrap();
        counters
            .reserve_payload(MVS_CAPTURE_V2_MAX_CUMULATIVE_PAYLOAD)
            .unwrap();
        assert!(counters.reserve_payload(1).is_err());
    }

    #[test]
    fn v1_wrapper_is_historical_unproven_and_strict_has_no_fallback() {
        let v1 = b"FRDMVS01\0\0\0\0\0\0\0\0\0\0\0\x01\x02";
        let wrapped = read_mvs_capture_v1_historical(v1).unwrap();
        assert_eq!(
            wrapped.provenance,
            MvsCaptureV2Provenance::HistoricalUnproven
        );
        assert!(read_strict(v1).is_err());
    }

    fn header() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FRDMVS02");
        bytes.extend_from_slice(&32u16.to_be_bytes());
        bytes.extend_from_slice(&[2, 0, 1, 0]);
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&0x0100_0000u32.to_be_bytes());
        bytes.extend_from_slice(&0x0100_001cu32.to_be_bytes());
        bytes.extend_from_slice(&4096u32.to_be_bytes());
        bytes.extend_from_slice(&30_000u32.to_be_bytes());
        bytes
    }

    fn event(kind: u16, ordinal: u64, generation: u64, timestamp: u64, body: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&((32 + body.len()) as u32).to_be_bytes());
        bytes.extend_from_slice(&kind.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&ordinal.to_be_bytes());
        bytes.extend_from_slice(&generation.to_be_bytes());
        bytes.extend_from_slice(&timestamp.to_be_bytes());
        bytes.extend_from_slice(body);
        bytes
    }

    fn created_body() -> Vec<u8> {
        join(&[
            &30_000u32.to_be_bytes(),
            &3u32.to_be_bytes(),
            &1u16.to_be_bytes(),
            &1u16.to_be_bytes(),
            &0u32.to_be_bytes(),
        ])
    }

    fn created_body_with_limit(limit: u32) -> Vec<u8> {
        join(&[
            &30_000u32.to_be_bytes(),
            &limit.to_be_bytes(),
            &1u16.to_be_bytes(),
            &1u16.to_be_bytes(),
            &0u32.to_be_bytes(),
        ])
    }

    fn armed_body(committed: (u16, u16), requested: (u16, u16)) -> Vec<u8> {
        join(&[
            &committed.0.to_be_bytes(),
            &committed.1.to_be_bytes(),
            &requested.0.to_be_bytes(),
            &requested.1.to_be_bytes(),
            &1u16.to_be_bytes(),
            &1u16.to_be_bytes(),
            &3u32.to_be_bytes(),
            &0u64.to_be_bytes(),
        ])
    }

    fn record_body(
        first: u64,
        last: u64,
        rect: crate::vnc::mvs_stream::MvsRect,
        payload: &[u8],
    ) -> Vec<u8> {
        join(&[
            &first.to_be_bytes(),
            &last.to_be_bytes(),
            &rect.x.to_be_bytes(),
            &rect.y.to_be_bytes(),
            &rect.width.to_be_bytes(),
            &rect.height.to_be_bytes(),
            &(payload.len() as u32).to_be_bytes(),
            payload,
        ])
    }

    fn terminal(
        kind: u16,
        ordinal: u64,
        timestamp: u64,
        reason: u16,
        source: u64,
        records: u64,
        type0: u64,
        type1: u64,
        type2: u64,
    ) -> Vec<u8> {
        event(
            kind,
            ordinal,
            0,
            timestamp,
            &join(&[
                &reason.to_be_bytes(),
                &0u16.to_be_bytes(),
                &0u32.to_be_bytes(),
                &source.to_be_bytes(),
                &records.to_be_bytes(),
                &type0.to_be_bytes(),
                &type1.to_be_bytes(),
                &type2.to_be_bytes(),
            ]),
        )
    }

    fn clean_fixture() -> Vec<u8> {
        let mut bytes = header();
        bytes.extend(event(1, 0, 0, 0, &created_body()));
        bytes.extend(event(2, 1, 0, 1, &armed_body((640, 480), (1920, 1080))));
        bytes.extend(event(
            3,
            2,
            0,
            2,
            &join(&[
                &1920u16.to_be_bytes(),
                &1080u16.to_be_bytes(),
                &7u32.to_be_bytes(),
                &0u64.to_be_bytes(),
            ]),
        ));
        bytes.extend(event(4, 3, 0, 3, &0u64.to_be_bytes()));
        bytes.extend(event(
            0x20,
            4,
            0,
            4,
            &record_body(0, 0, rect(0, 0, 0, 0), &[2]),
        ));
        bytes.extend(event(
            0x20,
            5,
            0,
            5,
            &record_body(1, 1, rect(0, 0, 1, 1), &[0]),
        ));
        bytes.extend(event(
            0x20,
            6,
            0,
            6,
            &record_body(2, 2, rect(1, 1, 1, 1), &[1]),
        ));
        bytes.extend(terminal(0xfe, 7, 30_000_000, 1, 3, 3, 1, 1, 1));
        bytes
    }

    fn capture_with_surface() -> Vec<u8> {
        let mut bytes = clean_fixture();
        insert_before_terminal(&mut bytes, event(0x10, 7, 1, 7, &surface_body(640, 480)));
        replace_terminal_ordinal(&mut bytes, 8);
        replace_terminal_generation(&mut bytes, 1);
        bytes
    }

    fn surface_transition_fixture(
        surface_generation: u64,
        surface: (u16, u16),
        record_rect: Option<crate::vnc::mvs_stream::MvsRect>,
    ) -> Vec<u8> {
        let mut bytes = header();
        bytes.extend(event(1, 0, 0, 0, &created_body()));
        bytes.extend(event(2, 1, 0, 1, &armed_body((2, 1), (2, 1))));
        bytes.extend(event(
            3,
            2,
            0,
            2,
            &join(&[
                &2u16.to_be_bytes(),
                &1u16.to_be_bytes(),
                &7u32.to_be_bytes(),
                &0u64.to_be_bytes(),
            ]),
        ));
        bytes.extend(event(4, 3, 0, 3, &0u64.to_be_bytes()));
        bytes.extend(event(
            0x10,
            4,
            surface_generation,
            4,
            &surface_body(surface.0, surface.1),
        ));
        let (terminal_ordinal, source, records, type0) = if let Some(rectangle) = record_rect {
            bytes.extend(event(
                0x20,
                5,
                surface_generation,
                5,
                &record_body(0, 0, rectangle, &[0]),
            ));
            (6, 1, 1, 1)
        } else {
            (5, 0, 0, 0)
        };
        bytes.extend(event(
            0xfe,
            terminal_ordinal,
            surface_generation,
            30_000_000,
            &terminal_body_full(1, 0, source, records, type0, 0, 0),
        ));
        bytes
    }

    fn capture_with_abort() -> Vec<u8> {
        let mut bytes = header();
        bytes.extend(event(1, 0, 0, 0, &created_body()));
        bytes.extend(event(0xff, 1, 0, 1, &terminal_body(257, 0, 0, 0, 0, 0)));
        bytes
    }

    fn abort_at_phase(phase: u64, reason: u16) -> Vec<u8> {
        let mut bytes = header();
        bytes.extend(event(1, 0, 0, 0, &created_body()));
        if phase >= 1 {
            bytes.extend(event(2, 1, 0, 1, &armed_body((640, 480), (640, 480))));
        }
        if phase >= 2 {
            bytes.extend(event(
                3,
                2,
                0,
                2,
                &join(&[
                    &640u16.to_be_bytes(),
                    &480u16.to_be_bytes(),
                    &7u32.to_be_bytes(),
                    &0u64.to_be_bytes(),
                ]),
            ));
        }
        if phase >= 3 {
            bytes.extend(event(4, 3, 0, 3, &0u64.to_be_bytes()));
        }
        let ordinal = phase + 1;
        bytes.extend(event(
            0xff,
            ordinal,
            0,
            ordinal,
            &terminal_body(reason, 0, 0, 0, 0, 0),
        ));
        bytes
    }

    fn event_cap_fixture(extra_surface: bool) -> Vec<u8> {
        let mut bytes = header();
        bytes.extend(event(1, 0, 0, 0, &created_body_with_limit(4096)));
        bytes.extend(event(2, 1, 0, 1, &armed_body((640, 480), (640, 480))));
        bytes.extend(event(
            3,
            2,
            0,
            2,
            &join(&[
                &640u16.to_be_bytes(),
                &480u16.to_be_bytes(),
                &7u32.to_be_bytes(),
                &0u64.to_be_bytes(),
            ]),
        ));
        bytes.extend(event(4, 3, 0, 3, &0u64.to_be_bytes()));
        bytes.extend(event(0x10, 4, 1, 4, &surface_body(640, 480)));
        for index in 0..4096u64 {
            bytes.extend(event(
                0x20,
                5 + index,
                1,
                5 + index,
                &record_body(index, index, rect(0, 0, 1, 1), &[0]),
            ));
        }
        let (ordinal, generation) = if extra_surface {
            bytes.extend(event(0x10, 4101, 2, 4101, &surface_body(640, 480)));
            (4102, 2)
        } else {
            (4101, 1)
        };
        bytes.extend(event(
            0xfe,
            ordinal,
            generation,
            ordinal,
            &terminal_body(2, 0, 4096, 4096, 4096, 0),
        ));
        bytes
    }

    fn capture_with_gap_then_abort() -> Vec<u8> {
        gap_fixture(9, 2, 262)
    }

    fn clean_capture_with_geometry(
        committed: (u16, u16),
        requested: (u16, u16),
        record_rect: crate::vnc::mvs_stream::MvsRect,
    ) -> Vec<u8> {
        let mut bytes = header();
        bytes.extend(event(1, 0, 0, 0, &created_body()));
        bytes.extend(event(2, 1, 0, 1, &armed_body(committed, requested)));
        bytes.extend(event(
            3,
            2,
            0,
            2,
            &join(&[
                &requested.0.to_be_bytes(),
                &requested.1.to_be_bytes(),
                &7u32.to_be_bytes(),
                &0u64.to_be_bytes(),
            ]),
        ));
        bytes.extend(event(4, 3, 0, 3, &0u64.to_be_bytes()));
        bytes.extend(event(0x20, 4, 0, 4, &record_body(0, 0, record_rect, &[0])));
        bytes.extend(terminal(0xfe, 5, 30_000_000, 1, 1, 1, 1, 0, 0));
        bytes
    }

    fn gap_fixture(reason: u16, stage: u16, terminal_reason: u16) -> Vec<u8> {
        let mut case = gap_case(reason);
        case.stage = stage;
        gap_fixture_for_case(case, terminal_reason)
    }

    #[derive(Clone, Copy)]
    struct GapCase {
        reason: u16,
        stage: u16,
        first: u64,
        last: u64,
        declared: u32,
        accumulated: u32,
        rectangle: (u16, u16, u16, u16),
        terminal: u16,
        cumulative_before: usize,
    }

    fn gap_cases() -> [GapCase; 12] {
        [
            GapCase {
                reason: 1,
                stage: 1,
                first: 7,
                last: 7,
                declared: 0,
                accumulated: 0,
                rectangle: (0, 0, 0, 0),
                terminal: 262,
                cumulative_before: 0,
            },
            GapCase {
                reason: 2,
                stage: 1,
                first: 7,
                last: 7,
                declared: (MVS_CAPTURE_V2_MAX_PAYLOAD + 1) as u32,
                accumulated: 0,
                rectangle: (0, 0, 1, 1),
                terminal: 262,
                cumulative_before: 0,
            },
            GapCase {
                reason: 3,
                stage: 1,
                first: 7,
                last: 7,
                declared: 5,
                accumulated: 0,
                rectangle: (0, 0, 1, 1),
                terminal: 262,
                cumulative_before: 0,
            },
            GapCase {
                reason: 4,
                stage: 2,
                first: 7,
                last: 8,
                declared: 5,
                accumulated: 3,
                rectangle: (0, 0, 1, 1),
                terminal: 262,
                cumulative_before: 0,
            },
            GapCase {
                reason: 5,
                stage: 3,
                first: 7,
                last: 8,
                declared: 5,
                accumulated: 3,
                rectangle: (0, 0, 1, 1),
                terminal: 262,
                cumulative_before: 0,
            },
            GapCase {
                reason: 6,
                stage: 4,
                first: 7,
                last: 8,
                declared: 5,
                accumulated: 3,
                rectangle: (0, 0, 1, 1),
                terminal: 263,
                cumulative_before: 0,
            },
            GapCase {
                reason: 7,
                stage: 5,
                first: 7,
                last: 8,
                declared: 5,
                accumulated: 3,
                rectangle: (0, 0, 1, 1),
                terminal: 261,
                cumulative_before: 0,
            },
            GapCase {
                reason: 8,
                stage: 6,
                first: 7,
                last: 8,
                declared: 5,
                accumulated: 3,
                rectangle: (0, 0, 1, 1),
                terminal: 266,
                cumulative_before: 0,
            },
            GapCase {
                reason: 9,
                stage: 2,
                first: 7,
                last: 7,
                declared: 0,
                accumulated: 0,
                rectangle: (0, 0, 0, 0),
                terminal: 262,
                cumulative_before: 0,
            },
            GapCase {
                reason: 10,
                stage: 1,
                first: 7,
                last: 8,
                declared: 5,
                accumulated: 3,
                rectangle: (0, 0, 1, 1),
                terminal: 262,
                cumulative_before: 0,
            },
            GapCase {
                reason: 11,
                stage: 8,
                first: 7,
                last: 7,
                declared: 4,
                accumulated: 0,
                rectangle: (0, 0, 1, 1),
                terminal: 262,
                cumulative_before: MVS_CAPTURE_V2_MAX_CUMULATIVE_PAYLOAD - 3,
            },
            GapCase {
                reason: 12,
                stage: 7,
                first: 7,
                last: 8,
                declared: 5,
                accumulated: 3,
                rectangle: (0, 0, 1, 1),
                terminal: 265,
                cumulative_before: 0,
            },
        ]
    }

    fn gap_case(reason: u16) -> GapCase {
        gap_cases()
            .into_iter()
            .find(|case| case.reason == reason)
            .unwrap()
    }

    fn gap_body_for_case(case: GapCase) -> Vec<u8> {
        gap_body(
            case.reason,
            case.stage,
            1,
            case.first,
            case.last,
            case.declared,
            case.accumulated,
            rect_from_tuple(case.rectangle),
        )
    }

    fn invalid_declared_for(case: GapCase) -> u32 {
        match case.reason {
            1 | 9 => 1,
            2 => 1,
            11 => 3,
            _ => 0,
        }
    }

    fn invalid_accumulated_for(case: GapCase) -> u32 {
        match case.reason {
            1 | 2 | 3 | 9 | 11 => 1,
            _ => case.declared,
        }
    }

    fn gap_fixture_for_case(mut case: GapCase, terminal_reason: u16) -> Vec<u8> {
        let mut bytes = header();
        bytes.extend(event(1, 0, 0, 0, &created_body()));
        bytes.extend(event(2, 1, 0, 1, &armed_body((640, 480), (640, 480))));
        bytes.extend(event(
            3,
            2,
            0,
            2,
            &join(&[
                &640u16.to_be_bytes(),
                &480u16.to_be_bytes(),
                &7u32.to_be_bytes(),
                &0u64.to_be_bytes(),
            ]),
        ));
        bytes.extend(event(4, 3, 0, 3, &0u64.to_be_bytes()));
        let had_span = case.last > case.first;
        case.first = 0;
        case.last = if had_span { 1 } else { 0 };
        let source_count = case.last + 1;
        bytes.extend(event(0x21, 4, 0, 4, &gap_body_for_case(case)));
        bytes.extend(event(
            0xff,
            5,
            0,
            5,
            &terminal_body(terminal_reason, 1, source_count, 0, 0, 0),
        ));
        bytes
    }

    fn parse_gap_fixture(
        flags: u32,
        first: u64,
        last: u64,
    ) -> anyhow::Result<StructuralMvsCaptureV2> {
        let mut bytes = header();
        bytes.extend(event(1, 0, 0, 0, &created_body()));
        bytes.extend(event(2, 1, 0, 1, &armed_body((640, 480), (640, 480))));
        bytes.extend(event(
            3,
            2,
            0,
            2,
            &join(&[
                &640u16.to_be_bytes(),
                &480u16.to_be_bytes(),
                &7u32.to_be_bytes(),
                &0u64.to_be_bytes(),
            ]),
        ));
        bytes.extend(event(4, 3, 0, 3, &0u64.to_be_bytes()));
        bytes.extend(event(
            0x21,
            4,
            0,
            4,
            &gap_body(9, 2, flags, first, last, 0, 0, rect(0, 0, 0, 0)),
        ));
        bytes.extend(event(
            0xff,
            5,
            0,
            5,
            &terminal_body(262, 1, last.saturating_add(1), 0, 0, 0),
        ));
        read_structural(&bytes)
    }

    fn gap_body(
        reason: u16,
        stage: u16,
        flags: u32,
        first: u64,
        last: u64,
        declared: u32,
        accumulated: u32,
        rectangle: crate::vnc::mvs_stream::MvsRect,
    ) -> Vec<u8> {
        join(&[
            &reason.to_be_bytes(),
            &stage.to_be_bytes(),
            &flags.to_be_bytes(),
            &first.to_be_bytes(),
            &last.to_be_bytes(),
            &declared.to_be_bytes(),
            &accumulated.to_be_bytes(),
            &rectangle.x.to_be_bytes(),
            &rectangle.y.to_be_bytes(),
            &rectangle.width.to_be_bytes(),
            &rectangle.height.to_be_bytes(),
        ])
    }

    fn surface_body(width: u16, height: u16) -> Vec<u8> {
        join(&[
            &width.to_be_bytes(),
            &height.to_be_bytes(),
            &1u16.to_be_bytes(),
            &0u16.to_be_bytes(),
        ])
    }
    fn terminal_body(
        reason: u16,
        gaps: u32,
        source: u64,
        records: u64,
        type0: u64,
        type1: u64,
    ) -> Vec<u8> {
        terminal_body_full(reason, gaps, source, records, type0, type1, 0)
    }
    fn terminal_body_full(
        reason: u16,
        gaps: u32,
        source: u64,
        records: u64,
        type0: u64,
        type1: u64,
        type2: u64,
    ) -> Vec<u8> {
        join(&[
            &reason.to_be_bytes(),
            &0u16.to_be_bytes(),
            &gaps.to_be_bytes(),
            &source.to_be_bytes(),
            &records.to_be_bytes(),
            &type0.to_be_bytes(),
            &type1.to_be_bytes(),
            &type2.to_be_bytes(),
        ])
    }
    fn rect(x: u16, y: u16, width: u16, height: u16) -> crate::vnc::mvs_stream::MvsRect {
        crate::vnc::mvs_stream::MvsRect {
            x,
            y,
            width,
            height,
        }
    }
    fn rect_from_tuple(rectangle: (u16, u16, u16, u16)) -> crate::vnc::mvs_stream::MvsRect {
        rect(rectangle.0, rectangle.1, rectangle.2, rectangle.3)
    }
    fn insert_before_terminal(bytes: &mut Vec<u8>, event: Vec<u8>) {
        let terminal = bytes.split_off(bytes.len() - 80);
        bytes.extend(event);
        bytes.extend(terminal);
    }
    fn replace_terminal_ordinal(bytes: &mut [u8], ordinal: u64) {
        let offset = bytes.len() - 80 + 8;
        bytes[offset..offset + 8].copy_from_slice(&ordinal.to_be_bytes());
    }
    fn replace_terminal_generation(bytes: &mut [u8], generation: u64) {
        let offset = bytes.len() - 80 + 16;
        bytes[offset..offset + 8].copy_from_slice(&generation.to_be_bytes());
    }
    fn mutate(bytes: &[u8], index: usize, value: u8) -> Vec<u8> {
        let mut copy = bytes.to_vec();
        copy[index] = value;
        copy
    }
    fn rewrite_byte(bytes: &[u8], index: usize, value: u8) -> Vec<u8> {
        mutate(bytes, index, value)
    }
    fn append_byte(bytes: &[u8], value: u8) -> Vec<u8> {
        let mut copy = bytes.to_vec();
        copy.push(value);
        copy
    }
    fn rewrite_prefix_u64(
        bytes: &[u8],
        event_offset: usize,
        field_offset: usize,
        value: u64,
    ) -> Vec<u8> {
        let mut copy = bytes.to_vec();
        copy[event_offset + field_offset..event_offset + field_offset + 8]
            .copy_from_slice(&value.to_be_bytes());
        copy
    }
    fn rewrite_u64_from_end(bytes: &[u8], footer_body_offset: usize, value: u64) -> Vec<u8> {
        let mut copy = bytes.to_vec();
        let index = bytes.len() - 48 + footer_body_offset;
        copy[index..index + 8].copy_from_slice(&value.to_be_bytes());
        copy
    }
    fn join(parts: &[&[u8]]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for part in parts {
            bytes.extend_from_slice(part);
        }
        bytes
    }

    struct ShortReader {
        inner: Cursor<Vec<u8>>,
        chunk: usize,
    }

    impl ShortReader {
        fn new(bytes: Vec<u8>, chunk: usize) -> Self {
            Self {
                inner: Cursor::new(bytes),
                chunk,
            }
        }
        fn consumed(&self) -> usize {
            self.inner.position() as usize
        }
    }

    impl Read for ShortReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let length = out.len().min(self.chunk);
            self.inner.read(&mut out[..length])
        }
    }
}
