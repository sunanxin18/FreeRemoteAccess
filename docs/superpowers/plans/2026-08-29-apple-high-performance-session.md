# Apple High Performance Session Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Apple adapter fail closed unless the stock Mac confirms the ARD 3.10 High Performance virtual display, then expose only that confirmed geometry and enable input only after its full baseline is presented.

**Architecture:** Add an Apple-private startup gate and make `AppleSurfacePublisher` pending until a strict post-`0x1d` `ServerState` is accepted. The confirmation transaction resets all pre-confirmation MVS state, publishes generation 1 using the server geometry, emits `TransportReady`, and requests a fresh full MVS baseline; the existing protocol-neutral presentation and input gates remain unchanged.

**Tech Stack:** Rust 2021, Cargo workspace, Apple HPSS/MVS encrypted session, ARD 3.10 literal wire builders, `frd-protocol-api` generation contract, winit/wgpu presentation gate.

**Spec:** `docs/superpowers/specs/2026-08-29-apple-high-performance-session-design.md`

## Global Constraints

- The Mac product route is strict Apple High Performance HPSS/MVS only.
- Do not add Standard, VNC, shared-console, Curtain, Lock Screen, Apple-ID, or server-side fallback behavior.
- Preserve the existing literal `0x1d`, `0x09`, and non-incremental framebuffer-request bytes and writer order.
- Do not infer or add an undocumented ARD field, flag, message, subtype, or retry packet.
- Initial public geometry comes only from a strictly parsed post-`0x1d` `0x451 ServerState`.
- Apple-specific startup state stays in `frd-protocol-apple`; do not modify RDP behavior or add Apple state to the public API, app reducer, renderer, or input router.
- Keep automated coverage focused on the core protocol/state contracts; live interoperability remains a separate evidence gate.
- Never place credentials, target addresses, raw pixels, or encrypted payloads in tests, logs, or documentation.

---

### Task 1: Add the Apple-private High Performance startup gate

**Files:**
- Create: `crates/frd-protocol-apple/src/high_performance.rs`
- Modify: `crates/frd-protocol-apple/src/lib.rs`
- Test: `crates/frd-protocol-apple/src/high_performance.rs`

**Interfaces:**
- Consumes: `DisplaySize`, the successful `0x1d` write timestamp, and strictly parsed `ServerStateGeometry`.
- Produces: `APPLE_HIGH_PERFORMANCE_UNAVAILABLE`, `HighPerformanceStartupGate::new`, `HighPerformanceStartupGate::observe_server_state`, `HighPerformanceStartupGate::ensure_not_timed_out`, and `HighPerformanceConfirmation`.

- [ ] **Step 1: Write the failing startup-gate tests**

Add tests with fixed `Instant` values and no sleeps:

```rust
#[test]
fn strict_startup_waits_for_server_state() {
    let now = Instant::now();
    let gate = HighPerformanceStartupGate::new(now);
    assert!(!gate.is_confirmed());
    assert_eq!(gate.ensure_not_timed_out(now + Duration::from_secs(4)), Ok(()));
}

#[test]
fn strict_startup_accepts_one_valid_server_state() {
    let now = Instant::now();
    let mut gate = HighPerformanceStartupGate::new(now);
    let geometry = ServerStateGeometry { width: 1440, height: 2560 };
    assert_eq!(
        gate.observe_server_state(geometry).unwrap(),
        HighPerformanceObservation::Confirmed(HighPerformanceConfirmation {
            size: DisplaySize::new(1440, 2560).unwrap(),
        })
    );
    assert_eq!(
        gate.observe_server_state(geometry).unwrap(),
        HighPerformanceObservation::Duplicate
    );
}

#[test]
fn strict_startup_times_out_without_server_state() {
    let now = Instant::now();
    let gate = HighPerformanceStartupGate::new(now);
    let error = gate
        .ensure_not_timed_out(now + Duration::from_secs(5))
        .unwrap_err();
    assert_eq!(error.code(), APPLE_HIGH_PERFORMANCE_UNAVAILABLE);
}
```

- [ ] **Step 2: Run the tests to verify RED**

Run:

```powershell
cargo +stable test -p frd-protocol-apple strict_startup -- --nocapture
```

Expected: compilation fails because `HighPerformanceStartupGate` and its result types do not exist.

- [ ] **Step 3: Implement the minimal state machine**

Use exactly these externally consumed shapes:

```rust
pub(crate) const APPLE_HIGH_PERFORMANCE_UNAVAILABLE: &str =
    "apple_high_performance_unavailable";
const HIGH_PERFORMANCE_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HighPerformanceConfirmation {
    pub(crate) size: DisplaySize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HighPerformanceObservation {
    Confirmed(HighPerformanceConfirmation),
    Duplicate,
}

pub(crate) struct HighPerformanceStartupGate {
    requested_at: Instant,
    confirmed: Option<HighPerformanceConfirmation>,
}
```

`observe_server_state` converts the already strictly parsed non-zero `u16` dimensions through `DisplaySize::new`. A second identical observation is `Duplicate`; a conflicting second observation returns the stable unavailable error. `ensure_not_timed_out` accepts all times strictly before the five-second deadline and fails at or after it. The error type implements `std::error::Error` and exposes `code()` without adding a dependency.

- [ ] **Step 4: Run focused GREEN and wire-layout guards**

Run:

```powershell
cargo +stable test -p frd-protocol-apple strict_startup -- --nocapture
cargo +stable test -p frd-protocol-apple build_set_display_config_preserves_literal_wire_layout
cargo +stable test -p frd-protocol-apple parse_server_state_sizes
```

Expected: all tests pass; no ARD builder bytes change.

- [ ] **Step 5: Commit the gate**

```powershell
git add crates/frd-protocol-apple/src/high_performance.rs crates/frd-protocol-apple/src/lib.rs
git commit -m "feat: add Apple High Performance startup gate"
```

### Task 2: Defer public generation until virtual-display confirmation

**Files:**
- Modify: `crates/frd-protocol-apple/src/surface_publisher.rs`
- Modify: `crates/frd-protocol-apple/src/network_reader.rs`
- Test: `crates/frd-protocol-apple/src/surface_publisher.rs`
- Test: `crates/frd-protocol-apple/src/network_reader.rs`

**Interfaces:**
- Consumes: Task 1's `HighPerformanceStartupGate` and `HighPerformanceConfirmation`.
- Produces: `AppleSurfacePublisher::pending`, `AppleSurfacePublisher::activate_initial_generation`, `NetworkFrameOutcome::HighPerformanceConfirmed`, and a generation-1 reset built only from confirmed geometry.

- [ ] **Step 1: Add RED tests for a pending publisher**

Add these focused cases beside existing publisher ordering tests:

```rust
#[test]
fn pending_publisher_emits_no_reset_or_frame() {
    let session = SessionId::new(1).unwrap();
    let publisher = AppleSurfacePublisher::pending(session);
    assert_eq!(publisher.generation(), 1);
    assert!(!publisher.is_active());
    assert!(recorded_updates().is_empty());
}

#[test]
fn confirmed_publisher_activates_generation_one_exactly_once() {
    let session = SessionId::new(1).unwrap();
    let mut publisher = AppleSurfacePublisher::pending(session);
    let mut runtime = recording_runtime(session);
    let size = PixelSize::new(1440, 2560).unwrap();
    publisher.activate_initial_generation(&mut runtime, size).unwrap();
    assert!(publisher.is_active());
    assert_eq!(recorded_generation_events(), vec![(session, 1, size)]);
    assert!(publisher.activate_initial_generation(&mut runtime, size).is_err());
}
```

Add reader tests named:

- `strict_startup_publishes_nothing_before_post_0x1d_server_state`
- `confirmed_server_state_activates_exactly_one_generation_and_fresh_full_request`
- `preconfirmation_mvs_never_publishes_surface`

The confirmation test must assert event/update order:

```text
SurfaceGenerationChanged(session, 1, confirmed_size)
Reset(session, 1, confirmed_size, Bgrx8UnormSrgb)
StageChanged(TransportReady)
```

It must also assert that the next wire write is exactly
`msg_fb_update_request(false, 0, 0, confirmed_width, confirmed_height)`.
The runtime publishes `TransportReady` after that write succeeds and before it
reads another application frame, so the requested baseline cannot be reduced
before the readiness event.

- [ ] **Step 2: Run the focused tests to verify RED**

```powershell
cargo +stable test -p frd-protocol-apple pending_publisher -- --nocapture
cargo +stable test -p frd-protocol-apple confirmed_server_state -- --nocapture
cargo +stable test -p frd-protocol-apple preconfirmation_mvs -- --nocapture
```

Expected: current code publishes generation 1 during `NetworkReaderRuntime::new`, so the new assertions fail.

- [ ] **Step 3: Make `AppleSurfacePublisher` explicitly pending**

Add an `active: bool` field. `pending(session_id)` initializes generation 1,
revision 0, `baseline_established=false`, `active=false` and performs no port
write. `activate_initial_generation` requires `active == false`, calls
`ProtocolRuntime::begin_generation(session_id, 1, size,
PixelFormat::Bgrx8UnormSrgb)`, and only then sets `active=true`.

`publish_committed` and `publish_committed_patch` return the new private
`PublicationOutcome::AwaitingHighPerformance` while inactive. Later
`begin_next_generation` requires the publisher to be active and retains the
existing monotonic generation behavior.

- [ ] **Step 4: Integrate confirmation transaction in the reader**

`NetworkReaderRuntime::new` constructs private generation-1 receiver,
request, surface, media-generation, dynamic-resolution, and pending publisher
state, but does not call `ProtocolRuntime::begin_generation`.

On the first strict `parse_server_state_geometry` success while the gate is
awaiting:

```rust
let confirmed = gate.observe_server_state(geometry)?;
let replacement = DisplaySurface::new(1, confirmed_pixel_size)?;
receiver.reset(1);
requests.reset_generation(1);
*surface.lock().unwrap() = replacement;
dynamic_resolution = DynamicResolutionRuntime::new(confirmed.size, opt_in);
publisher.activate_initial_generation(protocol_runtime, confirmed_pixel_size)?;
request_full_update(writer, &mut requests, confirmed.size.width, confirmed.size.height)?;
return Ok(NetworkFrameOutcome::HighPerformanceConfirmed {
    size: confirmed.size,
});
```

All allocations and strict parsing happen before publisher activation. Media
generation remains 1; do not call its monotonic `reset_generation(1)`. Existing
media control may remain idle or active, but it cannot publish a display frame.

When a complete MVS record yields `AwaitingHighPerformance`, consume it without
publishing a surface or treating it as a baseline. Confirmation resets the
assembler/decoder and sends the fresh full request.

- [ ] **Step 5: Keep later geometry handling unchanged**

After the startup gate is confirmed, duplicate matching `ServerState` records
are idempotent and later changed geometry goes through the existing
`commit_server_geometry` transaction. Run the current generation-drift and
pending-geometry tests unchanged.

- [ ] **Step 6: Run focused GREEN**

```powershell
cargo +stable test -p frd-protocol-apple pending_publisher -- --nocapture
cargo +stable test -p frd-protocol-apple confirmed_server_state -- --nocapture
cargo +stable test -p frd-protocol-apple preconfirmation_mvs -- --nocapture
cargo +stable test -p frd-protocol-apple server_state -- --nocapture
```

Expected: all focused startup and existing geometry tests pass.

- [ ] **Step 7: Commit deferred publication**

```powershell
git add crates/frd-protocol-apple/src/surface_publisher.rs crates/frd-protocol-apple/src/network_reader.rs
git commit -m "feat: require Apple virtual display confirmation"
```

### Task 3: Publish readiness only after confirmation and fail closed

**Files:**
- Modify: `crates/frd-protocol-apple/src/runtime.rs`
- Modify: `crates/frd-protocol-apple/src/factory.rs`
- Test: `crates/frd-protocol-apple/src/runtime.rs`
- Test: `crates/frd-protocol-apple/src/factory.rs`

**Interfaces:**
- Consumes: Task 2's `NetworkFrameOutcome::HighPerformanceConfirmed` and Task 1's stable failure code.
- Produces: delayed `TransportReady`, delayed capabilities, and a typed `ProtocolExit::Failed` with `apple_high_performance_unavailable`.

- [ ] **Step 1: Add RED runtime-order and failure tests**

Replace the old startup test that expects a Reset without `ServerState`. Add:

```rust
#[test]
fn production_session_waits_for_high_performance_confirmation() {
    // Mock server completes authentication and observes 0x1d/0x09/initial full request.
    // Before sending ServerState, event and frame recorders contain no
    // TransportReady, SurfaceGenerationChanged, Reset, Damage, or Boundary.
}

#[test]
fn production_session_confirms_then_requests_fresh_baseline() {
    // After strict 0x451 geometry, assert one Reset with that geometry,
    // TransportReady, and a second exact non-incremental request for it.
}

#[test]
fn missing_server_state_fails_as_high_performance_unavailable() {
    // Advance the injected clock to the five-second deadline.
    assert_eq!(exit, ProtocolExit::Failed(ProtocolError::adapter(
        ProtocolId::apple_hpss_mvs(),
        "apple_high_performance_unavailable",
    )));
}

#[test]
fn unencrypted_product_session_writes_no_hpss_application_frame() {
    // A completed compatibility authentication without session encryption
    // must fail before 0x1d, 0x09, or framebuffer-request bytes are observed.
}
```

Use the existing runtime mock connection and injected observation hooks; do not
sleep five real seconds.

- [ ] **Step 2: Run RED**

```powershell
cargo +stable test -p frd-protocol-apple production_session_ -- --nocapture
cargo +stable test -p frd-protocol-apple missing_server_state_ -- --nocapture
cargo +stable test -p frd-protocol-apple unencrypted_product_session_ -- --nocapture
```

Expected: current runtime emits `TransportReady` before `0x1d` and maps every
runtime error to `apple_runtime_failed`.

- [ ] **Step 3: Move readiness publication to the confirmation outcome**

Move the existing `connection.is_encrypted()` check ahead of `0x1d`, `0x09`,
the initial framebuffer request, `TransportReady`, capabilities, and audio
state. Ensure the product factory cannot treat an authenticated but unencrypted
compatibility session as an eligible High Performance session. Experimental
helpers may remain available only outside the product factory path.

Remove the early `TransportReady` and capabilities writes at the top of
`run_authenticated_session_inner`. When `handle_frame` returns
`HighPerformanceConfirmed`, publish exactly once:

```rust
runtime.publish_event(SessionEvent::StageChanged(ConnectionStage::TransportReady))?;
runtime.publish_event(SessionEvent::CapabilitiesChanged(SessionCapabilities {
    dynamic_resolution: dynamic_resolution_enabled,
    remote_audio: udp_media_enabled,
    text_input: true,
    ..SessionCapabilities::default()
}))?;
```

Audio `Starting` may be emitted only after this same confirmation. Keep the
existing encrypted writer and command loop ownership.

- [ ] **Step 4: Map only the startup-gate error to the stable code**

Update `protocol_exit_for_runtime_error` so peer close remains `Closed`, a
downcast `HighPerformanceUnavailable` becomes
`apple_high_performance_unavailable`, and unrelated errors remain
`apple_runtime_failed`. Do not parse display strings or error text.

- [ ] **Step 5: Run GREEN and protocol-neutral gate regressions**

```powershell
cargo +stable test -p frd-protocol-apple production_session_ -- --nocapture
cargo +stable test -p frd-protocol-apple missing_server_state_ -- --nocapture
cargo +stable test -p frd-protocol-apple unencrypted_product_session_ -- --nocapture
cargo +stable test -p frd-app current_full_baseline_presentation_enters_remote_session
cargo +stable test -p frd-shell-desktop input
```

Expected: readiness occurs only after confirmation; existing app presentation
and input gates pass without Apple-specific changes.

- [ ] **Step 6: Commit runtime fail-closed behavior**

```powershell
git add crates/frd-protocol-apple/src/runtime.rs crates/frd-protocol-apple/src/factory.rs
git commit -m "fix: fail closed outside Apple High Performance"
```

### Task 4: Record feature state and run bounded verification

**Files:**
- Modify: `README.md`
- Modify: `docs/ARD_SESSION_PROTOCOL.md`
- Modify after live evidence only: `README.md`

**Interfaces:**
- Consumes: Tasks 1-3 and the existing Windows product composition.
- Produces: accurate platform-matrix state and one bounded live evidence record.

- [ ] **Step 1: Update documentation before claiming live support**

In the Windows-client-to-Mac-server matrix, record strict High Performance
virtual-display confirmation as `开发中`, name `frd-protocol-apple`, and cite
this spec. Add to `docs/ARD_SESSION_PROTOCOL.md` that HPSS transport alone is
not a product-level virtual-display confirmation and that the existing `0x1d`
and `0x451` evidence is unchanged.

- [ ] **Step 2: Run the focused and workspace gates**

```powershell
cargo +stable fmt --all -- --check
cargo +stable test -p frd-protocol-apple
cargo +stable test -p frd-app
cargo +stable test -p frd-shell-desktop
cargo +stable test --workspace --no-default-features
cargo +stable test --workspace
cargo +stable build --workspace --no-default-features
cargo +stable build --workspace
cargo +stable run -- --help
cargo +stable run -- hpssview --help
```

Expected: all commands exit 0. These commands prove implementation and build
health, not hardware-display blanking.

- [ ] **Step 3: Build the Windows release client**

```powershell
cargo +stable build --release -p frd-shell-desktop
```

Expected: the product executable is rebuilt from the current commit. Record its
absolute path and SHA-256 before launch.

- [ ] **Step 4: Run one bounded Windows-to-Mac interoperability gate**

Use credentials only through the ignored local credential provider. Verify:

1. one client process and one Apple session;
2. same-account login blanks the Mac hardware displays;
3. the client shows a complete non-black virtual desktop;
4. reported ServerState, Reset, RemoteBinding, texture, and viewport dimensions
   agree;
5. keyboard, pointer, and wheel work only with app/remote focus;
6. disconnect restores the hardware displays;
7. no fallback protocol is selected.

If any item fails, retain `开发中` and record the exact failure without
claiming completion.

- [ ] **Step 5: Update the matrix from actual evidence and commit**

On a successful bounded run, change only the matching matrix cell to
`受限验证`, include date `2026-08-29` and the evidence path. Then commit:

```powershell
git add README.md docs/ARD_SESSION_PROTOCOL.md
git commit -m "docs: record Apple High Performance validation"
```

Expected: documentation distinguishes offline tests, release build, and live
interoperability.
