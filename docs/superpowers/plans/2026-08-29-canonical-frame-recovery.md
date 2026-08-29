# Canonical Frame Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Recover locally from bounded frame-mailbox loss so a correct Apple canonical CPU surface cannot leave stale black rectangles or a stopped visible desktop.

**Architecture:** Split decoder-baseline loss from downstream snapshot loss. Apple republishes a banded, current-generation BGRX snapshot from `DisplaySurface` on `NeedsFullSnapshot`, while a protocol-neutral one-shot launch barrier prevents any adapter worker from publishing before the desktop host installs its ports.

**Tech Stack:** Rust 2021, `frd-protocol-apple` canonical `DisplaySurface`, `frd-frame::FrameMailbox`, `frd-protocol-api::ProtocolRuntime`, winit desktop session host.

**Spec:** `docs/superpowers/specs/2026-08-29-canonical-frame-recovery-design.md`

## Global Constraints

- `NeedsFullBaseline` resets MVS receive state and requests a server full frame.
- `NeedsFullSnapshot` preserves decoder state and republishes from the local canonical CPU surface.
- Never fall back from snapshot recovery to a wire full request.
- Snapshot recovery remains generation-bound, strictly revision-monotonic, bounded, and fail-closed.
- Do not modify MVS grammar, ARD wire order, RDP behavior, renderer pixel semantics, or public protocol types.
- The desktop start barrier is protocol-neutral and must work identically for Apple and RDP.
- Do not add unbounded queues, recursive recovery, per-frame logs, credentials, pixels, or raw protocol payloads to diagnostics.
- Keep automated tests limited to the core publication and lifecycle contracts.

---

### Task 1: Republish a bounded canonical Apple snapshot

**Files:**
- Modify: `crates/frd-protocol-apple/src/surface_publisher.rs`
- Test: `crates/frd-protocol-apple/src/surface_publisher.rs`

**Interfaces:**
- Consumes: `DisplaySurface::bgrx_patch`, current publisher session/generation/revision, and `ProtocolRuntime::publish_surface`.
- Produces: `AppleSurfacePublisher::republish_full_snapshot` and a private testable band-size helper.

- [ ] **Step 1: Add RED recovery tests**

Add the following focused cases beside the existing publisher tests:

```rust
#[test]
fn damage_overflow_republishes_latest_canonical_bgrx() {
    // Establish a 2x1 full baseline, mutate the second canonical pixel,
    // force the next incremental Damage to return NeedsFullSnapshot, then
    // assert the replacement full snapshot contains the new BGRX bytes.
}

#[test]
fn boundary_overflow_advances_recovery_revision() {
    // Let Damage revision 2 enter the mailbox and make its Boundary overflow.
    // The replacement snapshot must begin at revision 3, never reuse 2.
}

#[test]
fn full_snapshot_recovery_bands_rows_and_marks_only_the_end_full() {
    let mut surface = DisplaySurface::new(1, PixelSize::new(4, 4).unwrap()).unwrap();
    seed_distinct_rows(&mut surface);
    publisher
        .republish_full_snapshot_with_patch_limit(&mut runtime, &surface, 1, 16)
        .unwrap();
    // Assert four one-row Damage records with consecutive revisions;
    // the first three boundaries are Incremental and the fourth FullBaseline.
    // Concatenating their pixels exactly reproduces the canonical BGRX surface.
}

#[test]
fn full_snapshot_recovery_rejects_a_limit_smaller_than_one_row() {
    let error = publisher
        .republish_full_snapshot_with_patch_limit(&mut runtime, &surface, 1, 15)
        .unwrap_err();
    assert_eq!(error, ProtocolError::FramePortRejected);
}
```

- [ ] **Step 2: Run focused tests to verify RED**

```powershell
cargo +stable test -p frd-protocol-apple overflow_republishes -- --nocapture
cargo +stable test -p frd-protocol-apple full_snapshot_recovery -- --nocapture
```

Expected: compilation fails because the recovery methods do not exist.

- [ ] **Step 3: Implement full-width band publication**

Add the production and private helper signatures exactly:

```rust
const FULL_SNAPSHOT_PATCH_BYTES: usize = 1024 * 1024;

pub(crate) fn republish_full_snapshot(
    &mut self,
    runtime: &mut ProtocolRuntime,
    surface: &DisplaySurface,
    generation: u64,
) -> Result<(), ProtocolError> {
    self.republish_full_snapshot_with_patch_limit(
        runtime,
        surface,
        generation,
        FULL_SNAPSHOT_PATCH_BYTES,
    )
}

fn republish_full_snapshot_with_patch_limit(
    &mut self,
    runtime: &mut ProtocolRuntime,
    surface: &DisplaySurface,
    generation: u64,
    patch_byte_limit: usize,
) -> Result<(), ProtocolError>;
```

The helper must:

1. reject a session/generation mismatch before allocation;
2. compute `row_bytes = width.checked_mul(4)` and require
   `patch_byte_limit >= row_bytes`;
3. use `rows_per_patch = patch_byte_limit / row_bytes`;
4. extract exact full-width `PixelRect` bands through `bgrx_patch`;
5. publish each band using the next revision and a matching boundary;
6. use `Incremental` for every non-final boundary and `FullBaseline` only for
   the final boundary;
7. set `baseline_established=false` before the first band and allow
   `publish_patch` to restore it only on the accepted final boundary;
8. propagate a second `NeedsFullSnapshot` as `ProtocolError::FramePortRejected`
   without recursion or a network request.

Do not increment `NativeMvsRenderObservability`; this is publication recovery,
not another decoder commit.

- [ ] **Step 4: Run focused GREEN**

```powershell
cargo +stable test -p frd-protocol-apple overflow_republishes -- --nocapture
cargo +stable test -p frd-protocol-apple full_snapshot_recovery -- --nocapture
```

Expected: all recovery, revision, byte-order, and band-completeness assertions pass.

- [ ] **Step 5: Commit publisher recovery**

```powershell
git add crates/frd-protocol-apple/src/surface_publisher.rs
git commit -m "fix: republish Apple canonical frame snapshots"
```

### Task 2: Keep mailbox recovery local in the Apple network reader

**Files:**
- Modify: `crates/frd-protocol-apple/src/network_reader.rs`
- Test: `crates/frd-protocol-apple/src/network_reader.rs`

**Interfaces:**
- Consumes: Task 1's `republish_full_snapshot`.
- Produces: distinct `NeedsFullBaseline` and `NeedsFullSnapshot` control flow while preserving the existing next-incremental request sequence.

- [ ] **Step 1: Add a RED reader integration test**

Add `mailbox_snapshot_recovery_preserves_decoder_and_sends_only_next_incremental` using a test surface publisher that returns
`ProtocolError::NeedsFullSnapshot` on the first normal Damage and accepts the
replacement bands. Process one deterministic complete 8x8 native type-0
record and assert:

```rust
assert!(!receiver.awaiting_full());
assert_eq!(peer.last_write(), protocol::msg_fb_update_request(true, 0, 0, 8, 8).unwrap());
assert_eq!(surface.native_mvs_observability.type_zero_applied_count, 1);
assert_eq!(surface.native_mvs_observability.content_revision, 1);
assert!(frames.end_with_full_baseline());
```

Also assert `requests.last_full_request` is unchanged. Current code sends a
non-incremental request and puts the decoder into awaiting-full, so the test is
deterministically RED.

- [ ] **Step 2: Run RED**

```powershell
cargo +stable test -p frd-protocol-apple mailbox_snapshot_recovery_ -- --nocapture
```

Expected: the assertion sees a `false` incremental flag or `awaiting_full == true`.

- [ ] **Step 3: Split the recovery branch**

Replace the combined branch in `handle_complete_mvs_record` with:

```rust
match publication {
    PublicationOutcome::NeedsFullBaseline => {
        receiver.request_full()?;
        let size = current_surface_size(surface);
        request_full_update(writer, requests, size.width, size.height)?;
        return Ok(());
    }
    PublicationOutcome::NeedsFullSnapshot => {
        let guard = surface.lock().unwrap();
        publisher
            .republish_full_snapshot(protocol_runtime, &guard, receiver.generation)
            .map_err(|error| anyhow::anyhow!(error.code()))?;
    }
    PublicationOutcome::Published
    | PublicationOutcome::IgnoredStale
    | PublicationOutcome::AwaitingHighPerformance => {}
}
```

Include `AwaitingHighPerformance` only if the High Performance plan has already
introduced it; otherwise the match has the four currently defined variants.
Release the surface lock before any Apple writer call. After successful local
republication, continue the existing full/partial boundary path so exactly one
normal next-incremental request is sent.

- [ ] **Step 4: Run GREEN and baseline-loss regressions**

```powershell
cargo +stable test -p frd-protocol-apple mailbox_snapshot_recovery_ -- --nocapture
cargo +stable test -p frd-protocol-apple needs_full_baseline -- --nocapture
cargo +stable test -p frd-protocol-apple type_one_cannot_publish_full_baseline -- --nocapture
```

Expected: local overflow preserves decoder state, while true baseline loss still requests a server full frame.

- [ ] **Step 5: Commit reader recovery**

```powershell
git add crates/frd-protocol-apple/src/network_reader.rs
git commit -m "fix: separate Apple snapshot and baseline recovery"
```

### Task 3: Prevent adapter publication before desktop ports are active

**Files:**
- Modify: `crates/frd-shell-desktop/src/application.rs`
- Test: `crates/frd-shell-desktop/src/application.rs`

**Interfaces:**
- Consumes: existing `BackgroundLaunchResult`, `LiveSessionPorts`, protocol worker, and `accept_launch_outcome` transaction.
- Produces: private `ProtocolStartBarrier`, returned by `launch_live_session` and released only after `SessionHost.active` is installed.

- [ ] **Step 1: Add RED lifecycle tests**

Add a test protocol session whose `run` increments an `AtomicUsize` and
immediately publishes one event and one Reset. Add:

```rust
#[test]
fn protocol_worker_waits_until_live_ports_are_installed() {
    host.begin_launch(permit, target, request, notify).unwrap();
    let outcome = receive_launch_outcome();
    assert_eq!(run_count.load(Ordering::Acquire), 0);
    assert!(!host.has_active_ports());
    assert_eq!(host.accept_launch_outcome(outcome, |_| {}).unwrap(), AcceptedLaunchOutcome::Started);
    wait_until(|| run_count.load(Ordering::Acquire) == 1);
    assert!(host.has_active_ports());
}

#[test]
fn cancelled_launch_drops_barrier_without_running_protocol() {
    let outcome = receive_launch_outcome_after_cancel();
    assert_eq!(host.accept_launch_outcome(outcome, |_| {}).unwrap(), AcceptedLaunchOutcome::CancelledStarted);
    assert_eq!(run_count.load(Ordering::Acquire), 0);
}
```

Use existing test channels and bounded waits; do not sleep arbitrarily.

- [ ] **Step 2: Run RED**

```powershell
cargo +stable test -p frd-shell-desktop protocol_worker_waits_ -- --nocapture
cargo +stable test -p frd-shell-desktop cancelled_launch_drops_barrier_ -- --nocapture
```

Expected: current protocol worker runs before `accept_launch_outcome` installs `active`.

- [ ] **Step 3: Add the one-shot start barrier**

Use a private sender wrapper:

```rust
struct ProtocolStartBarrier(Option<mpsc::Sender<()>>);

impl ProtocolStartBarrier {
    fn release(mut self) -> Result<(), SessionHostError> {
        self.0
            .take()
            .expect("protocol start barrier releases once")
            .send(())
            .map_err(|_| SessionHostError::ProtocolStartClosed)
    }
}
```

Add `ProtocolStartClosed` to the private host error path. Add
`start_barrier: ProtocolStartBarrier` to
`BackgroundLaunchResult::Started`. Make `launch_live_session` return
`(LiveSessionCleanup, LiveSessionPorts, ProtocolStartBarrier)`.

The protocol worker must begin with:

```rust
if protocol_start_rx.recv().is_err() {
    return;
}
let exit = catch_unwind(AssertUnwindSafe(|| session.run()))
    .unwrap_or(ProtocolExit::Failed(ProtocolError::Terminal));
```

In the non-cancelled `accept_launch_outcome` branch, install coordinator,
`active`, and cleanup handle first, then call `start_barrier.release()`. In every
cancelled or rejected branch, drop the barrier before cleanup so the waiting
worker exits and joins. Do not change the audio first-frame gate.

- [ ] **Step 4: Run GREEN and existing launch/cleanup tests**

```powershell
cargo +stable test -p frd-shell-desktop protocol_worker_waits_ -- --nocapture
cargo +stable test -p frd-shell-desktop cancelled_launch_drops_barrier_ -- --nocapture
cargo +stable test -p frd-shell-desktop launch -- --nocapture
cargo +stable test -p frd-shell-desktop cleanup -- --nocapture
```

Expected: the worker cannot publish before ports are active, and cancel/fatal cleanup remains bounded.

- [ ] **Step 5: Commit the protocol-neutral barrier**

```powershell
git add crates/frd-shell-desktop/src/application.rs
git commit -m "fix: activate session ports before protocol workers"
```

### Task 4: Verify recovery and update tracked feature state

**Files:**
- Modify: `README.md`
- Modify after live evidence only: `README.md`

**Interfaces:**
- Consumes: Tasks 1-3 and the High Performance session plan.
- Produces: accurate platform-matrix status and bounded black-to-restored live evidence.

- [ ] **Step 1: Mark the Windows-to-Mac recovery state accurately**

Update the relevant README matrix entry to `开发中`, name Apple canonical
snapshot recovery and the protocol-neutral start barrier, and cite this spec.
Do not claim wgpu or live interoperability from unit tests.

- [ ] **Step 2: Run focused and workspace verification**

```powershell
cargo +stable fmt --all -- --check
cargo +stable test -p frd-protocol-apple
cargo +stable test -p frd-shell-desktop
cargo +stable test --workspace --no-default-features
cargo +stable test --workspace
cargo +stable build --workspace --no-default-features
cargo +stable build --workspace
```

Expected: all commands exit 0. No release process is left running by automated tests.

- [ ] **Step 3: Run the bounded black-to-restored live gate**

Launch one current release product process using only the ignored credential
provider. During the stock macOS hardware-display transition, record:

- Apple canonical content revision;
- local snapshot recovery revision when triggered;
- renderer applied and presented revisions;
- confirmed geometry and viewport size.

Pass requires a complete restored remote desktop with no residual black
rectangles, continuing revisions, and no second client. Disconnect normally and
confirm no FreeRemoteDesk process remains.

- [ ] **Step 4: Update evidence status and commit**

Only after the bounded gate passes, change the matching matrix item to
`受限验证`, include date `2026-08-29` and the local evidence path, then commit:

```powershell
git add README.md
git commit -m "docs: record canonical frame recovery evidence"
```

If the gate fails, keep `开发中` and record the exact revision boundary where
progress stopped.
