//! Transactional writer for strict-cold FRDMVS02 captures.

use anyhow::{anyhow, ensure, Context, Result};
use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::framebuffer::validate_framebuffer_geometry;
use crate::vnc::mvs::MAX_MVS_DECODE_PIXELS;
use crate::vnc::mvs_capture_v2::{
    encode_event, CaptureCounters, MvsCaptureV2Gap, MvsCaptureV2Geometry,
    MVS_CAPTURE_V2_HEADER_BYTES, MVS_CAPTURE_V2_MAGIC, MVS_CAPTURE_V2_MAX_CUMULATIVE_PAYLOAD,
    MVS_CAPTURE_V2_MAX_DURATION_MS, MVS_CAPTURE_V2_MAX_EVENTS, MVS_CAPTURE_V2_MAX_EVENT_BODY,
    MVS_CAPTURE_V2_MAX_PAYLOAD, MVS_CAPTURE_V2_MAX_RECORDS,
};
use crate::vnc::mvs_stream::{MvsRecord, MvsRecordAssembler, MvsRect};

const EVENT_CREATED: u16 = 0x0001;
const EVENT_ARMED: u16 = 0x0002;
const EVENT_TRIGGERED: u16 = 0x0003;
const EVENT_RECORDING: u16 = 0x0004;
const EVENT_RECORD: u16 = 0x0020;
const EVENT_GAP: u16 = 0x0021;
const EVENT_CLEAN: u16 = 0x00fe;
const EVENT_ABORTED: u16 = 0x00ff;

pub const DEADLINE_MICROS: u64 = 5_000_000;
pub const INCOMPLETE_RECORD_MICROS: u64 = 2_000_000;

pub trait CaptureClock {
    fn elapsed_micros(&self) -> Result<u64>;
}

pub trait CaptureSink {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
    fn sync_data(&mut self) -> io::Result<()>;
    fn into_final_file(self: Box<Self>) -> CaptureIntoInnerResult;
}

pub trait CaptureFinalFile {
    fn sync_data(&mut self) -> io::Result<()>;
    fn relinquish(self: Box<Self>) -> io::Result<()>;
}

pub enum CaptureIntoInnerResult {
    Ready(Box<dyn CaptureFinalFile>),
    FlushFailed {
        error: io::Error,
        file: Box<dyn CaptureFinalFile>,
    },
}

struct InstantCaptureClock {
    origin: Instant,
}

impl CaptureClock for InstantCaptureClock {
    fn elapsed_micros(&self) -> Result<u64> {
        u64::try_from(self.origin.elapsed().as_micros()).context("capture elapsed micros 溢出")
    }
}

struct FileCaptureSink {
    writer: BufWriter<File>,
}

impl CaptureSink for FileCaptureSink {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }

    fn sync_data(&mut self) -> io::Result<()> {
        self.writer.get_ref().sync_data()
    }

    fn into_final_file(self: Box<Self>) -> CaptureIntoInnerResult {
        match self.writer.into_inner() {
            Ok(file) => {
                CaptureIntoInnerResult::Ready(Box::new(FileCaptureFinalFile { file: Some(file) }))
            }
            Err(error) => {
                let (error, writer) = error.into_parts();
                let (file, _) = writer.into_parts();
                CaptureIntoInnerResult::FlushFailed {
                    error,
                    file: Box::new(FileCaptureFinalFile { file: Some(file) }),
                }
            }
        }
    }
}

struct FileCaptureFinalFile {
    file: Option<File>,
}

impl CaptureFinalFile for FileCaptureFinalFile {
    fn sync_data(&mut self) -> io::Result<()> {
        self.file
            .as_ref()
            .ok_or_else(|| io::Error::other("capture file 已被 relinquish"))?
            .sync_data()
    }

    fn relinquish(mut self: Box<Self>) -> io::Result<()> {
        drop(self.file.take());
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureState {
    Created,
    Armed,
    Triggering,
    Recording,
    Finalizing,
    Clean,
    Aborted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CreatedConfig {
    pub deadline_ms: u32,
    pub record_limit: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArmedConfig {
    pub committed: MvsCaptureV2Geometry,
    pub requested: MvsCaptureV2Geometry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriterDecision {
    Continue,
    Finalized(CaptureState),
}

#[derive(Clone, Copy)]
struct PendingProvenance {
    first: u64,
    last: u64,
    total: u32,
    accepted: u32,
    rect: MvsRect,
    started_at: u64,
}

pub struct MvsCaptureV2Writer {
    sink: Option<Box<dyn CaptureSink>>,
    clock: Box<dyn CaptureClock>,
    state: CaptureState,
    config: CreatedConfig,
    counters: CaptureCounters,
    event_ordinal: u64,
    event_count: usize,
    cumulative_payload: usize,
    generation: u64,
    armed: Option<ArmedConfig>,
    trigger_mask: u32,
    assembler: MvsRecordAssembler,
    pending: Option<PendingProvenance>,
    source_frame_count: u64,
    record_count: u64,
    type_counts: [u64; 3],
    gap_count: u32,
    last_gap: Option<MvsCaptureV2Gap>,
    selected_terminal_reason: Option<u16>,
    selected_abort_reason: Option<u16>,
    last_timestamp_us: Cell<u64>,
    absolute_deadline: Option<Instant>,
}

impl MvsCaptureV2Writer {
    pub fn create_new(path: impl AsRef<Path>, config: CreatedConfig) -> Result<Self> {
        validate_created_config(config)?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path.as_ref())
            .with_context(|| format!("create_new FRDMVS02: {}", path.as_ref().display()))?;
        let sink = Box::new(FileCaptureSink {
            writer: BufWriter::new(file),
        });
        let origin = Instant::now();
        Self::new_with_origin_and_checked_add(sink, config, origin, |origin, duration| {
            origin.checked_add(duration)
        })
    }

    pub fn new(
        sink: Box<dyn CaptureSink>,
        clock: Box<dyn CaptureClock>,
        config: CreatedConfig,
    ) -> Result<Self> {
        validate_created_config(config)?;
        Self::new_validated(sink, clock, config, None)
    }

    fn new_with_origin_and_checked_add<F>(
        sink: Box<dyn CaptureSink>,
        config: CreatedConfig,
        origin: Instant,
        checked_add: F,
    ) -> Result<Self>
    where
        F: FnOnce(Instant, Duration) -> Option<Instant>,
    {
        let duration = Duration::from_millis(u64::from(config.deadline_ms));
        let absolute_deadline =
            checked_add(origin, duration).context("capture absolute deadline 溢出")?;
        Self::new_validated(
            sink,
            Box::new(InstantCaptureClock { origin }),
            config,
            Some(absolute_deadline),
        )
    }

    fn new_validated(
        mut sink: Box<dyn CaptureSink>,
        clock: Box<dyn CaptureClock>,
        config: CreatedConfig,
        absolute_deadline: Option<Instant>,
    ) -> Result<Self> {
        let mut body = Vec::with_capacity(16);
        body.extend_from_slice(&config.deadline_ms.to_be_bytes());
        body.extend_from_slice(&config.record_limit.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        let created = encode_event(EVENT_CREATED, 0, 0, 0, &body)?;
        let mut bytes = capture_header();
        bytes.extend_from_slice(&created);
        sink.write_all(&bytes)
            .context("写入 FRDMVS02 Header + Created")?;
        sink.flush().context("flush FRDMVS02 Created")?;

        let mut counters = CaptureCounters::default();
        counters.reserve_event()?;
        Ok(Self {
            sink: Some(sink),
            clock,
            state: CaptureState::Created,
            config,
            counters,
            event_ordinal: 1,
            event_count: 1,
            cumulative_payload: 0,
            generation: 0,
            armed: None,
            trigger_mask: 0,
            assembler: MvsRecordAssembler::default(),
            pending: None,
            source_frame_count: 0,
            record_count: 0,
            type_counts: [0; 3],
            gap_count: 0,
            last_gap: None,
            selected_terminal_reason: None,
            selected_abort_reason: None,
            last_timestamp_us: Cell::new(0),
            absolute_deadline,
        })
    }

    pub fn state(&self) -> CaptureState {
        self.state
    }

    pub fn selected_abort_reason(&self) -> Option<u16> {
        self.selected_abort_reason
    }

    pub fn selected_terminal_reason(&self) -> Option<u16> {
        self.selected_terminal_reason
    }

    pub fn gap_count(&self) -> u32 {
        self.gap_count
    }

    pub fn last_gap(&self) -> Option<&MvsCaptureV2Gap> {
        self.last_gap.as_ref()
    }

    pub fn absolute_deadline(&self) -> Result<Instant> {
        self.absolute_deadline
            .context("capture absolute deadline 不可用")
    }

    pub fn pre_trigger_checkpoint(&mut self) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Created, CaptureState::Armed])?;
        let now = match self.now() {
            Ok(now) => now,
            Err(error) => return self.fail_closed_output(error),
        };
        if now >= self.deadline_micros() {
            return self.finalize_at(false, 267, now);
        }
        Ok(WriterDecision::Continue)
    }

    pub fn arm(&mut self, config: ArmedConfig) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Created])?;
        if validate_geometry(config.committed).is_err()
            || validate_geometry(config.requested).is_err()
        {
            return self.finalize(false, 266);
        }

        let mut body = Vec::with_capacity(24);
        body.extend_from_slice(&config.committed.width.to_be_bytes());
        body.extend_from_slice(&config.committed.height.to_be_bytes());
        body.extend_from_slice(&config.requested.width.to_be_bytes());
        body.extend_from_slice(&config.requested.height.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes());
        body.extend_from_slice(&3u32.to_be_bytes());
        body.extend_from_slice(&0u64.to_be_bytes());
        let timestamp = match self.now() {
            Ok(timestamp) => timestamp,
            Err(error) => return self.fail_closed_output(error),
        };
        if timestamp >= self.deadline_micros() {
            return self.finalize_at(false, 267, timestamp);
        }
        if self
            .write_nonterminal(EVENT_ARMED, timestamp, &body, 0)
            .is_err()
        {
            return self.finalize(false, 259);
        }
        let arm_io = (|| -> io::Result<()> {
            let sink = self
                .sink
                .as_mut()
                .ok_or_else(|| io::Error::other("sink 已被消费"))?;
            sink.flush()?;
            sink.sync_data()
        })();
        if arm_io.is_err() {
            return self.finalize(false, 259);
        }
        let now = match self.now() {
            Ok(now) => now,
            Err(error) => return self.fail_closed(error),
        };
        if now >= self.deadline_micros() {
            return self.finalize_at(false, 267, now);
        }
        self.armed = Some(config);
        self.state = CaptureState::Armed;
        Ok(WriterDecision::Continue)
    }

    pub fn begin_trigger(&mut self) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Armed])?;
        let now = match self.now() {
            Ok(now) => now,
            Err(error) => return self.fail_closed(error),
        };
        if now >= self.deadline_micros() {
            return self.finalize_at(false, 267, now);
        }
        self.state = CaptureState::Triggering;
        Ok(WriterDecision::Continue)
    }

    pub fn trigger_write_succeeded(&mut self, bit: u32) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Triggering])?;
        let expected = match self.trigger_mask {
            0 => 1,
            1 => 2,
            3 => 4,
            _ => return Err(anyhow!("trigger 已完整")),
        };
        ensure!(bit == expected, "trigger mask 顺序无效");
        self.trigger_mask |= bit;
        let now = match self.now() {
            Ok(now) => now,
            Err(error) => return self.fail_closed(error),
        };
        if now >= self.deadline_micros() {
            return self.finalize_at(false, 260, now);
        }
        Ok(WriterDecision::Continue)
    }

    pub fn trigger_failed(&mut self) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Triggering])?;
        self.finalize(false, 260)
    }

    pub fn write_recording_gate(&mut self) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Triggering])?;
        ensure!(self.trigger_mask == 7, "trigger mask 尚未完整");
        let requested = self.armed.context("缺少 Armed 配置")?.requested;
        let mut triggered = Vec::with_capacity(16);
        triggered.extend_from_slice(&requested.width.to_be_bytes());
        triggered.extend_from_slice(&requested.height.to_be_bytes());
        triggered.extend_from_slice(&7u32.to_be_bytes());
        triggered.extend_from_slice(&0u64.to_be_bytes());
        let timestamp = match self.now() {
            Ok(timestamp) => timestamp,
            Err(error) => return self.fail_closed(error),
        };
        if timestamp >= self.deadline_micros() {
            return self.finalize_at(false, 260, timestamp);
        }
        if self
            .write_nonterminal(EVENT_TRIGGERED, timestamp, &triggered, 0)
            .is_err()
        {
            return self.finalize(false, 260);
        }
        let now = match self.now() {
            Ok(now) => now,
            Err(error) => return self.fail_closed(error),
        };
        if now >= self.deadline_micros() {
            return self.finalize_at(false, 260, now);
        }
        let timestamp = match self.now() {
            Ok(timestamp) => timestamp,
            Err(error) => return self.fail_closed(error),
        };
        if self
            .write_nonterminal(EVENT_RECORDING, timestamp, &0u64.to_be_bytes(), 0)
            .is_err()
        {
            return self.finalize(false, 260);
        }
        let flush = self.sink.as_mut().context("capture sink 已被消费")?.flush();
        if flush.is_err() {
            return self.finalize(false, 260);
        }
        let now = match self.now() {
            Ok(now) => now,
            Err(error) => return self.fail_closed(error),
        };
        if now >= self.deadline_micros() {
            return self.finalize_at(false, 260, now);
        }
        self.state = CaptureState::Recording;
        Ok(WriterDecision::Continue)
    }

    pub fn accept_non_mvs(&mut self) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Recording])?;
        let now = match self.now() {
            Ok(now) => now,
            Err(error) => return self.fail_closed(error),
        };
        self.post_frame(now)
    }

    pub fn reject_mvs_envelope(&mut self, rect: Option<MvsRect>) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Recording])?;
        let ordinal = match self.assign_source_ordinal() {
            Ok(ordinal) => ordinal,
            Err(error) => return self.fail_closed(error),
        };
        self.emit_gap(
            gap(1, ordinal, ordinal, 0, 0, rect.unwrap_or(zero_rect())),
            262,
        )
    }

    pub fn accept_mvs_begin(
        &mut self,
        rect: MvsRect,
        total: u32,
        first: &[u8],
    ) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Recording])?;
        let ordinal = match self.assign_source_ordinal() {
            Ok(ordinal) => ordinal,
            Err(error) => return self.fail_closed(error),
        };
        if let Some(pending) = self.pending {
            return self.emit_gap(
                gap(
                    10,
                    pending.first,
                    ordinal,
                    pending.total,
                    pending.accepted,
                    pending.rect,
                ),
                262,
            );
        }
        if total == 0 || total as usize > MVS_CAPTURE_V2_MAX_PAYLOAD {
            return self.emit_gap(gap(2, ordinal, ordinal, total, 0, rect), 262);
        }
        if first.len() > total as usize {
            return self.emit_gap(gap(3, ordinal, ordinal, total, 0, rect), 262);
        }
        if self
            .cumulative_payload
            .checked_add(total as usize)
            .map(|value| value > MVS_CAPTURE_V2_MAX_CUMULATIVE_PAYLOAD)
            .unwrap_or(true)
        {
            return self.emit_gap(gap(11, ordinal, ordinal, total, 0, rect), 262);
        }

        let started_at = match self.now() {
            Ok(started_at) => started_at,
            Err(error) => return self.fail_closed(error),
        };
        match self.assembler.begin(rect, total, first) {
            Ok(Some(record)) => self.complete_record(ordinal, ordinal, record),
            Ok(None) => {
                self.pending = Some(PendingProvenance {
                    first: ordinal,
                    last: ordinal,
                    total,
                    accepted: first.len() as u32,
                    rect,
                    started_at,
                });
                self.post_frame(started_at)
            }
            Err(error) => self.fail_closed(error),
        }
    }

    pub fn accept_mvs_continuation(&mut self, chunk: &[u8]) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Recording])?;
        let ordinal = match self.assign_source_ordinal() {
            Ok(ordinal) => ordinal,
            Err(error) => return self.fail_closed(error),
        };
        let Some(pending) = self.pending else {
            return self.emit_gap(gap(9, ordinal, ordinal, 0, 0, zero_rect()), 262);
        };
        let remaining = pending.total - pending.accepted;
        if chunk.len() > remaining as usize {
            return self.emit_gap(
                gap(
                    4,
                    pending.first,
                    ordinal,
                    pending.total,
                    pending.accepted,
                    pending.rect,
                ),
                262,
            );
        }
        let assembled = match self.assembler.push_continuation(chunk) {
            Ok(assembled) => assembled,
            Err(error) => return self.fail_closed(error),
        };
        match assembled {
            Some(record) => {
                self.pending = None;
                self.complete_record(pending.first, ordinal, record)
            }
            None => {
                let accepted = pending
                    .accepted
                    .checked_add(chunk.len() as u32)
                    .context("pending accumulated 溢出");
                let accepted = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => return self.fail_closed(error),
                };
                let state = self.pending.as_mut().unwrap();
                state.last = ordinal;
                state.accepted = accepted;
                let now = match self.now() {
                    Ok(now) => now,
                    Err(error) => return self.fail_closed(error),
                };
                self.post_frame(now)
            }
        }
    }

    pub fn poll_recording(&mut self) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Recording])?;
        let now = match self.now() {
            Ok(now) => now,
            Err(error) => return self.fail_closed(error),
        };
        if now >= self.deadline_micros() {
            return if self.pending.is_some() {
                self.emit_pending_gap(6, 263)
            } else {
                self.finalize_at(true, 1, now)
            };
        }
        if self.pending_expired(now) {
            return self.emit_pending_gap(5, 262);
        }
        Ok(WriterDecision::Continue)
    }

    pub fn read_failed(&mut self) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Recording])?;
        let now = match self.now() {
            Ok(now) => now,
            Err(error) => return self.fail_closed(error),
        };
        if now >= self.deadline_micros() {
            return if self.pending.is_some() {
                self.emit_pending_gap(6, 263)
            } else {
                self.finalize_at(true, 1, now)
            };
        }
        if self.pending_expired(now) {
            return self.emit_pending_gap(5, 262);
        }
        if self.pending.is_some() {
            self.emit_pending_gap(7, 261)
        } else {
            self.finalize(false, 261)
        }
    }

    pub fn generation_transition_attempted(&mut self) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Recording])?;
        if self.pending.is_some() {
            self.emit_pending_gap(8, 266)
        } else {
            self.finalize(false, 266)
        }
    }

    pub fn cancel(&mut self) -> Result<WriterDecision> {
        self.require_state(&[
            CaptureState::Created,
            CaptureState::Armed,
            CaptureState::Triggering,
            CaptureState::Recording,
        ])?;
        if self.state == CaptureState::Recording && self.pending.is_some() {
            self.emit_pending_gap(12, 265)
        } else {
            self.finalize(false, 265)
        }
    }

    pub fn connect_failed(&mut self) -> Result<WriterDecision> {
        self.created_operation_failed(257)
    }

    pub fn authentication_failed(&mut self) -> Result<WriterDecision> {
        self.created_operation_failed(258)
    }

    pub fn pre_trigger_deadline_failed(&mut self) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Created])?;
        self.finalize(false, 267)
    }

    fn created_operation_failed(&mut self, ordinary_reason: u16) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Created])?;
        let now = match self.now() {
            Ok(now) => now,
            Err(error) => return self.fail_closed_output(error),
        };
        let reason = if now >= self.deadline_micros() {
            267
        } else {
            ordinary_reason
        };
        self.finalize_at(false, reason, now)
    }

    pub fn output_failed(&mut self) -> Result<WriterDecision> {
        self.require_nonterminal()?;
        let reason = if self.state == CaptureState::Triggering {
            260
        } else {
            264
        };
        self.finalize(false, reason)
    }

    fn complete_record(
        &mut self,
        first: u64,
        last: u64,
        record: MvsRecord,
    ) -> Result<WriterDecision> {
        let payload_len = record.payload.len();
        let mut body = Vec::with_capacity(28 + payload_len);
        body.extend_from_slice(&first.to_be_bytes());
        body.extend_from_slice(&last.to_be_bytes());
        body.extend_from_slice(&record.rect.x.to_be_bytes());
        body.extend_from_slice(&record.rect.y.to_be_bytes());
        body.extend_from_slice(&record.rect.width.to_be_bytes());
        body.extend_from_slice(&record.rect.height.to_be_bytes());
        body.extend_from_slice(&(payload_len as u32).to_be_bytes());
        body.extend_from_slice(&record.payload);
        let timestamp = match self.now() {
            Ok(timestamp) => timestamp,
            Err(error) => return self.fail_closed(error),
        };
        if let Err(error) = self.write_nonterminal(EVENT_RECORD, timestamp, &body, payload_len) {
            return self.fail_closed(error);
        }
        self.record_count = match self
            .record_count
            .checked_add(1)
            .context("record count 溢出")
        {
            Ok(count) => count,
            Err(error) => return self.fail_closed(error),
        };
        let validation = match record.payload[0] {
            0 => {
                self.type_counts[0] += 1;
                self.validate_record_rect(record.rect)
            }
            1 => {
                self.type_counts[1] += 1;
                self.validate_record_rect(record.rect)
            }
            2 => {
                self.type_counts[2] += 1;
                ensure!(record.rect == zero_rect(), "type-2 rectangle 必须为零");
                Ok(())
            }
            _ => Err(anyhow!("未知 MVS payload tag（Record 已持久化）")),
        };
        if let Err(error) = validation {
            return self.fail_closed(error);
        }
        let now = match self.now() {
            Ok(now) => now,
            Err(error) => return self.fail_closed(error),
        };
        self.post_frame(now)
    }

    fn validate_record_rect(&self, rect: MvsRect) -> Result<()> {
        let surface = self
            .armed
            .context("Record 缺少 committed surface")?
            .committed;
        ensure!(
            rect.width != 0 && rect.height != 0,
            "MVS record rectangle 为零"
        );
        let right = rect
            .x
            .checked_add(rect.width)
            .context("MVS rectangle right 溢出")?;
        let bottom = rect
            .y
            .checked_add(rect.height)
            .context("MVS rectangle bottom 溢出")?;
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

    fn post_frame(&mut self, now: u64) -> Result<WriterDecision> {
        if now >= self.deadline_micros() {
            return if self.pending.is_some() {
                self.emit_pending_gap(6, 263)
            } else {
                self.finalize_at(true, 1, now)
            };
        }
        if self.record_count == u64::from(self.config.record_limit) {
            return self.finalize_at(true, 2, now);
        }
        Ok(WriterDecision::Continue)
    }

    fn pending_expired(&self, now: u64) -> bool {
        self.pending
            .and_then(|pending| now.checked_sub(pending.started_at))
            .map(|age| age >= INCOMPLETE_RECORD_MICROS)
            .unwrap_or(false)
    }

    fn emit_pending_gap(&mut self, reason: u16, terminal: u16) -> Result<WriterDecision> {
        let pending = self.pending.context("缺少 pending provenance")?;
        self.emit_gap(
            gap(
                reason,
                pending.first,
                pending.last,
                pending.total,
                pending.accepted,
                pending.rect,
            ),
            terminal,
        )
    }

    fn emit_gap(&mut self, diagnostic: MvsCaptureV2Gap, terminal: u16) -> Result<WriterDecision> {
        self.require_state(&[CaptureState::Recording])?;
        if let Err(error) = self.preflight_events(2) {
            return self.fail_closed(error);
        }
        let mut body = Vec::with_capacity(40);
        body.extend_from_slice(&diagnostic.reason.to_be_bytes());
        body.extend_from_slice(&diagnostic.stage.to_be_bytes());
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&diagnostic.first_source_frame_ordinal.to_be_bytes());
        body.extend_from_slice(&diagnostic.last_source_frame_ordinal.to_be_bytes());
        body.extend_from_slice(&diagnostic.declared_total.to_be_bytes());
        body.extend_from_slice(&diagnostic.accumulated_bytes.to_be_bytes());
        body.extend_from_slice(&diagnostic.rect.x.to_be_bytes());
        body.extend_from_slice(&diagnostic.rect.y.to_be_bytes());
        body.extend_from_slice(&diagnostic.rect.width.to_be_bytes());
        body.extend_from_slice(&diagnostic.rect.height.to_be_bytes());
        let timestamp = match self.now() {
            Ok(timestamp) => timestamp,
            Err(error) => return self.fail_closed(error),
        };
        if let Err(error) = self.write_nonterminal(EVENT_GAP, timestamp, &body, 0) {
            return self.fail_closed(error);
        }
        self.gap_count = 1;
        self.last_gap = Some(diagnostic);
        self.pending = None;
        self.assembler = MvsRecordAssembler::default();
        self.finalize(false, terminal)
    }

    fn write_nonterminal(
        &mut self,
        kind: u16,
        timestamp: u64,
        body: &[u8],
        payload: usize,
    ) -> Result<()> {
        self.preflight_events(2)?;
        let next_payload = self
            .cumulative_payload
            .checked_add(payload)
            .context("V2 payload 累计溢出")?;
        ensure!(
            next_payload <= MVS_CAPTURE_V2_MAX_CUMULATIVE_PAYLOAD,
            "V2 payload 累计超过上限"
        );
        let encoded = encode_event(kind, self.event_ordinal, self.generation, timestamp, body)?;
        self.sink
            .as_mut()
            .context("capture sink 已被消费")?
            .write_all(&encoded)
            .context("写入 FRDMVS02 event")?;
        self.counters.reserve_event()?;
        if payload != 0 {
            self.counters.reserve_payload(payload)?;
        }
        self.event_count += 1;
        self.cumulative_payload = next_payload;
        self.event_ordinal = self
            .event_ordinal
            .checked_add(1)
            .context("event ordinal 溢出")?;
        Ok(())
    }

    fn finalize(&mut self, clean: bool, reason: u16) -> Result<WriterDecision> {
        self.require_nonterminal()?;
        self.select_terminal(clean, reason);
        let timestamp = match self.now() {
            Ok(timestamp) => timestamp,
            Err(error) => return self.fail_closed(error),
        };
        self.finalize_selected(clean, timestamp)
    }

    fn finalize_at(&mut self, clean: bool, reason: u16, timestamp: u64) -> Result<WriterDecision> {
        self.require_nonterminal()?;
        self.select_terminal(clean, reason);
        self.finalize_selected(clean, timestamp)
    }

    fn select_terminal(&mut self, clean: bool, reason: u16) {
        self.selected_terminal_reason = Some(reason);
        if !clean {
            self.selected_abort_reason = Some(reason);
        }
        self.state = CaptureState::Finalizing;
    }

    fn finalize_selected(&mut self, clean: bool, timestamp: u64) -> Result<WriterDecision> {
        if let Err(error) = self.preflight_events(1) {
            return self.fail_closed(error);
        }
        let reason = match self
            .selected_terminal_reason
            .context("terminal reason 未选择")
        {
            Ok(reason) => reason,
            Err(error) => return self.fail_closed(error),
        };
        let mut body = Vec::with_capacity(48);
        body.extend_from_slice(&reason.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&self.gap_count.to_be_bytes());
        body.extend_from_slice(&self.source_frame_count.to_be_bytes());
        body.extend_from_slice(&self.record_count.to_be_bytes());
        body.extend_from_slice(&self.type_counts[0].to_be_bytes());
        body.extend_from_slice(&self.type_counts[1].to_be_bytes());
        body.extend_from_slice(&self.type_counts[2].to_be_bytes());
        let kind = if clean { EVENT_CLEAN } else { EVENT_ABORTED };
        let event = match encode_event(kind, self.event_ordinal, self.generation, timestamp, &body)
        {
            Ok(event) => event,
            Err(error) => return self.fail_closed(error),
        };
        let mut sink = self.sink.take().context("capture sink 已被消费")?;
        let mut failure = None;
        if let Err(error) = sink.write_all(&event) {
            failure = Some(anyhow!(error).context("写入 terminal event"));
        } else {
            if let Err(error) = self.counters.reserve_event() {
                failure = Some(error.context("terminal counter commit"));
            }
            self.event_count += 1;
            self.event_ordinal += 1;
        }
        consume_sink(sink, failure)?;
        self.state = if clean {
            CaptureState::Clean
        } else {
            CaptureState::Aborted
        };
        Ok(WriterDecision::Finalized(self.state))
    }

    fn fail_closed(&mut self, error: anyhow::Error) -> Result<WriterDecision> {
        self.state = CaptureState::Finalizing;
        let Some(sink) = self.sink.take() else {
            return Err(error);
        };
        consume_sink(sink, Some(error))?;
        unreachable!("consume_sink always returns the supplied primary error")
    }

    fn fail_closed_output(&mut self, error: anyhow::Error) -> Result<WriterDecision> {
        self.select_terminal(false, 264);
        self.fail_closed(error)
    }

    fn assign_source_ordinal(&mut self) -> Result<u64> {
        let ordinal = self.source_frame_count;
        ensure!(ordinal != u64::MAX, "MVS source ordinal 溢出");
        self.source_frame_count += 1;
        Ok(ordinal)
    }

    fn preflight_events(&self, slots: usize) -> Result<()> {
        let next = self
            .event_count
            .checked_add(slots)
            .context("V2 event 计数溢出")?;
        ensure!(next <= MVS_CAPTURE_V2_MAX_EVENTS, "V2 event 数量超过上限");
        Ok(())
    }

    fn deadline_micros(&self) -> u64 {
        u64::from(self.config.deadline_ms) * 1_000
    }

    fn now(&self) -> Result<u64> {
        let now = self.clock.elapsed_micros()?;
        ensure!(now >= self.last_timestamp_us.get(), "capture clock 倒退");
        self.last_timestamp_us.set(now);
        Ok(now)
    }

    fn require_state(&self, allowed: &[CaptureState]) -> Result<()> {
        ensure!(allowed.contains(&self.state), "capture state 转换无效");
        ensure!(self.sink.is_some(), "capture sink 已被消费");
        Ok(())
    }

    fn require_nonterminal(&self) -> Result<()> {
        ensure!(
            !matches!(
                self.state,
                CaptureState::Finalizing | CaptureState::Clean | CaptureState::Aborted
            ),
            "capture 已进入终态"
        );
        ensure!(self.sink.is_some(), "capture sink 已被消费");
        Ok(())
    }
}

fn consume_sink(sink: Box<dyn CaptureSink>, mut first_error: Option<anyhow::Error>) -> Result<()> {
    let mut file = match sink.into_final_file() {
        CaptureIntoInnerResult::Ready(file) => file,
        CaptureIntoInnerResult::FlushFailed { error, file } => {
            if first_error.is_none() {
                first_error = Some(anyhow!(error).context("final implicit flush"));
            }
            file
        }
    };
    if let Err(error) = file.sync_data() {
        if first_error.is_none() {
            first_error = Some(anyhow!(error).context("final file sync_data"));
        }
    }
    if let Err(error) = file.relinquish() {
        if first_error.is_none() {
            first_error = Some(anyhow!(error).context("final file relinquish"));
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn validate_created_config(config: CreatedConfig) -> Result<()> {
    ensure!(
        [5_000, 10_000, 15_000, 20_000, 30_000].contains(&config.deadline_ms),
        "V2 Created deadline 无效"
    );
    ensure!(
        (1..=MVS_CAPTURE_V2_MAX_RECORDS as u32).contains(&config.record_limit),
        "V2 Created record limit 无效"
    );
    Ok(())
}

fn gap(
    reason: u16,
    first: u64,
    last: u64,
    declared_total: u32,
    accumulated_bytes: u32,
    rect: MvsRect,
) -> MvsCaptureV2Gap {
    MvsCaptureV2Gap {
        reason,
        stage: match reason {
            1 | 2 | 3 | 10 => 1,
            4 | 9 => 2,
            5 => 3,
            6 => 4,
            7 => 5,
            8 => 6,
            12 => 7,
            11 => 8,
            _ => 0,
        },
        first_source_frame_ordinal: first,
        last_source_frame_ordinal: last,
        declared_total,
        accumulated_bytes,
        rect,
    }
}

fn zero_rect() -> MvsRect {
    MvsRect {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    }
}

fn validate_geometry(geometry: MvsCaptureV2Geometry) -> Result<()> {
    ensure!(
        geometry.width != 0 && geometry.height != 0,
        "surface geometry 为零"
    );
    validate_framebuffer_geometry(geometry.width.into(), geometry.height.into())?;
    Ok(())
}

fn capture_header() -> Vec<u8> {
    let mut header = Vec::with_capacity(MVS_CAPTURE_V2_HEADER_BYTES);
    header.extend_from_slice(&MVS_CAPTURE_V2_MAGIC);
    header.extend_from_slice(&(MVS_CAPTURE_V2_HEADER_BYTES as u16).to_be_bytes());
    header.extend_from_slice(&[2, 0, 1, 0]);
    header.extend_from_slice(&1u16.to_be_bytes());
    header.extend_from_slice(&(MVS_CAPTURE_V2_MAX_PAYLOAD as u32).to_be_bytes());
    header.extend_from_slice(&(MVS_CAPTURE_V2_MAX_EVENT_BODY as u32).to_be_bytes());
    header.extend_from_slice(&(MVS_CAPTURE_V2_MAX_RECORDS as u32).to_be_bytes());
    header.extend_from_slice(&MVS_CAPTURE_V2_MAX_DURATION_MS.to_be_bytes());
    header
}

#[cfg(test)]
mod tests {
    use super::{
        ArmedConfig, CaptureClock, CaptureFinalFile, CaptureIntoInnerResult, CaptureSink,
        CaptureState, CreatedConfig, MvsCaptureV2Writer, WriterDecision, DEADLINE_MICROS,
        INCOMPLETE_RECORD_MICROS,
    };
    use crate::vnc::mvs_capture_v2::{
        read_mvs_capture_v2_strict_cold, read_mvs_capture_v2_structural, MvsCaptureV2Geometry,
        MVS_CAPTURE_V2_MAX_CUMULATIVE_PAYLOAD, MVS_CAPTURE_V2_MAX_EVENTS,
    };
    use crate::vnc::mvs_stream::MvsRect;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;
    use std::io::Cursor;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct ManualClock {
        micros: Rc<RefCell<u64>>,
    }

    impl CaptureClock for ManualClock {
        fn elapsed_micros(&self) -> anyhow::Result<u64> {
            Ok(*self.micros.borrow())
        }
    }

    impl ManualClock {
        fn handle(&self) -> Rc<RefCell<u64>> {
            Rc::clone(&self.micros)
        }
    }

    #[derive(Default)]
    struct ScriptClock {
        samples: Rc<RefCell<VecDeque<u64>>>,
    }

    impl CaptureClock for ScriptClock {
        fn elapsed_micros(&self) -> anyhow::Result<u64> {
            Ok(self.samples.borrow_mut().pop_front().unwrap_or(0))
        }
    }

    #[derive(Clone, Copy)]
    enum ClockStep {
        Micros(u64),
        Error,
    }

    #[derive(Default)]
    struct FaultClock {
        steps: Rc<RefCell<VecDeque<ClockStep>>>,
    }

    impl CaptureClock for FaultClock {
        fn elapsed_micros(&self) -> anyhow::Result<u64> {
            match self
                .steps
                .borrow_mut()
                .pop_front()
                .unwrap_or(ClockStep::Micros(0))
            {
                ClockStep::Micros(value) => Ok(value),
                ClockStep::Error => Err(anyhow::anyhow!("injected clock failure")),
            }
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum SinkEvent {
        Write(Vec<u8>),
        Flush,
        IntoInner,
        Sync,
        Relinquish,
    }

    #[derive(Default)]
    struct RecordingSink {
        events: Rc<RefCell<Vec<SinkEvent>>>,
    }

    impl CaptureSink for RecordingSink {
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.events
                .borrow_mut()
                .push(SinkEvent::Write(bytes.to_vec()));
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.events.borrow_mut().push(SinkEvent::Flush);
            Ok(())
        }

        fn sync_data(&mut self) -> io::Result<()> {
            self.events.borrow_mut().push(SinkEvent::Sync);
            Ok(())
        }

        fn into_final_file(self: Box<Self>) -> CaptureIntoInnerResult {
            self.events.borrow_mut().push(SinkEvent::IntoInner);
            CaptureIntoInnerResult::Ready(Box::new(RecordingFinalFile {
                events: Rc::clone(&self.events),
                point: None,
            }))
        }
    }

    struct RecordingFinalFile {
        events: Rc<RefCell<Vec<SinkEvent>>>,
        point: Option<FailurePoint>,
    }

    impl CaptureFinalFile for RecordingFinalFile {
        fn sync_data(&mut self) -> io::Result<()> {
            self.events.borrow_mut().push(SinkEvent::Sync);
            if matches!(self.point, Some(FailurePoint::FinalSync)) {
                Err(io::Error::other("final sync failure"))
            } else {
                Ok(())
            }
        }

        fn relinquish(self: Box<Self>) -> io::Result<()> {
            self.events.borrow_mut().push(SinkEvent::Relinquish);
            if matches!(self.point, Some(FailurePoint::Relinquish)) {
                Err(io::Error::other("relinquish failure"))
            } else {
                Ok(())
            }
        }
    }

    #[derive(Clone, Copy)]
    enum FailurePoint {
        TerminalWrite,
        ArmedWrite,
        TriggeredWrite,
        RecordingWrite,
        RecordOrGapWrite,
        IntoInnerFlush,
        FinalSync,
        Relinquish,
    }

    struct FailingSink {
        events: Rc<RefCell<Vec<SinkEvent>>>,
        point: FailurePoint,
        writes: usize,
        flushes: usize,
        syncs: usize,
    }

    impl CaptureSink for FailingSink {
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.writes += 1;
            self.events
                .borrow_mut()
                .push(SinkEvent::Write(bytes.to_vec()));
            let fail = match self.point {
                FailurePoint::TerminalWrite => self.writes == 5,
                FailurePoint::ArmedWrite => self.writes == 2,
                FailurePoint::TriggeredWrite => self.writes == 3,
                FailurePoint::RecordingWrite => self.writes == 4,
                FailurePoint::RecordOrGapWrite => self.writes == 5,
                _ => false,
            };
            if fail {
                Err(io::Error::other("terminal write failure"))
            } else {
                Ok(())
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            self.events.borrow_mut().push(SinkEvent::Flush);
            Ok(())
        }

        fn sync_data(&mut self) -> io::Result<()> {
            self.syncs += 1;
            self.events.borrow_mut().push(SinkEvent::Sync);
            Ok(())
        }

        fn into_final_file(self: Box<Self>) -> CaptureIntoInnerResult {
            self.events.borrow_mut().push(SinkEvent::IntoInner);
            let file = Box::new(RecordingFinalFile {
                events: Rc::clone(&self.events),
                point: Some(self.point),
            });
            if matches!(self.point, FailurePoint::IntoInnerFlush) {
                CaptureIntoInnerResult::FlushFailed {
                    error: io::Error::other("into_inner flush failure"),
                    file,
                }
            } else {
                CaptureIntoInnerResult::Ready(file)
            }
        }
    }

    fn rect(x: u16, y: u16, width: u16, height: u16) -> MvsRect {
        MvsRect {
            x,
            y,
            width,
            height,
        }
    }

    fn created_writer() -> (
        MvsCaptureV2Writer,
        Rc<RefCell<Vec<SinkEvent>>>,
        Rc<RefCell<u64>>,
    ) {
        let sink = RecordingSink::default();
        let events = Rc::clone(&sink.events);
        let clock = ManualClock::default();
        let micros = clock.handle();
        let writer = MvsCaptureV2Writer::new(
            Box::new(sink),
            Box::new(clock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 3,
            },
        )
        .unwrap();
        (writer, events, micros)
    }

    fn fault_clock_writer() -> (
        MvsCaptureV2Writer,
        Rc<RefCell<Vec<SinkEvent>>>,
        Rc<RefCell<VecDeque<ClockStep>>>,
    ) {
        let sink = RecordingSink::default();
        let events = Rc::clone(&sink.events);
        let clock = FaultClock::default();
        let steps = Rc::clone(&clock.steps);
        let writer = MvsCaptureV2Writer::new(
            Box::new(sink),
            Box::new(clock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 3,
            },
        )
        .unwrap();
        (writer, events, steps)
    }

    fn failing_writer(point: FailurePoint) -> (MvsCaptureV2Writer, Rc<RefCell<Vec<SinkEvent>>>) {
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = FailingSink {
            events: Rc::clone(&events),
            point,
            writes: 0,
            flushes: 0,
            syncs: 0,
        };
        let writer = MvsCaptureV2Writer::new(
            Box::new(sink),
            Box::new(ManualClock::default()),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 3,
            },
        )
        .unwrap();
        (writer, events)
    }

    fn arm_config() -> ArmedConfig {
        ArmedConfig {
            committed: MvsCaptureV2Geometry {
                width: 640,
                height: 480,
            },
            requested: MvsCaptureV2Geometry {
                width: 800,
                height: 600,
            },
        }
    }

    fn armed_writer() -> (
        MvsCaptureV2Writer,
        Rc<RefCell<Vec<SinkEvent>>>,
        Rc<RefCell<u64>>,
    ) {
        let (mut writer, events, clock) = created_writer();
        assert_eq!(writer.arm(arm_config()).unwrap(), WriterDecision::Continue);
        (writer, events, clock)
    }

    fn recording_writer() -> (
        MvsCaptureV2Writer,
        Rc<RefCell<Vec<SinkEvent>>>,
        Rc<RefCell<u64>>,
    ) {
        let (mut writer, events, clock) = armed_writer();
        assert_eq!(writer.begin_trigger().unwrap(), WriterDecision::Continue);
        for bit in [1, 2, 4] {
            assert_eq!(
                writer.trigger_write_succeeded(bit).unwrap(),
                WriterDecision::Continue
            );
        }
        assert_eq!(
            writer.write_recording_gate().unwrap(),
            WriterDecision::Continue
        );
        (writer, events, clock)
    }

    fn drive_to_recording(writer: &mut MvsCaptureV2Writer) {
        drive_to_trigger_mask(writer);
        writer.write_recording_gate().unwrap();
    }

    fn drive_to_trigger_mask(writer: &mut MvsCaptureV2Writer) {
        writer.arm(arm_config()).unwrap();
        writer.begin_trigger().unwrap();
        for bit in [1, 2, 4] {
            writer.trigger_write_succeeded(bit).unwrap();
        }
    }

    fn capture_bytes(events: &[SinkEvent]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for event in events {
            if let SinkEvent::Write(part) = event {
                bytes.extend_from_slice(part);
            }
        }
        bytes
    }

    #[test]
    fn construction_writes_created_and_flushes_before_returning() {
        let sink = RecordingSink::default();
        let events = Rc::clone(&sink.events);
        let writer = MvsCaptureV2Writer::new(
            Box::new(sink),
            Box::new(ManualClock::default()),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 1,
            },
        )
        .unwrap();

        assert_eq!(writer.state(), CaptureState::Created);
        let events = events.borrow();
        assert_eq!(events.len(), 2);
        let SinkEvent::Write(bytes) = &events[0] else {
            panic!("first sink operation must write Header + Created")
        };
        assert_eq!(&bytes[..8], b"FRDMVS02");
        assert_eq!(bytes.len(), 80);
        assert_eq!(events[1], SinkEvent::Flush);
    }

    #[test]
    fn arm_is_flushed_and_synced_before_triggering_is_available() {
        let (mut writer, events, _) = created_writer();
        assert_eq!(writer.arm(arm_config()).unwrap(), WriterDecision::Continue);
        assert_eq!(writer.state(), CaptureState::Armed);
        let events = events.borrow();
        assert!(matches!(events[2], SinkEvent::Write(_)));
        assert_eq!(events[3], SinkEvent::Flush);
        assert_eq!(events[4], SinkEvent::Sync);
        drop(events);
        assert_eq!(writer.begin_trigger().unwrap(), WriterDecision::Continue);
        assert_eq!(writer.state(), CaptureState::Triggering);
    }

    #[test]
    fn deadline_after_first_trigger_attempt_is_reason_260_not_267() {
        let (mut writer, _, clock) = armed_writer();
        writer.begin_trigger().unwrap();
        *clock.borrow_mut() = DEADLINE_MICROS;
        assert_eq!(
            writer.trigger_write_succeeded(1).unwrap(),
            WriterDecision::Finalized(CaptureState::Aborted)
        );
        assert_eq!(writer.selected_abort_reason(), Some(260));
    }

    #[test]
    fn armed_post_sync_deadline_is_pre_trigger_reason_267() {
        let sink = RecordingSink::default();
        let events = Rc::clone(&sink.events);
        let clock = ScriptClock::default();
        clock.samples.borrow_mut().extend([0, DEADLINE_MICROS]);
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(sink),
            Box::new(clock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 3,
            },
        )
        .unwrap();
        writer.arm(arm_config()).unwrap();
        assert_eq!(writer.selected_abort_reason(), Some(267));
        let events = events.borrow();
        assert!(matches!(events[2], SinkEvent::Write(_)));
        assert_eq!(events[3], SinkEvent::Flush);
        assert_eq!(events[4], SinkEvent::Sync);
    }

    #[test]
    fn armed_uses_the_single_immediate_prewrite_sample_as_its_timestamp() {
        let sink = RecordingSink::default();
        let events = Rc::clone(&sink.events);
        let clock = ScriptClock::default();
        clock.samples.borrow_mut().extend([
            DEADLINE_MICROS - 2,
            DEADLINE_MICROS - 1,
            DEADLINE_MICROS,
            DEADLINE_MICROS,
        ]);
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(sink),
            Box::new(clock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 3,
            },
        )
        .unwrap();
        writer.pre_trigger_checkpoint().unwrap();
        writer.arm(arm_config()).unwrap();
        let events = events.borrow();
        let armed = events
            .iter()
            .find_map(|event| match event {
                SinkEvent::Write(bytes) if bytes[4..6] == [0, 2] => Some(bytes),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            u64::from_be_bytes(armed[24..32].try_into().unwrap()),
            DEADLINE_MICROS - 1
        );
    }

    #[test]
    fn armed_immediate_sample_equal_to_deadline_writes_no_armed_and_selects_267() {
        let sink = RecordingSink::default();
        let events = Rc::clone(&sink.events);
        let clock = ScriptClock::default();
        clock
            .samples
            .borrow_mut()
            .extend([DEADLINE_MICROS - 1, DEADLINE_MICROS]);
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(sink),
            Box::new(clock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 3,
            },
        )
        .unwrap();
        writer.pre_trigger_checkpoint().unwrap();
        writer.arm(arm_config()).unwrap();
        assert_eq!(writer.selected_abort_reason(), Some(267));
        assert!(!events
            .borrow()
            .iter()
            .any(|event| matches!(event, SinkEvent::Write(bytes) if bytes[4..6] == [0, 2])));
    }

    #[test]
    fn clock_regression_after_armed_write_consumes_the_sink() {
        let sink = RecordingSink::default();
        let events = Rc::clone(&sink.events);
        let clock = ScriptClock::default();
        clock.samples.borrow_mut().extend([10, 9]);
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(sink),
            Box::new(clock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 3,
            },
        )
        .unwrap();
        assert!(writer.arm(arm_config()).is_err());
        assert_eq!(writer.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());
    }

    #[test]
    fn post_triggered_deadline_keeps_triggered_but_never_writes_recording() {
        let sink = RecordingSink::default();
        let events = Rc::clone(&sink.events);
        let clock = ScriptClock::default();
        let samples = Rc::clone(&clock.samples);
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(sink),
            Box::new(clock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 3,
            },
        )
        .unwrap();
        drive_to_trigger_mask(&mut writer);
        samples
            .borrow_mut()
            .extend([DEADLINE_MICROS - 1, DEADLINE_MICROS]);
        writer.write_recording_gate().unwrap();
        assert_eq!(writer.selected_abort_reason(), Some(260));
        let events = events.borrow();
        assert!(events
            .iter()
            .any(|event| { matches!(event, SinkEvent::Write(bytes) if bytes[4..6] == [0, 3]) }));
        assert!(!events
            .iter()
            .any(|event| { matches!(event, SinkEvent::Write(bytes) if bytes[4..6] == [0, 4]) }));
    }

    #[test]
    fn triggered_uses_the_single_immediate_prewrite_sample_as_its_timestamp() {
        let sink = RecordingSink::default();
        let events = Rc::clone(&sink.events);
        let clock = ScriptClock::default();
        let samples = Rc::clone(&clock.samples);
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(sink),
            Box::new(clock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 3,
            },
        )
        .unwrap();
        drive_to_trigger_mask(&mut writer);
        samples
            .borrow_mut()
            .extend([DEADLINE_MICROS - 1, DEADLINE_MICROS, DEADLINE_MICROS]);
        writer.write_recording_gate().unwrap();
        let events = events.borrow();
        let triggered = events
            .iter()
            .find_map(|event| match event {
                SinkEvent::Write(bytes) if bytes[4..6] == [0, 3] => Some(bytes),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            u64::from_be_bytes(triggered[24..32].try_into().unwrap()),
            DEADLINE_MICROS - 1
        );
        assert_eq!(writer.selected_abort_reason(), Some(260));
    }

    #[test]
    fn triggered_immediate_sample_equal_to_deadline_writes_no_triggered_and_selects_260() {
        let sink = RecordingSink::default();
        let events = Rc::clone(&sink.events);
        let clock = ScriptClock::default();
        let samples = Rc::clone(&clock.samples);
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(sink),
            Box::new(clock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 3,
            },
        )
        .unwrap();
        drive_to_trigger_mask(&mut writer);
        samples.borrow_mut().push_back(DEADLINE_MICROS);
        writer.write_recording_gate().unwrap();
        assert_eq!(writer.selected_abort_reason(), Some(260));
        assert!(!events
            .borrow()
            .iter()
            .any(|event| matches!(event, SinkEvent::Write(bytes) if bytes[4..6] == [0, 3])));
    }

    #[test]
    fn recording_event_does_not_publish_state_before_post_flush_sample() {
        let sink = RecordingSink::default();
        let events = Rc::clone(&sink.events);
        let clock = ScriptClock::default();
        let samples = Rc::clone(&clock.samples);
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(sink),
            Box::new(clock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 3,
            },
        )
        .unwrap();
        writer.arm(arm_config()).unwrap();
        writer.begin_trigger().unwrap();
        for bit in [1, 2, 4] {
            writer.trigger_write_succeeded(bit).unwrap();
        }
        samples.borrow_mut().extend([
            DEADLINE_MICROS - 1,
            DEADLINE_MICROS - 1,
            DEADLINE_MICROS - 1,
            DEADLINE_MICROS,
        ]);
        writer.write_recording_gate().unwrap();
        assert_ne!(writer.state(), CaptureState::Recording);
        assert_eq!(writer.selected_abort_reason(), Some(260));
        let events = events.borrow();
        let recording_write = events.iter().rposition(|event| {
            matches!(event, SinkEvent::Write(bytes) if bytes.len() == 40 && bytes[4..6] == [0, 4])
        });
        assert!(recording_write.is_some());
        assert!(matches!(
            events[recording_write.unwrap() + 1],
            SinkEvent::Flush
        ));
    }

    #[test]
    fn trigger_mask_must_be_reported_in_wire_order() {
        let (mut writer, events, _) = armed_writer();
        writer.begin_trigger().unwrap();
        let before = events.borrow().len();
        assert!(writer.trigger_write_succeeded(2).is_err());
        assert_eq!(writer.state(), CaptureState::Triggering);
        assert_eq!(events.borrow().len(), before);
    }

    #[test]
    fn only_mvs_inputs_consume_source_ordinals_and_records_are_exact() {
        let (mut writer, events, clock) = recording_writer();
        writer.accept_non_mvs().unwrap();
        writer
            .accept_mvs_begin(rect(1, 2, 3, 4), 3, &[0, 10, 11])
            .unwrap();
        writer.accept_non_mvs().unwrap();
        writer
            .accept_mvs_begin(rect(5, 6, 7, 8), 4, &[1, 20])
            .unwrap();
        writer.accept_mvs_continuation(&[21, 22]).unwrap();
        *clock.borrow_mut() = DEADLINE_MICROS;
        writer.poll_recording().unwrap();

        let bytes = capture_bytes(&events.borrow());
        let parsed = read_mvs_capture_v2_structural(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(parsed.records.len(), 2);
        assert_eq!(
            (
                parsed.records[0].first_source_frame_ordinal,
                parsed.records[0].last_source_frame_ordinal,
                parsed.records[0].record.payload.as_slice()
            ),
            (0, 0, &[0, 10, 11][..])
        );
        assert_eq!(
            (
                parsed.records[1].first_source_frame_ordinal,
                parsed.records[1].last_source_frame_ordinal,
                parsed.records[1].record.payload.as_slice()
            ),
            (1, 2, &[1, 20, 21, 22][..])
        );
    }

    #[test]
    fn all_three_payload_tags_are_counted_from_complete_records() {
        let (mut writer, events, _) = recording_writer();
        writer.accept_mvs_begin(rect(1, 2, 3, 4), 1, &[0]).unwrap();
        writer.accept_mvs_begin(rect(5, 6, 7, 8), 1, &[1]).unwrap();
        writer.accept_mvs_begin(rect(0, 0, 0, 0), 1, &[2]).unwrap();
        let bytes = capture_bytes(&events.borrow());
        let parsed = read_mvs_capture_v2_structural(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(
            (
                parsed.terminal.type0_count,
                parsed.terminal.type1_count,
                parsed.terminal.type2_count,
                parsed.terminal.record_count
            ),
            (1, 1, 1, 3)
        );
    }

    #[test]
    fn all_gap_reasons_preserve_exact_provenance() {
        let zero = rect(0, 0, 0, 0);

        let (mut w1, _, _) = recording_writer();
        w1.reject_mvs_envelope(Some(rect(1, 2, 3, 4))).unwrap();
        assert_eq!(w1.last_gap().unwrap().reason, 1);

        let (mut w2, _, _) = recording_writer();
        w2.accept_mvs_begin(rect(1, 2, 3, 4), 0, &[]).unwrap();
        assert_eq!(
            (
                w2.last_gap().unwrap().reason,
                w2.last_gap().unwrap().declared_total
            ),
            (2, 0)
        );

        let (mut w3, _, _) = recording_writer();
        w3.accept_mvs_begin(rect(1, 2, 3, 4), 1, &[0, 1]).unwrap();
        assert_eq!(w3.last_gap().unwrap().reason, 3);

        let (mut w4, _, _) = recording_writer();
        w4.accept_mvs_begin(rect(1, 2, 3, 4), 4, &[0, 1]).unwrap();
        w4.accept_mvs_continuation(&[2, 3, 4]).unwrap();
        assert_eq!(
            (
                w4.last_gap().unwrap().reason,
                w4.last_gap().unwrap().accumulated_bytes
            ),
            (4, 2)
        );

        let (mut w5, _, c5) = recording_writer();
        w5.accept_mvs_begin(rect(1, 2, 3, 4), 4, &[0]).unwrap();
        *c5.borrow_mut() = INCOMPLETE_RECORD_MICROS;
        w5.poll_recording().unwrap();
        assert_eq!(w5.last_gap().unwrap().reason, 5);

        let (mut w6, _, c6) = recording_writer();
        w6.accept_mvs_begin(rect(1, 2, 3, 4), 4, &[0]).unwrap();
        *c6.borrow_mut() = DEADLINE_MICROS;
        w6.poll_recording().unwrap();
        assert_eq!(w6.last_gap().unwrap().reason, 6);

        let (mut w7, _, _) = recording_writer();
        w7.accept_mvs_begin(rect(1, 2, 3, 4), 4, &[0]).unwrap();
        w7.read_failed().unwrap();
        assert_eq!(w7.last_gap().unwrap().reason, 7);

        let (mut w8, _, _) = recording_writer();
        w8.accept_mvs_begin(rect(1, 2, 3, 4), 4, &[0]).unwrap();
        w8.generation_transition_attempted().unwrap();
        assert_eq!(w8.last_gap().unwrap().reason, 8);

        let (mut w9, _, _) = recording_writer();
        w9.accept_mvs_continuation(&[0]).unwrap();
        assert_eq!(
            (w9.last_gap().unwrap().reason, w9.last_gap().unwrap().rect),
            (9, zero)
        );

        let (mut w10, _, _) = recording_writer();
        w10.accept_mvs_begin(rect(1, 2, 3, 4), 4, &[0]).unwrap();
        w10.accept_mvs_begin(rect(9, 9, 1, 1), 2, &[0]).unwrap();
        assert_eq!(
            (
                w10.last_gap().unwrap().reason,
                w10.last_gap().unwrap().first_source_frame_ordinal,
                w10.last_gap().unwrap().last_source_frame_ordinal
            ),
            (10, 0, 1)
        );

        let (mut w11, _, _) = recording_writer();
        w11.cumulative_payload = MVS_CAPTURE_V2_MAX_CUMULATIVE_PAYLOAD - 3;
        w11.accept_mvs_begin(rect(1, 2, 3, 4), 4, &[0]).unwrap();
        assert_eq!(
            (
                w11.last_gap().unwrap().reason,
                w11.last_gap().unwrap().accumulated_bytes
            ),
            (11, 0)
        );

        let (mut w12, _, _) = recording_writer();
        w12.accept_mvs_begin(rect(1, 2, 3, 4), 4, &[0]).unwrap();
        w12.cancel().unwrap();
        assert_eq!(
            (w12.last_gap().unwrap().reason, w12.selected_abort_reason()),
            (12, Some(265))
        );
    }

    #[test]
    fn gap_table_freezes_all_fields_and_terminal_mappings() {
        let mut observed = Vec::new();
        let pending_rect = rect(1, 2, 3, 4);

        let (mut writer, _, _) = recording_writer();
        writer.reject_mvs_envelope(Some(pending_rect)).unwrap();
        observed.push((
            writer.last_gap().unwrap().clone(),
            writer.selected_abort_reason().unwrap(),
        ));

        for (total, first) in [(0, vec![]), (1, vec![0, 1])] {
            let (mut writer, _, _) = recording_writer();
            writer
                .accept_mvs_begin(pending_rect, total, &first)
                .unwrap();
            observed.push((
                writer.last_gap().unwrap().clone(),
                writer.selected_abort_reason().unwrap(),
            ));
        }

        let (mut writer, _, _) = recording_writer();
        writer.accept_mvs_begin(pending_rect, 4, &[0, 1]).unwrap();
        writer.accept_mvs_continuation(&[2, 3, 4]).unwrap();
        observed.push((
            writer.last_gap().unwrap().clone(),
            writer.selected_abort_reason().unwrap(),
        ));

        for reason in [5u16, 6, 7, 8] {
            let (mut writer, _, clock) = recording_writer();
            writer.accept_mvs_begin(pending_rect, 4, &[0]).unwrap();
            match reason {
                5 => {
                    *clock.borrow_mut() = INCOMPLETE_RECORD_MICROS;
                    writer.poll_recording().unwrap();
                }
                6 => {
                    *clock.borrow_mut() = DEADLINE_MICROS;
                    writer.poll_recording().unwrap();
                }
                7 => {
                    writer.read_failed().unwrap();
                }
                8 => {
                    writer.generation_transition_attempted().unwrap();
                }
                _ => unreachable!(),
            }
            observed.push((
                writer.last_gap().unwrap().clone(),
                writer.selected_abort_reason().unwrap(),
            ));
        }

        let (mut writer, _, _) = recording_writer();
        writer.accept_mvs_continuation(&[0]).unwrap();
        observed.push((
            writer.last_gap().unwrap().clone(),
            writer.selected_abort_reason().unwrap(),
        ));

        let (mut writer, _, _) = recording_writer();
        writer.accept_mvs_begin(pending_rect, 4, &[0]).unwrap();
        writer.accept_mvs_begin(rect(9, 9, 1, 1), 2, &[0]).unwrap();
        observed.push((
            writer.last_gap().unwrap().clone(),
            writer.selected_abort_reason().unwrap(),
        ));

        let (mut writer, _, _) = recording_writer();
        writer.cumulative_payload = MVS_CAPTURE_V2_MAX_CUMULATIVE_PAYLOAD - 3;
        writer.accept_mvs_begin(pending_rect, 4, &[0]).unwrap();
        observed.push((
            writer.last_gap().unwrap().clone(),
            writer.selected_abort_reason().unwrap(),
        ));

        let (mut writer, _, _) = recording_writer();
        writer.accept_mvs_begin(pending_rect, 4, &[0]).unwrap();
        writer.cancel().unwrap();
        observed.push((
            writer.last_gap().unwrap().clone(),
            writer.selected_abort_reason().unwrap(),
        ));

        let expected = [
            (1, 1, 0, 0, 0, 0, pending_rect, 262),
            (2, 1, 0, 0, 0, 0, pending_rect, 262),
            (3, 1, 0, 0, 1, 0, pending_rect, 262),
            (4, 2, 0, 1, 4, 2, pending_rect, 262),
            (5, 3, 0, 0, 4, 1, pending_rect, 262),
            (6, 4, 0, 0, 4, 1, pending_rect, 263),
            (7, 5, 0, 0, 4, 1, pending_rect, 261),
            (8, 6, 0, 0, 4, 1, pending_rect, 266),
            (9, 2, 0, 0, 0, 0, rect(0, 0, 0, 0), 262),
            (10, 1, 0, 1, 4, 1, pending_rect, 262),
            (11, 8, 0, 0, 4, 0, pending_rect, 262),
            (12, 7, 0, 0, 4, 1, pending_rect, 265),
        ];
        for ((gap, terminal), want) in observed.iter().zip(expected) {
            assert_eq!(
                (
                    gap.reason,
                    gap.stage,
                    gap.first_source_frame_ordinal,
                    gap.last_source_frame_ordinal,
                    gap.declared_total,
                    gap.accumulated_bytes,
                    gap.rect,
                    *terminal
                ),
                want
            );
        }
    }

    #[test]
    fn fifty_millisecond_continuations_do_not_refresh_pending_age() {
        let (mut writer, _, clock) = recording_writer();
        writer.accept_mvs_begin(rect(1, 2, 3, 4), 42, &[0]).unwrap();
        for step in 1..40 {
            *clock.borrow_mut() = step * 50_000;
            assert_eq!(
                writer.accept_mvs_continuation(&[1]).unwrap(),
                WriterDecision::Continue
            );
        }
        *clock.borrow_mut() = INCOMPLETE_RECORD_MICROS;
        writer.poll_recording().unwrap();
        assert_eq!(writer.last_gap().unwrap().reason, 5);
        assert_eq!(writer.last_gap().unwrap().accumulated_bytes, 40);
    }

    #[test]
    fn cancellation_uses_gap_only_when_pending() {
        let (mut pending, _, _) = recording_writer();
        pending
            .accept_mvs_begin(rect(1, 2, 3, 4), 8, &[0, 1])
            .unwrap();
        pending.cancel().unwrap();
        let gap = pending.last_gap().unwrap();
        assert_eq!(
            (
                gap.reason,
                gap.stage,
                gap.declared_total,
                gap.accumulated_bytes
            ),
            (12, 7, 8, 2)
        );
        assert_eq!(pending.selected_abort_reason(), Some(265));

        let (mut idle, _, _) = recording_writer();
        idle.cancel().unwrap();
        assert_eq!(idle.gap_count(), 0);
        assert_eq!(idle.selected_abort_reason(), Some(265));
    }

    #[test]
    fn post_frame_deadline_precedes_record_limit_after_complete_record_write() {
        let (mut writer, events, clock) = recording_writer();
        writer.config.record_limit = 1;
        *clock.borrow_mut() = DEADLINE_MICROS;
        writer.accept_mvs_begin(rect(1, 2, 3, 4), 1, &[0]).unwrap();
        assert_eq!(writer.state(), CaptureState::Clean);
        assert_eq!(writer.selected_terminal_reason(), Some(1));
        let bytes = capture_bytes(&events.borrow());
        let parsed = read_mvs_capture_v2_structural(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(parsed.records.len(), 1);
    }

    #[test]
    fn record_limit_wins_only_strictly_before_deadline() {
        let (mut writer, _, clock) = recording_writer();
        writer.config.record_limit = 1;
        *clock.borrow_mut() = DEADLINE_MICROS - 1;
        writer.accept_mvs_begin(rect(1, 2, 3, 4), 1, &[0]).unwrap();
        assert_eq!(writer.selected_terminal_reason(), Some(2));
    }

    #[test]
    fn clean_two_reuses_the_post_frame_selection_timestamp() {
        let sink = RecordingSink::default();
        let events = Rc::clone(&sink.events);
        let clock = ScriptClock::default();
        let samples = Rc::clone(&clock.samples);
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(sink),
            Box::new(clock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 1,
            },
        )
        .unwrap();
        drive_to_recording(&mut writer);
        samples
            .borrow_mut()
            .extend([0, 0, DEADLINE_MICROS - 1, DEADLINE_MICROS]);
        writer.accept_mvs_begin(rect(1, 2, 3, 4), 1, &[0]).unwrap();
        let bytes = capture_bytes(&events.borrow());
        let parsed = read_mvs_capture_v2_strict_cold(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(parsed.terminal.timestamp_us, DEADLINE_MICROS - 1);
        assert_eq!(
            parsed.terminal.reason,
            crate::vnc::mvs_capture_v2::MvsCaptureV2TerminalReason::RecordLimit
        );
    }

    #[test]
    fn read_generation_and_cancellation_keep_their_priority_contracts() {
        let (mut read, _, clock) = recording_writer();
        read.accept_mvs_begin(rect(1, 2, 3, 4), 4, &[0]).unwrap();
        *clock.borrow_mut() = DEADLINE_MICROS;
        read.read_failed().unwrap();
        assert_eq!(
            (
                read.last_gap().unwrap().reason,
                read.selected_abort_reason()
            ),
            (6, Some(263))
        );

        let (mut generation, _, clock) = recording_writer();
        generation
            .accept_mvs_begin(rect(1, 2, 3, 4), 4, &[0])
            .unwrap();
        *clock.borrow_mut() = DEADLINE_MICROS;
        generation.generation_transition_attempted().unwrap();
        assert_eq!(
            (
                generation.last_gap().unwrap().reason,
                generation.selected_abort_reason()
            ),
            (8, Some(266))
        );

        let (mut cancellation, _, clock) = recording_writer();
        cancellation
            .accept_mvs_begin(rect(1, 2, 3, 4), 4, &[0])
            .unwrap();
        *clock.borrow_mut() = DEADLINE_MICROS;
        cancellation.cancel().unwrap();
        assert_eq!(
            (
                cancellation.last_gap().unwrap().reason,
                cancellation.selected_abort_reason()
            ),
            (12, Some(265))
        );
    }

    #[test]
    fn complete_record_is_written_before_payload_classification() {
        let (mut writer, events, _) = recording_writer();
        assert!(writer.accept_mvs_begin(rect(1, 2, 3, 4), 1, &[9]).is_err());
        let events = events.borrow();
        assert!(events.iter().any(|event| {
            matches!(event, SinkEvent::Write(bytes) if bytes.len() == 61 && bytes[4..6] == [0, 0x20] && bytes[60] == 9)
        }));
    }

    #[test]
    fn invalid_record_geometry_is_detected_after_the_complete_record_write() {
        let (mut writer, events, _) = recording_writer();
        assert!(writer
            .accept_mvs_begin(rect(639, 479, 2, 2), 1, &[0])
            .is_err());
        assert!(events.borrow().iter().any(|event| {
            matches!(event, SinkEvent::Write(bytes) if bytes.len() == 61 && bytes[4..6] == [0, 0x20])
        }));
    }

    #[test]
    fn offending_continuation_emits_gap_without_a_partial_record() {
        let (mut writer, events, _) = recording_writer();
        writer
            .accept_mvs_begin(rect(1, 2, 3, 4), 4, &[0, 1])
            .unwrap();
        writer.accept_mvs_continuation(&[2, 3, 4]).unwrap();
        let bytes = capture_bytes(&events.borrow());
        let parsed = read_mvs_capture_v2_structural(&mut Cursor::new(bytes)).unwrap();
        assert!(parsed.records.is_empty());
        assert_eq!(parsed.gaps[0].reason, 4);
    }

    #[test]
    fn event_reservation_keeps_gap_and_terminal_transactional() {
        let (mut writer, events, _) = recording_writer();
        writer.event_count = MVS_CAPTURE_V2_MAX_EVENTS - 1;
        writer.accept_mvs_begin(rect(1, 2, 3, 4), 4, &[0]).unwrap();
        let before = events.borrow().len();
        assert!(writer.cancel().is_err());
        assert_eq!(events.borrow().len(), before + 3);
        assert_eq!(writer.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());
        assert_eq!(writer.gap_count(), 0);
    }

    fn assert_cleanup_tail(events: &[SinkEvent]) {
        let tail = &events[events.len() - 3..];
        assert_eq!(tail[0], SinkEvent::IntoInner);
        assert_eq!(tail[1], SinkEvent::Sync);
        assert_eq!(tail[2], SinkEvent::Relinquish);
    }

    fn assert_cleanup_exactly_once(events: &[SinkEvent]) {
        assert_cleanup_tail(events);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, SinkEvent::IntoInner))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, SinkEvent::Relinquish))
                .count(),
            1
        );
    }

    fn assert_future_input_rejected(
        writer: &mut MvsCaptureV2Writer,
        events: &Rc<RefCell<Vec<SinkEvent>>>,
    ) {
        let before = events.borrow().len();
        assert!(writer.accept_non_mvs().is_err());
        assert_eq!(events.borrow().len(), before);
    }

    #[test]
    fn clock_failures_after_each_irreversible_lifecycle_boundary_consume_the_sink() {
        let (mut armed, events, steps) = fault_clock_writer();
        steps
            .borrow_mut()
            .extend([ClockStep::Micros(0), ClockStep::Error]);
        assert!(armed.arm(arm_config()).is_err());
        assert_eq!(armed.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());
        assert_future_input_rejected(&mut armed, &events);

        let (mut trigger, events, steps) = fault_clock_writer();
        trigger.arm(arm_config()).unwrap();
        trigger.begin_trigger().unwrap();
        steps.borrow_mut().push_back(ClockStep::Error);
        assert!(trigger.trigger_write_succeeded(1).is_err());
        assert_eq!(trigger.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());
        assert_future_input_rejected(&mut trigger, &events);

        let (mut triggered, events, steps) = fault_clock_writer();
        drive_to_trigger_mask(&mut triggered);
        steps
            .borrow_mut()
            .extend([ClockStep::Micros(0), ClockStep::Error]);
        assert!(triggered.write_recording_gate().is_err());
        assert_eq!(triggered.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());
        assert_future_input_rejected(&mut triggered, &events);

        let (mut recording, events, steps) = fault_clock_writer();
        drive_to_trigger_mask(&mut recording);
        steps.borrow_mut().extend([
            ClockStep::Micros(0),
            ClockStep::Micros(0),
            ClockStep::Micros(0),
            ClockStep::Error,
        ]);
        assert!(recording.write_recording_gate().is_err());
        assert_eq!(recording.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());
        assert_future_input_rejected(&mut recording, &events);
    }

    #[test]
    fn created_clock_error_and_regression_consume_once_and_prohibit_retry() {
        let (mut checkpoint, events, steps) = fault_clock_writer();
        steps.borrow_mut().push_back(ClockStep::Error);
        assert!(checkpoint.pre_trigger_checkpoint().is_err());
        assert_eq!(checkpoint.state(), CaptureState::Finalizing);
        assert_eq!(checkpoint.selected_abort_reason(), Some(264));
        assert_cleanup_exactly_once(&events.borrow());
        assert_future_input_rejected(&mut checkpoint, &events);

        let (mut regression, events, steps) = fault_clock_writer();
        steps
            .borrow_mut()
            .extend([ClockStep::Micros(10), ClockStep::Micros(9)]);
        assert_eq!(
            regression.pre_trigger_checkpoint().unwrap(),
            WriterDecision::Continue
        );
        assert!(regression.pre_trigger_checkpoint().is_err());
        assert_eq!(regression.state(), CaptureState::Finalizing);
        assert_eq!(regression.selected_abort_reason(), Some(264));
        assert_cleanup_exactly_once(&events.borrow());
        assert_future_input_rejected(&mut regression, &events);

        let (mut arm, events, steps) = fault_clock_writer();
        steps.borrow_mut().push_back(ClockStep::Error);
        assert!(arm.arm(arm_config()).is_err());
        assert_eq!(arm.state(), CaptureState::Finalizing);
        assert_eq!(arm.selected_abort_reason(), Some(264));
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|event| matches!(event, SinkEvent::Write(_)))
                .count(),
            1
        );
        assert_cleanup_exactly_once(&events.borrow());
        assert_future_input_rejected(&mut arm, &events);

        let (mut arm_regression, events, steps) = fault_clock_writer();
        steps
            .borrow_mut()
            .extend([ClockStep::Micros(10), ClockStep::Micros(9)]);
        arm_regression.pre_trigger_checkpoint().unwrap();
        assert!(arm_regression.arm(arm_config()).is_err());
        assert_eq!(arm_regression.state(), CaptureState::Finalizing);
        assert_eq!(arm_regression.selected_abort_reason(), Some(264));
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|event| matches!(event, SinkEvent::Write(_)))
                .count(),
            1
        );
        assert_cleanup_exactly_once(&events.borrow());
        assert_future_input_rejected(&mut arm_regression, &events);
    }

    #[test]
    fn connect_and_auth_failures_sample_once_and_classify_deadline_equality_as_267() {
        type FailureOperation = fn(&mut MvsCaptureV2Writer) -> anyhow::Result<WriterDecision>;
        for (operation, ordinary_reason) in [
            (MvsCaptureV2Writer::connect_failed as FailureOperation, 257),
            (
                MvsCaptureV2Writer::authentication_failed as FailureOperation,
                258,
            ),
        ] {
            for (sample, expected_reason) in [
                (DEADLINE_MICROS - 1, ordinary_reason),
                (DEADLINE_MICROS, 267),
            ] {
                let sink = RecordingSink::default();
                let events = Rc::clone(&sink.events);
                let clock = ScriptClock::default();
                clock.samples.borrow_mut().push_back(sample);
                let mut writer = MvsCaptureV2Writer::new(
                    Box::new(sink),
                    Box::new(clock),
                    CreatedConfig {
                        deadline_ms: 5_000,
                        record_limit: 3,
                    },
                )
                .unwrap();

                assert_eq!(
                    operation(&mut writer).unwrap(),
                    WriterDecision::Finalized(CaptureState::Aborted)
                );
                assert_eq!(writer.selected_abort_reason(), Some(expected_reason));
                let parsed = read_mvs_capture_v2_structural(&mut Cursor::new(capture_bytes(
                    &events.borrow(),
                )))
                .unwrap();
                assert_eq!(parsed.terminal.timestamp_us, sample);
            }
        }
    }

    #[test]
    fn explicit_pre_trigger_deadline_failure_forces_267_before_clock_equality() {
        let sink = RecordingSink::default();
        let events = Rc::clone(&sink.events);
        let clock = ScriptClock::default();
        clock.samples.borrow_mut().push_back(DEADLINE_MICROS - 1);
        let mut writer = MvsCaptureV2Writer::new(
            Box::new(sink),
            Box::new(clock),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 3,
            },
        )
        .unwrap();

        assert_eq!(
            writer.pre_trigger_deadline_failed().unwrap(),
            WriterDecision::Finalized(CaptureState::Aborted)
        );
        assert_eq!(writer.selected_abort_reason(), Some(267));
        let parsed =
            read_mvs_capture_v2_structural(&mut Cursor::new(capture_bytes(&events.borrow())))
                .unwrap();
        assert_eq!(parsed.terminal.timestamp_us, DEADLINE_MICROS - 1);
        assert_cleanup_exactly_once(&events.borrow());
        assert_future_input_rejected(&mut writer, &events);
    }

    #[test]
    fn connect_and_auth_clock_errors_fail_closed_as_264() {
        type FailureOperation = fn(&mut MvsCaptureV2Writer) -> anyhow::Result<WriterDecision>;
        for operation in [
            MvsCaptureV2Writer::connect_failed as FailureOperation,
            MvsCaptureV2Writer::authentication_failed as FailureOperation,
        ] {
            let (mut writer, events, steps) = fault_clock_writer();
            steps.borrow_mut().push_back(ClockStep::Error);
            assert!(operation(&mut writer).is_err());
            assert_eq!(writer.selected_abort_reason(), Some(264));
            assert_eq!(writer.state(), CaptureState::Finalizing);
            assert_eq!(
                events
                    .borrow()
                    .iter()
                    .filter(|event| matches!(event, SinkEvent::Write(_)))
                    .count(),
                1
            );
            assert_cleanup_exactly_once(&events.borrow());
            assert_future_input_rejected(&mut writer, &events);

            let (mut regression, events, steps) = fault_clock_writer();
            steps
                .borrow_mut()
                .extend([ClockStep::Micros(10), ClockStep::Micros(9)]);
            regression.pre_trigger_checkpoint().unwrap();
            assert!(operation(&mut regression).is_err());
            assert_eq!(regression.selected_abort_reason(), Some(264));
            assert_eq!(regression.state(), CaptureState::Finalizing);
            assert_cleanup_exactly_once(&events.borrow());
            assert_future_input_rejected(&mut regression, &events);
        }
    }

    #[test]
    fn clock_regression_after_armed_sync_is_irreversible() {
        let (mut writer, events, steps) = fault_clock_writer();
        steps
            .borrow_mut()
            .extend([ClockStep::Micros(10), ClockStep::Micros(9)]);
        assert!(writer.arm(arm_config()).is_err());
        assert_eq!(writer.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());
    }

    #[test]
    fn record_gap_and_post_mutation_failures_are_fail_closed() {
        let (mut record, events) = failing_writer(FailurePoint::RecordOrGapWrite);
        drive_to_recording(&mut record);
        assert!(record.accept_mvs_begin(rect(1, 2, 3, 4), 1, &[0]).is_err());
        assert_eq!((record.source_frame_count, record.record_count), (1, 0));
        assert_eq!(record.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());
        assert_future_input_rejected(&mut record, &events);

        let (mut gap, events) = failing_writer(FailurePoint::RecordOrGapWrite);
        drive_to_recording(&mut gap);
        assert!(gap.reject_mvs_envelope(None).is_err());
        assert_eq!((gap.source_frame_count, gap.gap_count()), (1, 0));
        assert_eq!(gap.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());
        assert_future_input_rejected(&mut gap, &events);

        let (mut clock, events, steps) = fault_clock_writer();
        drive_to_recording(&mut clock);
        steps
            .borrow_mut()
            .extend([ClockStep::Micros(0), ClockStep::Micros(0), ClockStep::Error]);
        assert!(clock.accept_mvs_begin(rect(1, 2, 3, 4), 1, &[0]).is_err());
        assert_eq!((clock.source_frame_count, clock.record_count), (1, 1));
        assert_eq!(clock.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());
        assert_future_input_rejected(&mut clock, &events);

        let (mut continuation, events, steps) = fault_clock_writer();
        drive_to_recording(&mut continuation);
        continuation
            .accept_mvs_begin(rect(1, 2, 3, 4), 3, &[0])
            .unwrap();
        steps.borrow_mut().push_back(ClockStep::Error);
        assert!(continuation.accept_mvs_continuation(&[1]).is_err());
        assert_eq!(continuation.source_frame_count, 2);
        assert_eq!(continuation.pending.unwrap().accepted, 2);
        assert_eq!(continuation.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());
        assert_future_input_rejected(&mut continuation, &events);
    }

    #[test]
    fn post_write_classification_updates_derivable_footer_counters_then_closes() {
        let (mut unknown, events, _) = recording_writer();
        assert!(unknown.accept_mvs_begin(rect(1, 2, 3, 4), 1, &[9]).is_err());
        assert_eq!((unknown.record_count, unknown.type_counts), (1, [0, 0, 0]));
        assert_eq!(unknown.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());

        let (mut geometry, events, _) = recording_writer();
        assert!(geometry
            .accept_mvs_begin(rect(639, 479, 2, 2), 1, &[0])
            .is_err());
        assert_eq!(
            (geometry.record_count, geometry.type_counts),
            (1, [1, 0, 0])
        );
        assert_eq!(geometry.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());
    }

    #[test]
    fn gap_timestamp_and_event_reservation_failures_consume_after_ordinal_assignment() {
        let (mut clock, events, steps) = fault_clock_writer();
        drive_to_recording(&mut clock);
        steps.borrow_mut().push_back(ClockStep::Error);
        assert!(clock.reject_mvs_envelope(None).is_err());
        assert_eq!((clock.source_frame_count, clock.gap_count()), (1, 0));
        assert_eq!(clock.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());

        let (mut reservation, events, _) = recording_writer();
        reservation.event_count = MVS_CAPTURE_V2_MAX_EVENTS - 1;
        assert!(reservation.accept_mvs_continuation(&[0]).is_err());
        assert_eq!(
            (reservation.source_frame_count, reservation.gap_count()),
            (1, 0)
        );
        assert_eq!(reservation.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());
    }

    #[test]
    fn lifecycle_nonterminal_write_failures_map_and_consume_exactly() {
        let (mut armed, events) = failing_writer(FailurePoint::ArmedWrite);
        armed.arm(arm_config()).unwrap();
        assert_eq!(armed.selected_abort_reason(), Some(259));
        assert_cleanup_tail(&events.borrow());

        let (mut triggered, events) = failing_writer(FailurePoint::TriggeredWrite);
        drive_to_trigger_mask(&mut triggered);
        triggered.write_recording_gate().unwrap();
        assert_eq!(triggered.selected_abort_reason(), Some(260));
        assert_cleanup_tail(&events.borrow());

        let (mut recording, events) = failing_writer(FailurePoint::RecordingWrite);
        drive_to_trigger_mask(&mut recording);
        recording.write_recording_gate().unwrap();
        assert_eq!(recording.selected_abort_reason(), Some(260));
        assert_cleanup_tail(&events.borrow());
    }

    #[test]
    fn terminal_preflight_failure_selects_then_cleans_up_preserving_first_error() {
        let (mut writer, events) = failing_writer(FailurePoint::IntoInnerFlush);
        writer.event_count = MVS_CAPTURE_V2_MAX_EVENTS;
        let error = writer.cancel().unwrap_err();
        assert!(format!("{error:#}").contains("V2 event 数量超过上限"));
        assert_eq!(writer.selected_abort_reason(), Some(265));
        assert_eq!(writer.state(), CaptureState::Finalizing);
        assert_cleanup_tail(&events.borrow());
    }

    #[test]
    fn output_failed_uses_the_persisted_phase_mapping() {
        let (mut triggering, _, _) = armed_writer();
        triggering.begin_trigger().unwrap();
        triggering.output_failed().unwrap();
        assert_eq!(triggering.selected_abort_reason(), Some(260));

        let (mut recording, _, _) = recording_writer();
        recording.output_failed().unwrap();
        assert_eq!(recording.selected_abort_reason(), Some(264));

        let (mut created, _, _) = created_writer();
        created.output_failed().unwrap();
        assert_eq!(created.selected_abort_reason(), Some(264));
    }

    #[test]
    fn every_public_mutation_rejects_every_illegal_state_without_writing() {
        type Operation = (
            &'static str,
            &'static [CaptureState],
            fn(&mut MvsCaptureV2Writer) -> anyhow::Result<WriterDecision>,
        );
        let operations: &[Operation] = &[
            (
                "pre_trigger_checkpoint",
                &[CaptureState::Created, CaptureState::Armed],
                MvsCaptureV2Writer::pre_trigger_checkpoint,
            ),
            ("arm", &[CaptureState::Created], |writer| {
                writer.arm(arm_config())
            }),
            (
                "begin_trigger",
                &[CaptureState::Armed],
                MvsCaptureV2Writer::begin_trigger,
            ),
            (
                "trigger_write_succeeded",
                &[CaptureState::Triggering],
                |writer| writer.trigger_write_succeeded(1),
            ),
            (
                "trigger_failed",
                &[CaptureState::Triggering],
                MvsCaptureV2Writer::trigger_failed,
            ),
            (
                "write_recording_gate",
                &[CaptureState::Triggering],
                MvsCaptureV2Writer::write_recording_gate,
            ),
            (
                "accept_non_mvs",
                &[CaptureState::Recording],
                MvsCaptureV2Writer::accept_non_mvs,
            ),
            (
                "reject_mvs_envelope",
                &[CaptureState::Recording],
                |writer| writer.reject_mvs_envelope(None),
            ),
            ("accept_mvs_begin", &[CaptureState::Recording], |writer| {
                writer.accept_mvs_begin(rect(0, 0, 1, 1), 1, &[0])
            }),
            (
                "accept_mvs_continuation",
                &[CaptureState::Recording],
                |writer| writer.accept_mvs_continuation(&[0]),
            ),
            (
                "poll_recording",
                &[CaptureState::Recording],
                MvsCaptureV2Writer::poll_recording,
            ),
            (
                "read_failed",
                &[CaptureState::Recording],
                MvsCaptureV2Writer::read_failed,
            ),
            (
                "generation_transition_attempted",
                &[CaptureState::Recording],
                MvsCaptureV2Writer::generation_transition_attempted,
            ),
            (
                "cancel",
                &[
                    CaptureState::Created,
                    CaptureState::Armed,
                    CaptureState::Triggering,
                    CaptureState::Recording,
                ],
                MvsCaptureV2Writer::cancel,
            ),
            (
                "connect_failed",
                &[CaptureState::Created],
                MvsCaptureV2Writer::connect_failed,
            ),
            (
                "authentication_failed",
                &[CaptureState::Created],
                MvsCaptureV2Writer::authentication_failed,
            ),
            (
                "pre_trigger_deadline_failed",
                &[CaptureState::Created],
                MvsCaptureV2Writer::pre_trigger_deadline_failed,
            ),
            (
                "output_failed",
                &[
                    CaptureState::Created,
                    CaptureState::Armed,
                    CaptureState::Triggering,
                    CaptureState::Recording,
                ],
                MvsCaptureV2Writer::output_failed,
            ),
        ];
        let states = [
            CaptureState::Created,
            CaptureState::Armed,
            CaptureState::Triggering,
            CaptureState::Recording,
            CaptureState::Finalizing,
            CaptureState::Clean,
            CaptureState::Aborted,
        ];

        for &(name, allowed, operation) in operations {
            for &state in &states {
                if allowed.contains(&state) {
                    continue;
                }
                let (mut writer, events, _) = created_writer();
                writer.state = state;
                let before = events.borrow().len();
                assert!(operation(&mut writer).is_err(), "{name} accepted {state:?}");
                assert_eq!(
                    events.borrow().len(),
                    before,
                    "{name} wrote in illegal state {state:?}"
                );
            }
        }
    }

    #[test]
    fn finalization_orders_terminal_flush_sync_and_relinquish() {
        let (mut writer, events, _) = recording_writer();
        writer.cancel().unwrap();
        let events = events.borrow();
        let tail = &events[events.len() - 4..];
        assert!(matches!(tail[0], SinkEvent::Write(_)));
        assert_eq!(tail[1], SinkEvent::IntoInner);
        assert_eq!(tail[2], SinkEvent::Sync);
        assert_eq!(tail[3], SinkEvent::Relinquish);
    }

    #[test]
    fn every_observable_finalization_failure_is_non_success_and_consumes_sink() {
        for (point, expected_error) in [
            (FailurePoint::TerminalWrite, "写入 terminal event"),
            (FailurePoint::IntoInnerFlush, "final implicit flush"),
            (FailurePoint::FinalSync, "final file sync_data"),
            (FailurePoint::Relinquish, "final file relinquish"),
        ] {
            let events = Rc::new(RefCell::new(Vec::new()));
            let sink = FailingSink {
                events: Rc::clone(&events),
                point,
                writes: 0,
                flushes: 0,
                syncs: 0,
            };
            let mut writer = MvsCaptureV2Writer::new(
                Box::new(sink),
                Box::new(ManualClock::default()),
                CreatedConfig {
                    deadline_ms: 5_000,
                    record_limit: 3,
                },
            )
            .unwrap();
            drive_to_recording(&mut writer);
            let error = writer.cancel().unwrap_err();
            assert!(format!("{error:#}").contains(expected_error));
            assert_eq!(writer.state(), CaptureState::Finalizing);
            assert_cleanup_tail(&events.borrow());
            let count = events.borrow().len();
            assert!(writer.cancel().is_err());
            assert_eq!(events.borrow().len(), count);
        }
    }

    #[test]
    fn explicit_output_failure_uses_reason_264() {
        let (mut writer, _, _) = recording_writer();
        writer.output_failed().unwrap();
        assert_eq!(writer.selected_abort_reason(), Some(264));
    }

    #[test]
    fn concrete_create_new_is_exclusive_and_reopenable_after_finalization() {
        let path =
            std::env::temp_dir().join(format!("freeremotedesk-cold-v2-{}.mvs", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let config = CreatedConfig {
            deadline_ms: 5_000,
            record_limit: 1,
        };
        let mut writer = MvsCaptureV2Writer::create_new(&path, config).unwrap();
        assert!(MvsCaptureV2Writer::create_new(&path, config).is_err());
        writer.cancel().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(read_mvs_capture_v2_structural(&mut Cursor::new(bytes)).is_ok());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn concrete_create_new_exposes_one_stable_same_origin_absolute_deadline() {
        let path = std::env::temp_dir().join(format!(
            "freeremotedesk-cold-v2-deadline-{}.mvs",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let duration = Duration::from_millis(5_000);
        let before = Instant::now();
        let mut writer = MvsCaptureV2Writer::create_new(
            &path,
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 1,
            },
        )
        .unwrap();
        let after = Instant::now();

        let first = writer.absolute_deadline().unwrap();
        let second = writer.absolute_deadline().unwrap();
        assert_eq!(first, second);
        assert!(first >= before.checked_add(duration).unwrap());
        assert!(first <= after.checked_add(duration).unwrap());
        let operation_budget = first.checked_duration_since(Instant::now()).unwrap();
        assert!(operation_budget > Duration::ZERO);
        assert!(operation_budget <= duration);

        writer.cancel().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn injected_constructor_without_absolute_origin_reports_a_deterministic_error() {
        let (writer, _, _) = created_writer();
        assert!(format!("{:#}", writer.absolute_deadline().unwrap_err())
            .contains("absolute deadline 不可用"));
    }

    #[test]
    fn checked_absolute_deadline_failure_happens_before_any_sink_write() {
        let sink = RecordingSink::default();
        let events = Rc::clone(&sink.events);
        let result = MvsCaptureV2Writer::new_with_origin_and_checked_add(
            Box::new(sink),
            CreatedConfig {
                deadline_ms: 5_000,
                record_limit: 1,
            },
            Instant::now(),
            |_, _| None,
        );
        assert!(format!("{:#}", result.err().unwrap()).contains("absolute deadline 溢出"));
        assert!(events.borrow().is_empty());
    }
}
