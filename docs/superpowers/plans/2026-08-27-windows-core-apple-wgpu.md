# Windows Core, Apple HPSS, and wgpu Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Windows single-window winit/egui/wgpu client and preserve the verified `main@e2d1741` Apple username/password HPSS/MVS picture, input, dynamic-resolution, UDP, and Mac-to-PC audio behavior.

**Architecture:** Introduce protocol-neutral core/frame/session/application contracts, isolate shared stateless RFB wire parsing, then move the current Apple implementation behind `ProtocolFactory`. Pixel payloads move through a bounded mailbox into a dedicated wgpu remote pass; a single compositor owns the Windows Surface and egui overlay. The old minifb path remains only as a separately launched comparison package until the final Windows cutover plan.

**Tech Stack:** Rust 2021, Rust 1.96.0, winit 0.30.13, wgpu 30.0.1 (DX12 + WGSL), egui/egui-winit/egui-wgpu 0.36.1, raw-window-handle 0.6.2, existing Apple HPSS/MVS/SRTP/AAC-ELD implementation.

**Spec:** `docs/superpowers/specs/2026-08-27-winit-wgpu-windows-first-architecture-design.md`

## Global Constraints

- Start implementation in an isolated worktree created with `superpowers:using-git-worktrees`; base it on `main` containing spec commit `1ab5c76`.
- Do not merge or copy the historical `feat/five-platform-client` protocol implementation. `main@e2d1741` is the Apple/MVS/input behavior baseline.
- Mac product connections are Apple HPSS/MVS only; no VNC fallback, Apple ID, IDS, APNs, QuickRelay, or server helper.
- Apple 30/33/35/36 authentication, SessionCrypto, HPSS/MVS, media wire, and dynamic-resolution state stay in `frd-protocol-apple`.
- Protocol adapters may share only stateless `frd-wire-rfb` and protocol-neutral `frd-media-api`; they never depend on each other.
- Renderer and UI never import a concrete protocol crate. Only `apps/freeremotedesk-windows` registers factories.
- Pixel payloads and secrets are non-`Clone`; no full-frame clone is introduced to cross a thread boundary.
- `frd-app` owns UI/session/presentation state and input gates; renderer owns no UI or input policy.
- Preserve current type-1 decode, Cb/Cr ordering, pointer-outside silence, P3/P4 behavior, and P5 fail-closed behavior.
- Add only contract/protocol tests named below; do not add GUI snapshot or broad combinatorial tests.
- Credentials come from secure UI/credential providers and never appear in argv, logs, fixtures, captures, or commits.
- The winit/egui window is the only product login/session entry. CLI options only prefill one `ConnectionDraft`; missing fields always remain editable in the same form.

## File Structure

Create these packages as their first real task requires them:

```text
apps/freeremotedesk-windows/       Windows composition root
crates/frd-core/                   IDs, geometry, input, secrets, errors
crates/frd-frame/                  SurfaceUpdate, canonical surface, mailbox
crates/frd-media-api/              PCM frames and media ports
crates/frd-wire-rfb/               stateless RFB wire codec
crates/frd-protocol-api/           descriptors, factories, runtime ports
crates/frd-protocol-apple/         current Apple implementation
crates/frd-session/                coordinator and worker lifecycle
crates/frd-app/                    AppIntent reducer and ActiveSessionSlot
crates/frd-render-wgpu/            remote texture/upload/pass
crates/frd-compositor-wgpu/        Surface lease and present owner
crates/frd-ui-model/               copy-safe display state
crates/frd-ui-egui/                forms and toolbar
crates/frd-shell-desktop/          winit ApplicationHandler
crates/frd-platform-api/           credential/audio/clipboard ports
crates/frd-platform-windows/       Windows implementations
tools/frd-legacy-minifb-lab/       serialized comparison viewer
```

The existing root package remains the headless protocol lab during this plan. Product code must not depend on it.

---

### Task 1: Establish the workspace and immutable core identifiers

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Create: `rust-toolchain.toml`
- Create: `crates/frd-core/Cargo.toml`
- Create: `crates/frd-core/src/lib.rs`
- Create: `crates/frd-core/src/geometry.rs`
- Create: `crates/frd-core/src/session.rs`
- Create: `crates/frd-core/src/secret.rs`

**Interfaces:**
- Produces: `SessionId`, `PixelSize`, `PixelPoint`, `PixelRect`, `PhysicalViewport`, `SecretBuffer`, `SecretBytes`.
- Consumes: none.

- [ ] **Step 1: Add the workspace and the failing core tests**

Add `[workspace]` with resolver `2`, retain the existing root package, and add only `crates/frd-core` first. Pin the compatible dependency group in `[workspace.dependencies]`:

```toml
[workspace]
resolver = "2"
members = ["crates/frd-core"]

[workspace.dependencies]
winit = "=0.30.13"
wgpu = { version = "=30.0.1", default-features = false }
egui = "=0.36.1"
egui-winit = { version = "=0.36.1", default-features = false }
egui-wgpu = { version = "=0.36.1", default-features = false }
raw-window-handle = "=0.6.2"
```

Pin `channel = "1.96.0"` in `rust-toolchain.toml`. In `frd-core`, write tests first for nonzero geometry, checked rectangle bounds, monotonically allocated nonzero session IDs, `take()` leaving the source empty, and secret clearing when the transferred owner drops.

- [ ] **Step 2: Run the core tests and verify RED**

Run: `cargo test -p frd-core`

Expected: compile failures for the not-yet-defined types and methods.

- [ ] **Step 3: Implement the minimum core types**

Use these public signatures:

```rust
pub struct SessionId(std::num::NonZeroU64);
impl SessionId {
    pub fn allocate() -> Self;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelSize { pub width: u32, pub height: u32 }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelPoint { pub x: u32, pub y: u32 }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelRect { pub x: u32, pub y: u32, pub width: u32, pub height: u32 }

pub struct PhysicalViewport {
    pub drawable: PixelSize,
    pub content: PixelRect,
    pub remote: PixelSize,
}

pub struct SecretBuffer(Vec<u8>);
pub struct SecretBytes(Vec<u8>);
impl SecretBuffer {
    pub fn new(bytes: Vec<u8>) -> Self;
    pub fn take(&mut self) -> SecretBytes;
    pub fn is_empty(&self) -> bool;
}
impl SecretBytes {
    pub fn expose(&self) -> &[u8];
}
```

Neither secret type may derive `Clone` or `Debug`. `SecretBuffer::take` moves the allocation into `SecretBytes` and leaves the source empty; it must not erase the bytes before the consumer uses them. Both owners overwrite any allocation they still hold in `Drop`, so the final transferred owner performs the actual clearing.

- [ ] **Step 4: Run core tests and workspace regression tests**

Run: `cargo test -p frd-core`

Expected: PASS.

Run: `cargo test`

Expected: the existing root tests still pass.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml crates/frd-core
git commit -m "refactor: establish remote desktop workspace core"
```

### Task 2: Extract protocol-neutral input and viewport behavior

**Files:**
- Create: `crates/frd-core/src/input.rs`
- Create: `crates/frd-core/src/viewport.rs`
- Modify: `crates/frd-core/src/lib.rs`
- Modify: `src/pointer_input.rs`
- Modify: `src/viewer.rs`
- Modify: `src/vnc/hpss_viewer.rs`

**Interfaces:**
- Consumes: `PixelPoint`, `PixelRect`, `PixelSize`, `PhysicalViewport` from Task 1.
- Produces: `InputEvent`, `SessionInput`, `PointerInputState`, `ContentViewport::fit`, `ContentViewport::map_pointer`.

- [ ] **Step 1: Move the existing pointer safety tests into `frd-core` and add viewport tests**

Preserve the existing cases from `src/pointer_input.rs` and add assertions for aspect-fit landscape/portrait/1:1 mappings. Define expected API usage in tests:

```rust
let viewport = ContentViewport::fit(
    PixelSize { width: 2560, height: 1440 },
    PixelSize { width: 1280, height: 720 },
);
assert_eq!(viewport.map_pointer(640.0, 360.0), Some(PixelPoint { x: 1280, y: 720 }));
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run: `cargo test -p frd-core input`

Run: `cargo test -p frd-core viewport`

Expected: compile failures for the missing input/viewport types.

- [ ] **Step 3: Implement the normalized input contract**

```rust
pub enum InputEvent {
    PointerMove { remote: PixelPoint },
    PointerButton { button: PointerButton, state: ButtonState },
    Wheel { delta_x: f32, delta_y: f32 },
    PhysicalKey { code: PhysicalKeyCode, state: KeyState, modifiers: Modifiers },
    Text { utf8: String },
    ReleaseAll,
}

pub struct SessionInput {
    pub session_id: SessionId,
    pub generation: u64,
    pub event: InputEvent,
}
```

Move `PointerInputState` behavior, not minifb key types, into `frd-core`. Keep one release on drag-out, no re-press on held re-entry, and release-all on focus loss.

- [ ] **Step 4: Adapt legacy viewers to the extracted pointer state without changing behavior**

Replace `crate::pointer_input` uses with `frd_core::input`. Leave minifb event conversion local to the legacy viewers.

- [ ] **Step 5: Run focused and full tests**

Run: `cargo test -p frd-core`

Expected: PASS.

Run: `cargo test`

Expected: PASS with the existing pointer tests removed from the root duplicate.

- [ ] **Step 6: Commit**

```bash
git add crates/frd-core src/pointer_input.rs src/viewer.rs src/vnc/hpss_viewer.rs
git commit -m "refactor: extract protocol-neutral input mapping"
```

### Task 3: Implement the generation-bound frame contract and bounded mailbox

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/frd-frame/Cargo.toml`
- Create: `crates/frd-frame/src/lib.rs`
- Create: `crates/frd-frame/src/surface.rs`
- Create: `crates/frd-frame/src/mailbox.rs`

**Interfaces:**
- Consumes: `SessionId`, geometry types.
- Produces: `PixelFormat`, `PixelBuffer`, `PixelPatch`, `FrameCompleteness`, `SurfaceUpdate`, `FrameMailbox`, `PushOutcome`.

- [ ] **Step 1: Write RED tests for payload validation and mailbox overflow**

Cover exact-length/stride/bounds validation, stale session/generation rejection, non-clone payload ownership, and byte-budget overflow returning `PushOutcome::NeedsFullSnapshot`.

```rust
let outcome = mailbox.push(SurfaceUpdate::Damage { /* current session/gen */ });
assert_eq!(outcome, PushOutcome::Queued);
assert_eq!(mailbox.push(oversized_damage), PushOutcome::NeedsFullSnapshot);
```

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-frame`

Expected: compile failure until the frame types exist.

- [ ] **Step 3: Implement the exact frame types**

```rust
pub enum PixelFormat { Bgrx8UnormSrgb, Bgra8UnormSrgb, Rgba8UnormSrgb }
pub struct PixelBuffer(Box<[u8]>); // deliberately no Clone
pub struct PixelPatch { pub rect: PixelRect, pub stride_bytes: u32, pub pixels: PixelBuffer }
pub enum FrameCompleteness { Incremental, FullBaseline }
pub enum SurfaceUpdate {
    Reset { session_id: SessionId, generation: u64, size: PixelSize, format: PixelFormat },
    Damage { session_id: SessionId, generation: u64, revision: u64, patches: Vec<PixelPatch> },
    FrameBoundary { session_id: SessionId, generation: u64, revision: u64, completeness: FrameCompleteness },
}
```

`FrameMailbox::push` must validate before enqueueing, count both entries and pixel bytes, and clear queued damage for the current generation on overflow. It must not synthesize pixels; the producer receives `NeedsFullSnapshot` and republishes from its canonical CPU surface.

- [ ] **Step 4: Run GREEN and regression tests**

Run: `cargo test -p frd-frame`

Expected: PASS.

Run: `cargo test`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/frd-frame
git commit -m "feat: add generation-bound frame mailbox"
```

### Task 4: Define media, protocol, session, application, and UI-model boundaries

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/frd-media-api/{Cargo.toml,src/lib.rs}`
- Create: `crates/frd-protocol-api/{Cargo.toml,src/lib.rs}`
- Create: `crates/frd-session/{Cargo.toml,src/lib.rs,src/coordinator.rs}`
- Create: `crates/frd-ui-model/{Cargo.toml,src/lib.rs}`
- Create: `crates/frd-platform-api/{Cargo.toml,src/lib.rs}`
- Create: `crates/frd-app/{Cargo.toml,src/lib.rs,src/controller.rs}`

**Interfaces:**
- Consumes: core/frame contracts.
- Produces: `ProtocolDescriptor`, `ProtocolFactory`, `ProtocolRuntime`, `SessionCommand`, `SessionEvent`, `ServerIdentityChallenge`, `PresentationEvent`, `AppIntent`, `AppController`, `ActiveSessionSlot`.

- [ ] **Step 1: Write RED contract tests**

Test that automatic selection maps Mac OS to one Apple protocol ID, invalid target/protocol pairs fail before a worker starts, only one active session can occupy the slot, stale presentation events do not enter `RemoteSession`, and `FullBaseline` for the current session/generation does. Add current challenge-ID acceptance, stale challenge-decision rejection and pin-mismatch fail-closed cases for the protocol-neutral server-identity contract.

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-app -p frd-session -p frd-protocol-api`

Expected: compile failures for missing contracts.

- [ ] **Step 3: Implement the public contracts exactly once**

```rust
pub trait ProtocolFactory: Send + Sync {
    fn descriptor(&self) -> ProtocolDescriptor;
    fn create(&self, request: ConnectRequest, runtime: ProtocolRuntime)
        -> Result<Box<dyn ProtocolSession>, ProtocolError>;
}

pub trait ProtocolSession: Send {
    fn run(self: Box<Self>) -> ProtocolExit;
}

pub enum AppIntent {
    Connect(ConnectionSubmission),
    CancelConnect,
    Disconnect,
    ReturnToConnection,
}
```

`ProtocolFactory::create` must be nonblocking. `AppIntent` is only the low-frequency semantic UI boundary; remote input uses `AppController::route_input(SessionInput)` and pixels use `FrameMailbox`.

Define `SessionEvent::SurfaceGenerationChanged` and `PresentationEvent::FramePresented` with session/generation/revision. `AppController` computes effective capabilities as protocol ∩ platform ∩ product policy.

Define `SessionEvent::ServerIdentityChallenge`, `SessionCommand::ResolveServerIdentity` and the protocol-neutral `ServerIdentityStore` platform port. `ConnectRequest` carries at most one endpoint/protocol-specific saved SHA-256 pin snapshot loaded by the app before worker creation. The adapter emits a challenge; the app owns TrustOnce/TrustAndRemember/Reject policy and persistence. No platform store enters `ProtocolRuntime`.

```rust
pub struct ServerIdentityChallenge {
    pub session_id: SessionId,
    pub challenge_id: u64,
    pub protocol_id: ProtocolId,
    pub endpoint: Endpoint,
    pub sha256_fingerprint: [u8; 32],
    pub subject: String,
    pub issuer: String,
    pub validation: ServerIdentityValidation,
}

pub enum ServerIdentityDecision { TrustOnce, TrustAndRemember, Reject }

pub trait ServerIdentityStore: Send + Sync {
    fn load_pin(&self, protocol: &ProtocolId, endpoint: &Endpoint)
        -> Result<Option<[u8; 32]>, PlatformError>;
    fn store_pin(&self, protocol: &ProtocolId, endpoint: &Endpoint, pin: [u8; 32])
        -> Result<(), PlatformError>;
}
```

- [ ] **Step 4: Implement paired generation publication and stale input rejection**

Add `ProtocolRuntime::begin_generation(session_id, generation, size, format)` so it sends the lifecycle event, enqueues `SurfaceUpdate::Reset`, then issues one wake. `SessionCommand::Input` carries `SessionInput`; the writer-facing receiver rejects mismatched session/generation.

- [ ] **Step 5: Run GREEN**

Run: `cargo test -p frd-app -p frd-session -p frd-protocol-api -p frd-media-api`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/frd-media-api crates/frd-protocol-api crates/frd-session crates/frd-ui-model crates/frd-platform-api crates/frd-app
git commit -m "feat: define isolated application and protocol contracts"
```

### Task 5: Extract stateless RFB wire parsing without moving authentication policy

**Files:**
- Create: `crates/frd-wire-rfb/Cargo.toml`
- Create: `crates/frd-wire-rfb/src/lib.rs`
- Create: `crates/frd-wire-rfb/src/banner.rs`
- Create: `crates/frd-wire-rfb/src/server_init.rs`
- Create: `crates/frd-wire-rfb/src/messages.rs`
- Modify: `src/vnc/protocol.rs`
- Modify: `src/vnc/client.rs`

**Interfaces:**
- Consumes: core geometry.
- Produces: pure `decode_banner`, `decode_security_types`, `decode_server_init`, and bounded cursor/message codecs.

- [ ] **Step 1: Copy existing wire fixtures into RED tests in `frd-wire-rfb`**

Move the RFB 3.3 overflow rejection, Apple `003.889` banner normalization, ServerInit size/name bounds, and basic rectangle header tests. The tests pass byte slices and must not open sockets.

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-wire-rfb`

Expected: compile failures until pure decoders exist.

- [ ] **Step 3: Move only stateless codec code**

The new crate may own constants, byte cursors, banner parsing, ServerInit parsing, pixel-format structures, and message header encoding. It must not contain `TcpStream`, `RfbConn`, credentials, `pick_security`, SessionCrypto, VNC Auth, or fallback.

- [ ] **Step 4: Make the root legacy client consume the leaf crate**

Replace duplicate parsing in `src/vnc/protocol.rs`/`client.rs` with `frd_wire_rfb` calls while leaving the existing authentication and socket lifecycle in place for this commit.

- [ ] **Step 5: Run all tests**

Run: `cargo test -p frd-wire-rfb`

Expected: PASS.

Run: `cargo test`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/frd-wire-rfb src/vnc/protocol.rs src/vnc/client.rs
git commit -m "refactor: isolate stateless RFB wire codec"
```

### Task 6: Move Apple authentication and encrypted session ownership

**Files:**
- Create: `crates/frd-protocol-apple/Cargo.toml`
- Create: `crates/frd-protocol-apple/src/lib.rs`
- Create: `crates/frd-protocol-apple/src/factory.rs`
- Create: `crates/frd-protocol-apple/src/connection.rs`
- Move: `src/vnc/{ard.rs,rsa_srp.rs,srp.rs,session.rs}` to `crates/frd-protocol-apple/src/`
- Split: Apple portions of `src/vnc/{auth.rs,client.rs,protocol.rs}` into `crates/frd-protocol-apple/src/auth/`

**Interfaces:**
- Consumes: `ProtocolFactory`, `ProtocolRuntime`, `frd-wire-rfb` codecs.
- Produces: `AppleProtocolFactory`, `AppleProtocolSession`, Apple-only connection/writer.

- [ ] **Step 1: Add RED tests for strict Apple selection and writer ownership**

Move key derivation/auth vectors intact. Add `apple_factory_rejects_vnc_fallback` using a security list containing only type 2 and credentials; expect stable error code `apple_security_type_unavailable`. Add a test that one writer serializes two encrypted messages in command order.

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-protocol-apple auth`

Run: `cargo test -p frd-protocol-apple session`

Expected: compile failures until the Apple crate exists.

- [ ] **Step 3: Move Apple code without changing bytes or algorithms**

Create `AppleConnection` that owns `TcpStream`, receive buffer, and Apple crypto. Keep security types 30/33/35/36 in this crate. Delete Apple fallback branches from the new factory; do not change the legacy root client yet beyond imports needed to keep tests compiling.

- [ ] **Step 4: Create the single Apple protocol writer**

Move `send_encrypted` responsibility out of the viewer. The writer thread alone consumes input, framebuffer refresh, media control, and dynamic-resolution commands, and is the only owner allowed to mutate outbound SessionCrypto.

- [ ] **Step 5: Run focused vectors and full tests**

Run: `cargo test -p frd-protocol-apple key_derivation_vector`

Expected: PASS.

Run: `cargo test -p frd-protocol-apple`

Expected: PASS.

Run: `cargo test`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/frd-protocol-apple src/vnc
git commit -m "refactor: isolate Apple authentication and session writer"
```

### Task 7: Move HPSS/MVS/media runtime behind the Apple adapter

**Files:**
- Move: Apple-only files under `src/vnc/` into matching `crates/frd-protocol-apple/src/` modules: `hpss.rs`, `mvs*.rs`, `dynamic_resolution.rs`, `media_*.rs`, `srtp.rs`, `audio_codec.rs`, `cold_*.rs`.
- Create: `crates/frd-protocol-apple/src/runtime.rs`
- Create: `crates/frd-protocol-apple/src/network_reader.rs`
- Create: `crates/frd-protocol-apple/src/media_runtime.rs`
- Create: `crates/frd-protocol-apple/src/surface_publisher.rs`
- Modify: `src/vnc/hpss_viewer.rs`

**Interfaces:**
- Consumes: Tasks 3/4/6 contracts.
- Produces: working `AppleProtocolSession::run`, BGRX `SurfaceUpdate`, PCM media publication, generation commits.

- [ ] **Step 1: Move existing MVS/dynamic tests before runtime edits**

Keep all type-0/type-1, Cb/Cr, fragmentation, generation, full-boundary, partial transaction, and P5 fail-closed tests byte-for-byte unless import paths change. Add one adapter-level test proving type-1 cannot publish `FullBaseline` before a successful current-generation type-0.

- [ ] **Step 2: Run the moved tests and verify RED on integration symbols only**

Run: `cargo test -p frd-protocol-apple`

Expected: low-level moved tests pass; adapter/runtime tests fail until publication is wired.

- [ ] **Step 3: Split `hpss_viewer.rs` by responsibility**

Move these current symbol groups:

```text
network_reader.rs: MvsReceiveState, ReaderRequestState, service_reader_tick_at,
                   process_complete_mvs_record, service_network_reader_tick,
                   finish_network_full_boundary, handle_complete_mvs_record
media_runtime.rs:  ViewerMediaState, drain_udp_media, UDP/audio receive loop
surface_publisher.rs:
                   DisplaySurface, apply_rgb_rect_for_generation,
                   apply_prepared_mvs_to_surface_with,
                   partial framebuffer validation/application
runtime.rs:        run_viewer network/authenticated-session portion, cancellation,
                   reader/writer/audio joins
```

Do not move `minifb::Window`, CPU scaling, or minifb key conversion into the adapter.

- [ ] **Step 4: Publish canonical surface updates**

Use `ProtocolRuntime::begin_generation` on initial geometry and exact dynamic-resolution commits. After a successful full or partial transaction, copy only the dirty rectangle into a non-Clone `PixelBuffer`, publish `Damage`, then `FrameBoundary`. Mark `FullBaseline` only for a complete current-generation type-0 that passes the existing initial nonblack diagnostic; type-1 publishes `Incremental` only after baseline.

- [ ] **Step 5: Route media without platform leakage**

Keep SRTP/SRTCP and AAC-ELD decode in Apple. Publish bounded 48 kHz stereo `PcmFrame` values through `frd-media-api`; remove CPAL/device ownership from the adapter. Keep P5 failure before microphone open or network mode selection.

- [ ] **Step 6: Run protocol verification**

Run: `cargo test -p frd-protocol-apple`

Expected: PASS.

Run: `cargo test --no-default-features`

Expected: PASS.

Run: `cargo test`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/frd-protocol-apple src/vnc
git commit -m "refactor: publish Apple HPSS frames through protocol adapter"
```

### Task 8: Create the serialized legacy minifb comparison tool

**Files:**
- Create: `tools/frd-legacy-minifb-lab/Cargo.toml`
- Create: `tools/frd-legacy-minifb-lab/src/main.rs`
- Move: view-only portions of `src/viewer.rs`, `src/keysym.rs`, and `src/vnc/hpss_viewer.rs` into the tool.
- Modify: root `src/main.rs`
- Modify: root `Cargo.toml`

**Interfaces:**
- Consumes: Apple adapter/session/frame contracts.
- Produces: an explicitly named lab binary used only for serialized A/B validation.

- [ ] **Step 1: Add a workspace dependency guard command**

Run `cargo metadata --format-version 1` through a short PowerShell assertion in the task notes and verify that no package except `frd-legacy-minifb-lab` directly depends on `minifb`. The permanent dependency-boundary test is created with the Windows app in Task 11.

- [ ] **Step 2: Move the comparison viewer**

The lab binary may depend on `minifb` and translate its events into normalized `InputEvent`, but it must consume `ProtocolFactory`/`FrameMailbox`; it must not regain socket or SessionCrypto ownership. Remove product-facing `view`/`hpssview` dispatch from the root CLI or replace it with an error directing developers to the lab binary.

- [ ] **Step 3: Build both headless and lab configurations**

Run: `cargo build --no-default-features`

Expected: PASS without compiling minifb.

Run: `cargo build -p frd-legacy-minifb-lab --release`

Expected: PASS and produce only the explicitly named lab executable.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock src tools/frd-legacy-minifb-lab
git commit -m "refactor: isolate minifb as legacy comparison tool"
```

### Task 9: Implement the wgpu remote renderer and single compositor

**Files:**
- Create: `crates/frd-render-wgpu/{Cargo.toml,src/lib.rs,src/remote_texture.rs,src/pass.rs,src/shaders/remote_surface.wgsl}`
- Create: `crates/frd-compositor-wgpu/{Cargo.toml,src/lib.rs,src/surface.rs,src/state.rs}`

**Interfaces:**
- Consumes: `SurfaceUpdate`, owned `PresentationSurfaceLease`.
- Produces: `RemoteRenderer::apply_update`, `RemoteRenderer::record`, `PresentationCompositor::render`, `PresentationEvent`.

- [ ] **Step 1: Write RED pure state tests**

Test current session/generation rejection, FullBaseline tracking, reset clearing old interactivity, BGRX alpha policy, and wgpu 30 acquisition decisions (`Outdated` reconfigure, `Lost` recreate, `Suboptimal` present then reconfigure, Timeout/Occluded skip).

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-render-wgpu -p frd-compositor-wgpu`

Expected: compile failures.

- [ ] **Step 3: Implement remote texture upload and shader**

`RemoteRenderer::apply_update` validates session/generation, calls `Queue::write_texture` only for damage rectangles, and never creates a window-sized CPU scaled buffer. The fragment shader samples `Bgra8UnormSrgb` and writes alpha `1.0`.

- [ ] **Step 4: Implement owned Surface leases and the compositor**

```rust
pub trait PresentationHooks {
    fn before_submit(&self);
}

pub struct PresentationCompositor { /* owns Surface + lease + config */ }
impl PresentationCompositor {
    pub fn render(
        &mut self,
        remote: &mut RemoteRenderer,
        overlay: impl FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView),
        hooks: &dyn PresentationHooks,
    ) -> Result<Option<PresentationEvent>, PresentError>;
}
```

The compositor alone acquires, creates one encoder, records remote then overlay, calls `before_submit`, submits, presents, and emits the presentation acknowledgement. Surface `Drop` explicitly destroys Surface before releasing its native-window lease.

- [ ] **Step 5: Run GREEN and a deterministic texture smoke binary**

Run: `cargo test -p frd-render-wgpu -p frd-compositor-wgpu`

Expected: PASS.

Add an example that uploads a 2×2 BGRX red/green/blue/white fixture and renders it in a resizable Windows window. Run it manually and confirm no transparency or channel swap.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/frd-render-wgpu crates/frd-compositor-wgpu
git commit -m "feat: add wgpu remote surface compositor"
```

### Task 10: Implement Windows platform services and egui views

**Files:**
- Create: `crates/frd-platform-windows/{Cargo.toml,src/lib.rs,src/audio.rs,src/credentials.rs,src/server_identity.rs,src/single_instance.rs}`
- Create: `crates/frd-ui-egui/{Cargo.toml,src/lib.rs,src/connection.rs,src/session.rs}`
- Modify: `crates/frd-ui-model/src/lib.rs`
- Modify: `crates/frd-app/src/controller.rs`

**Interfaces:**
- Consumes: `AppIntent`, UI model, platform/media ports.
- Produces: `LaunchOptions`, `ConnectionDraft`, unified connection form, protocol selector, secure submission, PCM output, Windows single-instance guard.

- [ ] **Step 1: Write RED UI-model and single-session tests**

Test `Connection → Connecting → AwaitingFirstFrame → RemoteSession → Failed`, password removal after submit/failure, effective capability intersection, current presentation acknowledgement, and rejection of a second active session. Add launch-merge cases for empty CLI, partial CLI, complete CLI without `--connect`, complete CLI with `--connect`, and a failed credential provider. Test current-user DPAPI pin roundtrip and a different fingerprint for the same endpoint/protocol being rejected.

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-app -p frd-ui-model -p frd-platform-windows`

Expected: failures for missing state transitions and platform implementations.

- [ ] **Step 3: Implement low-frequency egui screens**

Connection fields are target system, address, port, protocol choice, username, and password. `Auto` resolves Mac OS to Apple HPSS/MVS before session creation. The view emits `AppIntent::Connect(ConnectionSubmission)` and never imports `frd-protocol-apple`.

Define `LaunchOptions` as optional non-secret CLI/provider inputs and merge it once into `ConnectionDraft`. Missing address, target system, port, protocol credentials, or provider values leave the app in `ConnectionForm`; they never terminate process startup. `connect_when_complete` triggers exactly one `AppIntent::Connect` only after full validation. Keep the password in a separate non-Clone `SecretBuffer`; do not place it in `ConnectionDraft`.

- [ ] **Step 4: Implement platform services**

Use a per-user Windows named mutex for one product process; a second launch returns stable error `windows_instance_already_running`. Implement `AudioOutput` over CPAL in `frd-platform-windows`, not the Apple crate. Credential environment fallback may read `FRD_USERNAME`/`FRD_PASSWORD` into `SecretBuffer` without logging or argv exposure.

Implement the protocol-neutral `ServerIdentityStore` in `frd-platform-windows`: DPAPI-protect endpoint/protocol/fingerprint records for the current user. `frd-app` loads the saved pin before connecting and persists only an explicit TrustAndRemember decision. The same egui window renders unknown-certificate details and sends the decision; adapters never access DPAPI directly.

- [ ] **Step 5: Run GREEN**

Run: `cargo test -p frd-app -p frd-ui-model -p frd-platform-windows`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/frd-platform-windows crates/frd-ui-egui crates/frd-ui-model crates/frd-app
git commit -m "feat: add Windows services and connection UI"
```

### Task 11: Build the single-window Windows application shell

**Files:**
- Create: `crates/frd-shell-desktop/{Cargo.toml,src/lib.rs,src/application.rs,src/input.rs}`
- Create: `apps/freeremotedesk-windows/{Cargo.toml,src/main.rs,src/cli.rs,tests/dependency_boundary.rs}`

**Interfaces:**
- Consumes: app controller, Apple factory, UI, renderer/compositor, Windows services.
- Produces: `DesktopApplication: winit::application::ApplicationHandler`, optional CLI prefill parsing, one product executable and one login/session flow.

- [ ] **Step 1: Write dependency-boundary and reducer RED tests**

Use `cargo metadata` to reject concrete protocol dependencies from core/session/app/UI/render/shell packages and reject minifb outside the legacy lab. Add a shell state test proving the Connect button starts a worker and leaves the event loop responsive.

- [ ] **Step 2: Run RED**

Run: `cargo test -p freeremotedesk-windows`

Expected: failures until the app and dependency graph exist.

- [ ] **Step 3: Implement the composition root**

`cli.rs` parses only non-secret target/protocol/provider options plus `--connect`; it must reject literal password arguments at clap-definition level. `main.rs` creates Windows services, registers only `AppleProtocolFactory` in this plan, merges `LaunchOptions` into the one connection form, creates the app controller, renderer/compositor, egui integration, and runs one winit EventLoop. No concrete protocol crate is imported outside this file.

Add CLI tests proving no arguments and partial arguments launch the form, complete values without `--connect` only prefill, complete values with `--connect` request one connection, and no `--password`/literal secret option exists.

- [ ] **Step 4: Implement `ApplicationHandler`**

Create the window and owned Surface lease in `resumed`. Drain SessionEvent before FrameMailbox on each wake. On redraw: apply frame updates, render remote pass, render egui overlay, call platform hook, submit/present, forward PresentationEvent to `frd-app`. Use `ControlFlow::Wait`; request redraw only for frame/session/UI/cursor changes.

- [ ] **Step 5: Implement event routing and generation input gates**

egui consumes controls first. Remote pointer/keyboard events are mapped only inside the physical content viewport and only while `frd-app` marks the current generation interactive. On `CursorLeft`, focus loss, generation change, disconnect, or shutdown, send exactly one `ReleaseAll` and clear local held state.

- [ ] **Step 6: Build and run the Windows test texture path**

Run: `cargo build -p freeremotedesk-windows --release`

Expected: `target/release/freeremotedesk-windows.exe` builds with DX12 and without minifb.

Run the executable with a test-texture developer flag that never opens a network connection. Confirm one window transitions Connection → test RemoteSession, resizes correctly, and remains responsive.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/frd-shell-desktop apps/freeremotedesk-windows
git commit -m "feat: add single-window Windows remote desktop app"
```

### Task 12: Validate Apple HPSS/MVS parity on the authorized Mac

**Files:**
- Modify only files proven necessary by observed parity failures under `crates/frd-protocol-apple/`, `crates/frd-render-wgpu/`, `crates/frd-app/`, or `crates/frd-shell-desktop/`.
- Create: `docs/validation/windows-apple-wgpu-parity.md`

**Interfaces:**
- Consumes: complete Windows app from Task 11.
- Produces: bounded live evidence and an Apple-equivalent Windows app ready for the RFB plan.

- [ ] **Step 1: Run the full offline matrix before live access**

Run:

```bash
cargo fmt -- --check
cargo test --workspace
cargo build --workspace --no-default-features
cargo build -p freeremotedesk-windows --release
```

Expected: all commands PASS. Confirm `cargo tree -p freeremotedesk-windows` contains winit/wgpu/egui and Apple adapter, but not minifb or the root protocol-lab package.

- [ ] **Step 2: Ensure serialized live execution**

Close any existing FreeRemoteDesk/minifb lab process. Launch exactly one Windows product process. Load credentials only through the secure UI/provider; do not echo them in commands or logs.

- [ ] **Step 3: Validate connection and frame correctness**

Against the authorized Mac, verify: username/password HPSS authentication, first current-generation type-0 FullBaseline, type-1 remote refresh, correct Cb/Cr color, nonblack initial diagnostic, and no return to the connection page during stable operation.

- [ ] **Step 4: Validate input and generation safety**

Verify pointer movement/click, keyboard input, pointer-outside silence, one release on drag-out, focus-loss release, no stale input after resize, dynamic-resolution exact acknowledgement, generation reset, and full baseline before re-enabling input.

- [ ] **Step 5: Validate media and idle performance**

Verify Mac-to-PC audio still authenticates, decodes non-silent 48 kHz stereo AAC-ELD, and plays through Windows. Confirm P5 stays fail-closed without opening a microphone. Observe that idle remote sessions do not perform fixed 60 Hz full-frame CPU scaling/upload.

- [ ] **Step 6: Record bounded evidence**

Write commit hash, build commands, target OS versions, one-window/one-session result, frame/color/input/audio results, and any unverified long-duration behavior to `docs/validation/windows-apple-wgpu-parity.md`. Do not include credentials, target secrets, packet keys, or captures.

- [ ] **Step 7: Commit the verified parity result**

```bash
git add crates/frd-protocol-apple crates/frd-render-wgpu crates/frd-app crates/frd-shell-desktop docs/validation/windows-apple-wgpu-parity.md
git commit -m "feat: complete Windows Apple HPSS wgpu parity"
```
