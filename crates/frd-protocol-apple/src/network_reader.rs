//! Apple HPSS/MVS reader、generation 与动态分辨率状态机。

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use frd_core::{PhysicalViewport, PixelRect, PixelSize};
use frd_frame::{PixelBuffer, PixelPatch};
use frd_protocol_api::{ProtocolError, ProtocolRuntime};

use crate::connection::AppleWriterHandle;
use crate::dynamic_resolution::{
    DisplaySize, DynamicResolutionCapability, DynamicResolutionController, GeometryCommit,
    ResolutionRequest,
};
use crate::hpss::{self, encoding, parse_media, Media};
use crate::media_runtime::ViewerMediaState;
use crate::mvs;
use crate::mvs_stream::{MvsRecord, MvsRecordAssembler, MvsRect};
use crate::protocol;
use crate::surface_publisher::{
    AppleSurfacePublisher, CpuFramebuffer as Framebuffer, DisplaySurface, MvsFrameKind,
    PublicationOutcome,
};

const MVS_INCOMPLETE_TIMEOUT: Duration = Duration::from_secs(2);
struct DynamicResolutionRuntime {
    controller: DynamicResolutionController,
    pending_since: Option<Instant>,
    initial_size: DisplaySize,
    opt_in: bool,
    evidence: DynamicCapabilityEvidence,
    armed: bool,
}

#[derive(Default)]
struct DynamicCapabilityEvidence {
    controlling_role: bool,
    matching_initial_server_state: bool,
    current_full_media_applied: bool,
    non_paused_media_activity: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TargetDisposition {
    Ready,
    Duplicate,
    Wait,
}

/// `ServerState` 几何观察的来源。只有精确 Pending 目标才是本地请求 ACK。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServerGeometryDisposition {
    Unchanged,
    RequestedAck,
    ServerInitiated,
}

/// 无副作用的 `ServerState` 几何提交计划。必须先完成所有可能失败的准备，
/// 才能把它应用到 controller、MVS receiver 与 display surface。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ServerGeometryPlan {
    disposition: ServerGeometryDisposition,
    controller_generation: u64,
}

struct ServerGeometryCommit {
    commit: GeometryCommit,
    disposition: ServerGeometryDisposition,
    previous: DisplaySize,
}

impl DynamicResolutionRuntime {
    fn new(initial_size: DisplaySize, enabled: bool) -> Self {
        // hpssview 是本地明确选择的交互控制角色；其余 Apple capability 谓词
        // 必须由本会话可观察事件逐步满足。MVS 活动只作为本实验的 active-media
        // 证据，不宣称它静态证明 Apple 私有的 isUsingAVCMediaStream 谓词。
        let capability = DynamicResolutionCapability::new(false, false, true, true);
        Self {
            controller: DynamicResolutionController::new(initial_size, enabled, capability),
            pending_since: None,
            initial_size,
            opt_in: enabled,
            evidence: DynamicCapabilityEvidence {
                controlling_role: true,
                ..Default::default()
            },
            armed: false,
        }
    }

    fn maybe_arm(&mut self) -> bool {
        if self.armed
            || !self.evidence.controlling_role
            || !self.evidence.matching_initial_server_state
            || !self.evidence.current_full_media_applied
            || !self.evidence.non_paused_media_activity
        {
            return false;
        }
        self.controller = DynamicResolutionController::new(
            self.initial_size,
            self.opt_in,
            DynamicResolutionCapability::new(
                self.evidence.current_full_media_applied,
                self.evidence.matching_initial_server_state,
                self.evidence.controlling_role,
                !self.evidence.non_paused_media_activity,
            ),
        );
        self.armed = true;
        self.opt_in
    }

    fn observe_initial_server_state(
        &mut self,
        observed: DisplaySize,
        current_surface: DisplaySize,
    ) -> bool {
        if !self.armed && current_surface == self.initial_size && observed == current_surface {
            self.evidence.matching_initial_server_state = true;
        }
        self.maybe_arm()
    }

    fn observe_full_applied(&mut self, generation: u64, current_surface: DisplaySize) -> bool {
        if !self.armed && generation == 1 && current_surface == self.initial_size {
            self.evidence.current_full_media_applied = true;
            self.evidence.non_paused_media_activity = true;
            return self.maybe_arm();
        }
        generation
            .checked_sub(1)
            .is_some_and(|internal| self.controller.mark_full_frame(internal))
    }

    fn target_disposition(&self, target: DisplaySize) -> TargetDisposition {
        if !self.opt_in || !self.armed {
            return TargetDisposition::Wait;
        }
        match self.controller.state() {
            crate::dynamic_resolution::DynamicResolutionState::Stable { size, .. }
                if *size == target =>
            {
                TargetDisposition::Duplicate
            }
            crate::dynamic_resolution::DynamicResolutionState::Stable { .. } => {
                TargetDisposition::Ready
            }
            _ => TargetDisposition::Wait,
        }
    }

    /// 调用方必须持有 runtime mutex。发送成功前 controller 保持 Stable，
    /// 因而 reader 既无法取得 mutex，也看不到尚未上 wire 的 Pending。
    fn send_target_with<F>(
        &mut self,
        target: DisplaySize,
        send: F,
    ) -> Result<Option<ResolutionRequest>>
    where
        F: FnOnce(DisplaySize) -> Result<Instant>,
    {
        if self.target_disposition(target) != TargetDisposition::Ready {
            return Ok(None);
        }
        let sent_at = send(target)?;
        let request = self
            .controller
            .request_target(target)
            .context("动态分辨率发送成功后无法激活 Pending")?;
        self.pending_since = Some(sent_at);
        Ok(Some(request))
    }

    fn observe_server_state(&mut self, size: DisplaySize) -> Option<GeometryCommit> {
        let commit = self.controller.observe_server_state(size)?;
        self.pending_since = None;
        Some(commit)
    }

    fn plan_server_geometry(
        &self,
        observed: DisplaySize,
        current_surface: DisplaySize,
        next_controller_generation: u64,
    ) -> Result<ServerGeometryPlan> {
        if let crate::dynamic_resolution::DynamicResolutionState::Pending {
            generation,
            previous,
            target,
            ..
        } = self.controller.state()
        {
            if *generation != next_controller_generation {
                bail!(
                    "Pending 几何 generation 与当前 surface 不一致: pending {} != next {}",
                    generation,
                    next_controller_generation
                );
            }
            if *previous != current_surface {
                bail!(
                    "Pending 几何 previous 与当前 surface 不一致: pending {:?} != current {:?}",
                    previous,
                    current_surface
                );
            }
            if *target == observed {
                return Ok(ServerGeometryPlan {
                    disposition: ServerGeometryDisposition::RequestedAck,
                    controller_generation: next_controller_generation,
                });
            }
        }
        if observed == current_surface {
            return Ok(ServerGeometryPlan {
                disposition: ServerGeometryDisposition::Unchanged,
                controller_generation: next_controller_generation,
            });
        }

        Ok(ServerGeometryPlan {
            disposition: ServerGeometryDisposition::ServerInitiated,
            controller_generation: next_controller_generation,
        })
    }

    /// `plan_server_geometry` 及所有可失败的准备完成后才能调用。
    fn apply_server_geometry_plan(
        &mut self,
        plan: ServerGeometryPlan,
        observed: DisplaySize,
        previous: DisplaySize,
    ) -> Result<()> {
        match plan.disposition {
            ServerGeometryDisposition::Unchanged => {}
            ServerGeometryDisposition::RequestedAck => {
                if !self
                    .controller
                    .pending_server_state_matches(observed, plan.controller_generation)
                {
                    bail!("已计划的 Pending ServerState 在提交前失效");
                }
                let commit = self
                    .observe_server_state(observed)
                    .context("已计划的 Pending ServerState 在提交前失效")?;
                if commit.generation != plan.controller_generation {
                    bail!(
                        "已计划的 Pending ServerState generation 在提交时改变: {} != {}",
                        commit.generation,
                        plan.controller_generation
                    );
                }
            }
            ServerGeometryDisposition::ServerInitiated => {
                self.pending_since = None;
                self.controller.apply_server_initiated_geometry(
                    plan.controller_generation,
                    previous,
                    observed,
                    self.opt_in && self.armed,
                );
            }
        }
        Ok(())
    }

    fn timeout_pending(&mut self, now: Instant) -> bool {
        let Some(since) = self.pending_since else {
            return false;
        };
        if now.duration_since(since) < Duration::from_secs(2) {
            return false;
        }
        let timed_out = self.controller.timeout_pending();
        if timed_out {
            self.pending_since = None;
        }
        timed_out
    }
}

#[derive(Default)]
struct ViewportRequestQueue {
    latest: Option<(DisplaySize, Instant)>,
}

impl ViewportRequestQueue {
    fn observe(&mut self, target: DisplaySize, now: Instant) {
        self.latest = Some((target, now));
    }

    fn service<F>(
        &mut self,
        runtime: &mut DynamicResolutionRuntime,
        now: Instant,
        send: F,
    ) -> Result<Option<ResolutionRequest>>
    where
        F: FnOnce(DisplaySize) -> Result<Instant>,
    {
        let Some((target, since)) = self.latest else {
            return Ok(None);
        };
        if now.duration_since(since) < Duration::from_millis(250) {
            return Ok(None);
        }
        match runtime.target_disposition(target) {
            TargetDisposition::Wait => Ok(None),
            TargetDisposition::Duplicate => {
                self.latest = None;
                Ok(None)
            }
            TargetDisposition::Ready => {
                let request = runtime.send_target_with(target, send)?;
                if request.is_some() {
                    self.latest = None;
                }
                Ok(request)
            }
        }
    }

    fn drop_latest(&mut self) {
        self.latest = None;
    }
}

/// 严格区分精确用户请求 ACK 与服务端主动几何变化；两者都以原子 generation
/// 替换 surface，但后者绝不被记录为用户 resize 成功。
fn commit_server_geometry(
    runtime: &mut DynamicResolutionRuntime,
    receiver: &mut MvsReceiveState,
    surface: &mut DisplaySurface,
    media_state: &mut ViewerMediaState,
    observed: DisplaySize,
    before_generation_commit: impl FnOnce() -> Result<()>,
) -> Result<Option<ServerGeometryCommit>> {
    let Some(replacement_size) = PixelSize::new(observed.width.into(), observed.height.into())
    else {
        return Ok(None);
    };
    let current = DisplaySize::new(
        surface.framebuffer.width as u16,
        surface.framebuffer.height as u16,
    )
    .expect("DisplaySurface dimensions are non-zero u16 values");
    let Some(generation) = surface.generation.checked_add(1) else {
        return Ok(None);
    };
    let Some(controller_generation) = generation.checked_sub(1) else {
        return Ok(None);
    };
    let plan = runtime.plan_server_geometry(observed, current, controller_generation)?;
    if plan.disposition == ServerGeometryDisposition::Unchanged {
        return Ok(None);
    }
    let replacement = DisplaySurface::new(generation, replacement_size)?;
    before_generation_commit()?;
    media_state.reset_generation(generation)?;
    runtime.apply_server_geometry_plan(plan, observed, current)?;
    receiver.reset(generation);
    *surface = replacement;
    Ok(Some(ServerGeometryCommit {
        commit: GeometryCommit {
            generation,
            size: observed,
        },
        disposition: plan.disposition,
        previous: current,
    }))
}

/// Viewer 读线程持有的、generation 绑定的 MVS 接收状态。
struct MvsReceiveState {
    assembler: MvsRecordAssembler,
    decoder: mvs::MvsDecodeState,
    generation: u64,
    incomplete_since: Option<Instant>,
}

impl MvsReceiveState {
    fn new(generation: u64) -> Self {
        Self {
            assembler: MvsRecordAssembler::default(),
            decoder: mvs::MvsDecodeState::new(generation),
            generation,
            incomplete_since: None,
        }
    }

    fn begin(&mut self, rect: MvsRect, total: u32, first: &[u8]) -> Result<Option<MvsRecord>> {
        self.begin_at(rect, total, first, Instant::now())
    }

    fn begin_at(
        &mut self,
        rect: MvsRect,
        total: u32,
        first: &[u8],
        now: Instant,
    ) -> Result<Option<MvsRecord>> {
        let result = self.assembler.begin(rect, total, first);
        self.incomplete_since = if matches!(result, Ok(None)) && self.assembler.is_pending() {
            Some(now)
        } else {
            None
        };
        result
    }

    fn push_continuation(&mut self, chunk: &[u8]) -> Result<Option<MvsRecord>> {
        let result = self.assembler.push_continuation(chunk);
        if !matches!(result, Ok(None)) {
            self.incomplete_since = None;
        }
        result
    }

    fn is_pending(&self) -> bool {
        self.assembler.is_pending()
    }

    fn reset(&mut self, generation: u64) {
        self.assembler.abort();
        self.incomplete_since = None;
        self.decoder.reset(generation);
        self.generation = generation;
    }

    fn install_tables(&mut self, payload: &[u8]) -> Result<()> {
        self.decoder.install_tables(self.generation, payload)
    }

    #[cfg(test)]
    fn prepare(
        &mut self,
        payload: &[u8],
        width: u16,
        height: u16,
    ) -> Result<mvs::MvsDecodeDecision> {
        self.decoder
            .prepare(self.generation, payload, width, height)
    }

    fn prepare_rect(
        &mut self,
        payload: &[u8],
        rect: MvsRect,
        display_size: DisplaySize,
    ) -> Result<mvs::MvsDecodeDecision> {
        self.decoder.prepare_rect(
            self.generation,
            payload,
            rect,
            display_size.width,
            display_size.height,
        )
    }

    fn commit(&mut self, prepared: mvs::PreparedGenerationMvs) -> Result<()> {
        self.decoder.commit(prepared).map(|_| ())
    }

    fn commit_opaque(
        &mut self,
        prepared: mvs::PreparedOpaqueMvsState,
    ) -> Result<crate::mvs_full::PreparedPartialPixels> {
        self.decoder.commit_opaque(prepared)
    }

    fn request_full(&mut self) -> Result<()> {
        self.assembler.abort();
        self.incomplete_since = None;
        self.decoder.request_full(self.generation)
    }

    fn timeout_incomplete(&mut self, now: Instant) -> Result<bool> {
        let Some(since) = self.incomplete_since else {
            return Ok(false);
        };
        if now.checked_duration_since(since).unwrap_or_default() < MVS_INCOMPLETE_TIMEOUT {
            return Ok(false);
        }
        self.request_full()?;
        Ok(true)
    }

    #[cfg(test)]
    fn awaiting_full(&self) -> bool {
        self.decoder.awaiting_full()
    }

    fn reject_truncated_mvs_envelope(&mut self, msg: &[u8]) -> Result<bool> {
        if hpss::is_truncated_mvs_envelope(msg) {
            self.request_full()?;
            return Ok(true);
        }
        Ok(false)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TableScheduleStatus {
    Scheduled,
    AlreadyScheduled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TableFollowupState {
    None,
    Scheduled { generation: u64, due: Instant },
    Sent { generation: u64 },
}

struct ReaderRequestState {
    generation: u64,
    framebuffer_request_in_flight: bool,
    last_full_request: Option<Instant>,
    table_followup: TableFollowupState,
}

impl ReaderRequestState {
    fn after_startup(sent_at: Instant, generation: u64) -> Self {
        Self {
            generation,
            framebuffer_request_in_flight: true,
            last_full_request: Some(sent_at),
            table_followup: TableFollowupState::None,
        }
    }

    fn framebuffer_request_in_flight(&self) -> bool {
        self.framebuffer_request_in_flight
    }

    fn consume_mvs_response(&mut self) {
        self.framebuffer_request_in_flight = false;
    }

    fn mark_incremental_request_sent(&mut self) {
        self.framebuffer_request_in_flight = true;
    }

    fn mark_full_request_sent(&mut self, sent_at: Instant) {
        self.framebuffer_request_in_flight = true;
        self.last_full_request = Some(sent_at);
    }

    fn reset_generation(&mut self, generation: u64) {
        self.generation = generation;
        self.framebuffer_request_in_flight = false;
        self.table_followup = TableFollowupState::None;
    }

    fn on_valid_table_record(
        &mut self,
        generation: u64,
        arrived_at: Instant,
    ) -> Result<TableScheduleStatus> {
        if generation != self.generation {
            bail!("MVS 表 follow-up 属于过期 generation {generation}");
        }
        match self.table_followup {
            TableFollowupState::None => {
                let rate_due = self
                    .last_full_request
                    .map(|last| last + Duration::from_millis(200))
                    .unwrap_or(arrived_at);
                let due = (arrived_at + Duration::from_millis(200)).max(rate_due);
                self.table_followup = TableFollowupState::Scheduled { generation, due };
                Ok(TableScheduleStatus::Scheduled)
            }
            TableFollowupState::Scheduled {
                generation: scheduled,
                ..
            } if scheduled == generation => Ok(TableScheduleStatus::AlreadyScheduled),
            TableFollowupState::Sent {
                generation: sent, ..
            } if sent == generation => {
                bail!("同一 generation 在 table follow-up 后再次返回 table-only 响应")
            }
            _ => bail!("MVS table follow-up generation 状态不一致"),
        }
    }

    fn table_followup_due(&self) -> Option<Instant> {
        match self.table_followup {
            TableFollowupState::Scheduled { due, .. } => Some(due),
            _ => None,
        }
    }

    fn table_followup_blocks_dynamic(&self) -> bool {
        !matches!(self.table_followup, TableFollowupState::None)
    }

    fn mark_table_followup_sent(&mut self, sent_at: Instant) -> Result<()> {
        let TableFollowupState::Scheduled { generation, .. } = self.table_followup else {
            bail!("MVS table follow-up 未处于 Scheduled")
        };
        self.table_followup = TableFollowupState::Sent { generation };
        self.mark_full_request_sent(sent_at);
        Ok(())
    }

    fn on_full_applied(&mut self, generation: u64) -> Result<()> {
        if generation != self.generation {
            bail!("MVS full boundary 属于过期 generation {generation}");
        }
        self.table_followup = TableFollowupState::None;
        Ok(())
    }

    fn send_rate_limited_full_at<S, W>(&mut self, now: Instant, sleep: S, write: W) -> Result<()>
    where
        S: FnMut(Duration),
        W: FnMut() -> Result<()>,
    {
        request_full_update_at(&mut self.last_full_request, now, sleep, write)?;
        self.framebuffer_request_in_flight = true;
        Ok(())
    }
}

#[derive(Default)]
struct ReaderTickOutcome {
    incomplete_recovered: bool,
    dynamic_timed_out: bool,
    table_followup_sent: bool,
    dynamic_request: Option<ResolutionRequest>,
}

#[allow(
    clippy::too_many_arguments,
    reason = "纯状态机测试需要显式注入四组状态与三条无 I/O 副作用的回调"
)]
fn service_reader_tick_at<S, FW, DW>(
    receiver: &mut MvsReceiveState,
    requests: &mut ReaderRequestState,
    queue: &mut ViewportRequestQueue,
    runtime: &mut DynamicResolutionRuntime,
    now: Instant,
    mut sleep: S,
    mut write_full: FW,
    send_dynamic: DW,
) -> Result<ReaderTickOutcome>
where
    S: FnMut(Duration),
    FW: FnMut() -> Result<()>,
    DW: FnMut(DisplaySize) -> Result<Instant>,
{
    let mut outcome = ReaderTickOutcome::default();

    if receiver.timeout_incomplete(now)? {
        requests.consume_mvs_response();
        requests.send_rate_limited_full_at(now, &mut sleep, &mut write_full)?;
        outcome.incomplete_recovered = true;
    }

    if runtime.timeout_pending(now) {
        queue.drop_latest();
        outcome.dynamic_timed_out = true;
    }

    if !outcome.incomplete_recovered
        && !receiver.is_pending()
        && !requests.framebuffer_request_in_flight()
        && runtime.pending_since.is_none()
        && requests.table_followup_due().is_some_and(|due| now >= due)
    {
        write_full()?;
        requests.mark_table_followup_sent(now)?;
        outcome.table_followup_sent = true;
    }

    if !outcome.incomplete_recovered
        && !outcome.table_followup_sent
        && !receiver.is_pending()
        && !requests.framebuffer_request_in_flight()
        && !requests.table_followup_blocks_dynamic()
    {
        outcome.dynamic_request = queue.service(runtime, now, send_dynamic)?;
    }

    Ok(outcome)
}

struct FullBoundaryOutcome {
    dynamic_request: Option<ResolutionRequest>,
    incremental_sent: bool,
}

fn finish_partial_boundary_at<I>(
    requests: &mut ReaderRequestState,
    mut send_incremental: I,
) -> Result<bool>
where
    I: FnMut() -> Result<()>,
{
    if requests.framebuffer_request_in_flight() {
        return Ok(false);
    }
    send_incremental()?;
    requests.mark_incremental_request_sent();
    Ok(true)
}

#[allow(
    clippy::too_many_arguments,
    reason = "full 边界事务显式携带四组状态及各类写回调以验证严格调用顺序"
)]
fn finish_full_boundary_at<S, FW, DW, IW>(
    receiver: &mut MvsReceiveState,
    requests: &mut ReaderRequestState,
    queue: &mut ViewportRequestQueue,
    runtime: &mut DynamicResolutionRuntime,
    now: Instant,
    sleep: S,
    write_full: FW,
    send_dynamic: DW,
    mut send_incremental: IW,
) -> Result<FullBoundaryOutcome>
where
    S: FnMut(Duration),
    FW: FnMut() -> Result<()>,
    DW: FnMut(DisplaySize) -> Result<Instant>,
    IW: FnMut() -> Result<()>,
{
    requests.on_full_applied(receiver.generation)?;
    let tick = service_reader_tick_at(
        receiver,
        requests,
        queue,
        runtime,
        now,
        sleep,
        write_full,
        send_dynamic,
    )?;
    let incremental_sent =
        if tick.dynamic_request.is_none() && !requests.framebuffer_request_in_flight() {
            send_incremental()?;
            requests.mark_incremental_request_sent();
            true
        } else {
            false
        };
    Ok(FullBoundaryOutcome {
        dynamic_request: tick.dynamic_request,
        incremental_sent,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReaderFrameClass {
    Continuation,
    ServerKeepalive,
    Query,
    ControlOrMedia,
}

fn reader_frame_class(receiver: &MvsReceiveState, msg: &[u8]) -> ReaderFrameClass {
    // Apple session frames preserve message boundaries, and the server may
    // interleave a complete MediaStream control message between MVS chunks.
    // Only a fully validated 0x3f2 message may preempt the opaque continuation
    // rule; heartbeat/query-shaped bytes remain MVS payload while reassembling.
    let interleaved_media_control = matches!(
        parse_media(msg),
        Ok(Media::PortAnnouncement(_) | Media::StreamAnswer(_))
    );
    if receiver.is_pending() && !interleaved_media_control {
        return ReaderFrameClass::Continuation;
    }
    match msg.first().copied() {
        Some(protocol::apple_session::SERVER_KEEPALIVE_MESSAGE_TYPE) => {
            ReaderFrameClass::ServerKeepalive
        }
        Some(hpss::msg::QUERY_08) => ReaderFrameClass::Query,
        _ => ReaderFrameClass::ControlOrMedia,
    }
}

fn incremental_request_after_full_apply(
    width: u16,
    height: u16,
) -> Result<[u8; protocol::FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES]> {
    protocol::msg_fb_update_request(true, 0, 0, width, height)
}

fn send_encrypted(writer: &AppleWriterHandle, message: &[u8]) -> Result<()> {
    writer.send_private_message(message)
}

fn request_full_update(
    writer: &AppleWriterHandle,
    requests: &mut ReaderRequestState,
    width: u16,
    height: u16,
) -> Result<()> {
    let now = Instant::now();
    let req = protocol::msg_fb_update_request(false, 0, 0, width, height)?;
    requests.send_rate_limited_full_at(now, thread::sleep, || send_encrypted(writer, &req))
}

fn request_full_update_at<S, W>(
    last_full_request: &mut Option<Instant>,
    now: Instant,
    sleep: S,
    write: W,
) -> Result<()>
where
    S: FnOnce(Duration),
    W: FnOnce() -> Result<()>,
{
    let limit = Duration::from_millis(200);
    let delay = last_full_request
        .and_then(|last| (last + limit).checked_duration_since(now))
        .unwrap_or_default();
    if !delay.is_zero() {
        sleep(delay);
    }
    write()?;
    *last_full_request = Some(now + delay);
    Ok(())
}

fn current_surface_size(surface: &Arc<Mutex<DisplaySurface>>) -> DisplaySize {
    let surface = surface.lock().unwrap();
    DisplaySize::new(
        surface.framebuffer.width as u16,
        surface.framebuffer.height as u16,
    )
    .expect("DisplaySurface dimensions are non-zero u16 values")
}

enum MvsRecordOutcome {
    TableInstalled,
    FullApplied {
        complete_surface: bool,
        publication: PreparedTypeZeroPublication,
    },
    PartialApplied {
        has_pixels: bool,
    },
    RecoveryRequested,
    Ignored,
}

/// type-0 decoder prepare 后一次性构造的发布数据。其 patch 在 codec commit 前已完成
/// generation/矩形/RGB layout 校验，commit 成功后仅按既定字节写入 CPU surface 并移交渲染器。
#[derive(Debug)]
struct PreparedTypeZeroPublication {
    patch: PixelPatch,
    contains_nonblack: bool,
}

fn is_complete_surface_frame(rect: MvsRect, display_size: DisplaySize) -> bool {
    rect.x == 0
        && rect.y == 0
        && rect.width == display_size.width
        && rect.height == display_size.height
}

/// 校验完成记录的矩形；无效远端输入只推进到 fail-closed 全量重同步，不能杀死读线程。
fn mark_recovery_for_invalid_mvs_geometry(
    receiver: &mut MvsReceiveState,
    rect: MvsRect,
    display_size: DisplaySize,
) -> Result<bool> {
    if let Err(error) = crate::mvs_stream::validate_mvs_rect_against_surface(
        rect,
        display_size.width,
        display_size.height,
    ) {
        eprintln!("[hpss-view] MVS 矩形无效，重同步: {error:#}");
        receiver.request_full()?;
        return Ok(false);
    }
    Ok(true)
}

/// 在 decoder commit 后将已预校验的 type-0 BGRX patch 原位写入持久 CPU surface。
/// `receiver` 只属于 reader 线程；持有 surface 锁期间不进行任何 socket I/O。
fn apply_prepared_mvs_to_surface(
    receiver: &mut MvsReceiveState,
    surface: &mut DisplaySurface,
    prepared: mvs::PreparedGenerationMvs,
    publication: &PreparedTypeZeroPublication,
) -> Result<()> {
    if surface.generation != receiver.generation {
        bail!(
            "MVS prepared frame generation 与当前 surface 不一致: receiver={}, surface={}",
            receiver.generation,
            surface.generation
        );
    }
    receiver.commit(prepared)?;
    apply_validated_bgrx_patch_to_framebuffer(&mut surface.framebuffer, &publication.patch);
    Ok(())
}

/// 将 MVS 解码 RGB 转换为可由 wgpu 直接消费的 BGRX patch。
/// 成功返回即代表 commit 后的 surface 写入不再需要分配、验证或返回错误。
fn prepare_type_zero_patch(
    decoded: &crate::mvs_full::DecodedMvsRect,
    rect: MvsRect,
) -> Result<PreparedTypeZeroPublication> {
    if decoded.width != usize::from(rect.width) || decoded.height != usize::from(rect.height) {
        bail!("MVS 原生解码矩形与 wire 矩形不一致");
    }
    mvs::validate_decoded_rgb_layout(rect.width, rect.height, decoded.rgb.len())?;
    let stride_bytes = u32::from(rect.width)
        .checked_mul(4)
        .context("MVS type-0 BGRX stride 溢出")?;
    let byte_count = usize::try_from(stride_bytes)
        .ok()
        .and_then(|stride| stride.checked_mul(usize::from(rect.height)))
        .context("MVS type-0 BGRX payload 溢出")?;
    let mut bgrx = Vec::new();
    bgrx.try_reserve_exact(byte_count)
        .context("MVS type-0 BGRX payload 分配失败")?;
    let mut contains_nonblack = false;
    for rgb in decoded.rgb.chunks_exact(mvs::MVS_RGB_CHANNEL_BYTES) {
        contains_nonblack |= rgb.iter().any(|component| *component != 0);
        bgrx.extend_from_slice(&[
            rgb[mvs::MVS_RGB_BLUE_OFFSET],
            rgb[mvs::MVS_RGB_GREEN_OFFSET],
            rgb[mvs::MVS_RGB_RED_OFFSET],
            0,
        ]);
    }
    debug_assert_eq!(bgrx.len(), byte_count);
    Ok(PreparedTypeZeroPublication {
        patch: PixelPatch {
            rect: PixelRect {
                x: u32::from(rect.x),
                y: u32::from(rect.y),
                width: u32::from(rect.width),
                height: u32::from(rect.height),
            },
            stride_bytes,
            pixels: PixelBuffer::new(bgrx),
        },
        contains_nonblack,
    })
}

/// 只接收 `prepare_type_zero_patch` 已完全校验的 patch，因此无分配且不返回错误。
fn apply_validated_bgrx_patch_to_framebuffer(framebuffer: &mut Framebuffer, patch: &PixelPatch) {
    debug_assert_eq!(patch.stride_bytes, patch.rect.width * 4);
    debug_assert!(patch.rect.width > 0 && patch.rect.height > 0);
    debug_assert!(
        patch.rect.x.checked_add(patch.rect.width).unwrap() <= framebuffer.width as u32
            && patch.rect.y.checked_add(patch.rect.height).unwrap() <= framebuffer.height as u32
    );
    debug_assert_eq!(
        patch.pixels.len(),
        usize::try_from(patch.stride_bytes).unwrap() * patch.rect.height as usize
    );
    let source = patch.pixels.as_bytes();
    let width = patch.rect.width as usize;
    for row in 0..patch.rect.height as usize {
        let source_row = row * patch.stride_bytes as usize;
        let destination_row =
            (patch.rect.y as usize + row) * framebuffer.width + patch.rect.x as usize;
        for column in 0..width {
            let source_pixel = source_row + column * 4;
            framebuffer.pixels_mut()[destination_row + column] = u32::from_le_bytes([
                source[source_pixel],
                source[source_pixel + 1],
                source[source_pixel + 2],
                source[source_pixel + 3],
            ]);
        }
    }
}

fn validate_partial_pixels_for_framebuffer(
    framebuffer: &Framebuffer,
    partial: &crate::mvs_full::PreparedPartialPixels,
) -> Result<()> {
    use crate::mvs_full::PartialPixelOperation;

    for operation in &partial.operations {
        match operation {
            PartialPixelOperation::Replace {
                x,
                y,
                width,
                height,
                rgb,
            } => {
                if *width == 0 || *height == 0 || *width > 8 || *height > 8 {
                    bail!("MVS type-1 replace tile 尺寸非法: {width}x{height}");
                }
                let width_u16 = u16::try_from(*width).context("MVS type-1 replace 宽度溢出")?;
                let height_u16 = u16::try_from(*height).context("MVS type-1 replace 高度溢出")?;
                mvs::validate_decoded_rgb_layout(width_u16, height_u16, rgb.len())?;
                if x.checked_add(*width)
                    .is_none_or(|right| right > framebuffer.width)
                    || y.checked_add(*height)
                        .is_none_or(|bottom| bottom > framebuffer.height)
                {
                    bail!("MVS type-1 framebuffer replace 超出 surface");
                }
            }
            PartialPixelOperation::Copy {
                source_x,
                source_y,
                destination_x,
                destination_y,
                width,
                height,
            } => {
                if *width == 0 || *height == 0 || *width > 8 || *height > 8 {
                    bail!("MVS type-1 copy tile 尺寸非法: {width}x{height}");
                }
                if source_x
                    .checked_add(*width)
                    .is_none_or(|right| right > framebuffer.width)
                    || source_y
                        .checked_add(*height)
                        .is_none_or(|bottom| bottom > framebuffer.height)
                    || destination_x
                        .checked_add(*width)
                        .is_none_or(|right| right > framebuffer.width)
                    || destination_y
                        .checked_add(*height)
                        .is_none_or(|bottom| bottom > framebuffer.height)
                {
                    bail!("MVS type-1 framebuffer copy 超出 surface");
                }
            }
        }
    }
    Ok(())
}

/// 只接收已经通过 `validate_partial_pixels_for_framebuffer` 的 decoder 输出。
/// replace 与 copy 都是固定 8x8 上限，因此提交后不再分配内存或返回错误。
fn apply_validated_partial_pixels_to_framebuffer(
    framebuffer: &mut Framebuffer,
    partial: &crate::mvs_full::PreparedPartialPixels,
) {
    use crate::mvs_full::PartialPixelOperation;

    for operation in &partial.operations {
        match operation {
            PartialPixelOperation::Replace {
                x,
                y,
                width,
                height,
                rgb,
            } => {
                debug_assert!(*width > 0 && *height > 0 && *width <= 8 && *height <= 8);
                debug_assert_eq!(rgb.len(), width * height * mvs::MVS_RGB_CHANNEL_BYTES);
                debug_assert!(*x + *width <= framebuffer.width);
                debug_assert!(*y + *height <= framebuffer.height);
                for row in 0..*height {
                    for column in 0..*width {
                        let source = (row * *width + column) * mvs::MVS_RGB_CHANNEL_BYTES;
                        let destination = (*y + row) * framebuffer.width + *x + column;
                        let red = u32::from(rgb[source + mvs::MVS_RGB_RED_OFFSET]);
                        let green = u32::from(rgb[source + mvs::MVS_RGB_GREEN_OFFSET]);
                        let blue = u32::from(rgb[source + mvs::MVS_RGB_BLUE_OFFSET]);
                        framebuffer.pixels_mut()[destination] = (red << 16) | (green << 8) | blue;
                    }
                }
            }
            PartialPixelOperation::Copy {
                source_x,
                source_y,
                destination_x,
                destination_y,
                width,
                height,
            } => {
                debug_assert!(*width > 0 && *height > 0 && *width <= 8 && *height <= 8);
                debug_assert!(*source_x + *width <= framebuffer.width);
                debug_assert!(*source_y + *height <= framebuffer.height);
                debug_assert!(*destination_x + *width <= framebuffer.width);
                debug_assert!(*destination_y + *height <= framebuffer.height);
                let mut staging = [0u32; 8 * 8];
                for row in 0..*height {
                    let source = (*source_y + row) * framebuffer.width + *source_x;
                    let staging_start = row * *width;
                    staging[staging_start..staging_start + *width]
                        .copy_from_slice(&framebuffer.pixels()[source..source + *width]);
                }
                for row in 0..*height {
                    let staging_start = row * *width;
                    let destination = (*destination_y + row) * framebuffer.width + *destination_x;
                    framebuffer.pixels_mut()[destination..destination + *width]
                        .copy_from_slice(&staging[staging_start..staging_start + *width]);
                }
            }
        }
    }
}

fn apply_prepared_partial_to_surface(
    receiver: &mut MvsReceiveState,
    surface: &mut DisplaySurface,
    prepared: mvs::PreparedOpaqueMvsState,
) -> Result<bool> {
    if surface.generation != receiver.generation {
        bail!(
            "MVS prepared partial generation 与当前 surface 不一致: receiver={}, surface={}",
            receiver.generation,
            surface.generation
        );
    }
    validate_partial_pixels_for_framebuffer(&surface.framebuffer, prepared.partial_pixels())?;
    let has_pixels = !prepared.partial_pixels().operations.is_empty();
    let partial_pixels = receiver.commit_opaque(prepared)?;
    if has_pixels {
        apply_validated_partial_pixels_to_framebuffer(&mut surface.framebuffer, &partial_pixels);
        surface.record_native_partial_applied();
    }
    Ok(has_pixels)
}

/// 已严格分类为像素记录后的原生 prepare/apply/commit 事务。
fn apply_native_mvs_frame(
    receiver: &mut MvsReceiveState,
    record: &MvsRecord,
    surface: &Arc<Mutex<DisplaySurface>>,
    dynamic_resolution: &Arc<Mutex<DynamicResolutionRuntime>>,
) -> Result<MvsRecordOutcome> {
    let (surface_generation, display_size) = {
        let surface = surface.lock().unwrap();
        (
            surface.generation,
            DisplaySize::new(
                surface.framebuffer.width as u16,
                surface.framebuffer.height as u16,
            )
            .expect("DisplaySurface dimensions are non-zero u16 values"),
        )
    };
    if receiver.generation != surface_generation {
        eprintln!("[hpss-view] 忽略过期 generation 的 MVS 记录");
        return Ok(MvsRecordOutcome::Ignored);
    }
    if !mark_recovery_for_invalid_mvs_geometry(receiver, record.rect, display_size)? {
        return Ok(MvsRecordOutcome::RecoveryRequested);
    }
    let complete_surface = is_complete_surface_frame(record.rect, display_size);
    let prepared = match receiver.prepare_rect(&record.payload, record.rect, display_size) {
        Ok(mvs::MvsDecodeDecision::Prepared(prepared)) => prepared,
        Ok(mvs::MvsDecodeDecision::PreparedOpaque(prepared)) => {
            let applied = {
                let mut surface = surface.lock().unwrap();
                if surface.generation != surface_generation {
                    Err(anyhow::anyhow!(
                        "MVS surface generation 在 partial 应用前发生变化"
                    ))
                } else {
                    apply_prepared_partial_to_surface(receiver, &mut surface, prepared)
                }
            };
            let has_pixels = match applied {
                Ok(true) => {
                    #[cfg(debug_assertions)]
                    eprintln!("[hpss-view] MVS type-1 增量像素与 codec 状态已提交");
                    true
                }
                Ok(false) => {
                    #[cfg(debug_assertions)]
                    eprintln!("[hpss-view] MVS type-1 no-op/cache 状态已提交");
                    false
                }
                Err(error) => {
                    eprintln!("[hpss-view] MVS type-1 framebuffer 事务失败，重同步: {error:#}");
                    receiver.request_full()?;
                    return Ok(MvsRecordOutcome::RecoveryRequested);
                }
            };
            // Partial pixels are visible, but a type-1 update is never the
            // complete type-0 codec baseline used by the P1 evidence latch.
            return Ok(MvsRecordOutcome::PartialApplied { has_pixels });
        }
        Ok(mvs::MvsDecodeDecision::RequestFull(reason)) => {
            eprintln!("[hpss-view] MVS 原生记录要求全量重同步: {reason:?}");
            receiver.request_full()?;
            return Ok(MvsRecordOutcome::RecoveryRequested);
        }
        Ok(mvs::MvsDecodeDecision::IgnoreStale) => {
            eprintln!("[hpss-view] 忽略过期 generation 的 MVS prepare");
            return Ok(MvsRecordOutcome::Ignored);
        }
        Err(error) => {
            eprintln!("[hpss-view] MVS 原生 prepare 失败，重同步: {error:#}");
            receiver.request_full()?;
            return Ok(MvsRecordOutcome::RecoveryRequested);
        }
    };

    let publication = match prepare_type_zero_patch(prepared.decoded(), record.rect) {
        Ok(publication) => publication,
        Err(error) => {
            eprintln!("[hpss-view] MVS type-0 BGRX prepare 失败，重同步: {error:#}");
            receiver.request_full()?;
            return Ok(MvsRecordOutcome::RecoveryRequested);
        }
    };
    let applied = {
        let mut surface = surface.lock().unwrap();
        if surface.generation != surface_generation {
            Err(anyhow::anyhow!("MVS surface generation 在应用前发生变化"))
        } else {
            apply_prepared_mvs_to_surface(receiver, &mut surface, prepared, &publication)
                .map(|()| surface.record_native_type_zero_applied())
        }
    };
    let _observability = match applied {
        Ok(observability) => observability,
        Err(error) => {
            eprintln!("[hpss-view] MVS 原生 framebuffer 事务失败，重同步: {error:#}");
            receiver.request_full()?;
            return Ok(MvsRecordOutcome::RecoveryRequested);
        }
    };

    #[cfg(debug_assertions)]
    eprintln!(
        "[hpss-view] native MVS: generation={}, rect=({},{} {}x{}), type0_total={}",
        receiver.generation,
        record.rect.x,
        record.rect.y,
        record.rect.width,
        record.rect.height,
        _observability.type_zero_applied_count,
    );
    if complete_surface {
        dynamic_resolution
            .lock()
            .unwrap()
            .observe_full_applied(receiver.generation, display_size);
        #[cfg(debug_assertions)]
        eprintln!("[hpss-view] 当前 generation 的完整 surface 证据已确认");
    }
    Ok(MvsRecordOutcome::FullApplied {
        complete_surface,
        publication,
    })
}

fn process_complete_mvs_record(
    receiver: &mut MvsReceiveState,
    record: MvsRecord,
    surface: &Arc<Mutex<DisplaySurface>>,
    dynamic_resolution: &Arc<Mutex<DynamicResolutionRuntime>>,
    writer: &AppleWriterHandle,
    requests: &mut ReaderRequestState,
) -> Result<MvsRecordOutcome> {
    let (surface_generation, display_size) = {
        let surface = surface.lock().unwrap();
        (
            surface.generation,
            DisplaySize::new(
                surface.framebuffer.width as u16,
                surface.framebuffer.height as u16,
            )
            .expect("DisplaySurface dimensions are non-zero u16 values"),
        )
    };
    if receiver.generation != surface_generation {
        return Ok(MvsRecordOutcome::Ignored);
    }

    match mvs::classify_mvs_record(record.rect, &record.payload) {
        Ok(mvs::MvsRecordKind::Tables(payload)) => {
            if let Err(e) = receiver.install_tables(payload) {
                eprintln!("[hpss-view] MVS 表初始化无效，重同步: {e:#}");
                receiver.request_full()?;
                return request_full_update(
                    writer,
                    requests,
                    display_size.width,
                    display_size.height,
                )
                .map(|()| MvsRecordOutcome::RecoveryRequested);
            }
            let status = requests.on_valid_table_record(receiver.generation, Instant::now())?;
            eprintln!("[hpss-view] MVS 量化表就绪，full follow-up 已调度: {status:?}");
            return Ok(MvsRecordOutcome::TableInstalled);
        }
        Ok(mvs::MvsRecordKind::Frame(_)) => {}
        Err(error) => {
            eprintln!("[hpss-view] MVS 表初始化候选无效，重同步: {error:#}");
            receiver.request_full()?;
            return request_full_update(writer, requests, display_size.width, display_size.height)
                .map(|()| MvsRecordOutcome::RecoveryRequested);
        }
    }

    let outcome = apply_native_mvs_frame(receiver, &record, surface, dynamic_resolution)?;
    if matches!(outcome, MvsRecordOutcome::RecoveryRequested) {
        return request_full_update(writer, requests, display_size.width, display_size.height)
            .map(|()| MvsRecordOutcome::RecoveryRequested);
    }
    Ok(outcome)
}

#[allow(
    clippy::too_many_arguments,
    reason = "网络适配层显式传入共享状态，避免隐藏全局状态和锁顺序"
)]
fn service_network_reader_tick(
    receiver: &mut MvsReceiveState,
    requests: &mut ReaderRequestState,
    surface: &Arc<Mutex<DisplaySurface>>,
    viewport_requests: &Arc<Mutex<ViewportRequestQueue>>,
    dynamic_resolution: &Arc<Mutex<DynamicResolutionRuntime>>,
    writer: &AppleWriterHandle,
    now: Instant,
) -> Result<ReaderTickOutcome> {
    let size = current_surface_size(surface);
    let full = protocol::msg_fb_update_request(false, 0, 0, size.width, size.height)?;
    // 固定锁序：queue → runtime → Apple writer。surface 已在上方解锁。
    let mut queue = viewport_requests.lock().unwrap();
    let mut runtime = dynamic_resolution.lock().unwrap();
    service_reader_tick_at(
        receiver,
        requests,
        &mut queue,
        &mut runtime,
        now,
        thread::sleep,
        || send_encrypted(writer, &full),
        |target| {
            let query = hpss::build_display_query(target);
            send_encrypted(writer, &query)?;
            Ok(Instant::now())
        },
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "网络 full 边界适配器显式传入共享状态，保持锁和写事务可审计"
)]
fn finish_network_full_boundary(
    receiver: &mut MvsReceiveState,
    requests: &mut ReaderRequestState,
    surface: &Arc<Mutex<DisplaySurface>>,
    viewport_requests: &Arc<Mutex<ViewportRequestQueue>>,
    dynamic_resolution: &Arc<Mutex<DynamicResolutionRuntime>>,
    writer: &AppleWriterHandle,
    now: Instant,
) -> Result<FullBoundaryOutcome> {
    let size = current_surface_size(surface);
    let full = protocol::msg_fb_update_request(false, 0, 0, size.width, size.height)?;
    let incremental = incremental_request_after_full_apply(size.width, size.height)?;
    let mut queue = viewport_requests.lock().unwrap();
    let mut runtime = dynamic_resolution.lock().unwrap();
    finish_full_boundary_at(
        receiver,
        requests,
        &mut queue,
        &mut runtime,
        now,
        thread::sleep,
        || send_encrypted(writer, &full),
        |target| {
            let query = hpss::build_display_query(target);
            send_encrypted(writer, &query)?;
            Ok(Instant::now())
        },
        || send_encrypted(writer, &incremental),
    )
}

fn log_reader_tick(outcome: &ReaderTickOutcome) {
    if outcome.incomplete_recovered {
        eprintln!("[hpss-view] MVS 不完整记录超时，已中止并请求全量重同步");
    }
    if outcome.dynamic_timed_out {
        eprintln!("[hpss-view] 动态分辨率确认超时，保留当前显示面并丢弃待处理 viewport");
    }
    if outcome.table_followup_sent {
        eprintln!("[hpss-view] MVS table follow-up full request 已发送");
    }
    if let Some(request) = outcome.dynamic_request {
        eprintln!(
            "[hpss-view] 请求动态分辨率 {}x{} (generation {})",
            request.target.width, request.target.height, request.generation
        );
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "完整 MVS 记录处理需要显式传入 generation 状态与事务写器"
)]
fn handle_complete_mvs_record(
    receiver: &mut MvsReceiveState,
    requests: &mut ReaderRequestState,
    record: MvsRecord,
    surface: &Arc<Mutex<DisplaySurface>>,
    viewport_requests: &Arc<Mutex<ViewportRequestQueue>>,
    dynamic_resolution: &Arc<Mutex<DynamicResolutionRuntime>>,
    writer: &AppleWriterHandle,
    protocol_runtime: &mut ProtocolRuntime,
    publisher: &mut AppleSurfacePublisher,
) -> Result<()> {
    requests.consume_mvs_response();
    let dirty = PixelRect {
        x: u32::from(record.rect.x),
        y: u32::from(record.rect.y),
        width: u32::from(record.rect.width),
        height: u32::from(record.rect.height),
    };
    let outcome = process_complete_mvs_record(
        receiver,
        record,
        surface,
        dynamic_resolution,
        writer,
        requests,
    )?;
    let full_applied = matches!(&outcome, MvsRecordOutcome::FullApplied { .. });
    let partial_applied = matches!(&outcome, MvsRecordOutcome::PartialApplied { .. });
    let publication = match outcome {
        MvsRecordOutcome::FullApplied {
            complete_surface,
            publication,
        } => {
            let surface = surface.lock().unwrap();
            publisher
                .publish_committed_patch(
                    protocol_runtime,
                    &surface,
                    receiver.generation,
                    publication.patch,
                    MvsFrameKind::TypeZero {
                        complete_surface,
                        initial_nonblack: publication.contains_nonblack,
                    },
                )
                .map_err(|error| anyhow::anyhow!(error.code()))?
        }
        MvsRecordOutcome::PartialApplied { has_pixels: true } => {
            let surface = surface.lock().unwrap();
            publisher
                .publish_committed(
                    protocol_runtime,
                    &surface,
                    receiver.generation,
                    dirty,
                    MvsFrameKind::TypeOne,
                )
                .map_err(|error| anyhow::anyhow!(error.code()))?
        }
        _ => PublicationOutcome::Published,
    };
    match publication {
        PublicationOutcome::NeedsFullBaseline => {
            receiver.request_full()?;
            let size = current_surface_size(surface);
            request_full_update(writer, requests, size.width, size.height)?;
            return Ok(());
        }
        PublicationOutcome::NeedsFullSnapshot => {
            let surface = surface.lock().unwrap();
            publisher
                .republish_full_snapshot(protocol_runtime, &surface, receiver.generation)
                .map_err(|error| anyhow::anyhow!(error.code()))?;
        }
        PublicationOutcome::Published | PublicationOutcome::IgnoredStale => {}
    }
    if full_applied {
        let boundary = finish_network_full_boundary(
            receiver,
            requests,
            surface,
            viewport_requests,
            dynamic_resolution,
            writer,
            Instant::now(),
        )?;
        if let Some(request) = boundary.dynamic_request {
            eprintln!(
                "[hpss-view] full 边界切换为动态分辨率 {}x{} (generation {})",
                request.target.width, request.target.height, request.generation
            );
        } else if boundary.incremental_sent {
            #[cfg(debug_assertions)]
            eprintln!("[hpss-view] MVS full 已应用并请求下一增量响应");
        }
    } else if partial_applied {
        let size = current_surface_size(surface);
        let incremental = incremental_request_after_full_apply(size.width, size.height)?;
        if finish_partial_boundary_at(requests, || send_encrypted(writer, &incremental))? {
            #[cfg(debug_assertions)]
            eprintln!("[hpss-view] MVS type-1 已提交并请求下一增量响应");
        }
    }
    Ok(())
}

pub(crate) enum NetworkFrameOutcome {
    Consumed,
    Media(Media),
}

pub(crate) struct NetworkReaderRuntime {
    receiver: MvsReceiveState,
    requests: ReaderRequestState,
    surface: Arc<Mutex<DisplaySurface>>,
    viewport_requests: Arc<Mutex<ViewportRequestQueue>>,
    dynamic_resolution: Arc<Mutex<DynamicResolutionRuntime>>,
    publisher: AppleSurfacePublisher,
}

impl NetworkReaderRuntime {
    pub(crate) fn new(
        protocol_runtime: &mut ProtocolRuntime,
        session_id: frd_core::SessionId,
        initial_size: DisplaySize,
        dynamic_resolution_enabled: bool,
        startup_fb_sent_at: Instant,
    ) -> Result<Self, ProtocolError> {
        let size = PixelSize::new(initial_size.width.into(), initial_size.height.into())
            .ok_or(ProtocolError::FramePortRejected)?;
        let generation = 1;
        let publisher = AppleSurfacePublisher::begin(protocol_runtime, session_id, size)?;
        let surface =
            DisplaySurface::new(generation, size).map_err(|_| ProtocolError::FramePortRejected)?;
        Ok(Self {
            receiver: MvsReceiveState::new(generation),
            requests: ReaderRequestState::after_startup(startup_fb_sent_at, generation),
            surface: Arc::new(Mutex::new(surface)),
            viewport_requests: Arc::new(Mutex::new(ViewportRequestQueue::default())),
            dynamic_resolution: Arc::new(Mutex::new(DynamicResolutionRuntime::new(
                initial_size,
                dynamic_resolution_enabled,
            ))),
            publisher,
        })
    }

    pub(crate) fn generation(&self) -> u64 {
        self.publisher.generation()
    }

    pub(crate) fn observe_viewport(&self, viewport: PhysicalViewport, now: Instant) {
        let width = viewport.content.width as usize;
        let height = viewport.content.height as usize;
        if let Some(target) = DisplaySize::from_viewport(width, height) {
            self.viewport_requests.lock().unwrap().observe(target, now);
        } else {
            self.viewport_requests.lock().unwrap().drop_latest();
        }
    }

    fn ensure_server_state_generation_coherence(
        &self,
        media_state: &ViewerMediaState,
    ) -> Result<()> {
        let surface_generation = self.surface.lock().unwrap().generation;
        if self.receiver.generation != surface_generation
            || self.requests.generation != surface_generation
            || self.publisher.generation() != surface_generation
            || media_state.generation() != surface_generation
        {
            bail!(
                "ServerState generation 不一致: surface {}, receiver {}, requests {}, publisher {}, media {}",
                surface_generation,
                self.receiver.generation,
                self.requests.generation,
                self.publisher.generation(),
                media_state.generation(),
            );
        }
        Ok(())
    }

    pub(crate) fn service_tick(&mut self, writer: &AppleWriterHandle, now: Instant) -> Result<()> {
        let outcome = service_network_reader_tick(
            &mut self.receiver,
            &mut self.requests,
            &self.surface,
            &self.viewport_requests,
            &self.dynamic_resolution,
            writer,
            now,
        )?;
        log_reader_tick(&outcome);
        Ok(())
    }

    pub(crate) fn handle_frame(
        &mut self,
        message: Vec<u8>,
        writer: &AppleWriterHandle,
        media_state: &mut ViewerMediaState,
        protocol_runtime: &mut ProtocolRuntime,
        before_generation_commit: &mut impl FnMut() -> Result<()>,
    ) -> Result<NetworkFrameOutcome> {
        if reader_frame_class(&self.receiver, &message) == ReaderFrameClass::Continuation {
            let record = match self.receiver.push_continuation(&message) {
                Ok(record) => record,
                Err(error) => {
                    eprintln!("[hpss-view] MVS continuation 结构错误，重同步: {error:#}");
                    self.receiver.request_full()?;
                    self.requests.consume_mvs_response();
                    let size = current_surface_size(&self.surface);
                    request_full_update(writer, &mut self.requests, size.width, size.height)?;
                    return Ok(NetworkFrameOutcome::Consumed);
                }
            };
            if let Some(record) = record {
                handle_complete_mvs_record(
                    &mut self.receiver,
                    &mut self.requests,
                    record,
                    &self.surface,
                    &self.viewport_requests,
                    &self.dynamic_resolution,
                    writer,
                    protocol_runtime,
                    &mut self.publisher,
                )?;
            }
            return Ok(NetworkFrameOutcome::Consumed);
        }

        match reader_frame_class(&self.receiver, &message) {
            ReaderFrameClass::ServerKeepalive | ReaderFrameClass::Query => {
                return Ok(NetworkFrameOutcome::Consumed);
            }
            ReaderFrameClass::ControlOrMedia => {}
            ReaderFrameClass::Continuation => unreachable!("continuation 已在上方处理"),
        }

        match parse_media(&message) {
            Ok(Media::Mvs {
                x,
                y,
                w,
                h,
                total,
                body,
            }) => {
                let record = match self.receiver.begin(
                    MvsRect {
                        x,
                        y,
                        width: w,
                        height: h,
                    },
                    total,
                    &body,
                ) {
                    Ok(record) => record,
                    Err(error) => {
                        eprintln!("[hpss-view] MVS 首片结构错误，重同步: {error:#}");
                        self.receiver.request_full()?;
                        self.requests.consume_mvs_response();
                        let size = current_surface_size(&self.surface);
                        request_full_update(writer, &mut self.requests, size.width, size.height)?;
                        return Ok(NetworkFrameOutcome::Consumed);
                    }
                };
                if let Some(record) = record {
                    handle_complete_mvs_record(
                        &mut self.receiver,
                        &mut self.requests,
                        record,
                        &self.surface,
                        &self.viewport_requests,
                        &self.dynamic_resolution,
                        writer,
                        protocol_runtime,
                        &mut self.publisher,
                    )?;
                }
                Ok(NetworkFrameOutcome::Consumed)
            }
            Ok(Media::State(encoding::SERVER_STATE)) => {
                if let Some((width, height)) = hpss::parse_server_state_w_h(&message) {
                    if let Some(observed) = DisplaySize::new(width, height) {
                        self.ensure_server_state_generation_coherence(media_state)?;
                        let commit = {
                            let mut dynamic = self.dynamic_resolution.lock().unwrap();
                            let mut surface = self.surface.lock().unwrap();
                            let current = DisplaySize::new(
                                surface.framebuffer.width as u16,
                                surface.framebuffer.height as u16,
                            )
                            .expect("DisplaySurface dimensions are non-zero u16 values");
                            dynamic.observe_initial_server_state(observed, current);
                            commit_server_geometry(
                                &mut dynamic,
                                &mut self.receiver,
                                &mut surface,
                                media_state,
                                observed,
                                before_generation_commit,
                            )?
                        };
                        if let Some(ServerGeometryCommit {
                            commit,
                            disposition,
                            previous,
                        }) = commit
                        {
                            eprintln!(
                                "[hpss-view] ServerState 几何 {:?}: {}x{} -> {}x{} (generation {})",
                                disposition,
                                previous.width,
                                previous.height,
                                commit.size.width,
                                commit.size.height,
                                commit.generation,
                            );
                            if disposition == ServerGeometryDisposition::ServerInitiated {
                                self.viewport_requests.lock().unwrap().drop_latest();
                            }
                            let size =
                                PixelSize::new(commit.size.width.into(), commit.size.height.into())
                                    .context("动态分辨率确认尺寸非法")?;
                            self.publisher
                                .begin_next_generation(protocol_runtime, commit.generation, size)
                                .map_err(|error| anyhow::anyhow!(error.code()))?;
                            self.requests.reset_generation(commit.generation);
                            request_full_update(
                                writer,
                                &mut self.requests,
                                commit.size.width,
                                commit.size.height,
                            )?;
                        }
                    }
                }
                Ok(NetworkFrameOutcome::Consumed)
            }
            Ok(Media::State(_)) | Ok(Media::Cursor { .. }) => Ok(NetworkFrameOutcome::Consumed),
            Ok(media @ (Media::PortAnnouncement(_) | Media::StreamAnswer(_))) => {
                Ok(NetworkFrameOutcome::Media(media))
            }
            Err(error) => match self.receiver.reject_truncated_mvs_envelope(&message) {
                Ok(true) => {
                    eprintln!("[hpss-view] MVS 信封截断，重同步: {error:#}");
                    self.requests.consume_mvs_response();
                    let size = current_surface_size(&self.surface);
                    request_full_update(writer, &mut self.requests, size.width, size.height)?;
                    Ok(NetworkFrameOutcome::Consumed)
                }
                Ok(false) => Ok(NetworkFrameOutcome::Consumed),
                Err(state_error) => Err(state_error),
            },
        }
    }
}

#[cfg(test)]
mod migrated_runtime_tests {
    use super::*;

    struct NoopProtocolEvents;

    impl frd_protocol_api::RuntimeEventSink for NoopProtocolEvents {
        fn publish(
            &self,
            _event: frd_protocol_api::SessionEvent,
        ) -> Result<(), frd_protocol_api::ProtocolError> {
            Ok(())
        }
    }

    struct NoopProtocolFrames;

    impl frd_protocol_api::SurfacePublisher for NoopProtocolFrames {
        fn publish(
            &self,
            _update: frd_frame::SurfaceUpdate,
        ) -> Result<(), frd_protocol_api::ProtocolError> {
            Ok(())
        }
    }

    struct SnapshotRecoveryFrames {
        updates: Arc<Mutex<Vec<frd_frame::SurfaceUpdate>>>,
        reject_next_damage: Mutex<bool>,
    }

    impl frd_protocol_api::SurfacePublisher for SnapshotRecoveryFrames {
        fn publish(
            &self,
            update: frd_frame::SurfaceUpdate,
        ) -> Result<(), frd_protocol_api::ProtocolError> {
            let mut reject_next_damage = self.reject_next_damage.lock().unwrap();
            if *reject_next_damage && matches!(update, frd_frame::SurfaceUpdate::Damage { .. }) {
                *reject_next_damage = false;
                return Err(frd_protocol_api::ProtocolError::NeedsFullSnapshot);
            }
            self.updates.lock().unwrap().push(update);
            Ok(())
        }
    }

    struct NoopProtocolWake;

    impl frd_protocol_api::RuntimeWake for NoopProtocolWake {
        fn wake(&self) -> Result<(), frd_protocol_api::ProtocolError> {
            Ok(())
        }
    }

    fn type_two_tables_fixture() -> [u8; 129] {
        let mut payload = [0u8; 129];
        payload[0] = 2;
        payload
    }

    fn decode_private_hex_fixture(hex: &str) -> Vec<u8> {
        let compact = hex.trim();
        assert!(compact.len().is_multiple_of(2));
        compact
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    fn media_stream_answer_fixture() -> Vec<u8> {
        let container = decode_private_hex_fixture(&crate::read_private_fixture_text(
            "ard_re/fixtures/avc_mode_4_answer.bplist.hex",
        ));
        let mut body = Vec::new();
        body.extend_from_slice(&2u16.to_be_bytes());
        body.extend_from_slice(&2u16.to_be_bytes());
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&(container.len() as u16).to_be_bytes());
        body.extend_from_slice(&(container.len() as u16).to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&[0; 4]);
        body.extend_from_slice(&container);
        body.extend_from_slice(&container);

        let mut frame = Vec::new();
        frame.extend_from_slice(&1u32.to_be_bytes());
        frame.extend_from_slice(&[0; 8]);
        frame.extend_from_slice(&crate::protocol::MEDIA_STREAM_CONTROL_ENCODING.to_be_bytes());
        frame.extend_from_slice(&(body.len() as u16).to_be_bytes());
        frame.extend_from_slice(&body);
        frame
    }

    fn server_state_message(size: DisplaySize) -> Vec<u8> {
        let mut server_state = vec![0u8; 94];
        server_state[0..4].copy_from_slice(&1u32.to_be_bytes());
        server_state[12..16].copy_from_slice(&encoding::SERVER_STATE.to_be_bytes());
        server_state[16..18].copy_from_slice(&76u16.to_be_bytes());
        server_state[18..20].copy_from_slice(&5u16.to_be_bytes());
        server_state[20..22].copy_from_slice(&size.width.to_be_bytes());
        server_state[22..24].copy_from_slice(&size.height.to_be_bytes());
        server_state
    }

    struct TestBitWriter {
        bytes: Vec<u8>,
        current: u8,
        used: u8,
    }

    impl TestBitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                used: 0,
            }
        }

        fn write_bits(&mut self, value: u32, count: u8) {
            for shift in (0..count).rev() {
                self.current = (self.current << 1) | (((value >> shift) & 1) as u8);
                self.used += 1;
                if self.used == 8 {
                    self.bytes.push(self.current);
                    self.current = 0;
                    self.used = 0;
                }
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.used != 0 {
                self.current <<= 8 - self.used;
                self.bytes.push(self.current);
            }
            self.bytes
        }
    }

    fn native_mode_zero_payload() -> Vec<u8> {
        let mut mode = TestBitWriter::new();
        mode.write_bits(1, 1);
        mode.write_bits(0, 3);
        mode.write_bits(0, 1);
        mode.write_bits(0x6d, 8);
        let mode = mode.finish();

        let mut data = TestBitWriter::new();
        data.write_bits(0x6d, 8);
        let data = data.finish();
        let data_offset = 6 + mode.len();
        let mut payload = vec![
            0,
            0,
            0,
            u8::try_from(data_offset >> 16).unwrap(),
            u8::try_from((data_offset >> 8) & 0xff).unwrap(),
            u8::try_from(data_offset & 0xff).unwrap(),
        ];
        payload.extend_from_slice(&mode);
        payload.extend_from_slice(&data);
        payload
    }

    fn native_opcode_zero_partial_payload() -> Vec<u8> {
        let mut bits = TestBitWriter::new();
        bits.write_bits(0, 2);
        bits.write_bits(0x6d, 8);
        bits.write_bits(0x76, 8);
        bits.write_bits(0x73, 8);
        let mut payload = vec![1, 0, 0];
        payload.extend(bits.finish());
        payload
    }

    fn native_mode_five_seed_payload() -> Vec<u8> {
        let mut mode = TestBitWriter::new();
        mode.write_bits(1, 1);
        mode.write_bits(5, 3);
        mode.write_bits(0, 1);
        mode.write_bits(0x6d, 8);
        let mode = mode.finish();

        let mut data = TestBitWriter::new();
        data.write_bits(0, 3);
        data.write_bits(0, 2);
        data.write_bits(0, 2);
        data.write_bits(0, 2);
        data.write_bits(0b0010, 4);
        data.write_bits(0x6d, 8);
        let data = data.finish();
        let data_offset = 6 + mode.len();
        let mut payload = vec![
            0,
            0,
            0,
            u8::try_from(data_offset >> 16).unwrap(),
            u8::try_from((data_offset >> 8) & 0xff).unwrap(),
            u8::try_from(data_offset & 0xff).unwrap(),
        ];
        payload.extend_from_slice(&mode);
        payload.extend_from_slice(&data);
        payload
    }

    fn native_opcode_one_partial_payload() -> Vec<u8> {
        let mut bits = TestBitWriter::new();
        bits.write_bits(1, 2);
        bits.write_bits(0, 6);
        bits.write_bits(0, 1);
        bits.write_bits(0b1010, 4);
        bits.write_bits(0, 1);
        bits.write_bits(0b1010, 4);
        bits.write_bits(0x6d, 8);
        bits.write_bits(0x76, 8);
        bits.write_bits(0x73, 8);
        let mut payload = vec![1, 1, 1];
        payload.extend(bits.finish());
        payload
    }

    fn native_record(rect: MvsRect) -> MvsRecord {
        MvsRecord {
            rect,
            payload: native_mode_zero_payload(),
        }
    }

    fn native_surface(width: usize, height: usize) -> Arc<Mutex<DisplaySurface>> {
        Arc::new(Mutex::new(
            DisplaySurface::new(1, PixelSize::new(width as u32, height as u32).unwrap()).unwrap(),
        ))
    }

    fn native_runtime(width: u16, height: u16) -> Arc<Mutex<DynamicResolutionRuntime>> {
        Arc::new(Mutex::new(DynamicResolutionRuntime::new(
            DisplaySize::new(width, height).unwrap(),
            true,
        )))
    }

    fn commit_native_mode_zero(receiver: &mut MvsReceiveState) {
        let decision = receiver.prepare(&native_mode_zero_payload(), 8, 8).unwrap();
        let mvs::MvsDecodeDecision::Prepared(prepared) = decision else {
            panic!("expected native preparation");
        };
        receiver.commit(prepared).unwrap();
    }
    fn arm_runtime(runtime: &mut DynamicResolutionRuntime, initial: DisplaySize, full_first: bool) {
        if full_first {
            assert!(!runtime.observe_full_applied(1, initial));
            assert!(runtime.observe_initial_server_state(initial, initial));
        } else {
            assert!(!runtime.observe_initial_server_state(initial, initial));
            assert!(runtime.observe_full_applied(1, initial));
        }
    }

    #[test]
    fn exact_ack_releases_old_pointer_once_before_generation_reset_and_clears_new_mask() {
        use std::io::Read as _;
        use std::net::{TcpListener, TcpStream};
        use std::sync::mpsc;

        use frd_core::{
            InputEvent, PixelPoint, PointerButtons, PointerSample, SessionId, WheelDelta,
        };
        use frd_protocol_api::ProtocolRuntime;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut connection = crate::AppleConnection::new(client);
        let writer = connection.writer_handle().unwrap();
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let mut protocol_runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopProtocolEvents),
            Box::new(NoopProtocolFrames),
            None,
            Box::new(NoopProtocolWake),
        );
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let mut reader = NetworkReaderRuntime::new(
            &mut protocol_runtime,
            session_id,
            initial,
            true,
            Instant::now(),
        )
        .unwrap();
        {
            let mut dynamic = reader.dynamic_resolution.lock().unwrap();
            arm_runtime(&mut dynamic, initial, false);
            dynamic
                .send_target_with(target, |_| Ok(Instant::now()))
                .unwrap()
                .unwrap();
        }
        let mut media = ViewerMediaState::new(
            crate::media_negotiation::AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();
        let mut pointer = crate::runtime::PointerWireState::default();
        pointer
            .handle(
                InputEvent::PointerSample(PointerSample::new(
                    PixelPoint { x: 10, y: 20 },
                    PointerButtons {
                        primary: true,
                        secondary: true,
                        ..Default::default()
                    },
                    WheelDelta {
                        horizontal: -1,
                        ..Default::default()
                    },
                )),
                &writer,
            )
            .unwrap();
        let mut server_state = vec![0u8; 94];
        server_state[0..4].copy_from_slice(&1u32.to_be_bytes());
        server_state[12..16].copy_from_slice(&encoding::SERVER_STATE.to_be_bytes());
        server_state[16..18].copy_from_slice(&76u16.to_be_bytes());
        server_state[18..20].copy_from_slice(&5u16.to_be_bytes());
        server_state[20..22].copy_from_slice(&target.width.to_be_bytes());
        server_state[22..24].copy_from_slice(&target.height.to_be_bytes());

        {
            let mut before_commit = || pointer.release_all(&writer);
            reader
                .handle_frame(
                    server_state.clone(),
                    &writer,
                    &mut media,
                    &mut protocol_runtime,
                    &mut before_commit,
                )
                .unwrap();
            reader
                .handle_frame(
                    server_state,
                    &writer,
                    &mut media,
                    &mut protocol_runtime,
                    &mut before_commit,
                )
                .unwrap();
        }
        pointer
            .handle(
                InputEvent::PointerSample(PointerSample::new(
                    PixelPoint { x: 30, y: 40 },
                    PointerButtons::default(),
                    WheelDelta::default(),
                )),
                &writer,
            )
            .unwrap();

        let mut wire = [0u8; 28];
        peer.read_exact(&mut wire).unwrap();
        assert_eq!(&wire[0..6], &[5, 69, 0, 10, 0, 20]);
        assert_eq!(&wire[6..12], &[5, 0, 0, 10, 0, 20]);
        assert_eq!(
            wire[12], 3,
            "release must precede the new-generation full request"
        );
        assert_eq!(&wire[22..28], &[5, 0, 0, 30, 0, 40]);
        assert_eq!(reader.generation(), 2);
        writer.shutdown().unwrap();
    }
    #[test]
    fn capability_arms_only_after_matching_state_and_current_full_in_either_order() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();

        for full_first in [false, true] {
            let mut runtime = DynamicResolutionRuntime::new(initial, true);
            assert!(runtime
                .send_target_with(target, |_| Ok(Instant::now()))
                .unwrap()
                .is_none());
            arm_runtime(&mut runtime, initial, full_first);
            assert!(runtime
                .send_target_with(target, |_| Ok(Instant::now()))
                .unwrap()
                .is_some());
        }
    }
    #[test]
    fn capability_mismatch_and_default_off_never_arm_dynamic_requests() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let mismatch = DisplaySize::new(1024, 768).unwrap();

        let mut mismatch_runtime = DynamicResolutionRuntime::new(initial, true);
        assert!(!mismatch_runtime.observe_initial_server_state(mismatch, initial));
        assert!(!mismatch_runtime.observe_full_applied(1, initial));
        assert!(mismatch_runtime
            .send_target_with(target, |_| Ok(Instant::now()))
            .unwrap()
            .is_none());

        let mut disabled = DynamicResolutionRuntime::new(initial, false);
        assert!(!disabled.observe_initial_server_state(initial, initial));
        assert!(!disabled.observe_full_applied(1, initial));
        assert!(disabled
            .send_target_with(target, |_| Ok(Instant::now()))
            .unwrap()
            .is_none());
        assert_eq!(disabled.target_disposition(target), TargetDisposition::Wait);
    }

    #[test]
    fn pending_mismatched_server_state_is_server_initiated_not_requested_ack() {
        let initial = DisplaySize::new(1440, 2560).unwrap();
        let requested = DisplaySize::new(1280, 720).unwrap();
        let server_initiated = DisplaySize::new(1456, 1080).unwrap();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        runtime
            .send_target_with(requested, |_| Ok(Instant::now()))
            .unwrap()
            .unwrap();
        let mut receiver = MvsReceiveState::new(1);
        let mut surface = DisplaySurface::new(1, PixelSize::new(1440, 2560).unwrap()).unwrap();
        let mut media = ViewerMediaState::new(
            crate::media_negotiation::AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();

        assert_eq!(
            commit_server_geometry(
                &mut runtime,
                &mut receiver,
                &mut surface,
                &mut media,
                server_initiated,
                || Ok(()),
            )
            .unwrap()
            .unwrap()
            .disposition,
            ServerGeometryDisposition::ServerInitiated
        );
        assert!(runtime.pending_since.is_none());
        assert!(matches!(
            runtime.controller.state(),
            crate::dynamic_resolution::DynamicResolutionState::Switching {
                generation: 1,
                previous,
                target,
            } if *previous == initial && *target == server_initiated
        ));
        assert_eq!(
            runtime.target_disposition(requested),
            TargetDisposition::Wait
        );
        assert!(runtime.observe_full_applied(2, server_initiated));
        assert!(matches!(
            runtime.controller.state(),
            crate::dynamic_resolution::DynamicResolutionState::Stable { generation: 1, size }
                if *size == server_initiated
        ));
        assert_eq!(
            runtime.target_disposition(requested),
            TargetDisposition::Ready
        );
    }

    #[test]
    fn pending_geometry_drift_rejects_mismatched_server_state_before_classification() {
        let initial = DisplaySize::new(1440, 2560).unwrap();
        let requested = DisplaySize::new(1280, 720).unwrap();
        let server_initiated = DisplaySize::new(1456, 1080).unwrap();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        runtime
            .send_target_with(requested, |_| Ok(Instant::now()))
            .unwrap()
            .unwrap();

        let generation_error = runtime
            .plan_server_geometry(server_initiated, initial, 2)
            .unwrap_err();
        assert!(generation_error.to_string().contains("generation"));

        let previous_error = runtime
            .plan_server_geometry(server_initiated, DisplaySize::new(1366, 768).unwrap(), 1)
            .unwrap_err();
        assert!(previous_error.to_string().contains("previous"));
    }

    #[test]
    fn pending_geometry_drift_rejects_unchanged_server_state_before_classification() {
        let initial = DisplaySize::new(1440, 2560).unwrap();
        let requested = DisplaySize::new(1280, 720).unwrap();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        runtime
            .send_target_with(requested, |_| Ok(Instant::now()))
            .unwrap()
            .unwrap();

        let error = runtime
            .plan_server_geometry(initial, initial, 2)
            .unwrap_err();
        assert!(error.to_string().contains("generation"));
    }

    #[test]
    fn failed_before_generation_commit_preserves_pending_surface_and_receiver() {
        let initial = DisplaySize::new(1440, 2560).unwrap();
        let requested = DisplaySize::new(1280, 720).unwrap();
        let server_initiated = DisplaySize::new(1456, 1080).unwrap();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        runtime
            .send_target_with(requested, |_| Ok(Instant::now()))
            .unwrap()
            .unwrap();
        let pending_since = runtime.pending_since;
        let mut receiver = MvsReceiveState::new(1);
        let mut surface = DisplaySurface::new(1, PixelSize::new(1440, 2560).unwrap()).unwrap();
        let mut media = ViewerMediaState::new(
            crate::media_negotiation::AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();

        let error = match commit_server_geometry(
            &mut runtime,
            &mut receiver,
            &mut surface,
            &mut media,
            server_initiated,
            || Err(anyhow::anyhow!("injected ReleaseAll failure")),
        ) {
            Ok(_) => panic!("injected ReleaseAll failure must propagate"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("injected ReleaseAll failure"));
        assert_eq!(runtime.pending_since, pending_since);
        assert!(matches!(
            runtime.controller.state(),
            crate::dynamic_resolution::DynamicResolutionState::Pending {
                generation: 1,
                previous,
                target,
            } if *previous == initial && *target == requested
        ));
        assert_eq!(surface.generation, 1);
        assert_eq!((surface.width(), surface.height()), (1440, 2560));
        assert_eq!(receiver.generation, 1);
    }

    #[test]
    fn repeated_server_geometry_is_unchanged_without_generation_side_effects() {
        let size = DisplaySize::new(1440, 2560).unwrap();
        let mut runtime = DynamicResolutionRuntime::new(size, false);
        let mut receiver = MvsReceiveState::new(1);
        let mut surface = DisplaySurface::new(1, PixelSize::new(1440, 2560).unwrap()).unwrap();
        let mut media = ViewerMediaState::new(
            crate::media_negotiation::AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();
        let mut releases = 0usize;

        assert!(commit_server_geometry(
            &mut runtime,
            &mut receiver,
            &mut surface,
            &mut media,
            size,
            || {
                releases += 1;
                Ok(())
            },
        )
        .unwrap()
        .is_none());
        assert_eq!(releases, 0);
        assert_eq!(surface.generation, 1);
        assert_eq!((surface.width(), surface.height()), (1440, 2560));
        assert_eq!(receiver.generation, 1);
    }

    #[test]
    fn server_initiated_geometry_replaces_surface_and_requests_full_when_dynamic_off() {
        use std::cell::Cell;
        use std::io::Read as _;
        use std::net::{TcpListener, TcpStream};
        use std::sync::mpsc;

        use frd_core::SessionId;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut connection = crate::AppleConnection::new(client);
        let writer = connection.writer_handle().unwrap();
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let mut protocol_runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopProtocolEvents),
            Box::new(NoopProtocolFrames),
            None,
            Box::new(NoopProtocolWake),
        );
        let initial = DisplaySize::new(1440, 2560).unwrap();
        let server_initiated = DisplaySize::new(1456, 1080).unwrap();
        let mut reader = NetworkReaderRuntime::new(
            &mut protocol_runtime,
            session_id,
            initial,
            false,
            Instant::now(),
        )
        .unwrap();
        let mut media = ViewerMediaState::new(
            crate::media_negotiation::AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();
        let releases = Cell::new(0usize);
        let mut before_generation_commit = || {
            releases.set(releases.get() + 1);
            Ok(())
        };

        reader.receiver.generation = 2;
        let error = match reader.handle_frame(
            server_state_message(server_initiated),
            &writer,
            &mut media,
            &mut protocol_runtime,
            &mut before_generation_commit,
        ) {
            Ok(_) => panic!("generation drift must reject ServerState"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("generation 不一致"));
        assert_eq!(releases.get(), 0);
        assert_eq!(reader.generation(), 1);
        assert_eq!(reader.receiver.generation, 2);
        assert_eq!(reader.requests.generation, 1);
        let surface = reader.surface.lock().unwrap();
        assert_eq!((surface.width(), surface.height()), (1440, 2560));
        drop(surface);

        reader.receiver.generation = 1;

        reader
            .handle_frame(
                server_state_message(server_initiated),
                &writer,
                &mut media,
                &mut protocol_runtime,
                &mut before_generation_commit,
            )
            .unwrap();

        assert_eq!(releases.get(), 1);
        assert_eq!(reader.generation(), 2);
        let surface = reader.surface.lock().unwrap();
        assert_eq!((surface.width(), surface.height()), (1456, 1080));
        drop(surface);
        assert_eq!(reader.receiver.generation, 2);
        assert_eq!(reader.requests.generation, 2);
        let mut full = [0u8; protocol::FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES];
        peer.read_exact(&mut full).unwrap();
        assert_eq!(
            full,
            protocol::msg_fb_update_request(false, 0, 0, 1456, 1080).unwrap()
        );
        writer.shutdown().unwrap();
    }
    #[test]
    fn resize_pending_becomes_visible_only_after_successful_send() {
        use std::sync::mpsc;
        use std::sync::{Arc, Mutex};
        use std::thread;

        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let sent_at = Instant::now();
        let mut initial_runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut initial_runtime, initial, false);
        let runtime = Arc::new(Mutex::new(initial_runtime));
        let reader_runtime = runtime.clone();
        let (start_tx, start_rx) = mpsc::channel();
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let (commit_tx, commit_rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            start_rx.recv().unwrap();
            attempt_tx.send(()).unwrap();
            let committed = reader_runtime
                .lock()
                .unwrap()
                .observe_server_state(target)
                .is_some();
            commit_tx.send(committed).unwrap();
        });

        let request = {
            let mut runtime = runtime.lock().unwrap();
            runtime
                .send_target_with(target, |_| {
                    start_tx.send(()).unwrap();
                    attempt_rx.recv().unwrap();
                    assert!(commit_rx.try_recv().is_err());
                    Ok(sent_at)
                })
                .unwrap()
                .unwrap()
        };

        assert!(commit_rx.recv().unwrap());
        reader.join().unwrap();
        assert_eq!(request.target, target);
        assert_eq!(runtime.lock().unwrap().pending_since, None);
    }
    #[test]
    fn failed_resize_send_leaves_controller_stable_without_timestamp() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);

        assert!(runtime
            .send_target_with(target, |_| anyhow::bail!("injected send failure"))
            .is_err());
        assert!(runtime.pending_since.is_none());
        assert!(matches!(
            runtime.controller.state(),
            crate::dynamic_resolution::DynamicResolutionState::Stable { size, .. }
                if *size == initial
        ));
    }
    #[test]
    fn latest_debounced_viewport_waits_for_inflight_then_sends_once() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let first = DisplaySize::new(1280, 720).unwrap();
        let latest = DisplaySize::new(1024, 768).unwrap();
        let started = Instant::now();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        let mut queue = ViewportRequestQueue::default();
        let mut sent = Vec::new();

        queue.observe(first, started);
        queue
            .service(&mut runtime, started + Duration::from_millis(250), |size| {
                sent.push(size);
                Ok(started + Duration::from_millis(250))
            })
            .unwrap();
        queue.observe(latest, started + Duration::from_millis(300));
        queue
            .service(&mut runtime, started + Duration::from_millis(550), |size| {
                sent.push(size);
                Ok(started + Duration::from_millis(550))
            })
            .unwrap();
        assert_eq!(sent, vec![first]);
        assert!(queue.latest.is_some());

        assert!(runtime.observe_server_state(first).is_some());
        assert!(runtime.observe_full_applied(2, first));
        queue
            .service(&mut runtime, started + Duration::from_millis(551), |size| {
                sent.push(size);
                Ok(started + Duration::from_millis(551))
            })
            .unwrap();
        queue
            .service(&mut runtime, started + Duration::from_millis(800), |size| {
                sent.push(size);
                Ok(started + Duration::from_millis(800))
            })
            .unwrap();

        assert_eq!(sent, vec![first, latest]);
        assert!(queue.latest.is_none());
    }
    #[test]
    fn old_incremental_inflight_defers_dynamic_until_fragmented_response_boundary() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let started = Instant::now();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        let mut receiver = MvsReceiveState::new(0);
        let mut requests = ReaderRequestState::after_startup(started, 0);
        let mut queue = ViewportRequestQueue::default();
        queue.observe(target, started);
        let mut dynamic_sends = Vec::new();
        let mut full_sends = 0;

        let before_response = service_reader_tick_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            started + Duration::from_millis(250),
            |_| {},
            || {
                full_sends += 1;
                Ok(())
            },
            |size| {
                dynamic_sends.push(size);
                Ok(started + Duration::from_millis(250))
            },
        )
        .unwrap();
        assert!(before_response.dynamic_request.is_none());
        assert!(requests.framebuffer_request_in_flight());

        let rect = MvsRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        assert!(receiver
            .begin_at(rect, 2, &[0], started + Duration::from_millis(300))
            .unwrap()
            .is_none());
        let while_fragmented = service_reader_tick_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            started + Duration::from_millis(350),
            |_| {},
            || {
                full_sends += 1;
                Ok(())
            },
            |size| {
                dynamic_sends.push(size);
                Ok(started + Duration::from_millis(350))
            },
        )
        .unwrap();
        assert!(while_fragmented.dynamic_request.is_none());
        assert!(receiver.push_continuation(&[1]).unwrap().is_some());
        requests.consume_mvs_response();

        let mut incrementals = 0;
        let boundary = finish_full_boundary_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            started + Duration::from_millis(351),
            |_| {},
            || {
                full_sends += 1;
                Ok(())
            },
            |size| {
                dynamic_sends.push(size);
                Ok(started + Duration::from_millis(351))
            },
            || {
                incrementals += 1;
                Ok(())
            },
        )
        .unwrap();

        assert!(boundary.dynamic_request.is_some());
        assert!(!boundary.incremental_sent);
        assert_eq!(dynamic_sends, vec![target]);
        assert_eq!(incrementals, 0);
        assert_eq!(full_sends, 0);
        let server_state = [
            0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0x04, 0x51, 0, 0x4c, 0, 5, 0x05, 0xa0, 0x0a, 0,
        ];
        assert_eq!(
            reader_frame_class(&receiver, &server_state),
            ReaderFrameClass::ControlOrMedia
        );
        assert!(runtime.observe_server_state(target).is_some());
    }
    #[test]
    fn full_boundary_without_mature_candidate_sends_one_incremental() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let started = Instant::now();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        let mut receiver = MvsReceiveState::new(0);
        let mut requests = ReaderRequestState::after_startup(started, 0);
        let mut queue = ViewportRequestQueue::default();
        requests.consume_mvs_response();
        let mut incrementals = 0;

        let boundary = finish_full_boundary_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            started + Duration::from_millis(10),
            |_| {},
            || Ok(()),
            |_| anyhow::bail!("dynamic send should not run"),
            || {
                incrementals += 1;
                Ok(())
            },
        )
        .unwrap();

        assert!(boundary.dynamic_request.is_none());
        assert!(boundary.incremental_sent);
        assert_eq!(incrementals, 1);
        assert!(requests.framebuffer_request_in_flight());
    }
    #[test]
    fn table_followup_is_one_shot_and_full_before_due_cancels_it() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let started = Instant::now();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        let mut receiver = MvsReceiveState::new(0);
        let mut requests = ReaderRequestState::after_startup(started, 0);
        let mut queue = ViewportRequestQueue::default();
        requests.consume_mvs_response();
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        assert_eq!(
            requests.on_valid_table_record(0, started).unwrap(),
            TableScheduleStatus::Scheduled
        );
        let due = requests.table_followup_due().unwrap();
        assert_eq!(
            requests
                .on_valid_table_record(0, started + Duration::from_millis(50))
                .unwrap(),
            TableScheduleStatus::AlreadyScheduled
        );
        assert_eq!(requests.table_followup_due(), Some(due));

        let mut table_full_writes = 0;
        service_reader_tick_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            due - Duration::from_millis(1),
            |_| {},
            || {
                table_full_writes += 1;
                Ok(())
            },
            |_| anyhow::bail!("dynamic send should not run"),
        )
        .unwrap();
        assert_eq!(table_full_writes, 0);

        requests.consume_mvs_response();
        commit_native_mode_zero(&mut receiver);
        let mut incrementals = 0;
        finish_full_boundary_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            due - Duration::from_millis(1),
            |_| {},
            || {
                table_full_writes += 1;
                Ok(())
            },
            |_| anyhow::bail!("dynamic send should not run"),
            || {
                incrementals += 1;
                Ok(())
            },
        )
        .unwrap();
        service_reader_tick_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            due + Duration::from_secs(1),
            |_| {},
            || {
                table_full_writes += 1;
                Ok(())
            },
            |_| anyhow::bail!("dynamic send should not run"),
        )
        .unwrap();

        assert_eq!(table_full_writes, 0);
        assert_eq!(incrementals, 1);
        assert!(requests.table_followup_due().is_none());
        assert!(!receiver.awaiting_full());
    }
    #[test]
    fn table_only_followup_sends_exactly_once_and_duplicate_after_sent_is_error() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let started = Instant::now();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        let mut receiver = MvsReceiveState::new(0);
        let mut requests = ReaderRequestState::after_startup(started, 0);
        let mut queue = ViewportRequestQueue::default();
        requests.consume_mvs_response();
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        assert_eq!(
            requests.on_valid_table_record(0, started).unwrap(),
            TableScheduleStatus::Scheduled
        );
        let due = requests.table_followup_due().unwrap();
        let mut writes = 0;

        let tick = service_reader_tick_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            due,
            |_| {},
            || {
                writes += 1;
                Ok(())
            },
            |_| anyhow::bail!("dynamic send should not run"),
        )
        .unwrap();
        assert!(tick.table_followup_sent);
        assert_eq!(writes, 1);
        requests.consume_mvs_response();
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        assert!(requests
            .on_valid_table_record(0, due + Duration::from_millis(1))
            .is_err());
        service_reader_tick_at(
            &mut receiver,
            &mut requests,
            &mut queue,
            &mut runtime,
            due + Duration::from_secs(1),
            |_| {},
            || {
                writes += 1;
                Ok(())
            },
            |_| anyhow::bail!("dynamic send should not run"),
        )
        .unwrap();
        assert_eq!(writes, 1);
    }
    #[test]
    fn loop_tick_expires_continuous_continuations_and_independently_times_out_dynamic() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let latest = DisplaySize::new(1024, 768).unwrap();
        let started = Instant::now();
        let mut runtime = DynamicResolutionRuntime::new(initial, true);
        arm_runtime(&mut runtime, initial, false);
        runtime.send_target_with(target, |_| Ok(started)).unwrap();
        let mut receiver = MvsReceiveState::new(0);
        let rect = MvsRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        assert!(receiver
            .begin_at(rect, 100, &[0], started)
            .unwrap()
            .is_none());
        let mut requests = ReaderRequestState::after_startup(started, 0);
        let mut queue = ViewportRequestQueue::default();
        queue.observe(latest, started + Duration::from_millis(10));
        let mut full_writes = 0;
        let mut final_tick = None;

        for step in 1..=40 {
            let now = started + Duration::from_millis(step * 50);
            let tick = service_reader_tick_at(
                &mut receiver,
                &mut requests,
                &mut queue,
                &mut runtime,
                now,
                |_| {},
                || {
                    full_writes += 1;
                    Ok(())
                },
                |_| anyhow::bail!("dynamic send should not run"),
            )
            .unwrap();
            if step < 40 {
                assert!(!tick.incomplete_recovered);
                assert!(receiver.push_continuation(&[]).unwrap().is_none());
            } else {
                final_tick = Some(tick);
            }
        }

        let final_tick = final_tick.unwrap();
        assert!(final_tick.incomplete_recovered);
        assert!(final_tick.dynamic_timed_out);
        assert_eq!(full_writes, 1);
        assert!(!receiver.is_pending());
        assert!(queue.latest.is_none());
        assert_eq!(
            reader_frame_class(&receiver, &[0x14]),
            ReaderFrameClass::ServerKeepalive
        );
        let server_state = [
            0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0x04, 0x51, 0, 0x4c, 0, 5, 0x05, 0xa0, 0x0a, 0,
        ];
        assert_eq!(
            reader_frame_class(&receiver, &server_state),
            ReaderFrameClass::ControlOrMedia
        );
    }
    #[test]
    fn full_resync_rate_limit_waits_then_writes_exactly_once() {
        let now = Instant::now();
        let mut last_request = Some(now);
        let mut delays = Vec::new();
        let mut writes = 0;

        request_full_update_at(
            &mut last_request,
            now,
            |delay| delays.push(delay),
            || {
                writes += 1;
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(delays, vec![Duration::from_millis(200)]);
        assert_eq!(writes, 1);
        assert_eq!(last_request, Some(now + Duration::from_millis(200)));
    }
    #[test]
    fn pending_record_treats_heartbeat_shaped_frame_as_opaque_continuation() {
        let mut receiver = MvsReceiveState::new(0);
        let rect = MvsRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };

        assert!(receiver.begin(rect, 2, &[0x00]).unwrap().is_none());
        let record = receiver.push_continuation(&[0x14]).unwrap().unwrap();

        assert_eq!(record.rect, rect);
        assert_eq!(record.payload, vec![0x00, 0x14]);
    }
    #[test]
    fn pending_record_treats_query_shaped_frame_as_opaque_continuation() {
        let mut receiver = MvsReceiveState::new(0);
        let rect = MvsRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };

        assert!(receiver.begin(rect, 2, &[0x00]).unwrap().is_none());
        let record = receiver.push_continuation(&[0x08]).unwrap().unwrap();

        assert_eq!(record.payload, vec![0x00, 0x08]);
    }

    #[test]
    #[ignore = "需要未纳入公开仓库的本地授权 AVConference fixture"]
    fn pending_record_does_not_swallow_complete_media_stream_answer() {
        let mut receiver = MvsReceiveState::new(1);
        let rect = MvsRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        assert!(receiver.begin(rect, 1024, &[0]).unwrap().is_none());

        assert_eq!(
            reader_frame_class(&receiver, &media_stream_answer_fixture()),
            ReaderFrameClass::ControlOrMedia
        );
        assert!(receiver.is_pending());
    }

    #[test]
    #[ignore = "需要未纳入公开仓库的本地授权 AVConference fixture"]
    fn pending_record_keeps_malformed_media_control_lookalike_as_continuation() {
        let mut receiver = MvsReceiveState::new(1);
        let rect = MvsRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        assert!(receiver.begin(rect, 1024, &[0]).unwrap().is_none());
        let mut truncated = media_stream_answer_fixture();
        truncated.pop();

        assert_eq!(
            reader_frame_class(&receiver, &truncated),
            ReaderFrameClass::Continuation
        );
    }
    #[test]
    fn truncated_mvs_envelope_requests_resynchronization() {
        let mut receiver = MvsReceiveState::new(0);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        commit_native_mode_zero(&mut receiver);
        let mut truncated = vec![0; 16];
        truncated[12..16].copy_from_slice(&encoding::MVS.to_be_bytes());

        assert!(receiver.reject_truncated_mvs_envelope(&truncated).unwrap());
        assert!(receiver.awaiting_full());
        assert!(hpss::is_truncated_mvs_envelope(&truncated));
    }
    #[test]
    fn full_apply_always_queues_next_incremental_request() {
        assert_eq!(
            incremental_request_after_full_apply(1440, 2560).unwrap(),
            protocol::msg_fb_update_request(true, 0, 0, 1440, 2560).unwrap()
        );
    }
    #[test]
    fn out_of_bounds_mvs_geometry_marks_recovery_instead_of_failing_the_reader() {
        let mut receiver = MvsReceiveState::new(0);
        let display = DisplaySize::new(640, 480).unwrap();
        let invalid = MvsRect {
            x: 630,
            y: 470,
            width: 20,
            height: 20,
        };

        assert!(!mark_recovery_for_invalid_mvs_geometry(&mut receiver, invalid, display).unwrap());
        assert!(receiver.awaiting_full());
    }

    #[test]
    fn prepared_type_zero_patch_converts_exact_bgrx_and_derives_complete_nonblack_gate() {
        let rect = MvsRect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        let decoded = crate::mvs_full::DecodedMvsRect {
            width: 2,
            height: 1,
            rgb: vec![0x11, 0x22, 0x33, 0, 0, 0],
        };

        let prepared = prepare_type_zero_patch(&decoded, rect).unwrap();

        assert_eq!(prepared.patch.rect.x, 0);
        assert_eq!(prepared.patch.rect.width, 2);
        assert_eq!(prepared.patch.stride_bytes, 8);
        assert_eq!(
            prepared.patch.pixels.as_bytes(),
            &[0x33, 0x22, 0x11, 0, 0, 0, 0, 0]
        );
        assert!(prepared.contains_nonblack);

        let black = crate::mvs_full::DecodedMvsRect {
            width: 2,
            height: 1,
            rgb: vec![0; 6],
        };
        assert!(
            !prepare_type_zero_patch(&black, rect)
                .unwrap()
                .contains_nonblack
        );
    }

    #[test]
    fn type_zero_patch_prepare_and_commit_failure_leave_visible_surface_unchanged() {
        let size = PixelSize::new(8, 8).unwrap();
        let mut surface = DisplaySurface::new(1, size).unwrap();
        surface.framebuffer.pixels_mut().fill(0x0012_3456);
        let before_prepare_failure = surface.framebuffer.pixels().to_vec();
        let rect = MvsRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        };
        let malformed_decoded = crate::mvs_full::DecodedMvsRect {
            width: 8,
            height: 8,
            rgb: vec![0; 3],
        };

        assert!(prepare_type_zero_patch(&malformed_decoded, rect).is_err());
        assert_eq!(surface.framebuffer.pixels(), before_prepare_failure);

        let mut receiver = MvsReceiveState::new(1);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let decision = receiver.prepare(&native_mode_zero_payload(), 8, 8).unwrap();
        let mvs::MvsDecodeDecision::Prepared(prepared) = decision else {
            panic!("expected native type-0 preparation");
        };
        let publication = prepare_type_zero_patch(prepared.decoded(), rect).unwrap();
        receiver.reset(2);
        let mut next_generation_surface = DisplaySurface::new(2, size).unwrap();
        next_generation_surface
            .framebuffer
            .pixels_mut()
            .fill(0x0065_4321);
        let before_commit_failure = next_generation_surface.framebuffer.pixels().to_vec();

        assert!(apply_prepared_mvs_to_surface(
            &mut receiver,
            &mut next_generation_surface,
            prepared,
            &publication,
        )
        .is_err());
        assert_eq!(
            next_generation_surface.framebuffer.pixels(),
            before_commit_failure
        );
    }

    #[test]
    fn native_subrectangle_applies_without_complete_surface_evidence() {
        let surface = native_surface(16, 8);
        let dynamic = native_runtime(16, 8);
        let mut receiver = MvsReceiveState::new(1);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let record = native_record(MvsRect {
            x: 8,
            y: 0,
            width: 8,
            height: 8,
        });

        assert!(matches!(
            apply_native_mvs_frame(&mut receiver, &record, &surface, &dynamic).unwrap(),
            MvsRecordOutcome::FullApplied {
                complete_surface: false,
                ..
            }
        ));
        let surface = surface.lock().unwrap();
        assert!(surface.framebuffer.pixels()[..8]
            .iter()
            .all(|pixel| *pixel == 0));
        assert!(surface.framebuffer.pixels()[8..16]
            .iter()
            .all(|pixel| *pixel == 0x00ff_ffff));
        drop(surface);
        assert!(!dynamic.lock().unwrap().evidence.current_full_media_applied);
        assert!(!receiver.awaiting_full());
    }
    #[test]
    fn slice_d_opaque_record_commits_through_orchestrator_without_surface_or_p1_publication() {
        let surface = native_surface(8, 8);
        surface
            .lock()
            .unwrap()
            .framebuffer
            .pixels_mut()
            .fill(0x0012_3456);
        let before = surface.lock().unwrap().framebuffer.pixels().to_vec();
        let dynamic = native_runtime(8, 8);
        let mut receiver = MvsReceiveState::new(1);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        commit_native_mode_zero(&mut receiver);
        assert!(!receiver.awaiting_full());
        let record = MvsRecord {
            rect: MvsRect {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            },
            payload: native_opcode_zero_partial_payload(),
        };

        assert!(matches!(
            apply_native_mvs_frame(&mut receiver, &record, &surface, &dynamic).unwrap(),
            MvsRecordOutcome::PartialApplied { has_pixels: false }
        ));
        assert_eq!(surface.lock().unwrap().framebuffer.pixels(), before);
        assert!(!receiver.awaiting_full());
        let runtime = dynamic.lock().unwrap();
        assert!(!runtime.evidence.current_full_media_applied);
        assert!(!runtime.evidence.non_paused_media_activity);
        assert!(!runtime.armed);
    }
    #[test]
    fn type_one_opcode_one_replaces_visible_pixels_transactionally() {
        let surface = native_surface(8, 8);
        let dynamic = native_runtime(8, 8);
        let mut receiver = MvsReceiveState::new(1);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let seed = receiver
            .prepare(&native_mode_five_seed_payload(), 8, 8)
            .unwrap();
        let mvs::MvsDecodeDecision::Prepared(seed) = seed else {
            panic!("expected mode-five seed preparation");
        };
        receiver.commit(seed).unwrap();
        surface
            .lock()
            .unwrap()
            .framebuffer
            .pixels_mut()
            .fill(0x0012_3456);
        let before = surface.lock().unwrap().framebuffer.pixels().to_vec();
        let record = MvsRecord {
            rect: MvsRect {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            },
            payload: native_opcode_one_partial_payload(),
        };

        assert!(matches!(
            apply_native_mvs_frame(&mut receiver, &record, &surface, &dynamic).unwrap(),
            MvsRecordOutcome::PartialApplied { has_pixels: true }
        ));
        let surface = surface.lock().unwrap();
        assert_ne!(surface.framebuffer.pixels(), before);
        assert_eq!(surface.native_mvs_observability.type_zero_applied_count, 0);
        assert_eq!(surface.native_mvs_observability.content_revision, 1);
        drop(surface);
        assert!(!dynamic.lock().unwrap().evidence.current_full_media_applied);
    }
    #[test]
    fn slice_d_opaque_response_boundary_sends_one_normal_incremental_only() {
        let mut requests = ReaderRequestState::after_startup(Instant::now(), 0);
        requests.consume_mvs_response();
        let mut writes = 0usize;

        assert!(finish_partial_boundary_at(&mut requests, || {
            writes += 1;
            Ok(())
        })
        .unwrap());
        assert_eq!(writes, 1);
        assert!(requests.framebuffer_request_in_flight());
        assert!(!finish_partial_boundary_at(&mut requests, || {
            writes += 1;
            Ok(())
        })
        .unwrap());
        assert_eq!(writes, 1);
    }
    #[test]
    fn invalid_geometry_is_rejected_before_native_prepare_or_surface_mutation() {
        let surface = native_surface(8, 8);
        let dynamic = native_runtime(8, 8);
        let mut receiver = MvsReceiveState::new(1);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let initial = native_record(MvsRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        });
        apply_native_mvs_frame(&mut receiver, &initial, &surface, &dynamic).unwrap();
        assert!(!receiver.awaiting_full());
        let before = surface.lock().unwrap().framebuffer.pixels().to_vec();
        let invalid = native_record(MvsRect {
            x: 1,
            y: 0,
            width: 8,
            height: 8,
        });

        assert!(matches!(
            apply_native_mvs_frame(&mut receiver, &invalid, &surface, &dynamic).unwrap(),
            MvsRecordOutcome::RecoveryRequested
        ));
        assert_eq!(surface.lock().unwrap().framebuffer.pixels(), before);
        assert!(receiver.awaiting_full());
    }
    #[test]
    fn type_zero_prepare_failure_preserves_visible_surface_and_requests_recovery() {
        let surface = native_surface(8, 8);
        let dynamic = native_runtime(8, 8);
        let mut receiver = MvsReceiveState::new(1);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let record = MvsRecord {
            rect: MvsRect {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            },
            payload: vec![0],
        };
        let before = surface.lock().unwrap().framebuffer.pixels().to_vec();

        assert!(matches!(
            apply_native_mvs_frame(&mut receiver, &record, &surface, &dynamic).unwrap(),
            MvsRecordOutcome::RecoveryRequested
        ));
        assert_eq!(surface.lock().unwrap().framebuffer.pixels(), before);
        assert!(receiver.awaiting_full());
        {
            let runtime = dynamic.lock().unwrap();
            assert!(!runtime.evidence.current_full_media_applied);
            assert!(!runtime.armed);
        }
    }
    #[test]
    fn type_one_and_decode_failure_preserve_visible_surface_and_request_resync() {
        let surface = native_surface(8, 8);
        surface
            .lock()
            .unwrap()
            .framebuffer
            .pixels_mut()
            .fill(0x0012_3456);
        let dynamic = native_runtime(8, 8);
        let mut receiver = MvsReceiveState::new(1);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let before = surface.lock().unwrap().framebuffer.pixels().to_vec();

        for payload in [vec![1, 0x6d, 0x76, 0x73], vec![0]] {
            let record = MvsRecord {
                rect: MvsRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                },
                payload,
            };
            assert!(matches!(
                apply_native_mvs_frame(&mut receiver, &record, &surface, &dynamic).unwrap(),
                MvsRecordOutcome::RecoveryRequested
            ));
            assert_eq!(surface.lock().unwrap().framebuffer.pixels(), before);
            assert!(receiver.awaiting_full());
        }
    }
    #[test]
    fn exact_native_surface_commits_before_arming_p1_evidence() {
        let surface = native_surface(8, 8);
        let dynamic = native_runtime(8, 8);
        let initial = DisplaySize::new(8, 8).unwrap();
        assert!(!dynamic
            .lock()
            .unwrap()
            .observe_initial_server_state(initial, initial));
        let mut receiver = MvsReceiveState::new(1);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let record = native_record(MvsRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        });

        assert!(matches!(
            apply_native_mvs_frame(&mut receiver, &record, &surface, &dynamic).unwrap(),
            MvsRecordOutcome::FullApplied {
                complete_surface: true,
                ..
            }
        ));
        assert!(!receiver.awaiting_full());
        let runtime = dynamic.lock().unwrap();
        assert!(runtime.evidence.current_full_media_applied);
        assert!(runtime.armed);
    }

    #[test]
    fn mailbox_snapshot_recovery_preserves_decoder_and_sends_only_next_incremental() {
        use std::io::Read as _;
        use std::net::{TcpListener, TcpStream};
        use std::sync::mpsc;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut connection = crate::AppleConnection::new(client);
        let writer = connection.writer_handle().unwrap();
        let session_id = frd_core::SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let frames = Arc::new(Mutex::new(Vec::new()));
        let mut protocol_runtime = frd_protocol_api::ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopProtocolEvents),
            Box::new(SnapshotRecoveryFrames {
                updates: frames.clone(),
                reject_next_damage: Mutex::new(true),
            }),
            None,
            Box::new(NoopProtocolWake),
        );
        let size = frd_core::PixelSize::new(8, 8).unwrap();
        let mut publisher =
            AppleSurfacePublisher::begin(&mut protocol_runtime, session_id, size).unwrap();
        let surface = native_surface(8, 8);
        let dynamic_resolution = native_runtime(8, 8);
        let mut receiver = MvsReceiveState::new(1);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let last_full_request = Instant::now() - Duration::from_secs(1);
        let mut requests = ReaderRequestState::after_startup(last_full_request, 1);
        let viewport_requests = Arc::new(Mutex::new(ViewportRequestQueue::default()));
        let record = native_record(MvsRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        });

        handle_complete_mvs_record(
            &mut receiver,
            &mut requests,
            record,
            &surface,
            &viewport_requests,
            &dynamic_resolution,
            &writer,
            &mut protocol_runtime,
            &mut publisher,
        )
        .unwrap();

        assert!(!receiver.awaiting_full());
        let mut write = [0; protocol::FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES];
        peer.read_exact(&mut write).unwrap();
        assert_eq!(
            write,
            protocol::msg_fb_update_request(true, 0, 0, 8, 8).unwrap()
        );
        peer.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut extra = [0];
        let error = peer.read(&mut extra).unwrap_err();
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ));
        assert_eq!(requests.last_full_request, Some(last_full_request));
        let surface = surface.lock().unwrap();
        assert_eq!(surface.native_mvs_observability.type_zero_applied_count, 1);
        assert_eq!(surface.native_mvs_observability.content_revision, 1);
        drop(surface);
        assert!(matches!(
            frames.lock().unwrap().last(),
            Some(frd_frame::SurfaceUpdate::FrameBoundary {
                completeness: frd_frame::FrameCompleteness::FullBaseline,
                ..
            })
        ));
        writer.shutdown().unwrap();
    }

    #[test]
    fn needs_full_baseline_sets_awaiting_full_and_sends_non_incremental_request() {
        use std::io::Read as _;
        use std::net::{TcpListener, TcpStream};
        use std::sync::mpsc;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut connection = crate::AppleConnection::new(client);
        let writer = connection.writer_handle().unwrap();
        let session_id = frd_core::SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let mut protocol_runtime = frd_protocol_api::ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopProtocolEvents),
            Box::new(NoopProtocolFrames),
            None,
            Box::new(NoopProtocolWake),
        );
        let size = frd_core::PixelSize::new(16, 8).unwrap();
        let mut publisher =
            AppleSurfacePublisher::begin(&mut protocol_runtime, session_id, size).unwrap();
        let surface = native_surface(16, 8);
        let dynamic_resolution = native_runtime(16, 8);
        let mut receiver = MvsReceiveState::new(1);
        receiver.install_tables(&type_two_tables_fixture()).unwrap();
        let last_full_request = Instant::now() - Duration::from_secs(1);
        let mut requests = ReaderRequestState::after_startup(last_full_request, 1);
        let viewport_requests = Arc::new(Mutex::new(ViewportRequestQueue::default()));
        let record = native_record(MvsRect {
            x: 8,
            y: 0,
            width: 8,
            height: 8,
        });

        handle_complete_mvs_record(
            &mut receiver,
            &mut requests,
            record,
            &surface,
            &viewport_requests,
            &dynamic_resolution,
            &writer,
            &mut protocol_runtime,
            &mut publisher,
        )
        .unwrap();

        assert!(receiver.awaiting_full());
        let mut write = [0; protocol::FRAMEBUFFER_UPDATE_REQUEST_MESSAGE_BYTES];
        peer.read_exact(&mut write).unwrap();
        assert_eq!(
            write,
            protocol::msg_fb_update_request(false, 0, 0, 16, 8).unwrap()
        );
        assert_ne!(requests.last_full_request, Some(last_full_request));
        writer.shutdown().unwrap();
    }
}
