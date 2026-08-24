# Winit/Wgpu Desktop Client Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Flutter/minifb product shell with one layered Rust `winit + egui + wgpu` client for Windows, macOS, and Linux while preserving and adapting the existing RDP/RFB/Apple ARD protocol core.

**Architecture:** Platform services and window hosting are independent from server protocol adapters. A platform-neutral `SessionEngine` exchanges bounded `SessionCommand`, `SessionEvent`, and generation-bound `RenderUpdate` values with one Rust UI; the renderer owns a persistent GPU texture and uploads only validated rectangles.

**Tech Stack:** Rust 1.96.0, winit 0.30.13, wgpu 30.0.1, egui/egui-winit/egui-wgpu 0.36.1, zeroize, secrecy, crossbeam-channel, IronRDP, GitHub Actions

**Spec:** `docs/superpowers/specs/2026-08-24-winit-wgpu-client-architecture-design.md`

## Global Constraints

- Phase 1 supports Windows, macOS, and Linux clients and packages before Android, iOS, or HarmonyOS work begins.
- Client platform adapters never construct RDP/RFB/ARD wire messages; protocol adapters never own windows, installers, or platform UI types.
- The product connects only to unmodified native remote services and never requires a remote companion.
- Mac login accepts only the native Mac username and password; Apple ID/IDS credentials remain forbidden.
- Passwords never enter argv, logs, recent connections, panic messages, or terminal errors.
- P1 generation transitions remain atomic across remote dimensions, renderer texture, input transform, and MVS reset.
- Unknown or malformed MVS partial updates remain fail-closed and request a bounded full resync.
- No live-interoperability claim is allowed until the authorized target has authenticated and rendered a non-empty frame.
- The final repository contains no Dart source, `pubspec.yaml`, Flutter runner/plugin/toolchain, `freeremote_ffi`, or `minifb` product dependency.

---

### Task 1: Platform-neutral frame and viewport contracts

**Files:**
- Create: `src/core/mod.rs`
- Create: `src/core/frame.rs`
- Create: `src/core/viewport.rs`
- Modify: `src/lib.rs`
- Test: `src/core/frame.rs`
- Test: `src/core/viewport.rs`

**Interfaces:**
- Consumes: decoded BGRA/RGBA byte rectangles and physical host/remote dimensions.
- Produces: `RemotePixelFormat`, `FrameRect`, `RenderUpdate`, `RemoteViewportTransform`, and stable validation errors.

- [x] **Step 1: Write failing frame-contract tests**

```rust
#[test]
fn dirty_rect_rejects_bytes_shorter_than_stride_times_height() {
    let error = RenderUpdate::dirty_rect(
        7,
        FrameRect::new(0, 0, 2, 2).unwrap(),
        RemotePixelFormat::Bgra8Srgb,
        8,
        vec![0; 15].into_boxed_slice(),
    )
    .unwrap_err();
    assert_eq!(error.code(), "dirty_rect_length_mismatch");
}

#[test]
fn stale_generation_is_classified_before_upload() {
    let state = RemoteSurfaceState::new(8, 1920, 1080).unwrap();
    assert_eq!(state.classify_generation(7), GenerationDisposition::Stale);
}
```

- [x] **Step 2: Run RED frame tests**

Run: `cargo test --locked core::frame --lib`

Expected: compilation fails because `core::frame` and the listed types do not exist.

- [x] **Step 3: Implement exact frame validation**

```rust
pub enum RenderUpdate {
    Reset { generation: u64, width: u32, height: u32, format: RemotePixelFormat },
    DirtyRect { generation: u64, rect: FrameRect, format: RemotePixelFormat,
        bytes_per_row: u32, pixels: Box<[u8]> },
    Present { generation: u64 },
}
```

Limit surfaces to 64 million pixels, reject zero dimensions and arithmetic overflow, require every rectangle to fit the current surface, and require `pixels.len() == bytes_per_row * rect.height` with `bytes_per_row >= rect.width * 4`.

- [x] **Step 4: Write and verify RED viewport tests**

```rust
#[test]
fn square_host_letterboxes_wide_remote_and_maps_center() {
    let transform = RemoteViewportTransform::new((1000, 1000), (1920, 1080), 1.0).unwrap();
    assert_eq!(transform.remote_point((500.0, 500.0)), Some((960, 540)));
    assert_eq!(transform.remote_point((500.0, 100.0)), None);
}
```

Run: `cargo test --locked core::viewport --lib`

Expected: FAIL because `RemoteViewportTransform` is missing.

- [x] **Step 5: Implement viewport mapping and verify GREEN**

Calculate one aspect-fit physical-pixel rectangle; reject non-finite/non-positive scale factors and zero dimensions; return `None` for letterbox coordinates and clamp in-surface coordinates to the current remote bounds.

Run: `cargo test --locked core --lib`

Expected: all core frame and viewport tests pass.

- [x] **Step 6: Commit**

```text
feat(core): add validated render and viewport contracts
```

### Task 2: Session engine, protocol, and platform boundaries

**Files:**
- Create: `src/session/mod.rs`
- Create: `src/session/engine.rs`
- Create: `src/session/backpressure.rs`
- Create: `src/protocols/mod.rs`
- Create: `src/platform/mod.rs`
- Modify: `src/lib.rs`
- Modify: `src/app/mod.rs`
- Test: `src/session/backpressure.rs`
- Test: `src/session/engine.rs`

**Interfaces:**
- Consumes: `ValidatedConnection`, `SessionCommand`, protocol events, and `RenderUpdate`.
- Produces: `ProtocolAdapter::run`, `SessionEngine`, `SessionSnapshot`, `UiWakeHandle`, `WindowHost`, and `PlatformServices`.

- [x] **Step 1: Write failing bounded-queue tests**

```rust
#[test]
fn full_queue_replaces_older_pending_present_for_same_generation() {
    let mut queue = RenderUpdateQueue::with_limits(2, 1024);
    queue.push(RenderUpdate::present(4)).unwrap();
    queue.push(RenderUpdate::present(4)).unwrap();
    assert_eq!(queue.len(), 1);
}

#[test]
fn reset_discards_queued_updates_from_older_generations() {
    let mut queue = RenderUpdateQueue::with_limits(8, 4096);
    queue.push(rect_update(2, 64)).unwrap();
    queue.push(RenderUpdate::reset(3, 4, 4, RemotePixelFormat::Bgra8Srgb).unwrap()).unwrap();
    assert!(queue.iter().all(|update| update.generation() >= 3));
}
```

- [x] **Step 2: Run RED queue tests**

Run: `cargo test --locked session::backpressure --lib`

Expected: compilation fails because the session queue is missing.

- [x] **Step 3: Implement the queue and adapter traits**

```rust
pub trait ProtocolAdapter: Send + 'static {
    fn run(
        self: Box<Self>,
        context: ProtocolContext,
        commands: Receiver<SessionCommand>,
        events: SessionEventSink,
    ) -> Result<(), SessionError>;
}

pub trait WindowHost {
    fn request_redraw(&self) -> Result<(), PlatformError>;
    fn surface_handle(&self) -> Result<SurfaceHandle, PlatformError>;
    fn set_fullscreen(&self, enabled: bool) -> Result<(), PlatformError>;
}
```

Use bounded crossbeam channels. Coalesce duplicate presents, reject updates larger than the byte budget, and evict only stale/coalescible entries; never silently drop the sole current-generation reset.

- [x] **Step 4: Write state-transition tests and verify RED/GREEN**

```rust
#[test]
fn session_rejects_connected_before_surface_reset() {
    let mut model = SessionModel::default();
    model.apply(SessionEvent::Connecting).unwrap();
    let error = model.apply(SessionEvent::Connected { generation: 1 }).unwrap_err();
    assert_eq!(error.code(), "connected_without_surface");
}
```

Run before implementation and confirm expected failure, then implement `Idle -> Connecting -> SurfaceReady -> Connected -> Disconnecting -> Idle` plus terminal `Failed` transitions and rerun `cargo test --locked session --lib`.

- [x] **Step 5: Commit**

```text
feat(session): add bounded protocol and platform boundaries
```

### Task 3: Secret-safe UI model

**Files:**
- Create: `src/ui/mod.rs`
- Create: `src/ui/application.rs`
- Create: `src/ui/secret_buffer.rs`
- Create: `src/ui/connection_view.rs`
- Create: `src/ui/session_view.rs`
- Modify: `src/lib.rs`
- Modify: `Cargo.toml`
- Test: `src/ui/application.rs`
- Test: `src/ui/secret_buffer.rs`

**Interfaces:**
- Consumes: user connection fields and `SessionSnapshot`.
- Produces: `FreeRemoteApplication`, `ConnectionFormState`, `UiAction`, and a zeroizing password buffer.

- [x] **Step 1: Write failing secret and form tests**

```rust
#[test]
fn terminal_submission_clears_password_without_clearing_endpoint() {
    let mut form = ConnectionFormState::fixture();
    form.password_mut().push_str("secret-value");
    form.finish_submission(SubmissionOutcome::Failed);
    assert!(form.password().is_empty());
    assert_eq!(form.host(), "mac.local");
}

#[test]
fn debug_output_never_contains_password() {
    let form = ConnectionFormState::with_password("secret-value");
    assert!(!format!("{form:?}").contains("secret-value"));
}
```

- [x] **Step 2: Run RED UI model tests**

Run: `cargo test --locked ui:: --lib`

Expected: compilation fails because the UI model does not exist.

- [x] **Step 3: Implement minimal model and egui views**

Use `Zeroizing<String>` for the editable password, move a copied secret directly into the existing `validate_connection` boundary at submission, immediately clear the edit buffer, and disable recent-password persistence. Render service choices `自动识别`, `Windows`, `Mac OS`, and `Linux / VNC`; show the domain field only for Windows.

- [x] **Step 4: Verify GREEN**

Run: `cargo test --locked ui:: --lib`

Expected: UI model, field visibility, redaction, and transition tests pass.

- [x] **Step 5: Commit**

```text
feat(ui): add secret-safe Rust application model
```

### Task 4: Winit host and wgpu renderer

**Files:**
- Create: `src/ui/winit_host.rs`
- Create: `src/ui/renderer.rs`
- Create: `src/ui/remote_texture.rs`
- Create: `src/ui/shaders/remote_surface.wgsl`
- Modify: `src/ui/mod.rs`
- Modify: `src/main.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Test: `src/ui/remote_texture.rs`
- Test: `src/ui/renderer.rs`

**Interfaces:**
- Consumes: `RenderUpdate` and `UiAction`.
- Produces: a single desktop `WinitHost`, persistent `wgpu::Texture`, and `run_desktop()`.

- [x] **Step 1: Write failing remote-texture state tests**

```rust
#[test]
fn reset_changes_texture_only_for_a_newer_generation() {
    let mut state = RemoteTextureState::empty();
    assert_eq!(state.apply_reset(4, 1280, 720).unwrap(), ResetDisposition::Created);
    assert_eq!(state.apply_reset(3, 1920, 1080).unwrap(), ResetDisposition::Stale);
    assert_eq!(state.dimensions(), Some((1280, 720)));
}

#[test]
fn surface_loss_does_not_change_remote_generation() {
    let mut state = RemoteTextureState::fixture(9, 800, 600);
    state.on_surface_lost();
    assert_eq!(state.generation(), Some(9));
}
```

- [x] **Step 2: Run RED renderer tests**

Run: `cargo test --locked ui::remote_texture --lib`

Expected: compilation fails because the texture state is missing.

- [x] **Step 3: Add exact GUI dependency versions**

Add `winit = 0.30.13`, `wgpu = 30.0.1`, `egui = 0.36.1`, `egui-winit = 0.36.1`, `egui-wgpu = 0.36.1`, `pollster = 0.4`, `bytemuck = 1.25`, `zeroize = 1`, and `crossbeam-channel = 0.5` behind a `gui` feature. Disable default link/clipboard/web/GL features and select DX12, Metal, or Vulkan through target-specific dependency tables.

- [x] **Step 4: Implement renderer and one-window entry**

Create one surface, one egui renderer, and one remote texture pipeline. Drain bounded updates before redraw, call `Queue::write_texture` for the validated rectangle only, and render aspect-fit geometry in WGSL. Use `ControlFlow::Wait`; request redraw only for input, UI animation, a new present, or surface recovery.

- [x] **Step 5: Verify local GUI and release build**

Run: `cargo test --locked ui:: --lib`, `cargo build --locked --release --features gui`, then launch the no-argument release binary and verify the single connection window renders and exits normally.

Expected: tests/build pass and exactly one product window appears.

- [x] **Step 6: Commit**

```text
feat(render): add single-window winit wgpu client
```

### Task 5: RFB and Apple ARD protocol adapters

**Files:**
- Create: `src/protocols/rfb.rs`
- Create: `src/protocols/apple_ard.rs`
- Modify: `src/protocols/mod.rs`
- Modify: `src/vnc/client.rs`
- Modify: `src/vnc/hpss_viewer.rs`
- Modify: `src/framebuffer.rs`
- Modify: `src/keysym.rs`
- Test: `src/protocols/rfb.rs`
- Test: `src/protocols/apple_ard.rs`

**Interfaces:**
- Consumes: existing RFB/ARD authentication, HPSS/MVS generation state, and `SessionCommand`.
- Produces: normalized `SessionEvent`/`RenderUpdate` without owning a local window.

- [x] **Step 1: Write failing adapter fixture tests**

```rust
#[test]
fn apple_full_frame_emits_reset_rect_and_present_for_one_generation() {
    let events = decode_fixture(APPLE_FULL_FRAME_FIXTURE).unwrap();
    assert!(matches!(events[0], RenderUpdate::Reset { generation: 1, .. }));
    assert!(matches!(events[1], RenderUpdate::DirtyRect { generation: 1, .. }));
    assert!(matches!(events[2], RenderUpdate::Present { generation: 1 }));
}

#[test]
fn malformed_partial_requests_resync_without_emitting_pixels() {
    let result = decode_fixture(MALFORMED_PARTIAL_FIXTURE).unwrap();
    assert!(result.render_updates().is_empty());
    assert_eq!(result.commands(), &[SessionCommand::RequestFullFrame]);
}
```

- [x] **Step 2: Run RED adapter tests**

Run: `cargo test --locked protocols::apple_ard --lib`

Expected: compilation fails because the adapter layer is missing.

- [x] **Step 3: Separate HPSS networking from local presentation**

Move the existing reader/media loop behind `AppleArdAdapter::run`. Replace shared-minifb presentation calls with bounded `RenderUpdate` emission; preserve current SRTP/audio teardown, dynamic-resolution acknowledgement, full-frame completeness, and stale-generation handling unchanged.

- [x] **Step 4: Map input commands**

Map `SessionCommand::Pointer`, `Key`, `Resize`, `Clipboard`, and `Disconnect` to the existing encrypted RFB/HPSS send functions. Use the current `RemoteViewportTransform`; do not retain startup dimensions after a generation commit.

- [x] **Step 5: Verify adapters and existing protocol suite**

Run: `cargo test --locked protocols:: --lib`, `cargo test --locked vnc:: --lib`, and `cargo test --locked --quiet`.

Expected: adapter fixtures and all existing cryptography/protocol tests pass.

- [x] **Step 6: Commit**

```text
refactor(ard): emit normalized session and render events
```

### Task 6: Windows RDP adapter

**Files:**
- Create: `src/protocols/rdp.rs`
- Modify: `src/protocols/mod.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Test: `src/protocols/rdp.rs`

**Interfaces:**
- Consumes: `ValidatedConnection` with `ProtocolKind::Rdp` and normalized commands.
- Produces: normalized state, resize, cursor, framebuffer, and terminal events.

- [x] **Step 1: Write failing RDP configuration tests**

```rust
#[test]
fn rdp_config_requires_credssp_and_preserves_domain() {
    let config = build_config(validated_windows_request()).unwrap();
    assert_eq!(config.destination().port(), 3389);
    assert_eq!(config.domain(), Some("WORKGROUP"));
    assert!(config.credssp_enabled());
}
```

- [x] **Step 2: Run RED RDP tests**

Run: `cargo test --locked protocols::rdp --lib`

Expected: compilation fails because the RDP adapter/configuration is absent.

- [x] **Step 3: Add minimal IronRDP client features and adapter**

Enable only client, TLS/rustls, CredSSP, graphics decode, input, and display-control dependencies; do not enable gateway, server, relay, device redirection, or custom DVC proxy features. Map image rectangles to the same BGRA/RGBA render contract and map server resize to a new generation.

- [x] **Step 4: Verify GREEN and full Rust suite**

Run: `cargo test --locked protocols::rdp --lib` and `cargo test --locked --quiet`.

Expected: RDP config/event tests and all existing tests pass.

- [x] **Step 5: Commit**

```text
feat(rdp): add native Windows service adapter
```

### Task 7: Desktop packaging and CI

**Files:**
- Create: `packaging/windows/Product.wxs`
- Create: `packaging/windows/build.ps1`
- Create: `packaging/macos/build.sh`
- Create: `packaging/linux/build.sh`
- Create: `packaging/check-artifact.ps1`
- Replace: `.github/workflows/build-five-platforms.yml`
- Modify: `README.md`
- Test: `packaging/check-artifact.tests.ps1`

**Interfaces:**
- Consumes: one Rust release binary and platform metadata.
- Produces: Windows ZIP/MSI, macOS app/DMG, and Linux AppDir/DEB/AppImage artifacts with SHA-256 manifests.

- [x] **Step 1: Write failing package-contract tests**

Test that zero-byte artifacts fail, valid artifacts generate lowercase SHA-256 sidecars, and artifact names exactly match `FreeRemoteAccess-{version}-{platform}-{arch}`.

Run: `pwsh -File packaging/check-artifact.tests.ps1`

Expected: FAIL because the verifier does not exist.

- [x] **Step 2: Implement verifier and native packaging scripts**

Use explicit staging directories under `target/package/<platform>` and never recursively delete outside that resolved path. Bundle only the release binary, licenses, icons, and platform metadata.

- [x] **Step 3: Replace CI with desktop native-host jobs**

Use Windows Server, macOS, and Ubuntu runners. Each job runs Rust format/tests/release build, produces its native packages, runs the artifact verifier, and uploads packages plus checksums. Remove Flutter setup and all `app/build` artifact paths.

- [x] **Step 4: Verify workflow and Windows package locally**

Run the package tests and Windows packaging script; parse the workflow as YAML and confirm the three platform jobs reference only Rust/native tools.

- [x] **Step 5: Commit**

```text
build: package Rust desktop clients
```

### Task 8: Remove Flutter, FFI, and minifb completely

**Files:**
- Delete: `app/`
- Delete: `native/freeremote_ffi/`
- Delete: `toolchains/flutter.json`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `src/main.rs`
- Delete: `src/viewer.rs`
- Modify: `src/vnc/hpss_viewer.rs`
- Modify: `.gitignore`
- Modify: `AGENTS.md`
- Modify: `README.md`
- Test: repository source/reference gate

**Interfaces:**
- Consumes: verified Rust GUI/adapters/packages from Tasks 1-7.
- Produces: one Rust-only product and no stale alternate UI/build path.

- [x] **Step 1: Run the pre-delete replacement gate**

Run all Rust tests, release build, local GUI smoke, and Windows package build. Do not delete legacy code unless these replacements pass.

- [x] **Step 2: Delete exact legacy paths**

Remove only the three exact roots/files listed above, remove the FFI workspace member, remove `minifb`, and rename the Cargo `viewer` feature to `gui` while preserving CLI diagnostics through the new runtime.

- [x] **Step 3: Add and run the absence gate**

Run an `rg --files`/content verifier that fails if tracked product files contain `.dart`, `pubspec.yaml`, Flutter runner/plugin/toolchain paths, `freeremote_ffi`, `minifb`, `update_with_buffer`, or the deleted Flutter workflow commands.

- [x] **Step 4: Run the complete offline matrix**

Run `cargo fmt --all -- --check`, `cargo test --locked --workspace`, `cargo test --locked --workspace --no-default-features`, `cargo build --locked --release`, `cargo build --locked --release --no-default-features`, help output, package tests, and the absence gate.

- [x] **Step 5: Commit**

```text
refactor: remove legacy Flutter and minifb clients
```

### Task 9: Live desktop interoperability and completion audit

**Files:**
- Create: `docs/validation/winit-wgpu-desktop-matrix.md`
- Modify: `README.md`

**Interfaces:**
- Consumes: native packages and authorized Windows/macOS/Linux targets.
- Produces: evidence separating build success from protocol interoperability.

- [x] **Step 1: Record fresh package evidence**

Record runner/OS, Rust version, artifact name, size, SHA-256, signing status, and local launch result for Windows, macOS, and Linux without secrets.

- [ ] **Step 2: Run authorized Mac validation after network recovery**

Authenticate with the Mac username/password path, require a complete non-empty non-black first generation, verify pointer/keyboard mapping, resize generation transition, clean disconnect, and Mac-to-PC audio. Keep P5 fail-closed.

- [ ] **Step 3: Run authorized Windows and Linux target validation**

For RDP and RFB respectively, authenticate to stock services, render a non-empty frame, exercise pointer and keyboard, resize when supported, and disconnect cleanly.

- [x] **Step 4: Perform requirement-by-requirement completion audit**

Compare the current repository and artifacts against every section of the design spec and every task above. Mark Android/iOS/HarmonyOS explicitly as Phase 2, not as missing Phase 1 work; do not mark Phase 1 complete if any desktop build/package/protocol gate lacks evidence.

- [x] **Step 5: Commit**

```text
docs: record desktop client interoperability evidence
```
