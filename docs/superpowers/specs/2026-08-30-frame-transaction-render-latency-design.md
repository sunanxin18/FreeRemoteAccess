# Frame Transaction and Render Latency Design

## Status

Approved in conversation on 2026-08-30 and corrected after independent
transaction, GPU-fault, and measurement review. This is the first implementation
gate for the Apple dual-mode and floating-control work. The floating control
island must not start until the performance acceptance here passes or a newly
measured bottleneck is explicitly re-scoped.

## Goal

Remove confirmed local synchronization amplification while preserving exact
frame order, startup atomicity, renderer fault observation, input ownership, and
bounded resource behavior. Server response time, local batch time, and
input-to-present time remain separately named measurements.

## Non-Goals

- Do not change ARD 3.10 bytes, MVS grammar, RDP/RFB semantics, authentication,
  or server request pacing.
- Do not drop revisions, merge overlapping writes out of order, or reduce frame
  requests merely to make the UI appear faster.
- Do not activate an empty texture from `Reset` before its first complete
  baseline.
- Do not add speculative transfer merging or GPU decode before post-fix profiles
  identify the remaining dominant cost.
- Do not add exhaustive timing permutations; tests remain focused, followed by
  the repository-mandated workspace matrix.

## Evidence and Root Causes

### `FrameResponseTiming` recovery correlation defect

`FrameResponseTiming` measures a successful framebuffer-request write through
complete decode, canonical-surface commit, and surface publication. A malformed
MVS response can synchronously send a replacement full request; that successful
write replaces the failed response's timing slot. The outer handler currently
sees neither full nor partial application and discards the replacement slot, so
the recovery response can never produce its timing event.

Timing ownership follows the request:

- a successful recovery full write replaces the failed response timestamp with
  its own generation-bound timestamp;
- `RecoveryRequested` preserves that new slot;
- ignored, stale, table-only, rejected, or unpublished input discards only an
  old slot when no replacement request was written;
- reset, failed write, detach, close, and new session clear timing state;
- only a valid published type-0 or non-empty type-1 response completes a sample;
  the malformed response emits none.

This correction remains in `frd-protocol-apple` and changes no wire recovery.

### Local synchronization amplification

The mailbox preserves publication order, but the shell currently calls
`RemoteRenderer::apply_update` separately for `Damage` and its matching
`FrameBoundary`. Each call creates and observes its own GPU fault scope. One
revision therefore pays independent validation/poll costs for pixel upload and
the zero-byte boundary, and a drain containing several revisions multiplies the
work. Separate operations can also request an intermediate redraw before the
revision is presentable.

The event loop itself is wait-based and type-1 already publishes dirty patches.
No evidence justifies blaming the network, dropping mailbox entries, or moving
decode to GPU first.

## `FrameTransactionCompiler` Contract

### One compiler owns one session

Construct `FrameTransactionCompiler` with exactly one `SessionId`. Every
`SurfaceUpdate` accepted by that compiler must carry that identity. A foreign
`SessionId` is a fatal structural rejection even if its generation or revision
looks valid. Switching sessions destroys the old compiler and constructs a new
one; a compiler never rebinds itself.

Within its bound session, a legal newer-generation `Reset` atomically discards
any pending older-generation startup/revision and starts a new pending startup.
A stale reset is rejected.

### Startup is `Reset + first FullBaseline` atomically

`Reset` is not independently executable. The compiler buffers:

1. one current-session `Reset` with generation, geometry, and pixel format;
2. the following matching `Damage`, including its ordered patch vector;
3. the matching first `FrameBoundary`.

Only if that first boundary is `FullBaseline` does the compiler emit one atomic
startup transaction containing `Reset + Revision`. The candidate texture,
initial damage, full-baseline receipt, and public installed binding therefore
become visible in one clean renderer commit.

A reset-only drain or reset plus damage without its boundary remains buffered.
It installs no renderer binding, allocates/uploads no public candidate texture,
presents nothing, and creates no frame-driven redraw request. UI events from the
same drain remain independently redrawable under the shell rule below.

If the first matching boundary is `Incremental`, the compiler fatally rejects
the startup. It cannot reinterpret it as a baseline or ask the renderer to show
an incomplete texture.

### Steady-state revision shape

After startup, each `Revision` contains exactly one `Damage` for the bound
`(session_id, generation, revision)` followed by exactly one matching
`FrameBoundary`. `Damage::patches` may contain any validated number of patches.
The compiler retains an incomplete revision until its boundary arrives; it does
not upload early or join it to another revision.

The compiler rejects:

- boundary without its matching damage;
- a second damage or a new revision before the pending one closes;
- damage after a boundary for the same revision;
- stale reset/damage/boundary;
- any foreign-session update;
- first-revision completeness other than `FullBaseline` after reset.

No sorting, deduplication, last-write-wins dropping, or cross-revision merging is
allowed.

### Exact FIFO and overlap semantics

The compiled executor preserves, byte-for-byte semantically:

1. transaction order from mailbox FIFO;
2. startup reset before its first revision;
3. revision order;
4. patch-vector order inside each damage;
5. row and byte order inside each patch;
6. overlap overwrite order exactly as published.

Parallel planning or transfer preparation may not reorder texture writes. A
later overlapping patch must observe the same final pixels as serial execution.
Any future transfer compiler must prove this equivalence before replacing the
ordered executor.

## `RemoteRenderer::apply_update_batch`

### Staged clean commit

`apply_update_batch` plans a non-empty compiled batch against staged
`RemoteUpdateState` in FIFO order. It validates all identities, generations,
revisions, geometry, bounds, strides, lengths, completeness, and receipts before
committed renderer state changes.

For an atomic startup transaction, texture allocation and initial damage happen
on a private candidate. The current public remote binding is not replaced until
the complete startup batch commits cleanly. For steady-state transactions,
writes execute in exact FIFO/patch/overlap order.

On clean commit, `BatchApplyOutcome` returns these four product facts atomically:

- `installed_surface: Option<InstalledSurface>` for a newly committed startup;
- `uploaded_rectangles: usize`;
- `had_texture_writes: bool`;
- `final_boundary: Option<PresentationReceipt>` for the greatest accepted
  current-generation boundary.

Concrete names may differ, but the shell cannot infer any fact from input
updates or constants. A reset-only/incomplete compiler state never calls
`apply_update_batch`, so it can never produce `installed_surface`.

Every accepted boundary advances staged FIFO state. Only the final current
generation receipt remains pending after the batch, exactly as in serial
execution. A successful swapchain submit/present confirms that receipt; no
intermediate or startup receipt is fabricated as presented. A FullBaseline
retains its input-gate meaning, and `FramePresented` never exists before real
submit and present.

### Scope lifecycle and error precedence

After `begin_fault_scope` succeeds, every control path must call `finish` exactly
once, regardless of planning/execution success or error. Use an owned guard or
equivalent finally-style structure so `?`, early return, panic conversion, or
secondary error cannot leave a scope unobserved.

The sequence is normative:

1. begin scope and record the actual begin observation;
2. execute the staged allocation/upload operation, retaining its result;
3. finish scope unconditionally and perform the actual device poll/observation;
4. if finish observes a GPU fault, return that GPU fault even when execution
   also failed; retain only a sanitized secondary execution diagnostic;
5. otherwise return the execution error, or commit and return the clean
   `BatchApplyOutcome`.

GPU observation has precedence because writes may already be partially visible
to the texture. On any structural/execution/GPU failure, block remote input,
detach the remote texture and binding, publish visible fatal detail, and prohibit
record, submit, or present. There is no non-fatal reset/full-snapshot recovery
contract in this design.

### Actual scope diagnostics

`BatchApplyOutcome` keeps its four product facts. Actual scope statistics are a
separate diagnostics value returned alongside/embedded on success and included
in the typed batch error on failure, for example:

- scope begins observed;
- scope finishes observed;
- device polls actually performed;
- observed GPU fault class, if any.

Values must come from instrumentation at the real begin/finish/poll sites. The
renderer and shell must not report hard-coded `1`, derive counts from batch
non-emptiness, or treat requested operations as completed observations.

When presentation is blocked by minimization, occlusion, or DPI transition
after a clean batch, `had_texture_writes` drives the existing bounded empty
submit. Failed batches never use that path because their texture is detached.

## Shell Redraw Contract

Runtime drain produces two independent decisions:

- `ui_redraw_needed`: connection stage, capability, latency text, diagnostics,
  cleanup/fatal, login, or other UI model state changed;
- `frame_redraw_needed`: a clean `BatchApplyOutcome` contains a presentable
  final boundary for the installed/current binding.

Event-only state changes still request redraw. Frame mailbox non-emptiness does
not. Reset-only, reset-plus-damage without boundary, or any other compiler-
buffered incomplete input requests no frame redraw, binding installation,
record, submit, or present.

The shell requests at most one redraw for each drain after OR-ing the two
decisions. UI redraw may render status without a new frame receipt. A frame
redraw can arise only from a clean presentable batch. `RuntimeWakeGate`, mailbox
capacity, overflow policy, and protocol producer behavior remain unchanged.

## Measurement Schema and Safe Sink

### Dedicated fixed schema

Metrics go only to a dedicated safe-schema performance sink. The schema has
fixed typed fields for run id, implementation (`serial` or `candidate`), phase,
monotonic timestamp, session id, generation, revision, transaction/rectangle
counts, batch CPU duration, mailbox age, actual scope begin/finish/poll counts,
process CPU delta, working set, response timing, and presentation timing.

Row granularity is fixed. Candidate emits exactly one `CandidateBatch` aggregate
row for every successful non-empty compiled batch. Serial emits exactly one
`SerialDrain` aggregate row for every non-empty mailbox drain, rather than one
row per `SurfaceUpdate`; this is the serial work unit corresponding to one
candidate aggregate row. Empty drains and compiler-buffered incomplete startup
emit no frame-work row.

Mailbox age is calculated from the monotonic enqueue timestamp carried by each
mailbox update envelope. A row uses `apply_or_drain_started_at -
earliest_constituent_enqueue_at`; wake time, decode completion, event-drain time,
and metrics-write time are not substitutes. A missing, future, or different
clock-domain enqueue timestamp invalidates the measurement run; it is never
reported as age zero.

It records no credentials, endpoint secrets, pixel bytes, clipboard/audio
content, raw protocol payload, free-form error text, or unbounded per-frame log.
Stable enum/error codes are allowed. The same sink and schema collect serial and
candidate runs.

The environment configuration is all-or-nothing. The fixed required set is
metrics output path, run id, and implementation label (`serial` or `candidate`):

- all required variables absent disables metrics cleanly;
- all present and valid enables the sink;
- any partial set, empty/invalid value, unknown implementation label, or unsafe
  output target returns the stable `InvalidConfiguration` result.

The application never guesses a missing field, silently disables a partial
configuration, or changes product behavior because metrics are disabled.

Fatal batch reporting uses a separate fixed compact safe schema containing only
stable component/operation/reason codes, session/generation/revision identity,
and actual scope deltas. Every possible encoded variant must fit
`MAX_SAFE_DETAIL_BYTES` in full. Runtime truncation is forbidden because it can
remove the differentiating code or produce invalid schema; free-form source
errors belong only in non-user-facing bounded diagnostics and never in the safe
detail.

### Timing names

- **frame-response:** fixed metric field `frame_response_ms` is the raw
  `FrameResponseTiming::sample_ms` from successful request write to valid
  decode, canonical commit, and surface publication; it is identity-tagged by
  session/generation. `FrameResponseTiming::smoothed_ms` remains UI-only and
  must never feed a performance row's worst-window p95 or any acceptance gate.
- **batch-commit:** compiler batch entry to clean `BatchApplyOutcome` or typed
  failure.
- **input-to-next-present:** an accepted, routed input for `(session,
  generation)` to the next successfully confirmed presentation for that same
  identity.

`input-to-next-present` never claims that the input caused the frame or that the
changed pixels were visible. Reset, detach, close, fatal, generation change, or
new session clears every outstanding input probe before another measurement.
Foreign/stale presentation cannot complete it.

The stronger name **input-to-visible** is allowed only in a controlled trial
where one isolated input has a human- or capture-verifiable visible effect and
the next observed presentation contains that correlated change. If correlation
cannot be proved, record the result as inconclusive, not input-to-visible.

### Fixed phases and worst-window aggregation

Serial and candidate runs use the same machine, target, build profile, workload,
sampling interval, and this fixed sequence:

1. 5-second visible warm-up, excluded;
2. 30-second visible remote-update phase;
3. 5-second minimized warm-up, excluded;
4. 30-second minimized phase while protocol updates continue;
5. restore and verify the latest presentable frame;
6. perform a normal session close and bounded cleanup.

Both serial and candidate comparison runs must remain fault-free through restore
and normal close. An injected structural/GPU fatal is never part of the latency,
CPU, or memory comparison. Fatal no-present evidence comes from the deterministic
Task 4/5 focused fault tests or from a separately labeled bounded fault run whose
rows are excluded from every serial/candidate performance aggregate.

Sample process CPU and working set at each phase boundary and every 1 second
inside each measured phase. Performance events retain monotonic timestamps.
For each 30-second phase, form every complete 5-second half-open window
`[t, t+5s)` starting at each integer second from 0 through 25.

Number boundary samples `S0` through `S30` at seconds 0 through 30. CPU window
delta uses the process-CPU counters at `St` and `S(t+5)`. Phase maximum working
set includes `S0..S30`; memory-trend first five are `S1..S5` and last five are
`S26..S30`.

Report exact worst-window values:

- batch/mailbox/input timing: highest per-window p95; ties choose earliest
  window;
- scope begins/finishes/polls and frame cadence: highest per-window sum;
- CPU: largest endpoint process-CPU delta in any complete window;
- working set: maximum sample in the phase;
- memory trend: median of the first five 1-second samples and median of the last
  five 1-second samples.

Do not average away the worst window or select a favorable interval after the
run.

## Performance Gates

The candidate passes only if the like-for-like serial/candidate comparison
satisfies all of the following:

1. every successful non-empty `CandidateBatch` row reports the actual delta
   `scope_begins = scope_finishes = scope_polls = 1`; across the run, each of the
   three aggregate sums exactly equals the successful non-empty candidate batch
   count. Error paths separately prove every begin has a finish;
2. visible local batch-commit worst-window p95 is at most 8 ms and at least 50
   percent below serial under the same workload;
3. for each visible and minimized phase independently:
   `candidate CPU worst-window delta <= max(serial delta * 1.10,
   serial delta + 0.5 seconds)`;
4. visible candidate maximum working set is at most visible serial maximum plus
   64 MiB, and minimized candidate maximum is at most minimized serial maximum
   plus 64 MiB;
5. in each phase, candidate median working set of the last five samples is at
   most candidate median of the first five samples plus 16 MiB;
6. `input-to-next-present` improves at least 50 percent in a controlled
   like-for-like probe without worsening frame-response timing; any
   input-to-visible claim separately meets the visible-correlation rule;
7. restore after the minimized phase shows the latest correct frame with exact
   color, revision/generation order, and working input;
8. deterministic Task 4/5 focused fault integration, or a separately excluded
   bounded fault run, records actual scope finish/poll but proves zero remote
   record, queue submit, surface present, and `FramePresented` after fatal.

If a controlled input correlation or environmental comparison is inconclusive,
report it as such. Do not convert it into a pass. Deterministic scope lifecycle,
startup atomicity, fault no-present, CPU, and memory thresholds remain required.

## Deferred Evidence-Ordered Optimizations

Only after the gates above may one deeper optimization be selected:

1. **Transfer compiler:** merge commands only with proof of exact FIFO and
   overlap equivalence.
2. **Direct type-1 BGRX output:** remove a second packing pass only if profiles
   show it dominates the Apple network thread; canonical transactional decoder
   state remains intact.
3. **WGPU compute decode:** move bounded MVS IDCT/color work only if post-batch
   profiles prove decode dominates and ARD fixtures retain a bit-exact CPU
   reference. This is not a generic hardware video decoder.

These are alternatives, not a mandatory feature list.

## Failure Behavior

- Timing-correlation failure suppresses that sample; it never invents a value or
  changes wire recovery.
- Foreign session, stale/malformed transaction, or incremental-first startup
  blocks input, detaches remote state, and terminates presentation visibly.
- Execution/GPU fault always finishes the begun scope; observed GPU fault wins
  over the execution error.
- A possibly partially written texture is detached and never recorded,
  submitted, or presented.
- Failure to meet the measurement gate blocks control-island implementation and
  never authorizes frame dropping.
- Fatal exit prints stable error content and does not wait for launch or cleanup
  before returning the visible failure.

## Protocol and Client-Platform Isolation

- `frd-frame` owns the session-bound compiler and transaction shapes.
- `frd-render-wgpu` owns exact FIFO execution, staged resources, outcomes,
  receipts, scope lifecycle, and actual diagnostics.
- `frd-shell-desktop` owns compiler lifetime per session, event/frame redraw
  decisions, fatal detach/no-present integration, and the safe metrics sink.
- `frd-protocol-apple` owns only timing-correlation repair and Apple surface
  publication; RDP and future RFB retain the same generic frame port.
- no platform handle, protocol identifier, or decoder type enters the generic
  renderer contract.

## Focused Verification

Focused automated coverage is bounded to core contracts:

1. malformed MVS response -> successful recovery request -> valid response
   produces timing only for the replacement request;
2. compiler rejects foreign sessions and stale resets, and session change uses a
   new compiler;
3. reset-only and reset-plus-damage-without-boundary install/upload/present
   nothing and request no frame redraw;
4. matching Reset + Damage + first FullBaseline emits one atomic startup;
   Incremental-first fatally rejects;
5. steady revisions preserve exact transaction, patch, row/byte, and overlap
   order;
6. clean commit returns all four `BatchApplyOutcome` product facts atomically;
7. every begun scope finishes on success and error; GPU observation fault has
   precedence;
8. every successful candidate row has actual `1/1/1` scope deltas and aggregate
   sums equal successful non-empty batch count; constants cannot satisfy the
   instrumentation seam;
9. fatal integration produces no remote record/submit/present/receipt and its
   complete compact safe detail fits `MAX_SAFE_DETAIL_BYTES` without truncation;
10. serial emits one aggregate row per non-empty drain, candidate emits one per
    successful non-empty batch, and mailbox age uses the earliest constituent
    enqueue timestamp;
11. all-absent metrics configuration disables, all-present enables, and any
    partial configuration returns stable `InvalidConfiguration`;
12. event-only changes redraw while incomplete frame input does not;
13. timing probes clear on reset/detach/close/generation/session change and only
    same-identity presentation completes input-to-next-present;
14. `frame_response_ms` records raw `sample_ms`; changing `smoothed_ms` cannot
    change worst-window p95 or gate results;
15. fault-run rows are excluded from latency/CPU/memory aggregates.

Do not add exhaustive timing permutations, visual snapshots, synthetic wire
grammars, or duplicate decoder tests.

## Required Workspace Completion Matrix

Focused tests are not completion. The implementation must also pass the
repository-mandated matrix from the worktree root:

```text
cargo fmt -- --check
cargo test
cargo test --no-default-features
cargo build --release
cargo build --no-default-features
cargo run -- --help
cargo run -- hpssview --help
```

Record each command and result. Compilation does not substitute for tests, and
top-level help does not substitute for `hpssview` help.

## Normative Dependencies and Invariants

- Apple mode isolation is defined only by
  `2026-08-30-apple-shared-high-performance-isolation-design.md`.
- floating overlay behavior is defined only by
  `2026-08-30-desktop-floating-control-island-design.md` and may begin only
  after this measurement gate passes.
- both designs consume the same session-bound
  `SurfaceUpdate`/`FrameTransaction`/presentation contract and cannot add a
  protocol/platform branch to the renderer.

## Completion Boundary

Compilation and focused tests prove neither live latency nor resource bounds.
Completion requires the full workspace matrix, actual scope diagnostics, fixed
serial/candidate phase report, explicit CPU/working-set thresholds, correct
restore, and controlled input-to-next-present evidence. README status records
the actual evidence date and any inconclusive visible-correlation result.
