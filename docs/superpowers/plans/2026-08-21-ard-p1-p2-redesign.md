# ARD P1/P2 Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement evidence-bounded dynamic-resolution generation switching
and robust MVS record/decode state without guessing Apple's partial bitstream.

**Architecture:** The transport layer first reassembles MVS payloads to their
declared length. A generation-aware decoder state classifies full/partial
payloads and requests conservative resynchronization. A separate pure dynamic
resolution controller commits geometry only after a matching server
acknowledgement, and the viewer atomically switches its framebuffer, input
mapping, and decoder generation.

**Tech Stack:** Rust 2021, `anyhow`, built-in Rust tests, `minifb` 0.28 behind
the `viewer` feature, existing `jpeg-decoder` full-frame path.

**Spec:** `docs/superpowers/specs/2026-08-21-ard-p1-p2-redesign.md`

## Global Constraints

- Do not decode type-1 MVS payloads as JPEG.
- Do not infer unverified partial-update fields or marker offsets.
- Dynamic resizing is opt-in through `--dynamic-resolution`; default is off.
- Viewport debounce is 250 ms; resize acknowledgement timeout is two seconds;
  full-frame resync is limited to one request per 200 ms.
- Candidate dimensions are at least 64 pixels per axis, fit in `u16`, and are
  rounded down to multiples of eight.
- User-facing messages and comments remain Simplified Chinese; Rust identifiers
  remain English.
- No credential or target-machine detail may enter source, docs, tests,
  commands, or logs.
- This checkout has no `.git`; tasks end with test evidence and SDD ledger
  entries rather than commits.

---

### Task 0: Unblock the Existing Compiler Gates

**Files:**
- Modify: `src/main.rs`
- Modify: `src/vnc/hpss_viewer.rs`

**Interfaces:**
- Produces a compilable baseline so focused tests in Tasks 1-2 can execute.
- Does not change protocol behavior or the heuristic MVS path.

- [ ] **Step 1: Confirm the existing RED compiler failures**

Run: `cargo test`

Expected: E0594 for assignments to immutable reader-local `w` and `h`.

Run: `cargo build --no-default-features`

Expected: E0432 for the viewer-only import inside `cmd_hpss`.

- [ ] **Step 2: Apply the minimal source fixes**

Delete the unused `use crate::vnc::hpss_viewer;` inside `cmd_hpss`. Change only
the reader closure's captured dimension bindings from `let w`/`let h` to
`let mut w`/`let mut h`. Do not alter the receive heuristic; Task 3 owns it.

- [ ] **Step 3: Verify the compiler gates and Task 1 tests**

Run: `cargo test mvs_stream::tests --no-default-features`

Expected: all Task 1 focused tests execute and pass.

Run: `cargo test mvs_stream::tests`

Expected: all Task 1 focused tests execute and pass with the viewer feature.

### Task 1: Exact MVS Record Reassembly

**Files:**
- Create: `src/vnc/mvs_stream.rs`
- Modify: `src/vnc/mod.rs`

**Interfaces:**
- Produces: `MvsRect`, `MvsRecord`, and `MvsRecordAssembler`.
- `MvsRecordAssembler::begin(&mut self, rect: MvsRect, total: u32,
  first: &[u8]) -> anyhow::Result<Option<MvsRecord>>`.
- `MvsRecordAssembler::push_continuation(&mut self, chunk: &[u8]) ->
  anyhow::Result<Option<MvsRecord>>`.
- `is_pending(&self) -> bool` and `abort(&mut self)`.

- [ ] **Step 1: Write the failing fragmentation tests**

Add inline tests whose hand-derived fixtures assert:

```rust
#[test]
fn reassembles_captured_fragment_lengths_without_dropping_bytes() {
    let rect = MvsRect { x: 0, y: 1256, width: 1358, height: 1112 };
    let first = vec![0x11; 32_748];
    let continuation = vec![0x22; 26_572];
    let mut assembler = MvsRecordAssembler::default();

    assert!(assembler.begin(rect, 59_320, &first).unwrap().is_none());
    let record = assembler.push_continuation(&continuation).unwrap().unwrap();

    assert_eq!(record.rect, rect);
    assert_eq!(record.payload.len(), 59_320);
    assert_eq!(&record.payload[..32_748], &first);
    assert_eq!(&record.payload[32_748..], &continuation);
}
```

Also add separate tests for first-chunk overflow, continuation overflow,
continuation without a start, and a second start while pending. Each mutation
would otherwise accept ambiguous wire boundaries.

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test mvs_stream::tests --no-default-features`

Expected: compilation fails because `mvs_stream` and its types do not exist.

- [ ] **Step 3: Implement the minimal assembler**

Use a private pending record containing `rect`, `total: usize`, and `payload`.
Reject `total == 0`, any append exceeding `total`, or invalid call ordering.
Return a complete record only when payload length equals `total`. Clear pending
state on structural errors so future frames can resynchronize.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test mvs_stream::tests --no-default-features`

Expected: all `mvs_stream` tests pass.

### Task 2: Strict MVS Payload and Generation State

**Files:**
- Modify: `src/vnc/mvs.rs`

**Interfaces:**
- Produces `MvsPayload<'a> { Full { metadata: u32, entropy: &'a [u8] },
  Partial { encoded: &'a [u8] } }`.
- Produces `parse_mvs_payload(&[u8]) -> anyhow::Result<MvsPayload<'_>>`.
- Produces `MvsDecodeState`, `MvsDecodeDecision<'a>`, and `MvsResyncReason`.
- `MvsDecodeState::new(generation: u64)`, `reset(generation)`,
  `install_tables(generation, init)`, `decide(generation, payload)`,
  `mark_full_applied(generation)`, `tables()`, and `awaiting_full()`.

- [ ] **Step 1: Write failing parser/state tests**

Add literal fixtures proving:

```rust
#[test]
fn parses_full_prefix_and_metadata() {
    let payload = [0x00, 0x0f, 0x19, 0x01, 0x02, 0x03, 0x04, 0xaa, 0xbb];
    let parsed = parse_mvs_payload(&payload).unwrap();
    assert_eq!(parsed, MvsPayload::Full {
        metadata: 0x0102_0304,
        entropy: &[0xaa, 0xbb],
    });
}

#[test]
fn partial_update_requests_full_instead_of_jpeg_decode() {
    let mut state = MvsDecodeState::new(7);
    state.install_tables(7, &[1; 129]).unwrap();
    let decision = state.decide(7, &[0x01, 0x0e, 0x13, 0xaa]).unwrap();
    assert_eq!(decision, MvsDecodeDecision::RequestFull(
        MvsResyncReason::UnsupportedPartial,
    ));
}
```

Add independent tests for malformed prefixes, missing tables, stale generation,
reset clearing tables/reference state, and successful `mark_full_applied`.

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test vnc::mvs::tests --no-default-features`

Expected: compilation fails on the new missing parser/state symbols.

- [ ] **Step 3: Implement strict classification and conservative decisions**

Full payloads require at least seven bytes. Partial payloads require at least
three bytes and remain opaque. Unknown prefixes return an error. A stale
generation returns `IgnoreStale`; missing tables, malformed payloads, and
partial payloads return `RequestFull`. `DecodeFull` exposes only the verified
entropy slice and never mutates reference readiness before
`mark_full_applied`.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test vnc::mvs::tests --no-default-features`

Expected: all MVS tests pass.

### Task 3: Integrate P2 and Restore Build Gates

**Files:**
- Modify: `src/vnc/hpss_viewer.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes `MvsRecordAssembler` and `MvsDecodeState` from Tasks 1-2.
- The reader consumes continuation application frames before `parse_media`.
- A full record enters `decode_mvs_block`; partial/malformed records request a
  non-incremental update at most once per 200 ms.

- [ ] **Step 1: Preserve the existing compiler failures as RED evidence**

Run: `cargo test`

Expected: E0594 at `hpss_viewer.rs` when assigning startup `w`/`h`.

Run: `cargo build --no-default-features`

Expected: E0432 from the viewer-only import inside `cmd_hpss`.

- [ ] **Step 2: Replace the heuristic receive path**

When `assembler.is_pending()`, call `push_continuation(&msg)` before heartbeat
or media classification. For a new MVS media header, call `begin` with its
declared `total`. Process only returned complete records. Remove
`assembled[4..]`, `total - 4`, and the branch that strips a type-1 prefix before
JPEG decoding.

Use `MvsDecodeState::decide` for every complete record. Only `DecodeFull`
calls `decode_mvs_block`; after a successful framebuffer apply, call
`mark_full_applied`. `RequestFull` aborts fragments and sends
`msg_fb_update_request(false, ...)` subject to the 200 ms limit.

- [ ] **Step 3: Apply the minimal build-gate repairs**

Remove the unused viewer-only import from `cmd_hpss`. Until Task 5 replaces
startup geometry with shared state, make the reader closure's local dimensions
mutable. Do not change unrelated warning sites.

- [ ] **Step 4: Verify the integrated P2 path**

Run: `cargo test`

Expected: all tests pass.

Run: `cargo build --no-default-features`

Expected: build succeeds.

### Task 4: Pure Dynamic Resolution Controller

**Files:**
- Create: `src/vnc/dynamic_resolution.rs`
- Modify: `src/vnc/mod.rs`

**Interfaces:**
- Produces `DisplaySize`, `DynamicResolutionCapability`,
  `DynamicResolutionController`, `DynamicResolutionState`,
  `ResolutionRequest`, and `GeometryCommit`.
- `request_target(size) -> Option<ResolutionRequest>` creates a pending next
  generation only from enabled stable state.
- `observe_server_state(size) -> Option<GeometryCommit>` commits only an exact
  pending target.
- `mark_full_frame(generation)` completes switching.
- `timeout_pending()` rolls back without changing the stable generation.

- [ ] **Step 1: Write failing state-machine tests**

Add independent tests that prove:

```rust
#[test]
fn matching_ack_commits_exactly_one_generation() {
    let mut controller = enabled_controller(DisplaySize::new(1440, 2560).unwrap());
    let target = DisplaySize::new(1280, 720).unwrap();
    let request = controller.request_target(target).unwrap();
    assert_eq!(request.generation, 1);

    assert!(controller.observe_server_state(DisplaySize::new(1024, 768).unwrap()).is_none());
    let commit = controller.observe_server_state(target).unwrap();
    assert_eq!(commit.generation, 1);
    assert_eq!(commit.size, target);
    assert!(controller.observe_server_state(target).is_none());
}
```

Also test every unavailable capability, disabled mode, duplicate target,
viewport alignment/clamping, timeout rollback, and wrong-generation full-frame
completion.

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test dynamic_resolution::tests --no-default-features`

Expected: compilation fails because the controller module does not exist.

- [ ] **Step 3: Implement the pure controller**

Keep stable generation/size separately from transient state so timeout cannot
accidentally advance them. A request uses `stable_generation + 1`. A matching
ack updates stable generation/size and enters `Switching`; only a matching full
frame returns to `Stable`. `DisplaySize::from_viewport` validates the minimum,
clamps to `u16::MAX`, and rounds both axes down to multiples of eight.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test dynamic_resolution::tests --no-default-features`

Expected: all dynamic-resolution tests pass.

### Task 5: Integrate P1 with Viewer Geometry and CLI

**Files:**
- Modify: `src/vnc/hpss.rs`
- Modify: `src/vnc/hpss_viewer.rs`
- Modify: `src/main.rs`

**Interfaces:**
- `hpss::build_display_query(DisplaySize) -> Vec<u8>` centralizes the existing
  16-byte `0x09` message.
- `run_viewer` receives `dynamic_resolution: bool`.
- `DisplaySurface` contains generation and framebuffer under one mutex.
- CLI `hpssview --dynamic-resolution` is an opt-in boolean.

- [ ] **Step 1: Write failing protocol-builder and pointer-mapping tests**

In `hpss.rs`, add a literal test asserting a 1920x1080 query equals:

```rust
[
    0x09, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0,
    0x07, 0x80, 0x04, 0x38,
]
```

Extract a pure `map_pointer(window_x, window_y, window_size, display_size)`
helper in `hpss_viewer.rs`. Test that the bottom-right point of a 640x360
window maps to `(1279, 719)` for a 1280x720 display and that coordinates clamp.

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test build_display_query --no-default-features`

Expected: fails because the builder does not exist.

Run: `cargo test map_pointer`

Expected: fails because the pure mapping helper does not exist.

- [ ] **Step 3: Implement shared display surface and opt-in resize requests**

Create a resizable minifb window. Render using `window.get_size()` every loop,
resize the scratch buffer to that exact drawable size, and map pointer input
from current window size to the current `DisplaySurface` size.

When `--dynamic-resolution` is enabled, debounce window-size changes for 250
ms. Pass the aligned size to `request_target`; send
`build_display_query(request.target)`. Record the request time. Feed every
`ServerState` size into `observe_server_state`; only a returned commit resets
the assembler and MVS decoder, atomically replaces `DisplaySurface`, and sends
a non-incremental full request. After two seconds without a matching ack, call
`timeout_pending` and keep the old surface.

- [ ] **Step 4: Wire the CLI flag**

Add `dynamic_resolution: bool` to `Cmd::Hpssview`, pass it through
`cmd_hpssview`, and then into `run_viewer`. The flag defaults to false through
Clap's boolean behavior.

- [ ] **Step 5: Verify P1 integration**

Run: `cargo test`

Expected: all tests pass.

Run: `cargo build --no-default-features`

Expected: build succeeds.

Run: `cargo run -- hpssview --help`

Expected: help lists `--dynamic-resolution` and contains no credential values.

### Task 6: Documentation and Full Verification

**Files:**
- Modify: `AGENTS.md`
- Modify: `docs/ARD_SESSION_PROTOCOL.md`

**Interfaces:**
- Documents the implemented safe P1/P2 behavior and preserves the exact
  verified/unverified boundary from the spec.

- [ ] **Step 1: Update project status**

Change P1 from build-breaking WIP to opt-in confirmation-driven switching.
Change P2 from heuristic partial-as-JPEG to exact record reassembly with safe
full resync. State explicitly that exact partial decoding remains blocked on a
real fixture or complete decoder recovery.

- [ ] **Step 2: Run formatting and the complete verification matrix**

Run in order:

```text
cargo fmt -- --check
cargo test --no-default-features
cargo test
cargo build --release
cargo build --no-default-features
cargo run -- --help
cargo run -- hpssview --help
```

Expected: every command exits zero; test output contains zero failures; help
contains no real target or credential data.

- [ ] **Step 3: Review acceptance criteria against the spec**

Read the spec's Acceptance Criteria and record each item as verified or still
blocked in the SDD report. Do not claim exact partial decoding or live dynamic
resolution without a real target validation run.
