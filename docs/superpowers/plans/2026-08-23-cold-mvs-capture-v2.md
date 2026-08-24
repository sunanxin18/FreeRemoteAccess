# Cold MVS Capture V2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:executing-plans` to execute this plan task-by-task, `superpowers:test-driven-development` for every behavior change, `superpowers:systematic-debugging` for unexpected failures, and `superpowers:verification-before-completion` before any completion claim. Steps use checkbox (`- [ ]`) syntax for tracking. Do not start the next task until the current task has an independent review result of `APPROVED`.

**Goal:** Build a client-only `FRDMVS02` cold-start capture path whose file proves create-before-connect, durable arm-before-trigger, continuous MVS-only assembler provenance, and a clean terminal before the existing decoder replays it.

**Architecture:** Add a bounded V2 event codec and two readers, then layer a transactional file writer/state machine around the existing MVS assembler. A dedicated child command receives one guarded binary stdin frame, establishes only an Apple TCP-MVS session, and records every complete type-2/type-0/type-1 record before codec classification. A byte-oriented Python launcher creates a fresh artifact directory, supplies credentials through anonymous stdin, waits for finalization, and accepts output only after fresh structural and strict-cold reopen.

**Tech Stack:** Rust 2021, Rust standard library, existing `anyhow`, `clap`, `png`, existing Apple TCP-MVS/RFB modules, Python 3 standard library, `unittest`, PowerShell hash/snapshot commands, Windows release build.

**Spec:** `docs/superpowers/specs/2026-08-23-cold-mvs-capture-v2-design.md`

## Global Constraints

- Read the approved spec completely before each task and treat its literal binary schema, state transitions, reason mapping, and test list as authoritative.
- Do not add or change Cargo dependencies or feature definitions. V2 uses checksum identifier zero and does not add an embedded checksum.
- Preserve `read_mvs_capture(data: &[u8]) -> anyhow::Result<Vec<MvsRecord>>` and every existing `FRDMVS01` behavior unchanged.
- All integers in `FRDMVS02` and `FRDSTD01` are unsigned big-endian. Header/prefix/body sizes are exact: file header 32, event prefix 32, `Created` 16, `Armed` 24, `Triggered` 16, `Recording` 8, `Surface` 8, `Record` header 28 plus payload, `Gap` 40, terminal 48.
- Enforce maximum payload `0x01000000`, event body `0x0100001c`, event bytes `0x0100003c`, 4096 records, 4102 events, cumulative payload `0x20000000`, duration 30000 ms, incomplete lifetime 2000 ms, and socket polling timeout 100 ms with checked arithmetic before allocation or storage.
- Strict-cold accepts only generation zero, authenticated committed geometry, `Created -> Armed -> Triggered -> Recording -> Record* -> Clean`, continuous MVS-input ordinals, no `Surface`, no `Gap`, and no `Aborted`. Structural reading remains bounded and returns diagnostic provenance only.
- Format 2.0 has no no-frame `Gap`: flags are exactly one, both ordinals are present and not `u64::MAX`, and the twelve reason/stage/terminal rows are literal.
- Reason 267 is available only before the first `0x1d` attempt. From that attempt through the successful post-`Recording`-flush deadline sample and atomic state transition, every deadline or network/capture-event I/O failure is reason 260.
- Use `OpenOptions::create_new(true)` for every V2 capture. Never overwrite, edit, restamp, rename as clean, or wrap an existing capture. V1 artifacts remain historical and unproven.
- The new mode is client-only Apple TCP MVS. It must not enable UDP, audio, a viewer, dynamic resolution, Apple ID, a Mac helper, a remote command, a server-side worker, or a GUI.
- Credentials, target, display identifier, stdin frame, and raw MVS payload must occur zero times in source, reports, metadata, argv, child environment, stdout, and stderr. Tests use fake canaries only. Never read or print real credential values during unit tasks.
- Explicit overwrite guarantees apply only to the launcher's mutable raw-file/field/frame byte arrays and the child's guarded input vector. Do not claim zeroization of Python/runtime, standard-library, DNS/authentication, kernel, allocator, or compiler copies.
- This workspace has no `.git` metadata. Do not initialize Git, run Git commands, commit, or revert unrelated changes. Use the snapshot/hash/review protocol below.
- Source edits use `apply_patch`. Generated build output, test output, snapshots, and hash manifests are mechanical artifacts and remain below the task SDD directory.
- Run Cargo commands serially. Preserve unrelated P1-P5, MVS decoder, audio, and UDP work visible in the shared workspace.

## File Responsibility Map

| File | Responsibility |
| --- | --- |
| `src/vnc/mvs_capture_v2.rs` | Literal V2 constants/data model/event encoding, streaming bounded structural parser, strict-cold validation, V1 provenance wrapper. No sockets or decoder calls. |
| `src/vnc/mvs_capture_v2_writer.rs` | Sink/clock seams, lifecycle state, timestamps/deadline gates, finalization, counters, MVS-only source ordinals, pending provenance, exact Gap/terminal selection. No authentication. |
| `src/vnc/cold_credentials.rs` | Exact `FRDSTD01` parser, one guarded byte vector, borrowed field slices, category-only errors, volatile overwrite and fence. No environment credential fallback. |
| `src/vnc/cold_hpss.rs` | Dedicated TCP-MVS orchestration after authentication: trigger ordering, encrypted application-frame classification, assembler/writer calls, deadline loop, mockable connection seam. No UDP/media transport/viewer. |
| `src/vnc/client.rs` | Optional absolute I/O deadline for connect/handshake/authentication; existing entry points retain their current timeout behavior. |
| `src/vnc/hpss.rs` | Reuse existing public HPSS builders/parser. Only narrowly expose a helper if `cold_hpss` cannot call an existing public symbol. Keep V1 capture code unchanged. |
| `src/vnc/mod.rs` | Register the four new Rust modules in both default and headless builds. |
| `src/main.rs` | Exact `hpss-capture-v2` clap surface, early cold-mode dispatch, create-new writer before stdin parsing/connect, sanitized exit categories, fresh-file `mvs-capture-v2-verify` structural/strict summary, strict offline acceptance seam. |
| `ard_re/cold_mvs_v2_provider.py` | Bounded byte parser for the ignored credential file, exact six labels, mutable arrays, `FRDSTD01` assembly, allowlisted child environment, overwrite helpers. |
| `ard_re/run_live_hpss.py` | Dedicated `cold-mvs-v2` branch before legacy string accessors, exclusive artifact directory, reviewed executable, child stdin launch, sanitized metadata, structural/strict reopen result handling. |
| `ard_re/test_cold_mvs_v2_provider.py` | Byte grammar, maxima, alias, zeroization, canary and environment tests. |
| `ard_re/test_run_live_hpss_cold.py` | Launcher command, collision, no-remote/no-UDP/no-GUI, process/finalization/reopen, output and metadata tests. |
| `.superpowers/sdd/2026-08-23-cold-mvs-capture-v2/progress.md` | Task ledger containing status, exact commands, exit codes, test counts, source hash-manifest paths, and independent review result only. |
| `.superpowers/sdd/2026-08-23-cold-mvs-capture-v2/task-N-report.md` | One sanitized RED/GREEN and self-review report per task. No raw payload or target/session data. |

## Target Interfaces

```rust
// src/vnc/mvs_capture_v2.rs
pub const MVS_CAPTURE_V2_MAGIC: [u8; 8] = *b"FRDMVS02";
pub const MAX_V2_PAYLOAD: usize = 0x0100_0000;
pub const MAX_V2_EVENT_BODY: usize = 0x0100_001c;
pub const MAX_V2_EVENT_BYTES: usize = 0x0100_003c;
pub const MAX_V2_RECORDS: u32 = 4096;
pub const MAX_V2_EVENTS: u32 = 4102;
pub const MAX_V2_CUMULATIVE_PAYLOAD: u64 = 0x2000_0000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureGeometry { pub width: u16, pub height: u16 }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct V2Record {
    pub generation: u64,
    pub first_source_ordinal: u64,
    pub last_source_ordinal: u64,
    pub rect: crate::vnc::mvs_stream::MvsRect,
    pub payload: Vec<u8>,
}

pub struct StructuralMvsCaptureV2 {
    pub committed: CaptureGeometry,
    pub requested: CaptureGeometry,
    pub records: Vec<V2Record>,
    pub terminal: V2Terminal,
    pub provenance: DiagnosticOnly,
}

pub struct StrictColdMvsCaptureV2 {
    pub committed: CaptureGeometry,
    pub requested: CaptureGeometry,
    pub records: Vec<V2Record>,
    pub terminal: CleanTerminal,
}

pub fn read_mvs_capture_v2_structural<R: std::io::Read>(
    reader: &mut R,
) -> anyhow::Result<StructuralMvsCaptureV2>;

pub fn read_mvs_capture_v2_strict_cold<R: std::io::Read>(
    reader: &mut R,
) -> anyhow::Result<StrictColdMvsCaptureV2>;
```

```rust
// src/vnc/mvs_capture_v2_writer.rs
pub trait CaptureClock { fn elapsed(&self) -> anyhow::Result<std::time::Duration>; }

pub trait CaptureSink {
    fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()>;
    fn flush(&mut self) -> std::io::Result<()>;
    fn sync_data(&mut self) -> std::io::Result<()>;
    fn relinquish(self: Box<Self>) -> std::io::Result<()>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureState { Created, Armed, Triggering, Recording, Finalizing, Clean, Aborted }

pub struct CreatedConfig { pub deadline_ms: u32, pub record_limit: u32 }
pub struct ArmedConfig { pub committed: CaptureGeometry, pub requested: CaptureGeometry }

pub struct MvsCaptureV2Writer {
    /* one sink, one clock, lifecycle, counters, assembler and pending provenance */
}

impl MvsCaptureV2Writer {
    pub fn create_new(path: &std::path::Path, config: CreatedConfig) -> anyhow::Result<Self>;
    pub fn arm(&mut self, config: ArmedConfig) -> anyhow::Result<()>;
    pub fn begin_triggering(&mut self) -> anyhow::Result<()>;
    pub fn trigger_write_succeeded(&mut self, mask_bit: u32) -> anyhow::Result<()>;
    pub fn write_triggered_gate(&mut self) -> anyhow::Result<()>;
    pub fn write_recording_gate(&mut self) -> anyhow::Result<()>;
    pub fn observe_non_mvs(&mut self) -> anyhow::Result<()>;
    pub fn accept_mvs_begin(&mut self, rect: MvsRect, total: u32, first: &[u8]) -> anyhow::Result<()>;
    pub fn accept_mvs_continuation(&mut self, chunk: &[u8]) -> anyhow::Result<()>;
    pub fn loop_top(&mut self) -> anyhow::Result<WriterDecision>;
    pub fn cancel(&mut self) -> anyhow::Result<()>;
    pub fn finalize(self) -> anyhow::Result<V2Terminal>;
}
```

```rust
// src/vnc/cold_credentials.rs
pub struct GuardedCredentialFrame { /* one Vec<u8>, validated ranges, port */ }
pub struct CredentialSlices<'a> { pub host: &'a str, pub username: &'a str, pub password: &'a str, pub port: u16 }

impl GuardedCredentialFrame {
    pub fn read_stdin_v1<R: std::io::Read>(reader: &mut R) -> anyhow::Result<Self>;
    pub fn with_slices<T>(&self, f: impl FnOnce(CredentialSlices<'_>) -> T) -> T;
    pub fn clear(&mut self);
}
```

```rust
// src/vnc/cold_hpss.rs and src/vnc/client.rs
pub fn run_authenticated_cold_session(
    conn: &mut crate::vnc::client::RfbConn,
    writer: &mut MvsCaptureV2Writer,
    requested: CaptureGeometry,
) -> anyhow::Result<()>;

pub fn connect_deadline_opts(
    addr: &std::net::SocketAddr,
    deadline: std::time::Instant,
    username: &str,
    password: &str,
    profile: crate::vnc::session::SessionEncodingProfile,
) -> anyhow::Result<crate::vnc::client::VncClient>;
```

## No-Git Snapshot, Hash, Ledger, and Review Protocol

For every task, create the task SDD directory before RED. Snapshot each listed source/test file; record `ABSENT` for a listed new file. Do not snapshot credentials, captures, `target`, Python bytecode, or binaries.

```powershell
$coldV2Root = '.superpowers/sdd/2026-08-23-cold-mvs-capture-v2'
New-Item -ItemType Directory -Path $coldV2Root -Force | Out-Null
$taskNumber = 1
$taskFiles = @('src/vnc/mvs_capture_v2.rs', 'src/vnc/mod.rs')
$beforeRoot = Join-Path $coldV2Root ("task-{0}-before" -f $taskNumber)
if (Test-Path -LiteralPath $beforeRoot) { throw 'task baseline already exists' }
New-Item -ItemType Directory -Path $beforeRoot | Out-Null
$manifest = foreach ($file in $taskFiles) {
    if (Test-Path -LiteralPath $file) {
        $destination = Join-Path $beforeRoot ($file -replace '[:\\/]', '__')
        Copy-Item -LiteralPath $file -Destination $destination
        [pscustomobject]@{ path = $file; sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash }
    } else {
        [pscustomobject]@{ path = $file; sha256 = 'ABSENT' }
    }
}
$manifest | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $beforeRoot 'sha256.json') -Encoding utf8
```

After GREEN, produce `task-N-after-sha256.json` with the same ordered file list, append exact commands/results to `progress.md`, and write `task-N-report.md` through `apply_patch`. An independent reviewer checks the approved spec, before copies, after files, RED evidence, GREEN output, secret boundary, and unrelated-file preservation. Record exactly `APPROVED` or `REJECTED: <sanitized finding>` in the ledger. A rejection is repaired inside the same task and reviewed again; no later task starts while rejected.

---

### Task 1: V2 Schema, Structural Reader, and Strict-Cold Reader

**Files:**

- Create: `src/vnc/mvs_capture_v2.rs`
- Modify: `src/vnc/mod.rs`
- Test: `src/vnc/mvs_capture_v2.rs` `#[cfg(test)]`
- Report: `.superpowers/sdd/2026-08-23-cold-mvs-capture-v2/task-1-report.md`

**Interfaces:**

- Consumes: `MvsRect`, `MAX_MVS_RECORD_PAYLOAD`, `validate_mvs_rect_against_surface`, unchanged `hpss::read_mvs_capture`.
- Produces: all V2 constants/types, `read_mvs_capture_v2_structural`, and `read_mvs_capture_v2_strict_cold` with the signatures in Target Interfaces; module-private `encode_event`, `read_prefix`, `validate_event_length`, `CaptureCounters::reserve_event`, and `CaptureCounters::reserve_payload` for Task 2.

- [ ] **Step 1: Create the no-Git baseline and ledger.**

Run the protocol with `taskNumber = 1` and files `src/vnc/mvs_capture_v2.rs`, `src/vnc/mod.rs`, `src/vnc/hpss.rs`. Create `progress.md` with Task 1 `IN_PROGRESS`, the spec hash, and baseline-manifest path; do not record workspace secrets.

- [ ] **Step 2: Add literal schema and boundary tests before production code.**

Add named tests for all header fields, every event body, big-endian encoding, body/total arithmetic, exact maximum acceptance, maximum+1 rejection, zero payload rejection, reserved bytes, flags, event ordinals, timestamp monotonicity, EOF, truncation at every field boundary, and unknown version/type. Freeze the Gap table as this literal test vector:

```rust
const GAP_ROWS: [(u16, u16, u16); 12] = [
    (1, 1, 262), (2, 1, 262), (3, 1, 262), (4, 2, 262),
    (5, 3, 262), (6, 4, 263), (7, 5, 261), (8, 6, 266),
    (9, 2, 262), (10, 1, 262), (11, 8, 262), (12, 7, 265),
];

#[test]
fn format_two_gap_always_has_present_non_sentinel_range() {
    for (flags, first, last) in [(0, 0, 0), (2, 0, 0), (1, u64::MAX, 0), (1, 0, u64::MAX)] {
        assert!(parse_gap_fixture(flags, first, last).is_err());
    }
}
```

Use internal length/counter validators for the 16 MiB and 512 MiB boundary cases so those tests do not allocate a 16 MiB payload or retain 512 MiB.

- [ ] **Step 3: Add structural and strict policy mutation tests.**

Build hand-authored event bytes and mutate one field per case. Cover 4102/4103 events, cumulative payload boundary, generation increment/skip/repeat, `Surface` geometry budget, rectangle checked overflow, table zero rectangle, type-0/type-1 nonzero rectangle, requested-versus-committed bounds, continuous record/source ranges, all terminal counters, all twelve Gap rows, immediate mapped terminal, zero/one Gap, `Clean` timestamp rules, and V1 provenance. Required policy assertions:

```rust
#[test]
fn strict_rejects_diagnostic_events_and_v1() {
    assert!(read_strict(&capture_with_surface()).is_err());
    assert!(read_strict(&capture_with_gap_then_abort()).is_err());
    assert!(read_strict(&capture_with_abort()).is_err());
    assert!(read_strict(b"FRDMVS01").is_err());
}

#[test]
fn requested_geometry_never_expands_active_surface() {
    let bytes = clean_capture_with_geometry((640, 480), (1920, 1080), rect(0, 0, 641, 1));
    assert!(read_structural(&bytes).is_err());
}
```

- [ ] **Step 4: Run RED.**

Run:

```powershell
cargo test mvs_capture_v2::tests -- --nocapture
```

Expected: compilation fails because `mvs_capture_v2` constants/types/readers are absent. Record the exact first error and exit code in Task 1 report.

- [ ] **Step 5: Implement the minimal event codec and bounded readers.**

Implement one checked cursor over `Read`, validate prefix/body length before allocating, reserve event/payload counters before retaining bytes, recompute all footer counters, and reject trailing data with a one-byte EOF probe. Structural parsing returns `DiagnosticOnly`; strict validation consumes only a complete structural result and rejects every diagnostic event. Keep V1 parsing in `hpss.rs` byte-for-byte unchanged. The strict reader returns records only after terminal/counter/range validation succeeds.

- [ ] **Step 6: Run GREEN and the V1 guard.**

Run:

```powershell
cargo fmt -- --check
cargo test mvs_capture_v2::tests -- --nocapture
cargo test hpss::tests::capture_round_trip_preserves_rect_and_payload -- --nocapture
cargo test main::tests::offline_mvs_replay_keeps_legacy_capture_rejection -- --nocapture
cargo test --no-default-features mvs_capture_v2::tests -- --nocapture
```

Expected: all commands exit zero; V1 tests prove the old signature and bytes remain intact.

- [ ] **Step 7: Hash, report, and independent review gate.**

Write after hashes and Task 1 report with RED/GREEN, exact retained allocations, schema arithmetic, Gap vector, V1 hash/diff review, and concerns. Update ledger to `AWAITING_REVIEW`; proceed only after independent `APPROVED`.

---

### Task 2: Transactional Writer, State Machine, and Assembler Provenance

**Files:**

- Create: `src/vnc/mvs_capture_v2_writer.rs`
- Modify: `src/vnc/mod.rs`
- Modify only if the wrapper cannot observe required state: `src/vnc/mvs_stream.rs`
- Test: `src/vnc/mvs_capture_v2_writer.rs` `#[cfg(test)]`
- Report: `.superpowers/sdd/2026-08-23-cold-mvs-capture-v2/task-2-report.md`

**Interfaces:**

- Consumes: Task 1 event codec/types/counters and existing `MvsRecordAssembler`.
- Produces: `CaptureClock`, `CaptureSink`, `CaptureState`, `CreatedConfig`, `ArmedConfig`, `WriterDecision`, `MvsCaptureV2Writer`; Task 5 calls these methods without constructing event bytes.

- [ ] **Step 1: Snapshot exact Task 2 files and mark ledger `IN_PROGRESS`.**

Use the no-Git protocol with `mvs_capture_v2_writer.rs`, `mod.rs`, `mvs_stream.rs`, and Task 1 module.

- [ ] **Step 2: Add lifecycle/deadline RED tests with manual clock and injected sink.**

The fake sink records `Write`, `Flush`, `Sync`, and `Relinquish`; the manual clock returns scripted elapsed durations. Freeze Created flush before any connector callback, Armed flush+sync before trigger, and every trigger sample. Required mutation tests must fail when: trigger occurs before arm sync; reason 267 is allowed after first `0x1d`; a post-trigger sample is skipped; `Recording` state is entered before its event flush/post-flush sample; or post-flush equality is emitted as `Clean` 1 rather than `Aborted` 260.

```rust
#[test]
fn recording_event_does_not_publish_state_before_post_flush_sample() {
    let (mut writer, sink, clock) = armed_writer();
    begin_and_complete_three_triggers(&mut writer);
    clock.set_micros(DEADLINE_MICROS);
    assert!(writer.write_recording_gate().is_err());
    assert_ne!(writer.state(), CaptureState::Recording);
    assert_eq!(writer.selected_abort_reason(), Some(260));
    assert!(sink.events().contains(&SinkEvent::Flush));
}
```

- [ ] **Step 3: Add assembler provenance, priority, Gap, and finalization RED tests.**

Cover one-frame and fragmented records, only MVS input consuming source ordinals, write-before-classify, 50 ms continuations reaching 2000 ms, reason 1-12 fields, budget check before first candidate byte, generation transition pending, read failure pending, cancellation pending/idle, event-slot reservation, payload checked addition, and no partial `Record`. Freeze post-frame order: complete/write Record, sample once, pending deadline -> Gap6+263, idle deadline -> Clean1, only then limit -> Clean2. Inject terminal write, implicit `into_inner` flush, `sync_data`, and relinquish failures.

```rust
#[test]
fn cancellation_uses_gap_only_when_pending() {
    let mut pending = recording_writer();
    pending.accept_mvs_begin(rect(1, 2, 3, 4), 8, &[0, 1]).unwrap();
    pending.cancel().unwrap();
    assert_eq!(pending.last_gap(), gap(12, 7, 0, 0, 8, 2, rect(1, 2, 3, 4)));
    assert_eq!(pending.selected_abort_reason(), Some(265));

    let mut idle = recording_writer();
    idle.cancel().unwrap();
    assert_eq!(idle.gap_count(), 0);
    assert_eq!(idle.selected_abort_reason(), Some(265));
}
```

- [ ] **Step 4: Run RED.**

Run `cargo test mvs_capture_v2_writer::tests -- --nocapture`.

Expected: compilation fails because the writer/state types are absent. Record the first exact error and exit code.

- [ ] **Step 5: Implement the minimal transactional writer.**

Write Header+Created+flush in construction; use one state enum and reject every illegal transition without writing. Track pending `{first,last,total,accepted,rect,started_at}` beside the existing assembler and update it only after checked validation. Assign source ordinal after MVS begin/continuation classification and before assembler submission; non-MVS does nothing. Serialize a complete Record before any codec classification. Select exactly one terminal, enter `Finalizing`, then write terminal -> consume BufWriter/implicit flush -> `File::sync_data` -> relinquish handle. Keep the sink outside decoder/surface ownership.

- [ ] **Step 6: Run GREEN.**

Run:

```powershell
cargo fmt -- --check
cargo test mvs_capture_v2_writer::tests -- --nocapture
cargo test mvs_stream::tests -- --nocapture
cargo test mvs_capture_v2::tests -- --nocapture
cargo test --no-default-features mvs_capture_v2_writer::tests -- --nocapture
```

Expected: all exit zero and Task 1/V1 behavior remains green.

- [ ] **Step 7: Hash, report, and independent review gate.**

Report every lifecycle edge, the twelve Gap mappings, post-frame priority, event reservation, finalization order, and lock/ownership review. Record after hashes and wait for `APPROVED`.

---

### Task 3: Rust `stdin-v1` Guarded Credential Provider

**Files:**

- Create: `src/vnc/cold_credentials.rs`
- Modify: `src/vnc/mod.rs`
- Test: `src/vnc/cold_credentials.rs` `#[cfg(test)]`
- Report: `.superpowers/sdd/2026-08-23-cold-mvs-capture-v2/task-3-report.md`

**Interfaces:**

- Consumes: only `std::io::Read` and standard volatile/fence primitives.
- Produces: `GuardedCredentialFrame::read_stdin_v1`, `with_slices`, `clear`, `CredentialSlices`; Task 5 borrows slices synchronously and never stores them.

- [ ] **Step 1: Snapshot Task 3 files and mark ledger `IN_PROGRESS`.**

Use the no-Git protocol for `cold_credentials.rs` and `mod.rs`.

- [ ] **Step 2: Add exact frame and category-error RED tests.**

Freeze `FRDSTD01`, payload length `8 + host + username + password`, BE u16/u32 fields, total 23..1554, host/user 1..255, password 1..1024, port 1..65535, EOF probe byte, UTF-8, host/user controls/whitespace, and password NUL. Test all maxima, each maximum+1, truncation at every fixed field, payload mismatch, and extra byte. Error strings contain only stable field categories and never canary bytes or their lengths.

- [ ] **Step 3: Add overwrite and no-copy RED tests.**

Under `cfg(test)`, inject an observer called from `clear`/`Drop` after volatile writes and the SeqCst compiler fence. Prove every initialized byte is zero after parse success, callback error, authentication callback success, and panic caught by the test. Statically scan this module for `.clone(`, `.to_owned(`, `.to_string(`, `format!(`, and interpolation of borrowed fields.

- [ ] **Step 4: Run RED.**

Run `cargo test cold_credentials::tests -- --nocapture`.

Expected: compilation fails because the guarded provider does not exist.

- [ ] **Step 5: Implement one guarded vector and borrowed ranges.**

Read at most 1555 bytes into one `Vec<u8>`, require exact EOF, validate offsets with checked addition, store only ranges/port, and derive `&str` on demand inside `with_slices`. `clear` uses `std::ptr::write_volatile` for every initialized byte then `std::sync::atomic::compiler_fence(Ordering::SeqCst)`; `Drop` calls it idempotently. Map all parse errors to category-only messages without source values.

- [ ] **Step 6: Run GREEN and static guards.**

Run:

```powershell
cargo fmt -- --check
cargo test cold_credentials::tests -- --nocapture
cargo test --no-default-features cold_credentials::tests -- --nocapture
rg -n "clone\(|to_owned\(|to_string\(|format!\(" src/vnc/cold_credentials.rs
```

Expected: tests exit zero; the `rg` output contains no production use on credential slices. Test-only fake byte construction is documented in the report.

- [ ] **Step 7: Hash, report, and independent review gate.**

Report owned-buffer scope and explicitly list unprovable copies from the spec. Record after hashes and wait for `APPROVED`.

---

### Task 4: Python Byte Provider and `cold-mvs-v2` Launcher

**Files:**

- Create: `ard_re/cold_mvs_v2_provider.py`
- Create: `ard_re/test_cold_mvs_v2_provider.py`
- Create: `ard_re/test_run_live_hpss_cold.py`
- Modify: `ard_re/run_live_hpss.py`
- Test guard: `ard_re/test_run_live_hpss.py`
- Test guard: `ard_re/test_run_live_hpss_hardening.py`
- Report: `.superpowers/sdd/2026-08-23-cold-mvs-capture-v2/task-4-report.md`

**Interfaces:**

- Consumes: local ignored credential file path from `FRD_CREDENTIALS_FILE`, reviewed executable and artifact-root gates already in `run_live_hpss.py`.
- Produces: `credential_stdin_v1(path: Path, port: int) -> ContextManager[bytearray]`, `sanitized_child_environment(source: Mapping[str, str]) -> dict[str, str]`, and `_run_cold_mvs_v2(executable, artifact_root, duration, max_records) -> ProbeExecution`.

- [ ] **Step 1: Snapshot all Task 4 files and mark ledger `IN_PROGRESS`.**

Include absent new files, current launcher, and both existing launcher test files.

- [ ] **Step 2: Add byte-parser RED tests for the complete frozen grammar.**

Use fake bytearrays only. Cover one optional leading BOM, BOM elsewhere, LF/CRLF, bare CR, empty/space/comment/single-line HTML comment, rejected multiline comment, prose termination, exact `项目|值` header, exact hyphen separator, multiple tables, malformed table-looking line, row outside table, unknown well-formed row, all six exact labels, duplicate aliases, no escape processing, and 1..65536 file bytes. Assert normalized/case-folded/translated/substring labels are not recognized.

Freeze the recognized labels exactly; no additional spelling is accepted:

```python
RECOGNIZED_LABELS = {
    "host": ("当前 Mac 主机", "目标主机（Mac mini）"),
    "username": ("当前 Mac 用户名", "Mac 用户名"),
    "password": ("Mac 登录密码", "VNC 密码"),
}
```

- [ ] **Step 3: Add mutable-array, environment, command, and artifact RED tests.**

Freeze this exact child command shape with fake paths:

```python
[
    executable,
    "hpss-capture-v2",
    "--credentials-stdin-v1",
    "--out", capture_path,
    "--seconds", str(duration),
    "--max-records", "4096",
]
```

Assert argv/environment/log/metadata/capture fixtures contain no canary; `FRD_USERNAME`, `FRD_PASSWORD`, direct target variables, and legacy secret variables are absent from the child environment. Assert no `--udp-media`, viewer command, remote command, delayed worker, or GUI. Freeze allowlist keys `SystemRoot`, `WINDIR`, `PATH`, `PATHEXT`, `TEMP`, and `TMP`; absent keys remain absent. Prove exclusive `cold-mvs-v2/<UTC-compact>-<16-random-hex>/capture.mvs` directory creation refuses collisions and never touches an existing artifact. Inject success and every read/parse/Popen/stdin/write/wait/reopen error and observe raw-file, field, and frame arrays overwritten in `finally`.

- [ ] **Step 4: Run RED.**

Run:

```powershell
python -m unittest ard_re.test_cold_mvs_v2_provider ard_re.test_run_live_hpss_cold -v
```

Expected: import/test failure because the provider and cold launcher branch are absent.

- [ ] **Step 5: Implement the byte provider and early cold branch.**

Implement `credential_stdin_v1` as the context manager owning this lifetime: validate the regular-file size first, allocate one mutable `bytearray(size)`, and fill it with `open(path, "rb").readinto(raw_file)` plus an EOF probe; do not create an immutable whole-file `bytes` object. Scan by indices, copy only recognized values into three mutable bytearrays, build one mutable frame, and overwrite every owned array in `finally`. In `main`, branch on `FRD_LIVE_MODE == "cold-mvs-v2"` before calling `mac_host`, `mac_username`, or `mac_password`. Spawn with `stdin=PIPE`, sanitized environment, byte argv containing no secret, close stdin, and wait for bounded child completion. After child exit, invoke the reviewed executable as `mvs-capture-v2-verify --input <capture-path> --strict-cold`; that second process opens the relinquished file afresh and returns only the sanctioned structural/strict counters. Persist only status, executable/capture SHA-256, size, counters, duration, and terminal category.

- [ ] **Step 6: Run GREEN, Python compilation, and legacy launcher guards.**

Run:

```powershell
python -m py_compile ard_re/cold_mvs_v2_provider.py ard_re/run_live_hpss.py ard_re/test_cold_mvs_v2_provider.py ard_re/test_run_live_hpss_cold.py
python -m unittest ard_re.test_cold_mvs_v2_provider ard_re.test_run_live_hpss_cold -v
python -m unittest ard_re.test_run_live_hpss ard_re.test_run_live_hpss_hardening -v
```

Expected: all exit zero; existing modes retain current behavior.

- [ ] **Step 7: Run content-suppressed canary/config scans.**

The command prints counts and redacted paths only. Require zero canary occurrences outside test fixtures, zero direct credential environment propagation, and zero private-address literals in changed Python helpers.

- [ ] **Step 8: Hash, report, and independent review gate.**

Report only counts/paths, overwrite ownership, command tokens without values, and test results. Record after hashes and wait for `APPROVED`.

---

### Task 5: CLI and Deadline-Bounded Apple TCP-MVS Session Integration

**Files:**

- Create: `src/vnc/cold_hpss.rs`
- Modify: `src/vnc/client.rs`
- Modify: `src/vnc/hpss.rs` only for a narrow `pub(crate)` helper exposure
- Modify: `src/vnc/mod.rs`
- Modify: `src/main.rs`
- Test: adjacent `#[cfg(test)]` modules in those files
- Report: `.superpowers/sdd/2026-08-23-cold-mvs-capture-v2/task-5-report.md`

**Interfaces:**

- Consumes: Tasks 1-3 public interfaces, `build_set_display_config`, `build_display_query`, `parse_media`, `parse_server_state_geometry`, `protocol::msg_fb_update_request`, and `SessionEncodingProfile::AppleTcpMvs`.
- Produces: `connect_deadline_opts`, `run_authenticated_cold_session`, CLI `hpss-capture-v2 --credentials-stdin-v1 --out PATH --seconds {5,10,15,20,30} --max-records 1..4096`, and CLI `mvs-capture-v2-verify --input PATH --strict-cold`.

- [ ] **Step 1: Snapshot Task 5 files and mark ledger `IN_PROGRESS`.**

Include all five production files plus `Cargo.toml`; its before/after hash must remain equal.

- [ ] **Step 2: Add CLI and secret-dispatch RED tests.**

Assert the exact capture command has no positional host, username/password env option, UDP, audio, viewer, dynamic-resolution, Apple-ID, remote, or GUI field. Reject seconds outside the five values and record counts outside 1..4096 before stdin/connect. Prove dispatch constructs Header+Created+flush/create-new before the guarded provider callback and never calls legacy credential accessors. Canary failures return category-only child output. Assert the verify command accepts only input path plus required strict-cold flag, reopens after the capture child has exited, calls structural validation before strict validation, and emits only dimensions, record/tag/source/gap counts, duration, and terminal category.

- [ ] **Step 3: Add absolute deadline RED tests.**

Add an optional deadline field to `RfbConn` behind new constructors while existing constructors default to `None`. Inject a clock/socket seam and prove connect, each authentication read/write, trigger writes, and session reads are bounded by remaining absolute duration; both read and write timeout are updated. Equality means expired. Existing `connect_timeout_opts` must retain its current behavior when no deadline is installed. Strict cold target parsing must not start unbounded DNS: accept numeric `IpAddr`/`SocketAddr`; reject a hostname with a category-only error before connect unless a cancellable standard-library-only resolver is implemented and covered by a timeout/cancellation test.

- [ ] **Step 4: Add trigger/session loop RED tests.**

Freeze `Created -> authenticate -> Armed flush+sync -> 0x1d -> 0x09 -> full 0x03 -> Triggered -> Recording write+flush -> post-flush sample -> first read`. Sample after each trigger, before/after Triggered, and after Recording flush. While assembler pending consume frames only as continuation; while idle classify non-MVS without ordinal and MVS begin with ordinal. Complete/write Record before type classification. Loop-top order is pending absolute deadline, idle absolute deadline, incomplete lifetime, cancellation, safe read. Port announcements are treated as non-MVS diagnostic input; do not instantiate `MediaTransport` or bind UDP.

- [ ] **Step 5: Run RED.**

Run:

```powershell
cargo test cold_hpss::tests -- --nocapture
cargo test main::tests::cold_capture_v2 -- --nocapture
```

Expected: compile/test failure because CLI/session/deadline entry points are absent.

- [ ] **Step 6: Implement minimal cold orchestration and sanitized errors.**

Open/create writer before provider parsing. Borrow credential slices only inside one synchronous closure, parse a numeric target without formatting its value, call `connect_deadline_opts` with `AppleTcpMvs`, require encryption, arm with authenticated ServerInit geometry, and choose requested geometry equal to committed geometry. Use a fixed non-secret display name constant only on the wire; never log it. Clear guarded bytes immediately after authentication returns, then run the session without credentials. Convert all inner errors to stable categories before returning to `main`, so `{e:#}` cannot expose source values.

- [ ] **Step 7: Run GREEN and both feature modes.**

Run:

```powershell
cargo fmt -- --check
cargo test cold_hpss::tests -- --nocapture
cargo test main::tests::cold_capture_v2 -- --nocapture
cargo test client::tests -- --nocapture
cargo test --no-default-features cold_hpss::tests -- --nocapture
cargo run -- hpss-capture-v2 --help
cargo run --no-default-features -- hpss-capture-v2 --help
cargo run -- mvs-capture-v2-verify --help
cargo run --no-default-features -- mvs-capture-v2-verify --help
```

Expected: all exit zero; both help outputs contain only the approved options and no credential/target value.

- [ ] **Step 8: Hash, report, and independent review gate.**

Report deadline samples, timeout ownership, no-UDP call graph, writer-before-provider order, error sanitization, Cargo hash equality, and test results. Wait for `APPROVED`.

---

### Task 6: Loopback Mock Cold-Capture Acceptance

**Files:**

- Modify test-only code: `src/vnc/cold_hpss.rs`
- Modify test-only code if CLI process coverage is required: `src/main.rs`
- Test guard: `src/vnc/mvs_capture_v2.rs`
- Test guard: `src/vnc/mvs_capture_v2_writer.rs`
- Report: `.superpowers/sdd/2026-08-23-cold-mvs-capture-v2/task-6-report.md`

**Interfaces:**

- Consumes: Task 5 authenticated session entry, `RfbConn::set_crypto`, `SessionCrypto::from_key_iv`, both V2 readers.
- Produces: no production API; one deterministic loopback acceptance fixture built in memory from hand-authored safe payloads.

- [ ] **Step 1: Snapshot Task 6 files and mark ledger `IN_PROGRESS`.**

Hash full production files even though permanent edits are test-only; after hash review must show production sections unchanged.

- [ ] **Step 2: Add the mock acceptance test and run RED.**

Bind `TcpListener` on loopback, install paired fixed test-only `SessionCrypto` values, and have the server thread verify exact encrypted `0x1d`, `0x09`, and full `0x03` order. Send encrypted application frames containing: one exact tag-2 table record; one valid tag-0 record; one tag-1 record; one record split across begin plus continuation; and interleaved keepalive/ServerState/control frames. Use no capture-derived payload bytes.

The test must assert fresh structural and strict reopen, Clean EOF, generation zero, no Surface/Gap/Aborted, continuous MVS-only ordinal ranges, exact tag/count/footer recomputation, non-MVS ordinal stability, committed geometry bounds, and all complete records written before classification. Assert type-1 is present as a captured record but is not counted as decoded pixels.

Run `cargo test cold_hpss::tests::loopback_mock_produces_strict_cold_capture -- --nocapture`.

Expected RED: the first missing orchestration/fixture assertion fails with an exact message; record it before correcting only the test seam or Task 5 integration defect.

- [ ] **Step 3: Make the smallest test-seam correction and run GREEN.**

Do not weaken strict reading, fabricate cache state, bypass encryption, or add a production alternate path. If the failure exposes production behavior inconsistent with the approved spec, return to Task 5, mark its review invalidated, repair under Task 5 RED/GREEN, and re-review before continuing.

Run:

```powershell
cargo fmt -- --check
cargo test cold_hpss::tests::loopback_mock_produces_strict_cold_capture -- --nocapture
cargo test mvs_capture_v2 -- --nocapture
cargo test mvs_capture_v2_writer -- --nocapture
cargo test --no-default-features cold_hpss::tests::loopback_mock_produces_strict_cold_capture -- --nocapture
```

- [ ] **Step 4: Hash, report, and independent review gate.**

Report mock frame categories/counts only, never bytes. Confirm test-only permanent diff, production hash status, and strict reopen. Wait for `APPROVED`.

---

### Task 7: Full Matrix, Isolated Release, Cold Live Capture, Strict Replay, and PNG Gate

**Files:**

- Modify test-only acceptance code: `src/main.rs`
- Modify: `ard_re/run_live_hpss.py` only if acceptance reveals a launcher-contract defect
- Create local artifacts only: `.superpowers/sdd/2026-08-23-cold-mvs-capture-v2/reviewed-release/<sha16>/freeremotedesk.exe`
- Create local artifacts only: `.superpowers/sdd/2026-08-23-cold-mvs-capture-v2/cold-mvs-v2/<UTC-compact>-<16-random-hex>/capture.mvs`
- Create PNG only after decoder gate: same capture directory `current-full.png`
- Report: `.superpowers/sdd/2026-08-23-cold-mvs-capture-v2/task-7-report.md`

**Interfaces:**

- Consumes: `read_mvs_capture_v2_structural`, `read_mvs_capture_v2_strict_cold`, existing production `replay_offline_mvs_records`, existing native MVS decoder, Python cold launcher.
- Produces: ignored test `main::tests::replay_external_cold_mvs_v2_capture`, sanitized acceptance counters, local artifact hashes/paths.

- [ ] **Step 1: Snapshot Task 7 source/test files and mark ledger `IN_PROGRESS`.**

Record hashes for every Rust/Python file changed by Tasks 1-6 and `Cargo.toml`. Do not snapshot credentials or prior captures.

- [ ] **Step 2: Add the explicit ignored replay acceptance test and record RED.**

The test reads only caller-supplied `FRD_MVS_CAPTURE` and `FRD_MVS_PNG`, opens the V2 file through `read_mvs_capture_v2_strict_cold`, asserts at least one tag-2, tag-0, and tag-1 record, then replays records in exact order through the production decoder. Type-1 may update only evidence-backed opaque/cache side effects and is never counted as decoded pixels. Diagnose the first cache/coefficient dependency using record ordinal, rectangle, tile index, mode, and cache index only. Write PNG only when at least one type-0 is successfully applied and the final RGB contains at least two distinct colors and is not all black.

Run without env values:

```powershell
cargo test replay_external_cold_mvs_v2_capture -- --ignored --nocapture
```

Expected RED: fail with the stable message that `FRD_MVS_CAPTURE` and `FRD_MVS_PNG` must be caller supplied.

- [ ] **Step 3: Run the full pre-live verification matrix.**

Run serially:

```powershell
python -m py_compile ard_re/cold_mvs_v2_provider.py ard_re/run_live_hpss.py ard_re/test_cold_mvs_v2_provider.py ard_re/test_run_live_hpss_cold.py
python -m unittest ard_re.test_cold_mvs_v2_provider ard_re.test_run_live_hpss_cold ard_re.test_run_live_hpss ard_re.test_run_live_hpss_hardening -v
cargo fmt -- --check
cargo test --no-default-features
cargo test
cargo build --no-default-features
cargo build --release
cargo run -- --help
cargo run -- hpss-capture-v2 --help
cargo run --no-default-features -- --help
cargo run --no-default-features -- hpss-capture-v2 --help
```

Expected: every command exits zero. Any failure blocks live execution.

- [ ] **Step 4: Build and freeze a reviewed release outside the default target.**

```powershell
$coldV2Root = (Resolve-Path '.superpowers/sdd/2026-08-23-cold-mvs-capture-v2').Path
$isolatedTarget = Join-Path $coldV2Root 'target-release'
$env:CARGO_TARGET_DIR = $isolatedTarget
cargo build --release
$builtExe = Join-Path $isolatedTarget 'release/freeremotedesk.exe'
$releaseHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $builtExe).Hash
$releaseRoot = Join-Path $coldV2Root ('reviewed-release/' + $releaseHash.Substring(0, 16).ToLowerInvariant())
New-Item -ItemType Directory -Path $releaseRoot -ErrorAction Stop | Out-Null
$reviewedExe = Join-Path $releaseRoot 'freeremotedesk.exe'
Copy-Item -LiteralPath $builtExe -Destination $reviewedExe -ErrorAction Stop
if ((Get-FileHash -Algorithm SHA256 -LiteralPath $reviewedExe).Hash -ne $releaseHash) { throw 'reviewed release hash mismatch' }
```

Have an independent reviewer verify source manifests, matrix outputs, and release hash before network access. Do not reuse or overwrite an older executable path.

- [ ] **Step 5: Run one bounded client-only cold capture.**

Set `FRD_EXECUTABLE` to `$reviewedExe`, `FRD_LIVE_MODE` to `cold-mvs-v2`, `FRD_LIVE_SECONDS` to `30`, `FRD_CREDENTIALS_FILE` to the ignored local credential file, and `FRD_LIVE_ARTIFACT_DIR` to the approved Task 7 artifact root. Do not put any credential/target value on the command line. Run:

```powershell
python ard_re/run_live_hpss.py
```

The launcher creates a new unique child directory and `capture.mvs`. If the terminal is not Clean, structural reopen fails, strict reopen fails, a Gap/Surface/generation change appears, or required tag counts are absent, mark Task 7 `BLOCKED` with the first sanitized event/reason/ordinal category. Do not retry in the same directory and do not generate PNG.

- [ ] **Step 6: Run explicit strict replay and PNG gate.**

Set `FRD_MVS_CAPTURE` to the new `capture.mvs` and `FRD_MVS_PNG` to `current-full.png` in the same unique directory, then run:

```powershell
cargo test replay_external_cold_mvs_v2_capture -- --ignored --nocapture
```

Expected GREEN: strict reader accepts Clean generation-zero capture; counters report at least one tag-2/tag-0/tag-1; replay applies at least one type-0; RGB has at least two colors and is not all black; PNG is created only at the caller path. If mode 6/7, missing seed, or any evidence gap blocks decoding, preserve the valid capture locally, omit PNG, and report `BLOCKED` without changing protocol code.

- [ ] **Step 7: Run content-suppressed secret and scope scans.**

Scan changed source/docs/reports/metadata plus child argv/environment/log fixtures. Exclude the ignored credential file, capture payloads, binaries, `target`, and Python bytecode from content output. Print only match counts and redacted paths. Required counts for real credentials, direct target literals, stdin bytes, raw payload, and canary leaks are zero. Confirm no UDP/audio/viewer/remote invocation in the cold call path.

- [ ] **Step 8: Final hashes, report, ledger, and independent acceptance review.**

Record exact matrix counts, executable/capture/PNG SHA-256 when those artifacts exist, dimensions/counters/error categories, and secret-scan counts only. The independent reviewer verifies spec coverage, all task reports, artifact create-new provenance, child exit status, fresh structural and strict reopen, decoder result, and PNG gate. Mark the ledger `COMPLETE` only after `APPROVED`; otherwise mark `BLOCKED` or the rejecting task `IN_PROGRESS`.

## Spec Coverage Matrix

| Approved spec section | Implemented and reviewed in |
| --- | --- |
| Approaches, constants, byte order, no checksum | Task 1 |
| Exact header, prefix, every event body and derived size | Task 1 |
| Structural/strict readers, limits, generation, geometry, V1 boundary | Task 1 |
| Created/Armed/Triggering/Recording/Finalizing lifecycle | Task 2 |
| Twelve Gap rows, ordinal ranges, counters, footer recomputation | Tasks 1-2 |
| Deadline samples, post-frame priority, cancellation, finalization | Task 2; exercised through Task 5 |
| `FRDSTD01`, bounded read, borrowed fields, guarded vector | Task 3 |
| Credential-file byte grammar, six labels, launcher-owned arrays | Task 4 |
| Exclusive artifact creation, sanitized metadata, child wait/reopen | Task 4 |
| CLI shape, writer-before-provider/connect, bounded TCP/auth, no UDP | Task 5 |
| Encrypted mock session, fragmented records, non-MVS ordinal exclusion | Task 6 |
| Full/headless matrix, release, cold live capture, decoder replay, PNG | Task 7 |
| Secret/client-only/non-goal boundaries | Every task; final scan in Task 7 |

## Plan Self-Review Gate

- [ ] **Coverage:** Verify every spec heading and every TDD bullet maps to at least one row/task above. Missing coverage is added to the owning task before execution.
- [ ] **Placeholder scan:** Build the writing-plans forbidden-vocabulary list from split string fragments in PowerShell and scan the plan; expected match count zero. The split construction keeps forbidden marker words out of the plan artifact itself.
- [ ] **Type consistency:** Check every symbol in Target Interfaces is produced by exactly one task and consumed under the same spelling/signature in later tasks.
- [ ] **File consistency:** Check every file named in a task appears in File Responsibility Map or is explicitly test/report/artifact-only.
- [ ] **Security consistency:** Scan the plan for private-address literals, credential values, raw capture bytes, argv credential flags, UDP enablement in the cold command, and absolute live paths; expected count zero.
- [ ] **No-Git consistency:** Set `$versionControl = 'g' + 'it'` and scan for a command line beginning with that executable; expected count zero. Only explanatory no-Git wording is permitted.
- [ ] **Schema consistency:** Recompute 32-byte header, 32-byte prefix, all body totals, `0x0100003c` maximum event, 4102 maximum events, twelve Gap stage/terminal rows, and 23..1554 stdin frame.
