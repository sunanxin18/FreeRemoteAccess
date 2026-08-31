# Frame Transaction Render Latency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove confirmed per-update GPU fault-scope/poll amplification while making startup `Reset + first FullBaseline` atomic, preserving exact FIFO/overlap writes, session ownership, actual fault observations, and fail-closed input/presentation behavior.

**Architecture:** `frd-frame` owns one session-bound cross-drain `FrameTransactionCompiler`; it buffers startup until one matching FullBaseline boundary can emit an atomic `Startup`, then emits revision-atomic steady-state transactions. `frd-render-wgpu` plans a whole non-empty transaction batch, executes FIFO allocation/uploads through an exactly-once observed scope runner, and returns four product facts separately from actual scope diagnostics. `frd-shell-desktop` owns compiler lifetime, independent UI/frame redraw decisions, fatal no-render dispatch, identity-bound timing probes, and a dedicated fixed-schema performance sink.

**Tech Stack:** Rust 2021, `std::sync::mpsc`, `Mutex`, `Instant`, owned `PixelBuffer`/`PixelPatch`, wgpu 30, winit, egui, Cargo test, PowerShell live-validation probes.

**Spec:** `docs/superpowers/specs/2026-08-30-frame-transaction-render-latency-design.md`

## Global Constraints

- The floating control island must not start until this plan's live measurement gate passes or a new measured bottleneck is explicitly re-scoped and approved.
- Do not change ARD 3.10 wire bytes, Apple request order/pacing, MVS grammar, RDP/RFB semantics, authentication, audio, or protocol producer behavior.
- Do not discard queued revisions, dirty rectangles, or old disjoint regions; do not sort, deduplicate, last-write-win, merge across revisions, or request fewer server frames to improve apparent latency.
- Do not add GPU compute, a generic hardware decoder, direct type-1 BGRX output, or transfer-region merging in this plan. Those remain post-measurement alternatives.
- `FrameTransaction` stays protocol- and platform-neutral. No HPSS/MVS/RDP identifier, Windows handle, AppKit object, X11/Wayland resource, or decoder type may enter `frd-frame` or `frd-render-wgpu`.
- `FrameTransactionCompiler::new(session_id)` binds permanently to one `SessionId` and is owned by that session's `LiveSessionPorts`; a foreign-session update is fatal and a session change constructs a new compiler.
- `Reset` is buffered with its matching Damage and first FullBaseline boundary. Reset-only and Reset-plus-Damage drains emit no transaction, allocate/upload/install nothing, and cause no frame redraw. Only a newer generation for the bound session may replace pending startup/revision state.
- `FrameTransaction::Startup` contains the Reset metadata and exactly one ordered Damage plus its FullBaseline boundary. Each steady `Revision` contains exactly one ordered Damage plus one matching boundary. Incremental-first startup is a typed fatal error.
- A compiler error or renderer batch error is fatal. Do not route it through the existing device/full-snapshot recovery path: current code has no approved non-fatal contract that can prove a partially written texture safe.
- A GPU fault may occur after one or more `queue.write_texture` calls. After a scope begins, execution never uses `?` or returns before exactly one `finish`; an observed GPU fault wins over a simultaneous execution error. CPU staged state must not commit, and the shell must block input, detach both `RemoteRenderer` and `RemoteBinding`, clear pending-write bookkeeping, publish stable typed details, and perform no record/submit/present of that texture.
- `FramePresented` remains possible only after an actual swapchain submit and present confirms the exact final receipt. A batch commit, empty submit, redraw request, or upload is not presentation.
- Keep `RuntimeWakeGate`, mailbox capacity/overflow behavior, generation events, full-baseline input gate, and blocked-presentation empty-submit behavior intact except for the explicitly described batch wiring.
- Runtime drain returns separate `ui_redraw_needed` and `frame_redraw_needed` facts. Mailbox non-emptiness is never a redraw fact; a frame redraw requires a clean presentable final boundary.
- Scope begin/finish/poll counts come only from the real renderer scope observer. They remain separate from the four `BatchApplyOutcome` product facts and are never hard-coded from batch non-emptiness.
- Metrics use only the dedicated fixed-schema bounded file sink. Do not redirect or parse general stderr, and do not emit credentials, endpoints, pixels, payloads, clipboard/audio content, or free-form errors.
- Fixed-schema `frame_response_ms` always records the individual `FrameResponseTiming::sample_ms`; `smoothed_ms` remains UI-only and never enters performance rows or comparisons.
- Metrics configuration is disabled only when all three variables are absent, enabled only when all are present and valid, and a stable pre-session fatal for every partial, empty, invalid, unknown, or unsafe-target combination. Mailbox age uses checked monotonic subtraction from the earliest constituent enqueue timestamp.
- Serial emits exactly one aggregate `SerialDrain` row per non-empty drain. Candidate emits `CandidateBatch` only for a successful non-empty compiled batch. Every such candidate row reports actual `{begins,finishes,polls}={1,1,1}`, and aggregate counts equal successful non-empty batches.
- Serial/candidate performance runs remain fault-free through restore and normal close. Fault no-present evidence comes from the separate injected Task 4/5 contract tests and never enters performance CSVs.
- Automated tests are limited to the timing-correlation, transaction, atomic batch, fatal-detach, and aggregate-metric contracts in this plan. Do not add visual snapshots, synthetic Apple grammars, duplicate MVS fixtures, or exhaustive timing permutations.
- Never put credentials, target addresses, raw remote pixels, encrypted payloads, or local secret-store contents in source, tests, logs, documentation, or command lines.
- The worktree currently contains 14 modified source files that predate `FrameTransaction`. Task 1 deliberately turns that exact inherited state plus the timing correction into a transparent baseline checkpoint. Do not describe all of those lines as newly implemented by this plan.
- In Task 1, review and stage exactly the listed 14 modified files. Do not stage the approved untracked specifications or this plan in the checkpoint unless the primary agent has already committed them separately.
- Before Task 1's checkpoint, and again before every later edit to `crates/frd-protocol-apple/src/network_reader.rs`, `crates/frd-frame/src/surface.rs`, or `crates/frd-shell-desktop/src/application.rs`, inspect the current diff. Use narrow `apply_patch` hunks; never replace any of those files wholesale.
- From Task 2 onward, start from the clean source checkpoint and make frequent independently reviewable commits. Before each commit run `git diff --check`, inspect `git diff --cached`, and confirm no unrelated file is staged.

---

### Task 1: Fix recovery timing ownership and checkpoint the inherited incremental-frame pipeline

**Files:**

- Modify/Test narrowly: `crates/frd-protocol-apple/src/network_reader.rs`
- Review and checkpoint without unrelated edits:
  - `crates/frd-app/src/controller.rs`
  - `crates/frd-app/src/lib.rs`
  - `crates/frd-frame/src/surface.rs`
  - `crates/frd-protocol-api/src/lib.rs`
  - `crates/frd-protocol-apple/src/high_performance.rs`
  - `crates/frd-protocol-apple/src/hpss.rs`
  - `crates/frd-protocol-apple/src/network_reader.rs`
  - `crates/frd-protocol-apple/src/runtime.rs`
  - `crates/frd-protocol-apple/src/surface_publisher.rs`
  - `crates/frd-shell-desktop/src/application.rs`
  - `crates/frd-shell-desktop/src/input.rs`
  - `crates/frd-ui-egui/src/session_chrome.rs`
  - `crates/frd-ui-model/src/chrome.rs`
  - `src/vnc/cold_hpss.rs`

**Interfaces:**

- Consumes: existing `ReaderRequestState::{begin_complete_mvs_response, discard_frame_response_timing, complete_frame_response_at, mark_full_request_sent}` and `request_full_update(&AppleWriterHandle, &mut ReaderRequestState, u16, u16) -> anyhow::Result<()>`.
- Produces: corrected request-owned timing behavior; `RecoveryRequested` preserves only the newly written recovery request's `TimedFramebufferRequest`; inherited frame-response UI, type-1 dirty-patch publication, wake coalescing, blocked-present empty submit, and input-gate changes become one named clean checkpoint for later tasks.

- [ ] **Step 1: Re-read the three protected diffs before editing**

```powershell
git diff -- crates/frd-protocol-apple/src/network_reader.rs
git diff -- crates/frd-frame/src/surface.rs
git diff -- crates/frd-shell-desktop/src/application.rs
git status --short
```

Expected: the current inherited diff remains present; no `FrameTransaction` implementation exists yet. Record the 14 modified paths shown in the Files block and do not remove, reformat, or rewrite their existing hunks.

- [ ] **Step 2: Add the failing end-to-end recovery timing test**

In `migrated_runtime_tests`, add an event sink that records only timing events:

```rust
struct TimingProtocolEvents(Arc<Mutex<Vec<FrameResponseTiming>>>);

impl frd_protocol_api::RuntimeEventSink for TimingProtocolEvents {
    fn publish(&self, event: frd_protocol_api::SessionEvent) -> Result<(), ProtocolError> {
        if let frd_protocol_api::SessionEvent::FrameResponseTiming(timing) = event {
            self.0.lock().unwrap().push(timing);
        }
        Ok(())
    }
}
```

Add `recovery_full_request_replaces_failed_response_timing_and_completes_on_next_valid_type_zero`. Use the existing loopback `AppleWriterHandle`, `native_surface(8, 8)`, `native_runtime(8, 8)`, `type_two_tables_fixture`, `AppleSurfacePublisher::begin`, and `handle_complete_mvs_record` helpers. The first record is malformed and must synchronously write one non-incremental recovery request. Assert the timing vector is still empty. Feed a valid `native_record` as the next response, then assert exactly one event with generation `1`; the malformed record itself must never produce a sample.

- [ ] **Step 3: Run the focused test to verify RED**

```powershell
cargo test -p frd-protocol-apple recovery_full_request_replaces_failed_response_timing_and_completes_on_next_valid_type_zero -- --nocapture
```

Expected: FAIL because the outer handler clears the recovery request's replacement timing slot, leaving the timing vector empty after the valid type-0 response.

- [ ] **Step 4: Add one helper that discards the failed incoming sample before writing recovery**

Implement the helper beside `process_complete_mvs_record`:

```rust
fn replace_failed_response_with_full_request(
    writer: &AppleWriterHandle,
    requests: &mut ReaderRequestState,
    display_size: DisplaySize,
) -> Result<MvsRecordOutcome> {
    requests.discard_frame_response_timing();
    request_full_update(
        writer,
        requests,
        display_size.width,
        display_size.height,
    )?;
    Ok(MvsRecordOutcome::RecoveryRequested)
}
```

Use this helper for invalid tables/classification and for `apply_native_mvs_frame` returning `RecoveryRequested`. Discarding happens before the write, so a failed write leaves no old sample; a successful write installs its own generation/timestamp through `mark_full_request_sent`.

- [ ] **Step 5: Preserve a successful replacement in the outer handler**

Capture the outcome before consuming it:

```rust
let recovery_requested = matches!(&outcome, MvsRecordOutcome::RecoveryRequested);
```

Change only the timing decision:

```rust
let frame_response_timing =
    if (full_applied || partial_applied) && publication == PublicationOutcome::Published {
        requests.complete_frame_response_at(receiver.generation, Instant::now())
    } else if recovery_requested {
        None
    } else {
        requests.discard_frame_response_timing();
        None
    };
```

Do not change the recovery bytes, sleeps, writer calls, next incremental request, publication logic, or MVS state machine.

- [ ] **Step 6: Run focused GREEN and existing timing/recovery guards**

```powershell
cargo test -p frd-protocol-apple recovery_full_request_replaces_failed_response_timing_and_completes_on_next_valid_type_zero -- --nocapture
cargo test -p frd-protocol-apple frame_response_timing_ -- --nocapture
cargo test -p frd-protocol-apple mailbox_snapshot_recovery_preserves_decoder_and_sends_only_next_incremental -- --nocapture
```

Expected: all pass; the new test observes one recovery-request sample only after the valid type-0 publication.

- [ ] **Step 7: Audit the inherited 14-file checkpoint and report its actual scope**

Review the full diff and explicitly report these inherited feature groups before committing:

1. type-1 destination compaction, owned dirty BGRX patches, one `Damage` vector plus boundary, and `PixelBuffer::from_boxed_slice` allocation adoption;
2. strict captured `ServerState` active-framebuffer parsing and its HP startup/runtime fixtures;
3. generation-bound `FrameResponseTiming`, EWMA/throttled UI value, and the recovery-correlation correction from Steps 2–5;
4. `RuntimeWakeGate` wake coalescing and pending texture-write empty submits when presentation is blocked;
5. remote keyboard/pointer focus gating that cannot re-enter remote ownership while input is blocked.

Also state that these are inherited baseline changes, not all authored by the current task.

- [ ] **Step 8: Run the known affected-crate checkpoint gate**

```powershell
cargo fmt --all -- --check
cargo test -p frd-protocol-api -p frd-protocol-apple -p frd-frame -p frd-render-wgpu -p frd-app -p frd-ui-model -p frd-ui-egui -p frd-shell-desktop
git diff --check
```

Expected: formatting succeeds; all affected crate tests pass; `git diff --check` is silent. If review or tests expose another defect, fix and re-run this gate before checkpointing.

- [ ] **Step 9: Stage exactly the inherited 14 source files and inspect the checkpoint**

```powershell
git add -- crates/frd-app/src/controller.rs crates/frd-app/src/lib.rs crates/frd-frame/src/surface.rs crates/frd-protocol-api/src/lib.rs crates/frd-protocol-apple/src/high_performance.rs crates/frd-protocol-apple/src/hpss.rs crates/frd-protocol-apple/src/network_reader.rs crates/frd-protocol-apple/src/runtime.rs crates/frd-protocol-apple/src/surface_publisher.rs crates/frd-shell-desktop/src/application.rs crates/frd-shell-desktop/src/input.rs crates/frd-ui-egui/src/session_chrome.rs crates/frd-ui-model/src/chrome.rs src/vnc/cold_hpss.rs
git diff --cached --name-only
git diff --cached --check
git diff --cached --stat
```

Expected: exactly the 14 paths listed above are staged. The approved untracked spec/plan documents are not silently included.

- [ ] **Step 10: Commit the transparent baseline checkpoint**

```powershell
git commit -m "perf: checkpoint incremental frame pipeline" -m "Checkpoints the inherited type-1 dirty-patch, active ServerState geometry, frame-response timing, wake coalescing, blocked-present flush, and input-gate work after fixing recovery timing ownership."
```

Expected: the source worktree is clean after the commit except for separately managed approved design/plan documents. Preserve this commit hash as the pre-`FrameTransaction` source baseline.

---

### Task 2: Add actual scope counters, a safe metrics sink, and the serial baseline

**Files:**

- Modify/Test: `crates/frd-frame/src/mailbox.rs`
- Modify: `crates/frd-frame/src/lib.rs`
- Modify/Test: `crates/frd-frame/tests/mailbox.rs`
- Modify/Test: `crates/frd-render-wgpu/src/gpu_fault.rs`
- Modify/Test: `crates/frd-render-wgpu/src/lib.rs`
- Modify: `crates/frd-render-wgpu/Cargo.toml`
- Modify: `Cargo.lock`
- Create/Test: `crates/frd-shell-desktop/src/frame_metrics.rs`
- Create/Test: `crates/frd-shell-desktop/src/frame_metrics_sink.rs`
- Modify/Test: `crates/frd-shell-desktop/src/fatal.rs`
- Modify: `crates/frd-shell-desktop/src/lib.rs`
- Modify/Test narrowly: `crates/frd-shell-desktop/src/application.rs`
- Create/Test: `tools/run-frame-metrics.ps1`
- Create/Test: `tools/compare-frame-metrics.ps1`

**Interfaces:**

- Consumes: existing `FrameMailbox::{push, pop, len}`, serial `RemoteRenderer::apply_update(SurfaceUpdate)`, `FrameResponseTiming`, successful routed `SessionCommand::Input`, actual `PresentationEvent::FramePresented`, and window minimized/restored lifecycle.
- Produces: public `EnqueuedSurfaceUpdate { enqueued_at, update }`, `FrameMailbox::{oldest_enqueued_at, pop_enqueued}`, and panic-free `checked_mailbox_age`; public `GpuScopeObservation { begins, finishes, polls }` produced through the same fakeable lifecycle seam used by the real begin/finish/poll sites; crate-visible `FramePipelineMetrics::from_environment(Instant)` and `BatchMetricContext`; `MetricSinkConfiguration::from_values(Option<OsString>, Option<OsString>, Option<OsString>) -> Result<Self, MetricSinkError>`; `FrameMetricsSink::open_from_environment(Instant) -> Result<Option<Self>, MetricSinkError>` with disabled/invalid/enabled truth table; one aggregate `SerialDrain` row per non-empty drain; fixed-schema bounded CSV event/process files; repeatable serial/candidate capture and worst-window comparison scripts.

- [ ] **Step 1: Add focused RED tests for timestamps, real scope counters, safe schema, and identity probes**

Add these exact tests:

1. `oldest_enqueued_at_tracks_the_retained_front_entry` brackets a successful push with two `Instant::now()` values, verifies the returned timestamp lies inside them, obtains the same timestamp through `pop_enqueued`, and verifies the queue then returns `None`.
2. `mailbox_age_returns_none_when_observation_precedes_enqueue_time` passes `observed_at < enqueued_at` to the pure checked-age helper and proves it returns `None` without panic; the normal ordering returns the exact duration. `serial_drain_age_uses_earliest_envelope_after_unlock` drains two timestamped envelopes, captures the drain start after releasing the mailbox lock, and proves the row uses the earlier enqueue rather than wake/drain/metrics-write time.
3. `scope_lifecycle_seam_records_begin_finish_poll_in_order` uses a recording observer and the pure lifecycle guard to assert event order plus delta `{ begins: 1, finishes: 1, polls: 1 }`; `scope_lifecycle_failed_begin_records_nothing` injects acquisition failure and asserts an empty event vector.
4. `dx12_scope_observation_smoke_reports_real_begin_finish_poll` runs separately on Windows against a real DX12 adapter and asserts one actual `{1,1,1}`; it may return an explicitly printed `SKIP adapter_unavailable` only when adapter acquisition returns `None`, never for a validation failure.
5. `safe_metric_sink_writes_only_the_fixed_header_and_typed_fields` writes one row to a temporary file and asserts the exact header and field count; endpoint/password/pixel/free-form-error fields must not exist in the schema.
6. `metric_sink_configuration_distinguishes_disabled_partial_and_enabled` exhaustively checks all eight presence combinations: all absent is disabled, all present is enabled, and each of six partial combinations is `InvalidConfiguration`. `metric_sink_configuration_rejects_empty_unknown_and_unsafe_values` covers empty/invalid run id, unknown implementation, an existing output, a directory/device/reparse target, a missing/non-directory/reparse parent, and a non-`.csv` target. `partial_metric_configuration_is_fatal_before_session_launch` and `invalid_metric_configuration_is_fatal_before_session_launch` assert the same stable report and a zero launch counter.
7. `serial_nonempty_drain_emits_one_aggregate_row` applies Reset, Damage with two rectangles, and Boundary through a deterministic serial fake whose actual deltas are `{1,1,1}`, `{1,1,1}`, `{1,1,1}`; assert exactly one `SerialDrain` row with `batch_result=Success`, rectangles `2`, and scopes `{3,3,3}`.
8. `input_to_next_present_requires_same_session_and_generation_and_clears_on_boundaries` sends an input for identity A/generation 7, proves foreign session and generation 8 presentations cannot complete it, proves matching presentation can, then separately proves reset, detach, close, fatal, generation change, and new session each clear the probe.
9. `frame_response_metric_uses_sample_not_ui_smoothed_value` supplies `FrameResponseTiming { sample_ms: 17, smoothed_ms: 83, .. }` and asserts the fixed-schema row stores `frame_response_ms=17`; the UI model may still consume `83` independently.
10. `worst_window_helpers_choose_highest_value_and_earliest_tie` feeds deterministic events into `tools/compare-frame-metrics.ps1` and asserts nearest-rank p95, sum, CPU endpoint delta, maximum working set, and first-five/last-five medians.

- [ ] **Step 2: Run the focused tests to verify RED**

```powershell
cargo test -p frd-frame oldest_enqueued_at_tracks_the_retained_front_entry -- --nocapture
cargo test -p frd-shell-desktop mailbox_age_returns_none_ -- --nocapture
cargo test -p frd-shell-desktop serial_drain_age_ -- --nocapture
cargo test -p frd-render-wgpu scope_lifecycle_seam_ -- --nocapture
cargo test -p frd-shell-desktop safe_metric_sink_ -- --nocapture
cargo test -p frd-shell-desktop metric_sink_configuration_distinguishes_ -- --nocapture
cargo test -p frd-shell-desktop metric_sink_configuration_rejects_ -- --nocapture
cargo test -p frd-shell-desktop partial_metric_configuration_ -- --nocapture
cargo test -p frd-shell-desktop invalid_metric_configuration_ -- --nocapture
cargo test -p frd-shell-desktop serial_nonempty_drain_ -- --nocapture
cargo test -p frd-shell-desktop input_to_next_present_ -- --nocapture
cargo test -p frd-shell-desktop frame_response_metric_uses_sample_ -- --nocapture
pwsh -NoProfile -File .\tools\compare-frame-metrics.ps1 -SelfTest
```

Expected: compile/file-not-found failures because the timestamp envelope, checked-age helper, lifecycle seam, sink configuration, serial aggregate, identity probe, and scripts do not exist. The real DX12 smoke is intentionally not part of RED because it is an environment check after implementation.

- [ ] **Step 3: Timestamp mailbox entries without changing admission or overflow semantics**

Use the public process-local envelope in `crates/frd-frame/src/mailbox.rs`, keep `pop() -> Option<SurfaceUpdate>` temporarily as a compatibility wrapper for pre-Task-5 callers, and place the crate-visible checked helper in `crates/frd-shell-desktop/src/frame_metrics.rs`:

```rust
pub struct EnqueuedSurfaceUpdate {
    pub enqueued_at: Instant,
    pub update: SurfaceUpdate,
}

pub fn oldest_enqueued_at(&self) -> Option<Instant> {
    self.queue.front().map(|entry| entry.enqueued_at)
}

pub fn pop_enqueued(&mut self) -> Option<EnqueuedSurfaceUpdate>;

pub(crate) fn checked_mailbox_age(
    apply_or_drain_started_at: Instant,
    enqueued_at: Instant,
) -> Option<Duration> {
    apply_or_drain_started_at.checked_duration_since(enqueued_at)
}
```

Capture `Instant::now()` only for an accepted queue entry. Reset replacement and overflow retention keep each retained entry's original timestamp; byte accounting reads `entry.update`; entry and pixel limits do not change. `pop_enqueued` moves the original timestamp and update together; temporary `pop` delegates to it and returns only `.update`. Re-export `EnqueuedSurfaceUpdate` from `frd-frame/src/lib.rs` beside `FrameMailbox`. A drain holds the mailbox lock only while moving the envelopes, releases it, then captures `apply_or_drain_started_at=Instant::now()`. Compute the minimum retained `enqueued_at` and call `checked_mailbox_age`; never derive age from wake, decode completion, lock-release, or metric-write time. A missing envelope or a future/different-clock timestamp is `MetricSinkError::InvalidObservation`, invalidates the run, and is never suppressed or encoded as zero.

- [ ] **Step 4: Instrument actual GPU scope lifecycle sites**

Define and export the observation value, then make one lifecycle seam own every real observation site:

```rust
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GpuScopeObservation {
    pub begins: u64,
    pub finishes: u64,
    pub polls: u64,
}

impl GpuScopeObservation {
    pub fn checked_delta(self, earlier: Self) -> Option<Self>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScopeLifecycleEvent { Begin, Finish, Poll }

pub(crate) trait ScopeLifecycleObserver: Send + Sync {
    fn record(&self, event: ScopeLifecycleEvent);
    fn snapshot(&self) -> GpuScopeObservation;
}

pub(crate) struct ObservedScopeLifecycle {
    observer: Arc<dyn ScopeLifecycleObserver>,
    finish_recorded: bool,
    poll_recorded: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScopeLifecycleError { DuplicateFinish, PollBeforeFinish, DuplicatePoll }

pub(crate) fn begin_observed_scope<T, E>(
    observer: Arc<dyn ScopeLifecycleObserver>,
    acquire: impl FnOnce() -> Result<T, E>,
) -> Result<(T, ObservedScopeLifecycle), E>;

impl ObservedScopeLifecycle {
    pub(crate) fn record_finish(&mut self) -> Result<(), ScopeLifecycleError>;
    pub(crate) fn record_poll(&mut self) -> Result<(), ScopeLifecycleError>;
}
```

The production `AtomicScopeLifecycleObserver` owns three `AtomicU64`s; the deterministic test observer owns a `Mutex<Vec<ScopeLifecycleEvent>>`. `GpuContext` retains the production observer and exposes `scope_observation()`. `GpuFaultScope::new` calls `begin_observed_scope` only after `begin_operation` succeeds and all three wgpu guards are owned. `GpuFaultScope::finish` retains the result of `record_finish` on its first line, pops the wgpu scopes, retains the result of `record_poll` immediately before the real `device.poll(wgpu::PollType::Poll)`, then combines lifecycle/GPU results; it never uses `?` before the actual poll. A lifecycle error maps to `GpuFaultClass::ObservationIncomplete` only after poll unless a higher-priority GPU fault wins. The lifecycle object rejects a duplicate/out-of-order finish or poll with the private `ScopeLifecycleError`; ownership makes those states unreachable in production, and no other code increments counters. The pure seam tests require no adapter. Add the real Windows DX12 smoke separately with `wgpu::Backends::DX12`; its only permitted skip is adapter acquisition returning `None`, printed exactly as `SKIP adapter_unavailable`. Live Task 7 candidate rows remain the acceptance proof for the real production path. No shell code may synthesize values from update counts or a constant.

Add this test-only executor dependency to `crates/frd-render-wgpu/Cargo.toml`, matching the shell crate, and accept the corresponding `Cargo.lock` package-dependency update:

```toml
[dev-dependencies]
pollster = "0.4"
```

Use `pollster::block_on` only in `dx12_scope_observation_smoke_reports_real_begin_finish_poll`; production renderer code must not depend on it.

- [ ] **Step 5: Define the bounded fixed-schema sink**

In `frame_metrics_sink.rs`, use exactly these safe enums and row fields:

```rust
const METRIC_SCHEMA_VERSION: u16 = 1;
const MAX_METRIC_ROWS: usize = 32_768;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SafeRunId(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetricSinkError {
    InvalidConfiguration,
    CreateFailed,
    WriteFailed,
    CapacityExceeded,
    InvalidObservation,
}

enum MetricSinkConfiguration {
    Disabled,
    Enabled {
        path: PathBuf,
        run_id: SafeRunId,
        implementation: MetricImplementation,
    },
}

impl MetricSinkConfiguration {
    fn from_values(
        path: Option<OsString>,
        run_id: Option<OsString>,
        implementation: Option<OsString>,
    ) -> Result<Self, MetricSinkError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetricImplementation { Serial, Candidate }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetricPhase {
    VisibleWarmup,
    VisibleMeasurement,
    MinimizedWarmup,
    MinimizedMeasurement,
    Restore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetricEventKind {
    PhaseBoundary,
    FrameResponse,
    SerialDrain,
    CandidateBatch,
    Presentation,
    InputToNextPresent,
    StableFault,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetricBatchResult { Success, SerialFailure, CompileFailure, RendererFailure }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetricBatchFailureClass { Compiler, RendererPlanning, RendererExecution, Gpu }

pub(crate) struct FrameMetricRow {
    pub(crate) run_id: SafeRunId,
    pub(crate) implementation: MetricImplementation,
    pub(crate) phase: MetricPhase,
    pub(crate) event: MetricEventKind,
    pub(crate) batch_result: Option<MetricBatchResult>,
    pub(crate) batch_failure_class: Option<MetricBatchFailureClass>,
    pub(crate) monotonic_us: u64,
    pub(crate) session_id: Option<u64>,
    pub(crate) generation: Option<u64>,
    pub(crate) revision: Option<u64>,
    pub(crate) source_updates: Option<u64>,
    pub(crate) transactions: Option<u64>,
    pub(crate) rectangles: Option<u64>,
    pub(crate) batch_cpu_us: Option<u64>,
    pub(crate) mailbox_age_us: Option<u64>,
    pub(crate) scope_begins: Option<u64>,
    pub(crate) scope_finishes: Option<u64>,
    pub(crate) scope_polls: Option<u64>,
    pub(crate) gpu_fault_code: Option<GpuFaultClass>,
    pub(crate) process_cpu_total_us: Option<u64>,
    pub(crate) process_cpu_delta_us: Option<u64>,
    pub(crate) working_set_bytes: Option<u64>,
    pub(crate) frame_response_ms: Option<u64>,
    pub(crate) input_to_next_present_us: Option<u64>,
}

pub(crate) struct FrameMetricsSink {
    writer: BufWriter<File>,
    run_id: SafeRunId,
    implementation: MetricImplementation,
    started_at: Instant,
    rows_written: usize,
    invalid: bool,
}

impl FrameMetricsSink {
    pub(crate) fn open_from_environment(
        started_at: Instant,
    ) -> Result<Option<Self>, MetricSinkError>;
    pub(crate) fn write_row(&mut self, row: FrameMetricRow) -> Result<(), MetricSinkError>;
}
```

The event file header is exactly:

```text
schema_version,run_id,implementation,phase,event,batch_result,batch_failure_class,monotonic_us,session_id,generation,revision,source_updates,transactions,rectangles,batch_cpu_us,mailbox_age_us,scope_begins,scope_finishes,scope_polls,gpu_fault_code,process_cpu_total_us,process_cpu_delta_us,working_set_bytes,frame_response_ms,input_to_next_present_us
```

`SafeRunId` accepts only ASCII letters, digits, `_`, and `-`, length `1..=64`. `MetricSinkConfiguration::from_values` implements the exact truth table: `(None,None,None) -> Disabled`; every partial combination returns `InvalidConfiguration`; `(Some,Some,Some)` becomes Enabled only after typed validation. Reject empty/non-Unicode run or implementation values, an implementation other than exactly `serial`/`candidate`, and an empty/non-Unicode output path. The output must be a new `.csv` regular-file path below an existing non-reparse directory, must not use a device namespace, and neither the target nor any traversed existing component may be a reparse point; an existing target, directory target, missing/non-directory parent, or unsafe component is `InvalidConfiguration`. `FrameMetricsSink::open_from_environment` reads only `FRD_FRAME_METRICS_PATH`, `FRD_FRAME_METRICS_RUN_ID`, and `FRD_FRAME_METRICS_IMPLEMENTATION`, delegates to that pure parser, maps Disabled to `Ok(None)`, and opens Enabled with `create_new(true)`; an OS create failure after validation is `CreateFailed`.

Write the fixed CSV header once, encode optional numeric/enum fields as empty cells, and flush at every phase boundary and completion. A successful serial non-empty drain emits one `SerialDrain` row with `batch_result=Success`; a serial failure emits that drain's single `SerialDrain` row with its closed failure class. A successful non-empty compiled batch emits exactly one `CandidateBatch` row with `batch_result=Success`; compile/renderer failures emit `StableFault` rather than a failed `CandidateBatch`, so the candidate row count remains the successful non-empty batch count. Other event kinds carry neither a batch result nor a failure class except that typed `StableFault` carries its closed failure result/class. After accepting row 32,768, the next write returns `MetricSinkError::CapacityExceeded` and marks the run invalid. Never accept a free-form string field or write to stderr.

Add `FatalReport::frame_metrics_startup(error: MetricSinkError) -> Self`. Before constructing `LiveSessionPorts`, starting a protocol runner, or opening any network session, initialize the metrics sink. Any partial or all-present-but-invalid configuration becomes `FatalReport::frame_metrics_startup(MetricSinkError::InvalidConfiguration)` with fixed `component="application"`, `operation="frame_metrics"`, `reason="frame_metrics_configuration_invalid"`, and `details="none"`; do not echo any environment value. Exhaustively map `CreateFailed|WriteFailed` to `frame_metrics_create_failed` and the unreachable-at-open `CapacityExceeded|InvalidObservation` to `frame_metrics_invalid_startup_state`, also with `details="none"`, so later enum additions cannot break shell compilation. All-absent configuration remains normal disabled startup and all-present valid configuration enables the sink.

- [ ] **Step 6: Implement identity-bound timings and fixed run phases**

Use these exact crate-visible construction and observation boundaries in `frame_metrics.rs`; `application.rs` never constructs sink rows directly:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MetricIdentity {
    pub(crate) session_id: SessionId,
    pub(crate) generation: u64,
}

struct InputToNextPresentProbe { identity: MetricIdentity, accepted_at: Instant }

struct MetricPhaseState {
    phase: MetricPhase,
    run_started_at: Instant,
    phase_started_at: Instant,
}

pub(crate) struct FramePipelineMetrics {
    sink: Option<FrameMetricsSink>,
    active_identity: Option<MetricIdentity>,
    pending_input: Option<InputToNextPresentProbe>,
    last_presented: Option<(MetricIdentity, Instant)>,
    run_phase: Option<MetricPhaseState>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BatchMetricContext {
    pub(crate) batch_started_at: Instant,
    pub(crate) source_update_count: usize,
    pub(crate) oldest_age: Option<Duration>,
    pub(crate) transaction_count: usize,
}

impl FramePipelineMetrics {
    pub(crate) fn from_environment(started_at: Instant) -> Result<Self, MetricSinkError>;
    pub(crate) fn begin_session(&mut self, session_id: SessionId);
    pub(crate) fn observe_generation(&mut self, session_id: SessionId, generation: u64);
    pub(crate) fn observe_input_sent(
        &mut self,
        identity: MetricIdentity,
        accepted_at: Instant,
    );
    pub(crate) fn observe_presented(
        &mut self,
        identity: MetricIdentity,
        revision: u64,
        at: Instant,
    );
    pub(crate) fn clear_input_probe(&mut self);
    pub(crate) fn detach(&mut self);
    pub(crate) fn close(&mut self);
    pub(crate) fn fatal(&mut self);
}
```

`FramePipelineMetrics::from_environment` is the only application constructor and delegates exactly once to `FrameMetricsSink::open_from_environment`; `Ok(None)` builds disabled metrics and every error is mapped through `FatalReport::frame_metrics_startup` before session/network launch. Start `VisibleWarmup` at the first actually confirmed FullBaseline presentation. Advance to `VisibleMeasurement` after exactly 5 seconds; that phase lasts 30 seconds. An actual minimized/occluded transition after visible measurement starts `MinimizedWarmup`; advance after 5 seconds to a 30-second `MinimizedMeasurement`. Restore starts `Restore`, records the next confirmed latest receipt, then completes and closes the bounded sink. Drive phase deadlines from the existing wait-based event loop, not a busy loop. `input-to-next-present` pairs only the earliest outstanding successfully routed non-ReleaseAll input with the next confirmed presentation of the exact same identity. `begin_session`, generation change, Reset observation, detach, close, fatal, or replacement session clears it before any later sample.

Add `observe_frame_response_timing(&mut self, timing: FrameResponseTiming)`: its `FrameResponse` row sets `frame_response_ms=Some(u64::from(timing.sample_ms))`. Never read `timing.smoothed_ms` in the metrics module or comparison script; that EWMA remains exclusively the throttled UI status value.

- [ ] **Step 7: Instrument the unchanged serial path with actual scope deltas**

Add:

```rust
struct DrainedFrameUpdates {
    updates: Vec<EnqueuedSurfaceUpdate>,
    drain_started_at: Instant,
    oldest_age: Duration,
}

#[derive(Default)]
struct SerialDrainAggregate {
    successful_updates: usize,
    uploaded_rectangles: usize,
    scope: GpuScopeObservation,
}

impl SerialDrainAggregate {
    fn observe(
        &mut self,
        outcome: ApplyOutcome,
        actual_delta: GpuScopeObservation,
    ) -> Result<(), MetricSinkError>;
}

fn drain_frame_updates(&mut self) -> DrainedFrameUpdates;
```

`drain_frame_updates` moves every `EnqueuedSurfaceUpdate` out while holding the mailbox lock, releases the lock, captures `drain_started_at` immediately before the first serial apply, selects the minimum constituent `enqueued_at`, and requires `checked_mailbox_age(drain_started_at, earliest)` to succeed. A non-empty drain with no valid age returns `MetricSinkError::InvalidObservation`, emits no fabricated age, and invalidates the measurement run. Before consuming the envelopes, retain `source_update_count=updates.len()` and inspect each `.update` Reset metadata by reference only to call `observe_generation` and clear the timing probe; never infer or install a binding from it. For every Reset, Damage, and Boundary call, snapshot `window.gpu.scope_observation()` immediately before and after, require `after.checked_delta(before)`, and feed the successful `ApplyOutcome` plus actual delta into one in-memory `SerialDrainAggregate`. `observe` increments rectangle count only from `ApplyOutcome::Damage { uploaded_rectangles }`, sums all three scope fields with checked arithmetic, and never emits a row itself.

After the last update, emit exactly one `SerialDrain` row for that non-empty drain with `batch_result=Success`, no failure class, `source_updates=source_update_count`, `transactions=0`, the aggregate's successful update/rectangle facts, summed actual scope observations, `mailbox_age_us=oldest_age`, and `batch_cpu_us = now - drain_started_at`. Do not emit one row per Reset/Damage/Boundary. If an existing serial apply fails, finish observing that attempted update, emit exactly one typed `SerialDrain` row for the whole non-empty drain with `batch_result=SerialFailure`, `batch_failure_class=Gpu` only for `RendererError::GpuFault` and otherwise `RendererExecution`, the aggregate accumulated before/through the failing attempt where actual deltas exist, and the stable fault code, then preserve the existing serial failure behavior. Use `if let RendererError::GpuFault(fault)` rather than an exhaustive match, so Task 4 can add variants without reopening the fixed schema. This makes serial and candidate successful work rows both one-per-drain/batch without changing protocol or renderer semantics. Record frame-response, presentation cadence, and identity-bound input-to-next-present through their separate event kinds. Lifecycle call sites in Step 6 clear probes even when no metric sink is enabled.

- [ ] **Step 8: Create the fixed capture and comparison scripts**

`tools/run-frame-metrics.ps1` accepts:

```powershell
param(
  [Parameter(Mandatory=$true)][ValidateSet('serial','candidate')][string]$Implementation,
  [Parameter(Mandatory=$true)][ValidatePattern('^[A-Za-z0-9_-]{1,64}$')][string]$RunId,
  [string]$OutputDirectory = '.\target\validation'
)
```

It resolves `OutputDirectory` beneath the repository `target\validation` tree, creates that normal directory before launch if absent, rejects reparse/device/out-of-tree targets, an existing client, or an existing output file, then sets the three sink variables. It launches the release client without redirecting stdout/stderr, waits for the sink's visible-warmup marker, and samples process CPU total plus working set at S0 and every one second through S30 for each measured phase. Use a `Stopwatch` deadline for each sample rather than accumulating 30 independent sleeps. At the visible phase end minimize with `ShowWindowAsync(SW_MINIMIZE)`, wait for the five-second minimized warm-up marker, sample minimized S0..S30, restore with `SW_RESTORE`, and wait for the Restore receipt marker. The user then disconnects/closes normally; the script requires exit code zero and rejects any `StableFault` row. The user performs the same continuous visible workload in both runs; no address or credential enters the script.

The process-sample file has this exact header and only typed values:

```text
schema_version,run_id,implementation,phase,second,monotonic_us,process_cpu_total_us,process_cpu_delta_us,working_set_bytes
```

For S0, `process_cpu_delta_us` is empty. For S1..S30 it is the current total minus the preceding one-second sample; the analyzer still uses endpoint totals for every five-second CPU equation.

`tools/compare-frame-metrics.ps1` accepts serial/candidate app-event and process-sample CSV paths. `VisibleMeasurement` enumerates every half-open window `[t,t+5)` for integer `t=0..25` for batch CPU, mailbox age, scope counts, Presentation, InputToNextPresent, and FrameResponse. Use nearest-rank p95 `sorted[(count * 95 + 99) / 100 - 1]`; ties retain the earliest window. Scope/cadence use the greatest window sum. `MinimizedMeasurement` still requires complete S0..S30 process samples, but static-workload event latency/window fields are N/A; it reports observed Batch/FrameResponse activity counts and totals, and requires zero observed Presentation rows. CPU uses `process_cpu_total_us[S(t+5)] - process_cpu_total_us[S(t)]`; working-set maximum uses S0..S30; first median uses S1..S5 and last median uses S26..S30. `-SelfTest` runs deterministic fixtures and exits nonzero on any mismatch.

Use this exact comparison-script boundary:

```powershell
param(
  [string]$SerialEvents,
  [string]$SerialProcessSamples,
  [string]$CandidateEvents,
  [string]$CandidateProcessSamples,
  [string]$OutputPath,
  [switch]$SelfTest
)
```

Its output includes the exact Task 7 predicates: every `CandidateBatch` row in a performance run has `batch_result=Success` and scope `{1,1,1}`, aggregate candidate begins=finishes=polls=successful non-empty batches, `visible_batch_cpu_8ms_and_no_regression`, `visible_scope_amplification_reduced_50_percent`, the unchanged visible CPU/working-set/input/frame-response gates, minimized CPU/working-set gates, `minimized_presentation_paused`, and identity-bearing Restore Presentations from serial and candidate. It exits nonzero for a non-success performance batch, false mandatory predicate, incomplete applicable visible window, missing S0..S30 sample, schema/capacity error, or non-monotonic identity/timestamp.

- [ ] **Step 9: Run focused GREEN and build the serial release**

```powershell
cargo test -p frd-frame oldest_enqueued_at_tracks_the_retained_front_entry -- --nocapture
cargo test -p frd-shell-desktop mailbox_age_returns_none_ -- --nocapture
cargo test -p frd-shell-desktop serial_drain_age_ -- --nocapture
cargo test -p frd-render-wgpu scope_lifecycle_seam_ -- --nocapture
cargo test -p frd-shell-desktop safe_metric_sink_ -- --nocapture
cargo test -p frd-shell-desktop metric_sink_configuration_distinguishes_ -- --nocapture
cargo test -p frd-shell-desktop metric_sink_configuration_rejects_ -- --nocapture
cargo test -p frd-shell-desktop partial_metric_configuration_ -- --nocapture
cargo test -p frd-shell-desktop invalid_metric_configuration_ -- --nocapture
cargo test -p frd-shell-desktop serial_nonempty_drain_ -- --nocapture
cargo test -p frd-shell-desktop input_to_next_present_ -- --nocapture
cargo test -p frd-shell-desktop frame_response_metric_uses_sample_ -- --nocapture
pwsh -NoProfile -File .\tools\compare-frame-metrics.ps1 -SelfTest
cargo build --release -p freeremotedesk-windows
```

Then run the separately labeled hardware smoke:

```powershell
cargo test -p frd-render-wgpu dx12_scope_observation_smoke_reports_real_begin_finish_poll -- --nocapture
```

Expected: deterministic tests and build pass. The DX12 smoke either passes with actual `{1,1,1}` or prints exactly `SKIP adapter_unavailable`; any other skip/failure is a failure. All three absent metric variables create no file, any partial configuration fails before session launch with stable content, all three valid values enable one fixed-schema file, and the serial renderer behavior remains unchanged.

- [ ] **Step 10: Commit diagnostics before collecting the baseline**

```powershell
git add -- Cargo.lock crates/frd-frame/src/mailbox.rs crates/frd-frame/src/lib.rs crates/frd-frame/tests/mailbox.rs crates/frd-render-wgpu/Cargo.toml crates/frd-render-wgpu/src/gpu_fault.rs crates/frd-render-wgpu/src/lib.rs crates/frd-shell-desktop/src/frame_metrics.rs crates/frd-shell-desktop/src/frame_metrics_sink.rs crates/frd-shell-desktop/src/fatal.rs crates/frd-shell-desktop/src/lib.rs crates/frd-shell-desktop/src/application.rs tools/run-frame-metrics.ps1 tools/compare-frame-metrics.ps1
git diff --cached --check
git diff --cached
git commit -m "perf: add safe frame pipeline measurements"
```

Expected: no file under `target/` is staged. Preserve this commit and release SHA-256 as the exact serial-baseline identity.

- [ ] **Step 11: Capture the fixed serial baseline before adding transactions**

```powershell
pwsh -NoProfile -File .\tools\run-frame-metrics.ps1 -Implementation serial -RunId serial_pre_batch
pwsh -NoProfile -File .\tools\compare-frame-metrics.ps1 -SelfTest
Get-FileHash '.\target\release\freeremotedesk-windows.exe' -Algorithm SHA256
```

Expected: the serial process remains fault-free through Restore and normal disconnect/close. Dedicated `serial_pre_batch` event and process CSV files contain one 5-second visible warm-up, 30-second visible measurement, one 5-second minimized warm-up, 30-second minimized measurement, and Restore receipt. Both measured phases contain S0..S30 process samples and six complete non-overlapping 5-second spans plus all 26 required sliding windows. Keep artifacts under `target`; do not commit them.

---

### Task 3: Add the cross-drain `FrameTransactionCompiler`

**Files:**

- Create: `crates/frd-frame/src/transaction.rs`
- Modify: `crates/frd-frame/src/lib.rs`
- Create/Test: `crates/frd-frame/tests/transactions.rs`

**Interfaces:**

- Consumes: owned `EnqueuedSurfaceUpdate`, `SessionId`, `PixelSize`, `PixelFormat`, `PixelPatch`, `FrameCompleteness`, and the process-local monotonic enqueue `Instant` from Task 2.
- Produces: public protocol-neutral `FrameReset`, `FrameRevision`, `FrameTransaction` with constituent timing/count accessors, `FrameTransactionError`, `FrameTransactionCompiler::new(SessionId)`, `compile<I>(&mut self, I) -> Result<Vec<FrameTransaction>, FrameTransactionError>`, `has_buffered_input(&self) -> bool`, `buffered_source_update_count(&self) -> usize`, and `earliest_buffered_enqueue_at(&self) -> Option<Instant>`.

- [ ] **Step 1: Write the focused RED compiler tests**

Add only these core tests:

1. `reset_and_damage_without_full_boundary_emit_nothing` proves Reset-only and Reset-plus-Damage drains return an empty vector while `has_buffered_input()` remains true.
2. `matching_full_baseline_emits_one_atomic_startup_without_copying_patches` feeds the matching first FullBaseline boundary in a later drain and asserts exactly one `Startup { reset, revision, .. }`, FullBaseline completeness, original two `PixelBuffer` pointers, and original patch order.
3. `incremental_first_startup_boundary_is_fatal` asserts `StartupBoundaryNotFullBaseline` and no emitted transaction.
4. `compiler_rejects_foreign_session_and_session_switch_uses_new_compiler` sends Reset/Damage/Boundary from another session and expects `ForeignSession`; then constructs `FrameTransactionCompiler::new(other_session)` and proves its own startup succeeds.
5. `newer_generation_discards_pending_startup_or_revision_but_stale_reset_fails` proves only a strictly greater generation for the bound session may replace buffered state; a numerically newer foreign `SessionId` remains `ForeignSession`, never a reset shortcut.
6. `steady_revision_errors_are_exact_and_never_emit` covers `UpdateBeforeReset`, `DuplicateDamage`, `RevisionWhilePending`, `BoundaryWithoutDamage`, `BoundaryMismatch`, and `StaleUpdate` after an already closed revision.
7. `compiler_carries_earliest_constituent_enqueue_across_drains` assigns increasing timestamps to Reset in drain one, Damage in drain two, and FullBaseline boundary in drain three; the emitted Startup must carry the Reset timestamp. Repeat with one complete steady revision plus a second revision split across drains and assert every transaction, then the batch minimum, retains the earliest timestamp of its own Reset/Damage/Boundary constituents.

- [ ] **Step 2: Run tests to verify RED**

```powershell
cargo test -p frd-frame --test transactions -- --nocapture
```

Expected: compile failure because the transaction types are not exported.

- [ ] **Step 3: Define the exact transaction and error types**

```rust
#[derive(Debug)]
pub struct FrameReset {
    pub session_id: SessionId,
    pub generation: u64,
    pub size: PixelSize,
    pub format: PixelFormat,
}

#[derive(Debug)]
pub struct FrameRevision {
    pub session_id: SessionId,
    pub generation: u64,
    pub revision: u64,
    pub patches: Vec<PixelPatch>,
    pub completeness: FrameCompleteness,
}

#[derive(Debug)]
pub enum FrameTransaction {
    Startup {
        earliest_constituent_enqueue_at: Instant,
        reset: FrameReset,
        revision: FrameRevision,
    },
    Revision {
        earliest_constituent_enqueue_at: Instant,
        revision: FrameRevision,
    },
}

impl FrameTransaction {
    pub fn earliest_constituent_enqueue_at(&self) -> Instant;
    pub fn source_update_count(&self) -> usize;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameTransactionError {
    InvalidReset,
    ForeignSession,
    StaleReset,
    UpdateBeforeReset,
    StaleUpdate,
    DuplicateDamage,
    RevisionWhilePending,
    BoundaryWithoutDamage,
    BoundaryMismatch,
    StartupBoundaryNotFullBaseline,
}
```

Keep private `CompilerSurfaceState { generation, last_revision }`, `PendingStartup { reset: FrameReset, earliest_constituent_enqueue_at: Instant, damage: Option<PendingDamage> }`, `PendingDamage { generation, revision, patches, earliest_constituent_enqueue_at: Instant }`, and steady `pending_revision: Option<PendingDamage>`. Do not derive or implement `Clone` for frame transactions, pending pixel owners, `PixelPatch`, or `PixelBuffer`.

- [ ] **Step 4: Implement cross-drain ordered compilation**

Use this public boundary:

```rust
pub struct FrameTransactionCompiler {
    session_id: SessionId,
    active: Option<CompilerSurfaceState>,
    pending_startup: Option<PendingStartup>,
    pending_revision: Option<PendingDamage>,
}

impl FrameTransactionCompiler {
    pub fn new(session_id: SessionId) -> Self;

    pub fn compile<I>(
        &mut self,
        updates: I,
    ) -> Result<Vec<FrameTransaction>, FrameTransactionError>
    where
        I: IntoIterator<Item = EnqueuedSurfaceUpdate>;

    pub fn has_buffered_input(&self) -> bool {
        self.pending_startup.is_some() || self.pending_revision.is_some()
    }

    pub fn buffered_source_update_count(&self) -> usize;
    pub fn earliest_buffered_enqueue_at(&self) -> Option<Instant>;
}
```

Check `update.session_id` before every other field and return `ForeignSession` without rebinding. A valid Reset has nonzero generation/geometry and is either the compiler's first reset or strictly newer than active/pending generation; it clears older pending startup/revision, retains the Reset envelope time in `PendingStartup`, and emits nothing. Startup Damage moves the entire ordered patch vector into `PendingStartup` and stores the minimum of Reset/Damage enqueue times; matching boundary emits nothing unless it is `FullBaseline`, then stores the minimum of Reset/Damage/Boundary times, moves Reset and Damage into exactly one `FrameTransaction::Startup`, installs compiler active metadata, and advances `last_revision`. `Incremental` at this first boundary returns `StartupBoundaryNotFullBaseline`. After startup, Damage/Boundary form one `FrameTransaction::Revision` carrying the minimum of those two envelope times. A legal newer-generation Reset discards both the old buffered pixels and their timestamps; a foreign session can never take that path. Never emit, allocate, upload, install, present, or request a frame redraw for incomplete buffered input. Because each emitted transaction owns its earliest constituent timestamp, Task 5 can take the batch minimum even when Reset, Damage, and Boundary arrived in different drains; it never substitutes the latest drain's front timestamp.

- [ ] **Step 5: Run focused GREEN and mailbox regression tests**

```powershell
cargo test -p frd-frame --test transactions -- --nocapture
cargo test -p frd-frame compiler_carries_earliest_constituent_enqueue_across_drains -- --nocapture
cargo test -p frd-frame --test mailbox -- --nocapture
```

Expected: all pass; startup is emitted only at FullBaseline, foreign sessions fail, and original pixel allocations/order reach the emitted transaction.

- [ ] **Step 6: Commit the frame contract**

```powershell
git add -- crates/frd-frame/src/transaction.rs crates/frd-frame/src/lib.rs crates/frd-frame/tests/transactions.rs
git diff --cached --check
git diff --cached
git commit -m "feat(frame): compile revision-atomic frame transactions"
```

---

### Task 4: Add staged renderer batch planning and atomic `BatchApplyOutcome`

**Files:**

- Modify/Test: `crates/frd-render-wgpu/src/gpu_fault.rs`
- Modify/Test: `crates/frd-render-wgpu/src/remote_texture.rs`
- Modify/Test: `crates/frd-render-wgpu/src/lib.rs`
- Modify/Test: `crates/frd-shell-desktop/src/fatal.rs`

**Interfaces:**

- Consumes: session-bound `Vec<FrameTransaction>` from Task 3, actual `GpuScopeObservation` from Task 2, current `RemoteUpdateState`, `RemoteTexture`, `PresentationReceipt`, `GpuFaultScope`, and `GpuContext::commit_if_unchanged`.
- Produces: four-fact `BatchApplyOutcome`; separate `FrameBatchIdentity`, `BatchScopeDiagnostics`, `BatchApplySuccess`, and typed identity-bearing `BatchApplyFailure`; exact FIFO `PlannedOperationExecutor`; generic private `BatchScopeBackend`/`execute_with_observed_scope` used by production and deterministic error tests; `RemoteRenderer::apply_update_batch(Vec<FrameTransaction>) -> Result<BatchApplySuccess, BatchApplyFailure>`; exhaustive stable shell tokens for every new `RendererError`. The serial API remains only until Task 5 migrates all callers.

- [ ] **Step 1: Add RED tests for startup facts, FIFO execution, and both error-precedence paths**

Add exactly these core tests:

1. `atomic_startup_plan_returns_all_four_product_facts` plans one Startup plus a steady revision and asserts installed startup identity/size/format, exact rectangle count, texture-write fact, and only the final current-generation receipt.
2. `recording_executor_preserves_fifo_patch_row_byte_and_overlap_order` uses a 3x2 surface and three two-row patches. Startup patch 0 covers all 3x2 pixels with stride 16 (12 payload bytes plus four `0xE0` padding bytes per row); startup patch 1 covers `(1,0,2,2)` with stride 12 and `0xE1` padding; the steady patch covers `(0,0,2,2)` with stride 12 and `0xE2` padding. Give each BGRX pixel a distinct byte id: base rows `A B C / D E F`, second patch `G H / I J`, final patch `K L / M N`. Assert the recorder sees Reset, startup patch 0, startup patch 1, steady patch 0 in FIFO order, never copies padding sentinels, and produces the exact 24-byte serial result `K L H / M N J` where each symbolic pixel is `[id,id,id,0]`.
3. `execution_error_still_finishes_and_returns_execution_primary` injects an execution error with a clean finish; assert actual delta begins=1, finishes=1, polls=1, execution error is primary, and no state/resource commits.
4. `gpu_fault_wins_when_execution_and_finish_both_fail` injects one execution error plus finish Validation fault; assert the GPU fault is primary, the typed execution code is secondary, actual begins=finishes=polls=1, and no state/resource commits.
5. `batch_receipt_is_not_presented_before_real_submit` preserves the existing receipt test through the new success wrapper.
6. `fatal_renderer_error_tokens_cover_new_batch_variants` calls `present_error_code` once with each of `PresentError::Renderer(RendererError::EmptyBatch)`, `PresentError::Renderer(RendererError::BatchExecutionPanicked)`, and `PresentError::Renderer(RendererError::ScopeObservationInvalid)`, then asserts the three exact stable tokens.

- [ ] **Step 2: Run the focused tests to verify RED**

```powershell
cargo test -p frd-render-wgpu atomic_startup_ -- --nocapture
cargo test -p frd-render-wgpu recording_executor_ -- --nocapture
cargo test -p frd-render-wgpu execution_error_still_finishes_ -- --nocapture
cargo test -p frd-render-wgpu gpu_fault_wins_ -- --nocapture
cargo test -p frd-shell-desktop fatal_renderer_error_tokens_ -- --nocapture
```

Expected: compile failure because batch success/failure, executor, scope runner, and exhaustive shell tokens for the new renderer variants do not exist.

- [ ] **Step 3: Define the exact public atomic outcome**

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameBatchIdentity {
    pub session_id: SessionId,
    pub generation: u64,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstalledSurface {
    pub session_id: SessionId,
    pub generation: u64,
    pub size: PixelSize,
    pub format: PixelFormat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatchApplyOutcome {
    pub installed_surface: Option<InstalledSurface>,
    pub uploaded_rectangles: usize,
    pub had_texture_writes: bool,
    pub final_boundary: Option<PresentationReceipt>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatchScopeDiagnostics {
    pub observation: GpuScopeObservation,
    pub observed_fault: Option<GpuFaultClass>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatchApplySuccess {
    pub outcome: BatchApplyOutcome,
    pub scope: BatchScopeDiagnostics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatchApplyFailure {
    pub identity: Option<FrameBatchIdentity>,
    pub primary: RendererError,
    pub secondary_execution: Option<RendererError>,
    pub scope: Option<BatchScopeDiagnostics>,
}
```

Add closed `RendererError::{EmptyBatch, BatchExecutionPanicked, ScopeObservationInvalid}` variants. In the same task, extend `crates/frd-shell-desktop/src/fatal.rs::renderer_error_code` exhaustively with `renderer_empty_batch`, `renderer_batch_execution_panicked`, and `renderer_scope_observation_invalid`; this keeps `cargo check -p frd-shell-desktop` exhaustive before Task 5 adds the richer batch report. Implement `BatchApplyFailure::planning(identity, error)`, `begin(identity, fault)`, and `counter_regressed(identity)` with `scope=None`; no successful scope began for planning/begin failure and no valid delta exists for counter regression. `identity` is the offending transaction for planning failure, the planned final identity for execution/begin/finish/commit failure, and only `None` for an empty vector where no transaction identity exists. `BatchApplySuccess` always owns one actual `BatchScopeDiagnostics`; failure owns `Some(actual_diagnostics)` only when `checked_delta` succeeded. Export all six renderer types. `BatchApplyOutcome` contains only product facts; identity/scope observations never enter it. Do not let the shell infer either product facts or scope counts.

- [ ] **Step 4: Refactor state planning so a staged copy can retain owned patches**

Derive `Clone` only for copyable `RemoteIdentity`/`RemoteUpdateState`; retain pixel buffers exclusively in owned planned operations. Add `format: PixelFormat` to `RemoteIdentity`. Plan `FrameTransaction::Startup { reset, revision, .. }` by validating reset and first FullBaseline revision against one staged state before making a private candidate; plan `FrameTransaction::Revision { revision, .. }` against that staged active identity. Use borrowed metadata commit:

```rust
fn commit_metadata(&mut self, plan: &PlannedUpdate) {
    match &plan.data {
        PlannedUpdateData::StartupReset { session_id, generation, size, format } => {
            self.current = Some(RemoteIdentity {
                session_id: *session_id,
                generation: *generation,
                size: *size,
                format: *format,
                last_damage_revision: 0,
                last_boundary_revision: 0,
            });
            self.pending_receipt = None;
            self.unpresented_full_baseline = false;
            self.baseline_presented = false;
            self.recovery = None;
        }
        PlannedUpdateData::Damage { revision, .. } => {
            let current = self
                .current
                .as_mut()
                .expect("damage plan requires reset state");
            current.last_damage_revision = *revision;
            self.pending_receipt = None;
        }
        PlannedUpdateData::Boundary(receipt) => {
            let current = self
                .current
                .as_mut()
                .expect("boundary plan requires reset state");
            current.last_boundary_revision = receipt.revision;
            if receipt.completeness == FrameCompleteness::FullBaseline {
                self.unpresented_full_baseline = true;
            }
            self.pending_receipt = Some(*receipt);
        }
    }
}
```

The Startup reset metadata, its first Damage, and its FullBaseline boundary enter one `PlannedBatch`; they cannot be committed independently. Keep the temporary serial helper isolated until Task 5 deletes it. No pixel buffer is cloned into staged state.

- [ ] **Step 5: Plan every transaction before any GPU write**

Add the exact private aggregate:

```rust
struct PlannedBatch {
    identity: FrameBatchIdentity,
    staged_state: RemoteUpdateState,
    operations: Vec<PlannedUpdate>,
    installed_surface: Option<InstalledSurface>,
    uploaded_rectangles: usize,
    had_texture_writes: bool,
    final_boundary: Option<PresentationReceipt>,
}
```

Define private `BatchPlanningFailure { identity: Option<FrameBatchIdentity>, error: RendererError }`. `RemoteUpdateState::plan_batch(Vec<FrameTransaction>) -> Result<PlannedBatch, BatchPlanningFailure>` clones only metadata, validates every transaction before scope begin, and retains FIFO operations. Before validating each transaction, derive its stable identity from its Reset/Revision metadata so any planning error names the offending identity. Startup contributes private candidate allocation, ordered patches, FullBaseline receipt, and `installed_surface`; each Revision contributes ordered patches and one boundary. Rectangle accumulation uses checked arithmetic. `PlannedBatch.identity` is the final transaction identity, and only the final current-generation receipt remains in `final_boundary`; startup allocation is never public before clean commit. The process-local `earliest_constituent_enqueue_at` is ignored by renderer planning and never changes rendering behavior.

Define a pure executor boundary:

```rust
trait PlannedOperationExecutor {
    type Resource;
    fn allocate(&mut self, reset: &FrameReset) -> Result<Self::Resource, RendererError>;
    fn write_patch(
        &mut self,
        resource: &Self::Resource,
        revision: u64,
        patch_index: usize,
        patch: &PixelPatch,
        upload: UploadDescriptor,
    ) -> Result<(), RendererError>;
}
```

The production implementation calls `create_remote_texture`/`queue.write_texture`; the test recorder copies rows/bytes into a tiny buffer. `execute_planned_operations` iterates transaction, patch-vector, row, and byte order exactly once and never sorts or parallelizes overlapping writes.

- [ ] **Step 6: Run execution and finish with exactly-once observation and GPU precedence**

Use one private backend seam. Production delegates to the real `GpuContext`/`GpuFaultScope`; deterministic error tests use `RecordingBatchScopeBackend`, whose fake scope drives the same `ObservedScopeLifecycle` from Task 2:

```rust
trait BatchScopeBackend {
    type Scope;
    type CleanToken;

    fn observation(&self) -> GpuScopeObservation;
    fn begin(&self) -> Result<Self::Scope, GpuFaultClass>;
    fn finish(&self, scope: Self::Scope) -> Result<Self::CleanToken, GpuFaultClass>;
}

struct ScopedExecution<T, R> {
    clean_token: T,
    prepared: R,
    scope: BatchScopeDiagnostics,
}

fn execute_with_observed_scope<B, R>(
    backend: &B,
    identity: FrameBatchIdentity,
    execute: impl FnOnce() -> Result<R, RendererError>,
) -> Result<ScopedExecution<B::CleanToken, R>, BatchApplyFailure>
where
    B: BatchScopeBackend;
```

`GpuContextBatchScopeBackend::begin` calls the real `GpuContext::begin_fault_scope`; its `finish` calls the owned `GpuFaultScope::finish`. `RecordingBatchScopeBackend` can inject begin/finish results but cannot manufacture counts: its begin/finish/poll observations are recorded by the Task 2 lifecycle seam. Implement the public method with one non-empty scope:

```rust
pub fn apply_update_batch(
    &mut self,
    transactions: Vec<FrameTransaction>,
) -> Result<BatchApplySuccess, BatchApplyFailure> {
    if transactions.is_empty() {
        return Err(BatchApplyFailure::planning(None, RendererError::EmptyBatch));
    }
    let planned = self
        .state
        .plan_batch(transactions)
        .map_err(|failure| {
            BatchApplyFailure::planning(failure.identity, failure.error)
        })?;
    self.run_planned_batch(planned)
}
```

Planning errors return `BatchApplyFailure` with `scope=None`. `run_planned_batch` constructs its infallible executor, then calls `execute_with_observed_scope(&GpuContextBatchScopeBackend::new(&self.context), planned.identity, execute)`; only after that helper returns clean success may it call `commit_clean_batch`. After `begin` succeeds inside the helper, do not use `?`, `return`, or commit until finish resolves. Use this normative helper body:

```rust
let mut executor = WgpuPlannedExecutor::new(
    &self.context,
    &self.bind_group_layout,
    &self.sampler,
    self.remote.as_ref(),
);
let scoped = execute_with_observed_scope(
    &GpuContextBatchScopeBackend::new(&self.context),
    planned.identity,
    || execute_planned_operations(&mut executor, &planned.operations),
)?;
self.commit_clean_batch(
    scoped.clean_token,
    planned,
    scoped.prepared,
    scoped.scope.observation,
)
```

The `?` above is after the helper has finished every successfully begun scope. Its helper body is:

```rust
let before = backend.observation();
let scope = match backend.begin() {
    Ok(scope) => scope,
    Err(fault) => return Err(BatchApplyFailure::begin(Some(identity), fault)),
};
let execution = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    execute()
}))
.unwrap_or(Err(RendererError::BatchExecutionPanicked));
let finish = backend.finish(scope);
let observation = backend.observation().checked_delta(before);

match (finish, execution) {
    (Err(gpu), execution) => Err(BatchApplyFailure {
        identity: Some(identity),
        primary: RendererError::GpuFault(gpu),
        secondary_execution: execution.err(),
        scope: observation.map(|observation| BatchScopeDiagnostics {
            observation,
            observed_fault: Some(gpu),
        }),
    }),
    (Ok(_), _) if observation.is_none() => {
        Err(BatchApplyFailure::counter_regressed(Some(identity)))
    }
    (Ok(_), Err(execution)) => Err(BatchApplyFailure {
        identity: Some(identity),
        primary: execution,
        secondary_execution: None,
        scope: Some(BatchScopeDiagnostics {
            observation: observation.expect("checked above"),
            observed_fault: None,
        }),
    }),
    (Ok(clean_token), Ok(prepared)) => Ok(ScopedExecution {
        clean_token,
        prepared,
        scope: BatchScopeDiagnostics {
            observation: observation.expect("checked above"),
            observed_fault: None,
        },
    }),
}
```

Construct the infallible executor before calling the helper. After begin, the fallible/panicking execution body is captured as a value and no branch crosses backend finish. The production finish owns the only real poll. The explicit match gives a finish GPU fault precedence over execution error and retains only its typed secondary `RendererError`; no free-form panic/error text is kept. `failure.scope=None` means no actual delta is available and invalidates that metrics run; it is never replaced with zero or one. Actual observations come from Task 2 counters at the real sites, never from constants. The two injected error tests call this generic helper with the recording backend, so they do not require or silently skip a GPU adapter.

- [ ] **Step 7: Commit CPU state/resources only behind the clean token**

Use one resource carrier and pure clean-commit helper:

```rust
struct PreparedBatchResources<R> {
    final_startup: Option<R>,
    superseded: Vec<R>,
}

fn commit_planned_batch_after_gpu<R>(
    state: &mut RemoteUpdateState,
    resource: &mut Option<R>,
    planned: PlannedBatch,
    prepared: PreparedBatchResources<R>,
) -> (BatchApplyOutcome, Vec<R>);
```

Call it only inside one `GpuContext::commit_if_unchanged` after clean finish. It atomically installs staged state and the private startup candidate, returns the four product facts, and returns old/superseded handles for drop after the observer lock. If commit detects a later GPU epoch/fault, return GPU-primary `BatchApplyFailure` with `identity=Some(planned.identity)`, the already observed scope delta, and `observed_fault=Some(fault)`; detach later in Task 5 and leave staged state/resource unchanged. No individual-patch retry exists.

- [ ] **Step 8: Run renderer GREEN and retain temporary caller compatibility**

```powershell
cargo test -p frd-render-wgpu atomic_startup_ -- --nocapture
cargo test -p frd-render-wgpu recording_executor_ -- --nocapture
cargo test -p frd-render-wgpu execution_error_still_finishes_ -- --nocapture
cargo test -p frd-render-wgpu gpu_fault_wins_ -- --nocapture
cargo test -p frd-render-wgpu batch_receipt_ -- --nocapture
cargo test -p frd-shell-desktop fatal_renderer_error_tokens_ -- --nocapture
cargo check -p frd-shell-desktop
```

Expected: all pass; every successful begin has one observed finish/poll on both injected error paths, GPU fault precedence holds, FIFO overlap pixels match serial execution, and the temporary serial caller still builds.

- [ ] **Step 9: Commit the renderer batch API**

```powershell
git add -- crates/frd-render-wgpu/src/gpu_fault.rs crates/frd-render-wgpu/src/remote_texture.rs crates/frd-render-wgpu/src/lib.rs crates/frd-shell-desktop/src/fatal.rs
git diff --cached --check
git diff --cached
git commit -m "perf(renderer): apply frame transactions in one GPU batch"
```

---

### Task 5: Wire one compiler and one renderer batch into the desktop shell

**Files:**

- Modify/Test narrowly: `crates/frd-shell-desktop/src/application.rs`
- Modify/Test: `crates/frd-shell-desktop/src/fatal.rs`
- Modify/Test: `crates/frd-shell-desktop/src/frame_metrics.rs`
- Modify: `crates/frd-shell-desktop/src/lib.rs`
- Modify/Test: `crates/frd-render-wgpu/src/lib.rs`
- Modify/Test: `crates/frd-render-wgpu/src/remote_texture.rs`

**Interfaces:**

- Consumes: session-bound compiler from Task 3; `BatchApplySuccess`/identity-bearing `BatchApplyFailure` and actual scope diagnostics from Task 4; timestamped mailbox envelopes, crate-visible `BatchMetricContext`/identity metrics from Task 2, `PendingTextureWrites`, `InputRouter`, `RuntimeWakeGate`, `RemoteBinding`, and stable `FatalReport` output.
- Produces: one `FrameTransactionCompiler::new(session_id)` per `LiveSessionPorts`; `BatchMetricContext { batch_started_at, source_update_count, oldest_age, transaction_count }` carried by compile success/failure; direct non-empty `apply_compiled_drain -> Result<BatchApplySuccess, BatchApplyFailure>`; metrics observers that consume the full `BatchApplyFailure`; `RuntimeDrainOutcome { ui_redraw_needed, frame_redraw_needed }`; atomic startup binding install; fatal Wake/RedrawRequested no-render dispatch; no serial renderer callers.

- [ ] **Step 1: Add focused RED tests for independent redraw decisions and direct non-empty apply**

Add these exact tests:

1. `event_only_wake_requests_one_ui_redraw_and_zero_frame_redraw` feeds a latency/capability event with an empty mailbox and asserts one total redraw, `ui_redraw_needed=true`, `frame_redraw_needed=false`, and zero renderer batch calls.
2. `reset_only_and_reset_damage_without_boundary_request_no_frame_redraw` drains each partial startup across two wakes and asserts zero transactions, zero batch calls, no binding, and `frame_redraw_needed=false` both times.
3. `atomic_startup_full_baseline_installs_binding_and_requests_one_frame_redraw` supplies the matching FullBaseline boundary, asserts exactly one direct batch call, installs only `success.outcome.installed_surface`, and yields one frame redraw.
4. `one_nonempty_compiled_drain_calls_renderer_once` proves the helper accepts only a non-empty vector and returns `BatchApplySuccess` directly, with no `Option` layer.
5. `batch_metric_context_reaches_success_and_full_failure_observers` uses distinct start/source/age/transaction values; assert success receives all four plus product/scope facts, and failure receives the same context plus identity, primary, secondary, optional scope, and GPU class from the entire `BatchApplyFailure`. Success emits `CandidateBatch`; compile/renderer failures emit `StableFault`, never failed `CandidateBatch` rows.
6. `candidate_metric_row_uses_observed_scope_and_success_is_one_one_one` runs one successful batch through the recording lifecycle backend, passes its diagnostics to the observer, and asserts the one CandidateBatch row contains actual `{1,1,1}`. A second pure sink fixture supplies a deliberately nonconforming observed `{2,1,1}` and proves the row retains `2` and the comparison gate rejects it, so a constant `{1,1,1}` implementation cannot satisfy the test.

Use this test seam:

```rust
fn apply_compiled_drain(
    transactions: Vec<FrameTransaction>,
    apply: impl FnOnce(Vec<FrameTransaction>)
        -> Result<BatchApplySuccess, BatchApplyFailure>,
) -> Result<BatchApplySuccess, BatchApplyFailure> {
    debug_assert!(!transactions.is_empty());
    apply(transactions)
}
```

- [ ] **Step 2: Add RED fatal ordering and callback-integration tests**

Introduce a private test seam:

```rust
trait FrameBatchFailureTarget {
    fn block_remote_input(&mut self);
    fn detach_remote_surface(&mut self);
    fn clear_pending_texture_writes(&mut self);
}

fn terminate_failed_frame_batch(
    target: &mut impl FrameBatchFailureTarget,
    failure: FrameDrainFailure,
) -> FatalReport;
```

With a recording fake, assert the exact order `block_remote_input`, `detach_remote_surface`, `clear_pending_texture_writes`; assert the report contains only a stable compiler/renderer token and no redraw/present callback is invoked.

Add a callback seam whose production `render_now` closure is the only entry to remote record/submit/present:

```rust
enum RuntimeDrainCallback { Wake, RedrawRequested }

trait RuntimeDrainCallbackTarget {
    fn request_redraw(&mut self);
    fn render_now(&mut self);
    fn terminate(&mut self, report: FatalReport);
}

fn dispatch_runtime_drain(
    callback: RuntimeDrainCallback,
    result: Result<RuntimeDrainOutcome, FatalReport>,
    target: &mut impl RuntimeDrainCallbackTarget,
) {
    match (callback, result) {
        (_, Err(report)) => target.terminate(report),
        (RuntimeDrainCallback::Wake, Ok(outcome)) if outcome.any_redraw() => {
            target.request_redraw();
        }
        (RuntimeDrainCallback::Wake, Ok(_)) => {}
        (RuntimeDrainCallback::RedrawRequested, Ok(_)) => target.render_now(),
    }
}
```

`fatal_wake_has_zero_redraw_record_submit_present_and_receipt` and `fatal_redraw_requested_has_zero_record_submit_present_and_receipt` use a fake whose `render_now` increments separate record, queue-submit, surface-present, and FramePresented counters. Both fatal paths must increment only terminate; every other counter remains zero.

`frame_batch_longest_fixed_schema_fits_safe_detail_without_truncation` constructs the longest valid combination: primary `GpuFault(DeviceLost)` (`p=gpu`), secondary `BoundaryWithoutMatchingDamage` (`s=boundary`), session/generation/revision all `u64::MAX`, scope begins/finishes/polls all `u64::MAX`, and `g=lost`. Encode `s` as the fixed four-part `secondary:session:generation:revision` field, assert the exact six-key `p;s;b;f;o;g` string, ASCII-only content, and exact byte length `155 <= MAX_SAFE_DETAIL_BYTES`. The constructor assigns this fixed string directly; the test must not call a sanitizer, truncate, or raise the 160-byte bound to pass.

The production adapter is `ApplicationDrainCallbacks<'a> { application: &'a mut DesktopApplication, event_loop: &'a ActiveEventLoop }` with `fn new(application: &'a mut DesktopApplication, event_loop: &'a ActiveEventLoop) -> Self`. `request_redraw` calls the existing window request once, `render_now` calls the existing `render()` once, and `terminate` calls `handle_application_fatal` once. It contains no protocol or renderer logic.

- [ ] **Step 3: Run focused tests to verify RED**

```powershell
cargo test -p frd-shell-desktop event_only_wake_ -- --nocapture
cargo test -p frd-shell-desktop reset_only_and_reset_damage_ -- --nocapture
cargo test -p frd-shell-desktop atomic_startup_full_baseline_ -- --nocapture
cargo test -p frd-shell-desktop batch_metric_context_ -- --nocapture
cargo test -p frd-shell-desktop candidate_metric_row_ -- --nocapture
cargo test -p frd-shell-desktop fatal_wake_ -- --nocapture
cargo test -p frd-shell-desktop fatal_redraw_requested_ -- --nocapture
cargo test -p frd-shell-desktop frame_batch_longest_fixed_schema_ -- --nocapture
```

Expected: compile failure because the separate redraw result, direct apply seam, callback integration seam, and compact bounded batch-fatal constructor do not exist.

- [ ] **Step 4: Store the compiler in `LiveSessionPorts` and compile while the mailbox lock is released promptly**

Add:

```rust
struct LiveSessionPorts {
    session_id: SessionId,
    commands: mpsc::Sender<SessionCommand>,
    events: mpsc::Receiver<SessionEvent>,
    mailbox: Arc<Mutex<FrameMailbox>>,
    frame_compiler: FrameTransactionCompiler,
}

struct CompiledFrameDrain {
    transactions: Vec<FrameTransaction>,
    metrics: BatchMetricContext,
}

struct FrameCompileFailure {
    error: FrameTransactionError,
    metrics: BatchMetricContext,
}
```

Construct `FrameTransactionCompiler::new(session_id)` only when accepting that same live session. Dropping/replacing `LiveSessionPorts` drops its compiler and buffered pixels; never move a compiler to another session. Implement:

```rust
fn drain_frame_transactions(&mut self) -> Result<CompiledFrameDrain, FrameCompileFailure>;
```

Under the mailbox lock, move every `EnqueuedSurfaceUpdate` out and release the lock; do not compute age while holding it. Capture `batch_started_at=Instant::now()` immediately before `active.frame_compiler.compile(envelopes)`. On successful non-empty output, set `transaction_count=transactions.len()`, derive `source_update_count` from transaction structure (`Startup=3`, each steady `Revision=2`), take the minimum `FrameTransaction::earliest_constituent_enqueue_at()` across the whole batch, and compute `oldest_age=checked_mailbox_age(batch_started_at, earliest)`. If that required timestamp is missing/future/incomparable, call the metrics invalidation path with `MetricSinkError::InvalidObservation`; never emit a CandidateBatch row with an empty/zero age. This remains correct when Reset, Damage, and Boundary crossed drains because the compiler retained their minimum timestamp.

On compile failure, build `FrameCompileFailure.metrics` from the same `batch_started_at`, the current drained-envelope count plus the compiler's pre-call buffered constituent count, the minimum pre-call/current enqueue timestamp when checked, and transaction count zero; the resulting `StableFault` row invalidates the run if age is absent rather than inventing zero. Reset-only and Reset-plus-Damage drains leave owned startup state in that bound compiler and return an empty transaction vector/context; they emit no frame-work metric row. A foreign update returns typed fatal before any batch call. `BatchMetricContext` is imported from `frame_metrics.rs`; do not redeclare a structurally identical application-local type.

- [ ] **Step 5: Make compiler and renderer errors stable fatal inputs**

Define:

```rust
enum FrameDrainFailure {
    Compile(FrameTransactionError),
    Render(BatchApplyFailure),
}
```

Add `FatalReport::frame_transaction(FrameTransactionError)` and `FatalReport::frame_batch(&BatchApplyFailure)` with closed exhaustive mappings; do not format arbitrary `Debug` text. Define private typed `CompactFrameBatchDetail { primary: RendererError, secondary: Option<RendererError>, identity: Option<(u64, u64, u64)>, scope: Option<GpuScopeObservation>, gpu: Option<GpuFaultClass> }` and `encode_compact_frame_batch_detail(CompactFrameBatchDetail) -> String`. Production constructs it from the full failure using `session_id.get()`. The maximum-length test constructs this typed value directly with `u64::MAX`, because `SessionId` deliberately exposes no arbitrary raw constructor. `frame_batch` uses dedicated compact token mappers and exactly this schema:

```rust
fn compact_renderer_error_code(error: RendererError) -> &'static str {
    match error {
        RendererError::StaleUpdate => "stale",
        RendererError::InvalidGeometry => "geom",
        RendererError::TextureBudgetExceeded => "budget",
        RendererError::UnsupportedPixelFormat => "pixfmt",
        RendererError::NonMonotonicRevision => "rev",
        RendererError::BoundaryWithoutMatchingDamage => "boundary",
        RendererError::InvalidPatch => "patch",
        RendererError::ResetRequired => "reset",
        RendererError::StalePresentationReceipt => "receipt",
        RendererError::TextureDimensionUnsupported => "dim",
        RendererError::UnsupportedTargetFormat => "target",
        RendererError::GpuFault(_) => "gpu",
        RendererError::EmptyBatch => "empty",
        RendererError::BatchExecutionPanicked => "panic",
        RendererError::ScopeObservationInvalid => "scope",
    }
}

fn compact_gpu_fault_code(fault: GpuFaultClass) -> &'static str {
    match fault {
        GpuFaultClass::Validation => "v",
        GpuFaultClass::OutOfMemory => "oom",
        GpuFaultClass::Internal => "int",
        GpuFaultClass::DeviceLost => "lost",
        GpuFaultClass::ObservationIncomplete => "obs",
    }
}

let primary = compact_renderer_error_code(failure.primary);
let secondary = failure
    .secondary_execution
    .map(compact_renderer_error_code)
    .unwrap_or("n");
let (session, generation, revision) = failure
    .identity
    .map(|identity| {
        (
            identity.session_id.get().to_string(),
            identity.generation.to_string(),
            identity.revision.to_string(),
        )
    })
    .unwrap_or_else(|| ("n".to_owned(), "n".to_owned(), "n".to_owned()));
let secondary_and_identity =
    format!("{secondary}:{session}:{generation}:{revision}");
let (begins, finishes, polls) = match failure.scope {
    Some(scope) => (
        scope.observation.begins.to_string(),
        scope.observation.finishes.to_string(),
        scope.observation.polls.to_string(),
    ),
    None => ("n".to_owned(), "n".to_owned(), "n".to_owned()),
};
let gpu = failure
    .scope
    .and_then(|scope| scope.observed_fault)
    .or_else(|| match failure.primary {
        RendererError::GpuFault(fault) => Some(fault),
        _ => None,
    })
    .map(compact_gpu_fault_code)
    .unwrap_or("n");
let details = format!(
    "p={primary};s={secondary_and_identity};b={begins};f={finishes};o={polls};g={gpu}"
);
```

Use `n` for an absent secondary, identity component, scope count, or GPU class. The `s` value is always exactly `secondary:session_id:generation:revision`; every number is decimal `u64`. All other values are fixed ASCII tokens. Assign `details` directly without calling the truncating sanitizer. The longest valid encoding is exactly 155 bytes (`p=gpu;s=boundary:<20>:<20>:<20>;b=<20>;f=<20>;o=<20>;g=lost`), so the exhaustive token/maximum-number test proves every variant fits; do not increase `MAX_SAFE_DETAIL_BYTES=160` or add a runtime truncation fallback.

Use this exhaustive compiler mapping in `fatal.rs`:

```rust
fn frame_transaction_error_code(error: FrameTransactionError) -> &'static str {
    match error {
        FrameTransactionError::InvalidReset => "frame_transaction_invalid_reset",
        FrameTransactionError::ForeignSession => "frame_transaction_foreign_session",
        FrameTransactionError::StaleReset => "frame_transaction_stale_reset",
        FrameTransactionError::UpdateBeforeReset => "frame_transaction_update_before_reset",
        FrameTransactionError::StaleUpdate => "frame_transaction_stale_update",
        FrameTransactionError::DuplicateDamage => "frame_transaction_duplicate_damage",
        FrameTransactionError::RevisionWhilePending => "frame_transaction_revision_while_pending",
        FrameTransactionError::BoundaryWithoutDamage => {
            "frame_transaction_boundary_without_damage"
        }
        FrameTransactionError::BoundaryMismatch => "frame_transaction_boundary_mismatch",
        FrameTransactionError::StartupBoundaryNotFullBaseline => {
            "frame_transaction_startup_boundary_not_full_baseline"
        }
    }
}
```

`FatalReport::frame_transaction` sets `component="presentation"`, `operation="frame_transaction"`, `reason="frame_transaction_invalid"`, and `details` to one static token. `FatalReport::frame_batch` sets `component="presentation"`, `operation="frame_batch"`, `reason="frame_batch_failed"` and the six-key compact schema above. The 155-byte longest-combination test proves the complete primary/secondary/identity/scope/GPU detail fits 160 bytes without loss. `terminate_failed_frame_batch` exhaustively selects these constructors for `FrameDrainFailure::{Compile, Render}`; renderer failure metrics are written before this constructor runs. No `Debug` or free-form text is emitted.

Implement `FrameBatchFailureTarget` for `DesktopApplication`: block/release input first; call `window.renderer.detach()`, set `window.remote = None`, and clear `PendingTextureWrites`; then return the report. Add the exact bookkeeping methods below and remove `record_damage_upload` after the serial API disappears:

```rust
fn record_batch(&mut self, had_texture_writes: bool) {
    self.pending |= had_texture_writes;
}

fn clear(&mut self) {
    self.pending = false;
}
```

Do not send the failure to `transition_presentation_error`, do not request a full snapshot, and do not schedule redraw.

- [ ] **Step 6: Replace the serial loop and return independent UI/frame redraw facts**

Replace the serial measurement method with these candidate methods; they end `batch-commit` at the call time, preserve product/scope separation, always write the actual success observation, and write failure scope fields only when `failure.scope` is `Some(actual_diagnostics)`:

```rust
fn observe_batch_success(
    &mut self,
    context: BatchMetricContext,
    outcome: &BatchApplyOutcome,
    scope: &BatchScopeDiagnostics,
);
fn observe_batch_failure(
    &mut self,
    context: BatchMetricContext,
    failure: &BatchApplyFailure,
);
fn observe_compile_failure(
    &mut self,
    context: BatchMetricContext,
    error: FrameTransactionError,
);
```

Define the drain result independently:

```rust
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RuntimeDrainOutcome {
    ui_redraw_needed: bool,
    frame_redraw_needed: bool,
}

impl RuntimeDrainOutcome {
    fn any_redraw(self) -> bool {
        self.ui_redraw_needed || self.frame_redraw_needed
    }
}

fn drain_runtime(&mut self) -> Result<RuntimeDrainOutcome, FatalReport>;
```

Reduce `SessionEvent`s first and set `ui_redraw_needed` when connection stage, capabilities, latency text, diagnostics, cleanup/detach, login, or other UI model state changes. Mailbox/source-update count never sets either fact. If compiler output is empty, return without calling the renderer and keep `frame_redraw_needed=false`. For a non-empty vector call the direct helper and convert the typed error only after recording the same timing/scope boundary:

```rust
let metrics = drain.metrics;
let applied = apply_compiled_drain(drain.transactions, |transactions| {
    window.renderer.apply_update_batch(transactions)
});
let success = match applied {
    Ok(success) => success,
    Err(failure) => {
        self.frame_metrics.observe_batch_failure(metrics, &failure);
        return Err(terminate_failed_frame_batch(
            self,
            FrameDrainFailure::Render(failure),
        ));
    }
};
```

Handle `FrameCompileFailure` by first calling `observe_compile_failure(failure.metrics, failure.error)`, which emits one typed `StableFault` row with `batch_result=CompileFailure`/`batch_failure_class=Compiler`, and then `terminate_failed_frame_batch(self, FrameDrainFailure::Compile(failure.error))` before any batch call. Only after clean success, call `observe_batch_success(metrics, &success.outcome, &success.scope)`, which emits the sole `CandidateBatch` row with `batch_result=Success` and no failure class; then install `RemoteBinding` from `success.outcome.installed_surface` and record the four product facts separately. `installed_surface` can exist only for atomic Startup. `observe_batch_failure(metrics, &failure)` emits one `StableFault` row with `batch_result=RendererFailure` and derives one closed class from the entire failure: `GpuFault`/`ScopeObservationInvalid -> Gpu`, `scope=None -> RendererPlanning`, otherwise `RendererExecution`; it copies `failure.identity`, actual optional scope observation, observed GPU class, and retains the typed optional secondary for the compact fatal report. A renderer/compile failure never emits `CandidateBatch`.

Record pending writes from `success.outcome.had_texture_writes`. Set `frame_redraw_needed=true` only when `success.outcome.final_boundary` is present for the installed/current binding. All three observers use `context.batch_started_at` through the call time, preserve source-update/transaction counts and mailbox age, and take actual scope/fault facts only from `BatchApplySuccess` or the full typed failure; they never synthesize constant `1`. A successful non-empty batch with no valid `oldest_age` marks the sink `InvalidObservation` and cannot become a valid performance run. Preserve the bounded empty submit only for a clean batch blocked by presentation.

- [ ] **Step 7: Make the Wake handler request redraw exactly once or exit fatal immediately**

Use:

```rust
DesktopUserEvent::Wake => {
    self.runtime_wake_gate.consume();
    let result = self.drain_runtime();
    let mut callbacks = ApplicationDrainCallbacks::new(self, event_loop);
    dispatch_runtime_drain(
        RuntimeDrainCallback::Wake,
        result,
        &mut callbacks,
    );
    drop(callbacks);
    self.maybe_finish_exit(event_loop);
}
```

For Wake success, request exactly one redraw iff `outcome.any_redraw()`. For `WindowEvent::RedrawRequested`, dispatch the drain first and call `render_now` only on success; do not schedule a second redraw merely because that callback already owns one. Fatal dispatch calls `handle_application_fatal` and returns on the same callback stack, so no render/record/submit/present follows. Event-only UI changes still redraw; incomplete compiler input does not create a frame redraw.

- [ ] **Step 8: Migrate offline test-texture and device-recovery callers**

Replace `test_texture_updates(SessionId) -> [SurfaceUpdate; 3]` with one atomic transaction:

```rust
fn test_texture_transactions(session_id: SessionId) -> Vec<FrameTransaction> {
    vec![FrameTransaction::Startup {
        earliest_constituent_enqueue_at: Instant::now(),
        reset: FrameReset {
            session_id,
            generation: 1,
            size: PixelSize::new(2, 2).expect("test texture size is non-zero"),
            format: PixelFormat::Bgrx8UnormSrgb,
        },
        revision: FrameRevision {
            session_id,
            generation: 1,
            revision: 1,
            patches: vec![PixelPatch {
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                },
                stride_bytes: 8,
                pixels: PixelBuffer::new(vec![
                    0, 0, 255, 0,
                    0, 255, 0, 0,
                    255, 0, 0, 0,
                    255, 255, 255, 0,
                ]),
            }],
            completeness: FrameCompleteness::FullBaseline,
        },
    }]
}
```

Both initial offline display and test-only device recovery call `apply_update_batch(test_texture_transactions(session_id))`. Product batch failure remains fatal; do not infer a production full-snapshot recovery from this offline fixture.

- [ ] **Step 9: Remove the serial renderer entry point after every caller is migrated**

Delete public `ApplyOutcome` and `RemoteRenderer::apply_update`; update the renderer API boundary test to accept only `Vec<FrameTransaction> -> Result<BatchApplySuccess, BatchApplyFailure>`. Remove the Task 2-only `serial_nonempty_drain_emits_one_aggregate_row` fake/helper at this cutover instead of retaining deleted renderer types; its passing Task 2 command and captured serial CSV remain the baseline evidence consumed by Tasks 6–7. Verify mechanically:

```powershell
rg -n "ApplyOutcome|apply_update\(" crates apps src
```

Expected: no production match. `apply_update_batch` is the only remote upload entry point.

- [ ] **Step 10: Run focused GREEN**

```powershell
cargo test -p frd-shell-desktop event_only_wake_ -- --nocapture
cargo test -p frd-shell-desktop reset_only_and_reset_damage_ -- --nocapture
cargo test -p frd-shell-desktop atomic_startup_full_baseline_ -- --nocapture
cargo test -p frd-shell-desktop batch_metric_context_ -- --nocapture
cargo test -p frd-shell-desktop candidate_metric_row_ -- --nocapture
cargo test -p frd-shell-desktop fatal_wake_ -- --nocapture
cargo test -p frd-shell-desktop fatal_redraw_requested_ -- --nocapture
cargo test -p frd-shell-desktop frame_batch_longest_fixed_schema_ -- --nocapture
cargo test -p frd-shell-desktop pending_texture_write_state_tracks_actual_and_fallback_submits -- --nocapture
cargo test -p frd-render-wgpu batch_ -- --nocapture
cargo test -p frd-frame --test transactions -- --nocapture
```

Expected: all pass; UI-only redraw works, incomplete startup never calls renderer, Startup installs atomically, fatal callbacks produce zero remote record/submit/present/receipt, and no serial renderer API remains.

- [ ] **Step 11: Commit the shell cutover**

```powershell
git add -- crates/frd-shell-desktop/src/application.rs crates/frd-shell-desktop/src/fatal.rs crates/frd-shell-desktop/src/frame_metrics.rs crates/frd-shell-desktop/src/lib.rs crates/frd-render-wgpu/src/lib.rs crates/frd-render-wgpu/src/remote_texture.rs
git diff --cached --check
git diff --cached
git commit -m "perf(shell): apply one frame transaction batch per drain"
```

---

### Task 6: Run focused contracts and the complete workspace matrix

**Files:**

- Verify only; no source file is created or modified.

**Interfaces:**

- Consumes: Tasks 1–5 commits and the approved spec.
- Produces: focused core-contract evidence, citation of the recorded Task 2 `serial_nonempty_drain_emits_one_aggregate_row` pass plus the `serial_pre_batch` CSV artifact after that test/API is removed, a separately identified injected fault-contract result set excluded from performance CSVs, and every repository-mandated test/build/help result before live testing.

- [ ] **Step 1: Run the intentionally bounded focused contract suites**

```powershell
cargo test -p frd-protocol-apple recovery_full_request_replaces_failed_response_timing_and_completes_on_next_valid_type_zero -- --nocapture
cargo test -p frd-frame oldest_enqueued_at_tracks_the_retained_front_entry -- --nocapture
cargo test -p frd-frame --test transactions -- --nocapture
cargo test -p frd-frame compiler_carries_earliest_constituent_enqueue_across_drains -- --nocapture
cargo test -p frd-shell-desktop mailbox_age_returns_none_ -- --nocapture
cargo test -p frd-render-wgpu scope_lifecycle_seam_ -- --nocapture
cargo test -p frd-render-wgpu atomic_startup_ -- --nocapture
cargo test -p frd-render-wgpu recording_executor_ -- --nocapture
cargo test -p frd-render-wgpu execution_error_still_finishes_ -- --nocapture
cargo test -p frd-render-wgpu gpu_fault_wins_ -- --nocapture
cargo test -p frd-shell-desktop fatal_renderer_error_tokens_ -- --nocapture
cargo test -p frd-shell-desktop safe_metric_sink_ -- --nocapture
cargo test -p frd-shell-desktop metric_sink_configuration_distinguishes_ -- --nocapture
cargo test -p frd-shell-desktop metric_sink_configuration_rejects_ -- --nocapture
cargo test -p frd-shell-desktop partial_metric_configuration_ -- --nocapture
cargo test -p frd-shell-desktop invalid_metric_configuration_ -- --nocapture
cargo test -p frd-shell-desktop event_only_wake_ -- --nocapture
cargo test -p frd-shell-desktop reset_only_and_reset_damage_ -- --nocapture
cargo test -p frd-shell-desktop atomic_startup_full_baseline_ -- --nocapture
cargo test -p frd-shell-desktop batch_metric_context_ -- --nocapture
cargo test -p frd-shell-desktop candidate_metric_row_ -- --nocapture
cargo test -p frd-shell-desktop fatal_wake_ -- --nocapture
cargo test -p frd-shell-desktop fatal_redraw_requested_ -- --nocapture
cargo test -p frd-shell-desktop frame_batch_longest_fixed_schema_ -- --nocapture
cargo test -p frd-shell-desktop input_to_next_present_ -- --nocapture
cargo test -p frd-shell-desktop frame_response_metric_uses_sample_ -- --nocapture
pwsh -NoProfile -File .\tools\compare-frame-metrics.ps1 -SelfTest
```

Run the real hardware smoke separately so its environment classification cannot hide deterministic contract failures:

```powershell
cargo test -p frd-render-wgpu dx12_scope_observation_smoke_reports_real_begin_finish_poll -- --nocapture
```

Expected: every currently present deterministic command passes. Cite, rather than rerun, the recorded Task 2 `serial_nonempty_drain_emits_one_aggregate_row` and `serial_drain_age_uses_earliest_envelope_after_unlock` passing results, then verify the retained serial CSV has one `SerialDrain` row per non-empty drain. The current protocol-neutral age contracts are `mailbox_age_returns_none_when_observation_precedes_enqueue_time`, `batch_metric_context_reaches_success_and_full_failure_observers`, and `compiler_carries_earliest_constituent_enqueue_across_drains`; do not recreate a deleted serial renderer seam. `ApplyOutcome` and its test fake no longer exist. The hardware smoke passes with actual `{1,1,1}` or reports only `SKIP adapter_unavailable`; any other outcome fails. Record the four injected execution/GPU/fatal test names and exit results as the separate fault-contract evidence consumed by Task 7; they are never merged into performance CSVs. Do not add duplicate decoder, visual snapshot, or synthetic wire tests.

- [ ] **Step 2: Run the complete pinned-toolchain workspace test/build/help matrix**

```powershell
cargo fmt -- --check
cargo test --workspace
cargo test --workspace --no-default-features
cargo build --workspace --release
cargo build --workspace --no-default-features
cargo run -- --help
cargo run -- hpssview --help
```

Expected: all seven commands pass under repository-pinned Rust 1.96.0. Record each command and exit result separately; no command substitutes for another.

- [ ] **Step 3: Build the release client and verify the upload boundary**

```powershell
cargo build --release -p freeremotedesk-windows
rg -n "\bApplyOutcome\b|\.apply_update\(" crates apps src
rg -n "apply_update_batch" crates/frd-render-wgpu crates/frd-shell-desktop
git diff --check
git status --short
```

Expected: release build succeeds; the serial upload API search is empty; batch usage exists only in renderer/shell and test seams; no uncommitted source change remains.

- [ ] **Step 4: Perform a fresh-eyes invariant audit**

Read the final `transaction.rs`, renderer batch function, and shell drain/fatal path. Confirm:

1. malformed Apple MVS recovery keeps only the successful replacement request timing and completes it only on the next valid published response; fixed-schema `frame_response_ms` uses its raw `sample_ms`, never UI `smoothed_ms`;
2. the session-bound compiler rejects foreign updates/stale reset and a session change constructs a new compiler;
3. Reset-only/Reset-plus-Damage remain buffered with no renderer call, upload, binding, frame redraw, record, submit, or present;
4. Startup becomes executable only as Reset plus matching FullBaseline; Incremental-first is the exact typed fatal; binding installs only in the atomic clean batch;
5. every accepted transaction, patch, row, byte, padded-stride crop, and overlapping write remains FIFO-equivalent to serial;
6. clean commit returns all four product facts atomically, separate from actual scope diagnostics;
7. deterministic lifecycle seam and real DX12 smoke (unless adapter unavailable) observe the real begin/finish/poll sites, and both error paths finish/poll exactly once with GPU fault precedence;
8. each successful CandidateBatch row contains observed `{1,1,1}`, constants fail the fakeable seam, and whole-run/window aggregate sums equal successful non-empty candidate batch count;
9. compiler/renderer fatal cannot reach remote record, queue submit, surface present, or `FramePresented`, does not invoke full-snapshot recovery, and its complete six-key detail fits 160 bytes without truncation;
10. serial emits one `SerialDrain` row per non-empty drain, candidate emits one CandidateBatch per successful non-empty batch, and age uses the earliest constituent enqueue retained across drains with checked subtraction;
11. all-absent/all-present/partial-or-invalid metrics configuration maps to disabled/enabled/stable pre-session fatal exactly, including unsafe output rejection;
12. event-only UI changes request one UI redraw while Reset/incomplete frame input requests no frame redraw;
13. identity input probes clear on reset/detach/close/fatal/generation/session changes and only same-session/same-generation presentation completes input-to-next-present;
14. fixed serial/candidate performance runs are fault-free through restore/normal close, and fault-run rows/tests are labeled and excluded from latency/CPU/memory aggregates.

Expected: all fourteen are demonstrable from exact call paths and recorded tests. Any violation returns to the owning task before live testing.

---

### Task 7: Correct the fixed-capture comparator gates

**Files:**

- Modify: `tools/compare-frame-metrics.ps1`
- Modify: this plan

**Inputs and result:**

- Consume only the retained valid fixed-schema captures `serial_capacity_click_20260831_23` and `candidate_capacity_click_20260831_23`. Each has five phases, complete measured-phase S0..S30 process samples, and zero `StableFault` rows.
- Produce a JSON comparison only when every mandatory predicate passes. A false predicate, incomplete applicable visible window, invalid schema/identity/order, or missing required process sample fails closed and leaves no output file.

- [ ] **Step 1: Keep visible measurements complete and make static minimized measurements explicit N/A**

For `VisibleMeasurement`, retain every complete `[t,t+5)` window requirement for Batch CPU, mailbox age, scope begins/finishes/polls, Presentation, InputToNextPresent, and FrameResponse. Preserve earliest-tie worst-window reporting, the visible InputToNextPresent and FrameResponse predicates, and the existing process CPU, maximum working-set, and working-set trend gates.

For `MinimizedMeasurement`, require the complete S0..S30 process samples and retain its CPU, maximum-working-set, and trend gates. `InputToNextPresent`, Batch CPU, mailbox age, per-window scope sums, Presentation windows, and FrameResponse p95 are JSON `null`/N/A for the static workload: no synthetic zero-latency samples and no favorable-window selection. Report observed Batch and FrameResponse activity counts/totals. Require the observed minimized Presentation count to be exactly zero for serial and candidate; any minimized Presentation is a mandatory paused-compositor failure. Restore is separate and requires an identity-bearing Presentation from both serial and candidate.

- [ ] **Step 2: Apply the approved visible batch-CPU and scope-amplification predicates**

Every observed `CandidateBatch` remains `Success` with exact `{scope_begins, scope_finishes, scope_polls}={1,1,1}`; whole-run candidate begins=finishes=polls=batch count remains mandatory. Replace the former 50-percent batch-CPU claim with the truthfully named predicate `visible_batch_cpu_8ms_and_no_regression`:

```text
candidate_visible_batch_cpu_worst_p95_us <= 8000

candidate_visible_batch_cpu_worst_p95_us
    <= max(ceil(serial_visible_batch_cpu_worst_p95_us * 110 / 100),
           serial_visible_batch_cpu_worst_p95_us + 500)
```

Add the mandatory, truthfully named `visible_scope_amplification_reduced_50_percent` predicate. Both visible source-update totals must be nonzero and the comparison must use exact decimal/cross-multiplied arithmetic:

```text
candidate_visible_scope_polls_total / candidate_visible_source_updates_total
    <= 0.5 *
       (serial_visible_scope_polls_total / serial_visible_source_updates_total)
```

- [ ] **Step 3: Test and compare the retained captures**

Before behavior changes, extend `Invoke-SelfTest` with real fixtures proving that idle minimized events remain statistically valid and N/A, a minimized Presentation is rejected, and the two approved visible predicates have their boundary behavior. Run `./tools/compare-frame-metrics.ps1 -SelfTest` RED, then make the minimal change and rerun it GREEN.

Run the comparator against the two retained CSV pairs with a new `target/validation` output path. Inspect that JSON to confirm all named mandatory predicates are true, visible metrics retain their worst complete windows, minimized latency/window fields are `null`, minimized Presentation counts are zero, and both Restore receipts carry identity. Do not alter retained CSV files, connect to the Mac, or infer live interoperability beyond those bounded captures.

- [ ] **Step 4: Check and commit only the correction**

```powershell
git diff --check
git status --short
git add -- tools/compare-frame-metrics.ps1 docs/superpowers/plans/2026-08-30-frame-transaction-render-latency.md
git diff --cached --check
git commit -m "fix: correct frame metrics gate"
```

Expected: only the comparator and this plan are tracked changes. The generated comparison JSON and the Task 7 implementation report remain untracked/ignored. Do not merge or push.
