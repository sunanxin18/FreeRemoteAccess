# Cross-Platform Session Title Bar and App Icon Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the shared icon-only session chrome, a native-behaving Windows title bar, an unobscured remote-content viewport, and a standards-aligned cross-platform app-icon asset family with the Windows window and executable icons installed.

**Architecture:** `frd-ui-model` owns protocol-neutral chrome state, `frd-ui-egui` owns glyph rendering and accessibility, and `frd-shell-desktop` owns the common three-zone layout plus platform adapters. The Windows adapter extends the client frame and handles non-client hit testing so custom centered content coexists with Windows drag, resize, system controls, and Snap Layout. Canonical app-icon layers feed deterministic exports; only the Windows package consumes them in this phase.

**Tech Stack:** Rust 2021, egui 0.36.1, winit 0.30.13, wgpu 30.0.1, Win32/DWM through `windows-sys` 0.59, built-in imagegen, `image` and `ico` for deterministic asset export, `winresource` for the Windows executable resource.

**Spec:** `docs/superpowers/specs/2026-08-28-cross-platform-titlebar-app-icon-design.md`

## Global Constraints

- Login and required identity decisions may use the client area; after connect begins, persistent controls and status must live only in title-bar chrome.
- Windows/Linux keep their native-side controls and macOS keeps native traffic lights; FreeRemoteDesk session controls remain geometrically centered.
- Every state/action glyph has a Simplified-Chinese tooltip, accessible label, keyboard focus, and a shape distinction that does not depend on color.
- Renderer, pointer mapping, and dynamic-resolution reporting use one identical physical-pixel content rectangle below the title bar.
- Protocol, decoder, framebuffer, and renderer-core crates must not depend on platform title-bar APIs.
- Windows must preserve drag, edge resize, maximize/restore, close, DPI behavior, keyboard system menu, and Windows 11 Snap Layout.
- Existing uncommitted Apple protocol/input/render work is preserved; staging and commits remain path-scoped.
- Cargo runs use `RUSTUP_TOOLCHAIN=stable` and `CARGO_BUILD_JOBS=2` and execute serially.

---

### Task 1: Protocol-Neutral Session Chrome Model

**Files:**
- Create: `crates/frd-ui-model/src/chrome.rs`
- Modify: `crates/frd-ui-model/src/lib.rs`
- Modify: `crates/frd-app/src/lib.rs`

**Interfaces:**
- Consumes: `frd_app::AppPage`, `frd_protocol_api::ConnectionStage`, `SessionCapabilities`.
- Produces: `SessionChromeModel`, `ConnectionGlyph`, `CapabilityGlyphState`, `SessionChromeAction`, and `AppController::session_chrome()`.

- [ ] **Step 1: Write failing literal-mapping tests**

Add tests proving that connecting exposes `Connecting + Cancel`, awaiting a frame exposes `WaitingForFrame + Cancel`, a live session exposes `Connected + Disconnect` plus stable audio/clipboard slots, and disconnecting exposes no action. Each expected enum value is a hand-written literal.

```rust
assert_eq!(
    controller.session_chrome(),
    Some(SessionChromeModel {
        connection: ConnectionGlyph::Connected,
        diagnostics: None,
        audio: CapabilityGlyphState::Unavailable,
        clipboard: CapabilityGlyphState::Unavailable,
        action: Some(SessionChromeAction::Disconnect),
    })
);
```

- [ ] **Step 2: Run the focused test and observe RED**

Run: `cargo test -p frd-app session_chrome -- --nocapture`

Expected: compilation fails because `SessionChromeModel` and `session_chrome` do not exist.

- [ ] **Step 3: Implement the minimal shared model**

Define the enums and immutable model in `frd-ui-model::chrome`. Map `AppPage` to the model in `frd-app`; do not put egui, winit, or platform types in either crate. Keep audio and clipboard slots present for every post-connect state so capability changes cannot shift title-bar geometry.

- [ ] **Step 4: Run focused and owning-crate tests GREEN**

Run: `cargo test -p frd-ui-model && cargo test -p frd-app`

- [ ] **Step 5: Review boundary mutations**

Mentally mutate connected to connecting, swap audio and clipboard, and expose Disconnect while disconnecting. Confirm a named test fails for each mutation.

---

### Task 2: Icon-Only egui Session Cluster

**Files:**
- Create: `crates/frd-ui-egui/src/session_chrome.rs`
- Modify: `crates/frd-ui-egui/src/lib.rs`
- Modify: `crates/frd-ui-egui/src/session.rs`
- Modify: `crates/frd-ui-egui/Cargo.toml`

**Interfaces:**
- Consumes: `SessionChromeModel` and `SessionChromeAction` from Task 1.
- Produces: `show_session_chrome(ui: &mut egui::Ui, model: &SessionChromeModel) -> Option<SessionChromeAction>` and `SessionChromeMetrics` containing the fixed slot geometry.

- [ ] **Step 1: Write failing behavior tests for semantic slots**

Test the pure presentation mapping rather than egui internals: each connection state has a distinct path identifier and accessible label; audio/clipboard active and unavailable states have distinct path identifiers; disabled slots cannot return an action; all variants reserve the same four slot widths.

```rust
assert_eq!(glyph(ConnectionGlyph::Connected).accessible_name, "已连接");
assert_ne!(
    glyph(ConnectionGlyph::Connected).path_id,
    glyph(ConnectionGlyph::Connecting).path_id
);
assert_eq!(chrome_metrics(&connected).total_width, chrome_metrics(&waiting).total_width);
```

- [ ] **Step 2: Run the focused test and observe RED**

Run: `cargo test -p frd-ui-egui session_chrome -- --nocapture`

Expected: compilation fails because the new module and mapping do not exist.

- [ ] **Step 3: Implement compact vector glyphs**

Draw connection, speaker, clipboard, and disconnect glyphs with `egui::Painter` paths and primitives so they remain resolution independent and require no icon font. Use fixed 32-point interaction slots, visible keyboard focus, `on_hover_text` tooltips, and widget accessibility labels. Unsupported capabilities remain visible and disabled.

- [ ] **Step 4: Remove post-connect text/toolbars from `show_session_page`**

Keep only connection-form, required identity-decision, and login-surface failure content in `session.rs`. Connecting, awaiting-frame, disconnecting, and remote-session states render no persistent client-area widget.

- [ ] **Step 5: Run the crate tests GREEN**

Run: `cargo test -p frd-ui-egui`

---

### Task 3: Common Three-Zone Window Chrome and Windows Native Adapter

**Files:**
- Create: `crates/frd-shell-desktop/src/window_chrome.rs`
- Create: `crates/frd-shell-desktop/src/platform/mod.rs`
- Create: `crates/frd-shell-desktop/src/platform/windows.rs`
- Create: `crates/frd-shell-desktop/src/platform/macos.rs`
- Create: `crates/frd-shell-desktop/src/platform/linux.rs`
- Modify: `crates/frd-shell-desktop/src/lib.rs`
- Modify: `crates/frd-shell-desktop/src/application.rs`
- Modify: `crates/frd-shell-desktop/Cargo.toml`

**Interfaces:**
- Consumes: `SessionChromeMetrics`, `SessionChromeAction`, `winit::window::Window`.
- Produces: `ChromeLayout::for_window(width_px, height_px, scale_factor, native_leading_px, native_trailing_px)`, `WindowChromeAdapter`, `ChromeHitRegions`, and `WindowChromeAction`.

- [ ] **Step 1: Write failing pure layout and hit-test tests**

Use literal rectangles to prove the session cluster center equals `window_width / 2` even when native leading/trailing widths differ. Test that the title-bar drag region excludes all session and window buttons, and that maximize hit testing returns the semantic maximize region.

```rust
let layout = ChromeLayout::for_window(1200, 800, 1.5, 72, 144).unwrap();
assert_eq!(layout.session_center_x(), 600);
assert!(!layout.drag_region.contains(layout.audio_button.center()));
assert_eq!(layout.hit_test(layout.maximize_button.center()), ChromeHit::Maximize);
```

- [ ] **Step 2: Run the focused test and observe RED**

Run: `cargo test -p frd-shell-desktop window_chrome -- --nocapture`

Expected: compilation fails because `ChromeLayout` does not exist.

- [ ] **Step 3: Implement common geometry and platform trait**

Define a platform-neutral adapter contract:

```rust
pub trait WindowChromeAdapter {
    fn configure(&mut self, window: &winit::window::Window) -> Result<(), WindowChromeError>;
    fn native_insets(&self, window: &winit::window::Window) -> NativeChromeInsets;
    fn publish_hit_regions(&mut self, regions: ChromeHitRegions);
    fn execute(&mut self, window: &winit::window::Window, action: WindowChromeAction);
}
```

macOS and Linux modules compile behind target cfg and expose the same contract; this phase does not claim runtime validation for them.

- [ ] **Step 4: Implement Windows DWM/non-client integration**

Use `DwmExtendFrameIntoClientArea`, `WM_NCCALCSIZE`, `WM_NCHITTEST`, and a window subclass with lifetime-owned shared hit regions. Pass caption-button hit testing through `DwmDefWindowProc` first. Return `HTMAXBUTTON` over maximize/restore so Windows 11 supplies Snap Layout; return resize-edge codes around the frame and `HTCAPTION` only for the drag region. Force one frame recalculation with `SetWindowPos(..., SWP_FRAMECHANGED)` after installation and remove the subclass on destruction.

- [ ] **Step 5: Connect window actions**

Map minimize, maximize/restore, close, and session actions without exposing Win32 types to shared UI. Double-clicking a drag region toggles maximize; Alt+Space remains handled by the system menu path.

- [ ] **Step 6: Run Windows shell tests GREEN**

Run: `cargo test -p frd-shell-desktop`

---

### Task 4: Single Remote-Content Rectangle and Input Isolation

**Files:**
- Modify: `crates/frd-core/src/viewport.rs`
- Modify: `crates/frd-shell-desktop/src/application.rs`
- Modify: `crates/frd-render-wgpu/src/pass.rs`
- Modify: `crates/frd-render-wgpu/src/remote_texture.rs`
- Modify: `crates/frd-compositor-wgpu/src/lib.rs`

**Interfaces:**
- Consumes: `ChromeLayout::content_rect`, existing `ContentViewport::fit_in` and `RemoteRenderer::record_in` worktree changes.
- Produces: one physical-pixel `ContentViewport` reused by rendering, pointer input, and viewport reporting.

- [ ] **Step 1: Extend the existing failing geometry tests**

Add a maximize/restore and DPI case with literal expected rectangles. Add an ownership test proving title-bar mouse and keyboard events do not create `InputEvent`, while a point one pixel below the title bar maps to remote row zero.

- [ ] **Step 2: Run focused tests and observe RED**

Run: `cargo test -p frd-core inset_drawable && cargo test -p frd-shell-desktop title_bar_input -- --nocapture`

Expected: the new DPI transition or title-bar ownership assertion fails before the common chrome rectangle is wired.

- [ ] **Step 3: Replace `remote-toolbar` with title-bar chrome**

Render the center session cluster in the one title-bar row. Delete the second-row toolbar path. Feed `ChromeLayout::content_rect` to `ContentViewport::fit_in`, `RemoteRenderer::record_in`, pointer mapping, and dynamic-resolution viewport publication.

- [ ] **Step 4: Dispatch chrome actions and preserve remote keyboard focus**

Convert `SessionChromeAction::Cancel/Disconnect` to existing `AppIntent` values. Consume every pointer event inside the title bar. Keyboard input goes remote only while the remote content owns focus and the session gate is interactive.

- [ ] **Step 5: Run affected tests GREEN**

Run: `cargo test -p frd-core && cargo test -p frd-render-wgpu && cargo test -p frd-compositor-wgpu && cargo test -p frd-shell-desktop`

---

### Task 5: Canonical Icon Layers and Deterministic Platform Exports

**Files:**
- Create: `assets/app-icon/source/portal-foreground.png`
- Create: `assets/app-icon/source/app-icon-master.png`
- Create: `assets/app-icon/apple/background.png`
- Create: `assets/app-icon/apple/foreground.png`
- Create: `assets/app-icon/apple/monochrome.png`
- Create: `assets/app-icon/android/background.png`
- Create: `assets/app-icon/android/foreground.png`
- Create: `assets/app-icon/android/monochrome.png`
- Create: `assets/app-icon/google-play/icon-512.png`
- Create: `assets/app-icon/windows/freeremotedesk.ico`
- Create: `assets/app-icon/windows/window-icon-64.rgba`
- Create: `assets/app-icon/linux/hicolor/16x16/apps/freeremotedesk.png`
- Create: `assets/app-icon/linux/hicolor/32x32/apps/freeremotedesk.png`
- Create: `assets/app-icon/linux/hicolor/48x48/apps/freeremotedesk.png`
- Create: `assets/app-icon/linux/hicolor/64x64/apps/freeremotedesk.png`
- Create: `assets/app-icon/linux/hicolor/128x128/apps/freeremotedesk.png`
- Create: `assets/app-icon/linux/hicolor/256x256/apps/freeremotedesk.png`
- Create: `assets/app-icon/linux/hicolor/512x512/apps/freeremotedesk.png`
- Create: `tools/frd-icon-assets/Cargo.toml`
- Create: `tools/frd-icon-assets/src/main.rs`
- Modify: `Cargo.toml`

**Interfaces:**
- Consumes: the approved imagegen portal foreground and the full-bleed sRGB
  navy background `rgba(6, 27, 69, 255)`.
- Produces: validated Apple/Android source layers, Google Play PNG, Windows ICO/raw RGBA, and Linux PNG sizes.

- [ ] **Step 1: Generate the transparent canonical foreground**

Use built-in imagegen in edit mode with `remote3.jpeg` as the identity reference. Preserve the oval portal and central spiral, remove all background/shadow/mask, simplify fine particles, and require genuine alpha. Save the selected result to `assets/app-icon/source/portal-foreground.png`; keep the original source unchanged.

- [ ] **Step 2: Write failing asset-export tests**

The tool tests create exports in a temporary directory and assert literal dimensions, RGBA modes, ICO entries `[16, 24, 32, 48, 64, 128, 256]`, Google Play 512-by-512 size, Android essential alpha bounds within the scaled 66/108 safe zone, and nonempty foreground pixels at 16-by-16.

- [ ] **Step 3: Run the tool tests and observe RED**

Run: `cargo test -p frd-icon-assets -- --nocapture`

Expected: compilation fails because the exporter does not exist.

- [ ] **Step 4: Implement deterministic exports**

Use the `image` crate for alpha-aware Lanczos resizing/compositing and the `ico` crate for PNG-compressed multi-resolution ICO entries. Generate a full-bleed navy background, flattened master, monochrome foreground from the canonical alpha mask, and platform outputs. Reject any source whose essential alpha bounds exceed the Android safe zone after normalization.

- [ ] **Step 5: Generate and verify committed assets**

Run: `cargo run -p frd-icon-assets -- assets/app-icon/source/portal-foreground.png assets/app-icon`

Then run: `cargo test -p frd-icon-assets`

---

### Task 6: Install the Windows Window and Executable Icons

**Files:**
- Create: `apps/freeremotedesk-windows/build.rs`
- Create: `apps/freeremotedesk-windows/tests/icon_resource.rs`
- Modify: `apps/freeremotedesk-windows/Cargo.toml`
- Modify: `apps/freeremotedesk-windows/src/main.rs`
- Modify: `crates/frd-shell-desktop/src/application.rs`

**Interfaces:**
- Consumes: `assets/app-icon/windows/freeremotedesk.ico` and `window-icon-64.rgba` from Task 5.
- Produces: `DesktopWindowConfiguration { icon: Option<winit::window::Icon> }` and a release executable carrying the same icon family.

- [ ] **Step 1: Write failing window-configuration and resource tests**

Test that the Windows app constructs a 64-by-64 icon from exactly `64 * 64 * 4` bytes and passes it to the shell configuration. In the Windows-only integration test, call `ExtractIconExW` on `env!("CARGO_BIN_EXE_freeremotedesk-windows")`, assert that the executable reports at least one large and one small icon, and destroy both returned handles after the assertion.

- [ ] **Step 2: Run focused tests and observe RED**

Run: `cargo test -p freeremotedesk-windows window_icon -- --nocapture`

Expected: compilation fails because the configuration and embedded bytes do not exist.

- [ ] **Step 3: Add runtime and executable icon wiring**

Build the runtime `winit::window::Icon` from `include_bytes!` raw RGBA and pass it through `DesktopWindowConfiguration`. Use `winresource` in `build.rs` to set `freeremotedesk.ico`, emit `cargo:rerun-if-changed`, and fail the Windows build if the asset is missing or invalid.

- [ ] **Step 4: Run the focused tests GREEN**

Run: `cargo test -p freeremotedesk-windows`

---

### Task 7: Serial Verification and Windows Acceptance

**Files:**
- Modify: `docs/validation/windows-apple-wgpu-parity.md`

**Interfaces:**
- Consumes: all previous tasks and the authorized credential provider.
- Produces: fresh automated evidence plus bounded Windows UI/live-session evidence.

- [ ] **Step 1: Run formatting and complete serial tests**

Run, one at a time:

```powershell
$env:RUSTUP_TOOLCHAIN='stable'
$env:CARGO_BUILD_JOBS='2'
cargo fmt --all -- --check
cargo test --workspace
cargo build --workspace --no-default-features
cargo build -p freeremotedesk-windows --release
cargo clippy -p freeremotedesk-windows -p frd-shell-desktop -p frd-ui-egui -p frd-ui-model --all-targets -- -D warnings
```

- [ ] **Step 2: Inspect packaged icon resources**

Run the asset verifier against `target/release/freeremotedesk-windows.exe` and confirm the ICO contains every required size. Launch one instance and verify the window, taskbar, Alt-Tab, and Explorer use the same identity.

- [ ] **Step 3: Run offline title-bar acceptance**

Use `--test-texture` at 100, 150, and 200 percent scaling. Verify native drag, edge resize, Snap Layout hover, minimize, maximize/restore, close, centered glyphs, tooltips, keyboard focus, and that no second toolbar appears.

- [ ] **Step 4: Run one bounded live Apple session**

Load credentials only from the approved environment provider. Verify one complete frame and incremental refresh, title-bar controls do not overlap the remote image, title-bar pointer events do not reach the Mac, remote keyboard input still works, and Disconnect exits cleanly. Do not launch a second client.

- [ ] **Step 5: Record evidence and audit every spec requirement**

Update the validation document with commands, exit codes, observed DPI/window behavior, live session outcome, and any unverified macOS/Linux runtime items. Re-read the spec line by line and leave the goal active if any Windows deliverable or required evidence is missing.
