# Apple High Performance Session Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use
> `superpowers:subagent-driven-development` or
> `superpowers:executing-plans` task by task. Each task is committed and
> independently reviewed before the next task starts.

**Goal:** Make the Apple product adapter fail closed unless the stock Mac
confirms the evidence-bounded High Performance virtual display, then expose
only its confirmed geometry and enable input only after its full baseline is
presented.

**Architecture:** Add an Apple-private typed startup gate, make the Apple
surface publisher pending, isolate the reader from pre-confirmation MVS side
effects, and commit the confirmed reader state only after the existing
confirmed-size full request writes successfully. Runtime readiness is emitted
after generation activation. The product factory accepts only the encrypted
Apple security path. Existing app/presentation/input contracts remain
protocol-neutral.

**Spec:**
`docs/superpowers/specs/2026-08-29-apple-high-performance-session-design.md`

## Global Constraints

- The Mac product route is strict Apple High Performance HPSS/MVS only.
- Do not add Standard, VNC, shared-console, Curtain, Lock Screen, Apple-ID,
  server helper, or second-session fallback behavior.
- Preserve the literal `0x1d`, `0x09`, and non-incremental framebuffer-request
  builders, bytes, sleeps, encrypted writer ownership, and observed startup
  order. Do not infer a new ARD field, flag, subtype, message, or retry packet.
- Initial public geometry comes only from a strictly parsed post-`0x1d`
  `0x451 ServerState`.
- The encrypted-product eligibility check runs before any HPSS application
  write. Shared legacy authentication research APIs may remain, but the product
  factory cannot select them.
- Pending MVS may only be reassembled/discarded. It cannot decode, mutate the
  CPU surface, install tables, update P1 evidence, publish, recover, or write a
  next request.
- All private preparation and the confirmed-size full write happen before
  `ProtocolRuntime::begin_generation`. Public port failure keeps the existing
  terminal/no-rollback contract and never emits readiness.
- Apple-specific state stays in `frd-protocol-apple`; RDP, app reducer, wgpu,
  compositor, and input router remain unchanged.
- Keep automated tests to the core wire/order/state contracts. Live display
  blanking is a separate bounded interoperability gate.
- Never put credentials, addresses, raw pixels, or encrypted payloads in tests,
  logs, docs, or command lines.

---

### Task 1: Add the typed Apple High Performance startup gate

**Files:**

- Create: `crates/frd-protocol-apple/src/high_performance.rs`
- Modify: `crates/frd-protocol-apple/src/lib.rs`
- Test: `crates/frd-protocol-apple/src/high_performance.rs`

**Produces:**

- `APPLE_HIGH_PERFORMANCE_UNAVAILABLE`
- concrete `HighPerformanceUnavailable: std::error::Error`
- `HighPerformanceStartupGate`
- `HighPerformanceConfirmation`
- `HighPerformanceObservation`

- [ ] **Step 1: Add deterministic RED tests**

Use fixed `Instant` values and no sleeps. Cover:

1. pending accepts `ensure_not_timed_out` strictly before five seconds;
2. one strict geometry confirms and exposes its `DisplaySize`;
3. an identical second observation is `Duplicate`;
4. timeout at exactly five seconds transitions to persistent Failed;
5. `observe_server_state_at` at/after the deadline fails even if the previous
   tick was before the deadline;
6. a conflicting observation through the gate fails (reader will not use the
   gate for valid later resize);
7. every failure exposes exactly `apple_high_performance_unavailable`.

Construct real `ServerStateGeometry` fixtures including its required
`record_count` field.

- [ ] **Step 2: Verify RED**

```powershell
cargo +stable test -p frd-protocol-apple strict_startup -- --nocapture
```

Expected: missing module/types or failed new assertions.

- [ ] **Step 3: Implement the minimal three-state gate**

Use private `Awaiting`, `Confirmed`, and `Failed` state. The five-second
deadline starts at the timestamp captured immediately after the successful
`0x1d` write. Store that origin and compare with
`saturating_duration_since`; do not construct a future `Instant` by addition
or leave a representational-overflow panic path.

`observe_server_state_at(geometry, observed_at)` must check the deadline and
geometry in one mutable operation. At/after the deadline it stores Failed and
returns `HighPerformanceUnavailable`. Before the deadline it converts the
already strictly parsed geometry through `DisplaySize::new`. Matching confirmed
geometry is Duplicate; conflicting direct reuse fails. `ensure_not_timed_out`
also persists Failed.

Tests assert the confirmed literal width/height independently of
`DisplaySize::new` and cover zero width or height transitioning to persistent
Failed.

The concrete error type implements `Display`, `Error`, and `code()` without
string parsing or a dependency. It must remain intact through `anyhow` so the
runtime can downcast it later.

- [ ] **Step 4: Run focused GREEN and strict parser guards**

```powershell
cargo +stable test -p frd-protocol-apple strict_startup -- --nocapture
cargo +stable test -p frd-protocol-apple parse_server_state_sizes -- --nocapture
cargo +stable test -p frd-protocol-apple build_set_display_config_preserves_literal_wire_layout -- --nocapture
```

- [ ] **Step 5: Commit**

```text
feat: add Apple High Performance startup gate
```

---

### Task 2: Make Apple surface publication explicitly pending

**Files:**

- Modify/Test: `crates/frd-protocol-apple/src/surface_publisher.rs`
- Modify only the required exhaustive match in
  `crates/frd-protocol-apple/src/network_reader.rs`

**Produces:**

- `AppleSurfacePublisher::pending`
- `AppleSurfacePublisher::activate_initial_generation`
- `AppleSurfacePublisher::is_active`
- private `PublicationOutcome::AwaitingHighPerformance`

- [ ] **Step 1: Add RED publisher tests**

Prove:

1. `pending(session)` performs no event/frame/wake publication;
2. activation publishes generation 1 once with the exact confirmed size;
3. repeated activation fails closed;
4. inactive `publish_committed` and `publish_committed_patch` return
   `AwaitingHighPerformance` before stale/completeness/patch work;
5. inactive canonical snapshot recovery rejects without allocation/publication;
6. the existing `begin` convenience, if retained for focused existing tests,
   is only `pending + activate` and does not create a second behavior.

- [ ] **Step 2: Verify RED**

```powershell
cargo +stable test -p frd-protocol-apple pending_publisher -- --nocapture
cargo +stable test -p frd-protocol-apple awaiting_high_performance -- --nocapture
```

- [ ] **Step 3: Implement pending/active state**

Store generation 1, session, revision zero, no baseline, and no active size
while pending. `activate_initial_generation` calls
`ProtocolRuntime::begin_generation` first and marks the publisher active only
after it succeeds. `begin_next_generation` requires active state.

Check inactive state before any `bgrx_patch`, complete-baseline validation, or
revision work. Add `AwaitingHighPerformance` to every exhaustive private match;
inside canonical snapshot recovery it is an invalid state and returns the
existing frame-port error. In the current reader publication match, the new
variant must return immediately without a boundary/full/incremental wire write;
it is unreachable on the active production constructor until Task 3 moves the
pending short-circuit before decoding. Do not otherwise modify reader behavior
or Task 1/2 canonical recovery semantics.

- [ ] **Step 4: Run GREEN and publisher regressions**

```powershell
cargo +stable test -p frd-protocol-apple pending_publisher -- --nocapture
cargo +stable test -p frd-protocol-apple awaiting_high_performance -- --nocapture
cargo +stable test -p frd-protocol-apple full_snapshot_recovery -- --nocapture
cargo +stable test -p frd-protocol-apple surface_publisher -- --nocapture
```

- [ ] **Step 5: Commit**

```text
feat: defer Apple surface generation publication
```

---

### Task 3: Isolate pending MVS and commit confirmed reader state

**Files:**

- Modify/Test: `crates/frd-protocol-apple/src/network_reader.rs`
- Modify/Test: `crates/frd-protocol-apple/src/runtime.rs`

**Consumes:** Task 1 gate/error and Task 2 pending publisher.

**Produces:** strict pending `NetworkReaderRuntime`, one
`NetworkFrameOutcome::HighPerformanceConfirmed`, confirmed geometry generation
1, delayed readiness, and typed startup failure mapping.

- [ ] **Step 1: Add RED pending-reader tests**

Core tests must prove:

1. constructor publishes no generation/Reset/frame;
2. valid pre-confirmation full, table, and partial MVS records are
   reassembled/discarded without decoder/CPU/P1/publication changes and without
   any next wire request;
3. malformed/truncated/incomplete pre-confirmation MVS causes no recovery write;
4. `service_tick(requested_at + 5s)` returns the concrete typed unavailable
   error without sleeping and does not run dynamic/MVS recovery;
5. malformed initial `0x451` fails immediately as the same typed error;
6. full-write failure during confirmation leaves event/frame/wake recorders
   empty and publisher pending;
7. successful confirmation writes exactly the existing confirmed-size
   non-incremental request first, then publishes one generation/Reset, returns
   one confirmation outcome, clears old assembly/table/in-flight/viewport
   state, and leaves a fresh full request in flight;
8. a matching later ServerState is unchanged and a changed later ServerState
   still uses the existing generation-2 geometry path.

Update existing geometry tests to confirm generation 1 explicitly before
testing resize; do not weaken their drift/transaction assertions.

- [ ] **Step 2: Verify RED**

```powershell
cargo +stable test -p frd-protocol-apple preconfirmation_mvs -- --nocapture
cargo +stable test -p frd-protocol-apple high_performance_confirmation -- --nocapture
cargo +stable test -p frd-protocol-apple malformed_initial_server_state -- --nocapture
```

- [ ] **Step 3: Construct the reader pending**

The reader receives two distinct timestamps:

- successful `0x1d` return time for the five-second gate;
- initial pre-confirmation full-write time for wire-rate limiting.

It creates generation-1 private receiver/request/surface/dynamic state from
`ServerInit` only as a bounded scratch shape and creates a pending publisher.
It does not call `begin_generation`.

While the gate is Awaiting:

- MVS fragments may enter the assembler, but a completed record is discarded
  before `process_complete_mvs_record`;
- assembler/parse failures abort/reset private assembly and continue without a
  writer call;
- media control may be consumed but cannot publish audio state;
- `service_tick` only checks the startup gate deadline;
- viewport/dynamic queues are not serviced.

- [ ] **Step 4: Implement the confirmation transaction**

For the first `Media::State(SERVER_STATE)` while awaiting:

1. run `parse_server_state_geometry` and map any failure directly to
   `HighPerformanceUnavailable`;
2. call `gate.observe_server_state_at(geometry, now)` so deadline and geometry
   are one decision;
3. prepare new generation-1 receiver, replacement `DisplaySurface`, fresh
   `DynamicResolutionRuntime`, empty viewport queue, and a new request state;
4. the new request state retains only the earlier full-write timestamp for
   rate limiting; in-flight/table/generation state is new;
5. write one exact confirmed-size non-incremental request using that prepared
   request state;
6. only after the write succeeds, install private state and call
   `publisher.activate_initial_generation`;
7. return `HighPerformanceConfirmed { size }` exactly once.

Call `observe_initial_server_state(confirmed, confirmed)` on the new dynamic
state so existing P1 evidence can later combine with the confirmed full
baseline. Do not reset media generation 1. If public activation fails, runtime
is terminal and readiness is not emitted.

After confirmation, bypass the startup gate: matching geometry remains
unchanged and changed geometry uses the current `commit_server_geometry`
transaction.

- [ ] **Step 5: Integrate runtime order and typed failure**

In `run_authenticated_session_inner`:

1. check `connection.is_encrypted()` before the first HPSS application write;
2. preserve existing sleeps/builders and write `0x1d -> 0x09 -> initial full`;
3. capture the `0x1d` and initial-full timestamps separately;
4. construct the pending reader;
5. remove early `TransportReady`, capabilities, and audio Starting;
6. before confirmation, do not service media or emit Playing;
7. on `HighPerformanceConfirmed`, publish exactly once in the existing order:
   `TransportReady`, capabilities, then optional audio Starting, before the
   next read;
8. convert peer close while pending to `HighPerformanceUnavailable`; preserve
   `Closed` after confirmation;
9. in `protocol_exit_for_runtime_error`, downcast the concrete typed startup
   error before generic peer-close handling; unrelated errors remain
   `apple_runtime_failed`.

Do not convert the typed startup error to `anyhow!(error.code())`; preserve it
in the error chain.

- [ ] **Step 6: Update existing runtime mocks without weakening them**

Mocks that previously relied on constructor-time Reset must send an encrypted,
strictly valid `0x451` before expecting generation/worker behavior. Add core
runtime assertions:

- before ServerState: no TransportReady/generation/Reset/frame;
- after ServerState: exact fresh full request, one generation/Reset, then one
  readiness event;
- typed timeout mapping is covered through the no-sleep reader tick plus direct
  runtime error mapping;
- pending peer close maps unavailable, confirmed peer close maps Closed.

- [ ] **Step 7: Run focused and full GREEN**

```powershell
cargo +stable test -p frd-protocol-apple preconfirmation_mvs -- --nocapture
cargo +stable test -p frd-protocol-apple high_performance_confirmation -- --nocapture
cargo +stable test -p frd-protocol-apple production_session_ -- --nocapture
cargo +stable test -p frd-protocol-apple server_state -- --nocapture
cargo +stable test -p frd-protocol-apple
```

- [ ] **Step 8: Commit**

```text
feat: require Apple virtual display confirmation
```

---

### Task 4: Restrict the product factory to encrypted Apple sessions

**Files:**

- Modify/Test: `crates/frd-protocol-apple/src/factory.rs`
- Modify only if a named constant must be exposed crate-privately:
  `crates/frd-protocol-apple/src/auth/mod.rs`

- [ ] **Step 1: Add RED eligibility tests**

Prove:

1. an offer containing only legacy Apple types 30/33 is rejected with
   `apple_high_performance_unavailable` before selecting an unencrypted type;
2. an offer containing the encrypted product type selects that existing named
   type and preserves current authentication;
3. `connect_authenticated` rejects any unexpectedly unencrypted established
   result before returning it to runtime;
4. the rejected mock peer observes no HPSS `0x1d`, `0x09`, or framebuffer
   application bytes.

Use existing named security constants; do not duplicate numeric wire values.
Shared research selection helpers and experimental sessions remain unchanged.

- [ ] **Step 2: Verify RED**

```powershell
cargo +stable test -p frd-protocol-apple product_high_performance_security -- --nocapture
cargo +stable test -p frd-protocol-apple unencrypted_product_session -- --nocapture
```

- [ ] **Step 3: Implement product-only eligibility**

Add a product-private offered-type check before `authenticate_negotiated` and a
defence-in-depth `connection.is_encrypted()` check after
`finish_authenticated_session`. Both return the stable typed product error as a
`ProtocolError` code; neither writes an HPSS application frame. Do not change
the generic authentication preference used by research paths.

- [ ] **Step 4: Run GREEN and authentication regressions**

```powershell
cargo +stable test -p frd-protocol-apple product_high_performance_security -- --nocapture
cargo +stable test -p frd-protocol-apple unencrypted_product_session -- --nocapture
cargo +stable test -p frd-protocol-apple auth -- --nocapture
cargo +stable test -p frd-protocol-apple
```

- [ ] **Step 5: Commit**

```text
fix: reject unencrypted Apple product sessions
```

---

### Task 5: Record state, verify, build, and run the bounded Mac gate

**Files:**

- Modify: `README.md`
- Modify: `docs/ARD_SESSION_PROTOCOL.md`
- Add/update only an existing repository-approved local evidence record path;
  do not commit credentials, captures, or raw pixels.

- [ ] **Step 1: Update documentation before live claims**

Mark Windows-client to Mac-server strict High Performance confirmation as
`开发中`, name `frd-protocol-apple`, cite the design, and distinguish ARD wire
evidence from the local fail-closed gate. HPSS authentication/transport alone
is not virtual-display confirmation.

- [ ] **Step 2: Run automated gates**

Run builds/tests with normal Cargo concurrency (no artificial serial setting):

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

These prove implementation/build health only.

- [ ] **Step 3: Build and fingerprint the Windows release**

```powershell
cargo +stable build --release -p frd-shell-desktop
```

Record the absolute executable path, SHA-256, commit, and ensure only one
FreeRemoteDesk client process is used for the gate.

- [ ] **Step 4: Run one bounded Windows-to-Mac gate**

Load credentials only through the ignored local credential/secure-store path.
Verify:

1. same-account login blanks the Mac hardware displays through stock macOS;
2. Windows shows a complete, continuously updating virtual desktop;
3. strict ServerState, Reset, binding, texture, and viewport dimensions agree
   and aspect ratio is preserved;
4. pointer, keyboard, and wheel work only with app/remote focus;
5. disconnect restores the hardware displays;
6. no Standard/VNC/Curtain/Lock or second-session fallback is selected.

If any item fails, keep `开发中`, record the exact stable failure and do not
claim High Performance support.

- [ ] **Step 5: Update evidence-backed matrix and commit**

Only after a successful bounded gate, change the matching matrix cell to
`受限验证` with date `2026-08-29` and the approved evidence path.

```text
docs: record Apple High Performance validation
```
