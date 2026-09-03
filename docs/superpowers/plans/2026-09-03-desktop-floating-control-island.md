# Desktop Floating Control Island Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the persistent 44-point Windows session title bar with the approved movable, auto-hiding, accessible floating control island without changing the remote framebuffer geometry or any server protocol.

**Architecture:** Keep one winit window, one egui pass, one WGPU command encoder, and one present. Split remote-content geometry from overlay geometry, drive the overlay from a shell-owned deadline state machine, and route every island hit locally before remote input. Window operations cross only the `WindowChromeAdapter`; protocol adapters continue to publish the existing protocol-neutral session model.

**Tech Stack:** Rust 2021, winit 0.30.13, egui/egui-winit/egui-wgpu 0.36.1, wgpu 30.0.1, AccessKit through egui-winit, Win32/DWM through windows-sys 0.59, pinned Google Material Symbols Rounded subset.

**Spec:** `docs/superpowers/specs/2026-08-30-desktop-floating-control-island-design.md`

## Global Constraints

- Current implementation target is Windows; macOS and Linux expose only capabilities proven by their platform adapters.
- The island uses the existing window, swapchain, encoder, submit, and present; no helper window, second render thread, or blur pass.
- Island visibility and position never alter `RemoteContentLayout`, server viewport, framebuffer generation, aspect fit, or pointer transform.
- Hidden state renders only a centered 50-percent-alpha green line and schedules no continuous redraw.
- Pointer, wheel, key, IME, drag, tooltip, popover, and accessibility actions owned by the island never enter `SessionInput`.
- Hover reveal waits 150 ms; hide after leaving the complete island/transient union waits 700 ms.
- Forced reveal publishes the local hit map only after `ReleaseAll` is accepted; failure blocks remote input and exposes a stable shell diagnostic.
- FreeRemoteDesk-owned glyphs come only from the pinned Material Symbols Rounded subset and retain Simplified-Chinese tooltip/accessibility names and 44-by-44 logical-point targets.
- Keep Apple High Performance, Apple Standard/MVS, RDP, and future RFB protocol state out of the floating-chrome modules.
- Automated coverage is limited to state, geometry invariance, command routing, input ownership, and deterministic icon/font checks.
- Preserve the user's existing uncommitted `README.md`, `crates/frd-protocol-apple/src/hpss.rs`, `src/main.rs`, and `docs/validation/apple-dual-mode-blockers-20260901.md` work.

---

### Task 1: Protocol-neutral island actions and platform capabilities

**Files:**
- Modify: `crates/frd-ui-model/src/chrome.rs`
- Modify: `crates/frd-ui-model/src/lib.rs`
- Modify: `crates/frd-app/src/controller.rs`
- Modify: `crates/frd-app/src/lib.rs`

**Interfaces:**
- Consumes: existing `SessionChromeModel`, `SessionTiming`, `CapabilityGlyphState`, and `AppIntent::{CancelConnect, Disconnect}`.
- Produces: `IslandAction`, `IslandWindowCapabilities`, and a `SessionChromeModel` whose session action is expressed as `Option<IslandAction>`.

- [ ] **Step 1: Write failing model and controller tests**

```rust
#[test]
fn remote_session_exposes_disconnect_but_no_platform_window_policy() {
    let chrome = connected_controller().session_chrome().unwrap();
    assert_eq!(chrome.action, Some(IslandAction::Disconnect));
    assert_eq!(chrome.presentation_timing.unwrap().milliseconds, 24);
}

#[test]
fn window_capabilities_are_shell_data_not_protocol_data() {
    assert_eq!(IslandWindowCapabilities::default(), IslandWindowCapabilities::NONE);
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run: `cargo test -p frd-ui-model island -- --nocapture && cargo test -p frd-app island -- --nocapture`

Expected: compilation fails because `IslandAction` and `IslandWindowCapabilities` do not exist.

- [ ] **Step 3: Replace the old action type in one public model path**

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IslandAction {
    ShowConnectionDetails,
    CancelConnect,
    Disconnect,
    ToggleRemoteAudio,
    OpenClipboard,
    MinimizeWindow,
    ToggleMaximizeWindow,
    CloseWindow,
    ShowSystemMenu,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IslandWindowCapabilities {
    pub minimize: bool,
    pub maximize: bool,
    pub close: bool,
    pub system_menu: bool,
    pub begin_move: bool,
}

impl IslandWindowCapabilities {
    pub const NONE: Self = Self {
        minimize: false,
        maximize: false,
        close: false,
        system_menu: false,
        begin_move: false,
    };

    pub const WINDOWS: Self = Self {
        minimize: true,
        maximize: true,
        close: true,
        system_menu: true,
        begin_move: true,
    };
}
```

Delete `SessionChromeAction`; do not add an alias. The controller emits only connection/session actions. Audio and clipboard remain disabled unless their existing product commands are actually available; the shell, not the controller, supplies window capabilities.

- [ ] **Step 4: Run focused model/app tests and check the old symbol is gone**

Run: `cargo test -p frd-ui-model && cargo test -p frd-app && rg -n "SessionChromeAction" crates apps`

Expected: both test suites pass; `rg` returns no production or test references.

- [ ] **Step 5: Commit**

```powershell
git add crates/frd-ui-model/src/chrome.rs crates/frd-ui-model/src/lib.rs crates/frd-app/src/controller.rs crates/frd-app/src/lib.rs
git commit -m "refactor(ui): define protocol-neutral island actions"
```

---

### Task 2: Floating state machine and invariant geometry

**Files:**
- Create: `crates/frd-shell-desktop/src/floating_chrome.rs`
- Modify: `crates/frd-shell-desktop/src/window_chrome.rs`
- Modify: `crates/frd-shell-desktop/src/lib.rs`

**Interfaces:**
- Consumes: `frd_core::PixelRect`, logical scale, window physical size, native safe-area insets, and `IslandWindowCapabilities`.
- Produces: `FloatingChromeController`, `ControlIslandState`, `RemoteContentLayout`, `ChromeOverlayLayout`, `ChromeHitMap`, `ChromeHitTarget`, and `WindowChromeCommand`.

- [ ] **Step 1: Write state-machine and geometry tests**

```rust
#[test]
fn hover_reveals_after_150_ms_and_hides_700_ms_after_leave() {
    let start = Instant::now();
    let mut chrome = FloatingChromeController::connected_default(start);
    chrome.observe_top_sensor(true, false, start);
    assert_eq!(chrome.state(), ControlIslandState::RevealPending);
    assert!(!chrome.advance(start + Duration::from_millis(149)));
    assert!(chrome.advance(start + Duration::from_millis(150)));
    chrome.observe_island_union(false, false, start + Duration::from_millis(150));
    assert!(!chrome.advance(start + Duration::from_millis(849)));
    assert!(chrome.advance(start + Duration::from_millis(850)));
    assert_eq!(chrome.state(), ControlIslandState::Hidden);
}

#[test]
fn island_visibility_never_changes_remote_content() {
    let snapshot = ChromeGeometrySnapshot::new(1600, 900, 1.5, NativeChromeInsets::default()).unwrap();
    let hidden = snapshot.layouts(ControlIslandPlacement::default(), false).unwrap();
    let visible = snapshot.layouts(ControlIslandPlacement::default(), true).unwrap();
    assert_eq!(hidden.remote, visible.remote);
    assert_eq!(hidden.remote.content_rect, PixelRect { x: 0, y: 0, width: 1600, height: 900 });
}
```

Also cover reveal cancellation, held-input deferral, pin/unpin, clamped reposition, line has no hit target, island controls precede remote content, and reposition/window-move targets are distinct.

- [ ] **Step 2: Run the focused shell tests and verify RED**

Run: `cargo test -p frd-shell-desktop floating_chrome -- --nocapture`

Expected: compilation fails because the floating controller and split layouts do not exist.

- [ ] **Step 3: Implement the pure controller and geometry types**

```rust
pub const REVEAL_DELAY: Duration = Duration::from_millis(150);
pub const HIDE_DELAY: Duration = Duration::from_millis(700);
pub const TOP_SENSOR_POINTS: f32 = 12.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlIslandState { Hidden, RevealPending, Visible, HidePending, Pinned }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlIslandPlacement {
    pub normalized_center_x: f32,
    pub top_points: f32,
}

impl Default for ControlIslandPlacement {
    fn default() -> Self {
        Self { normalized_center_x: 0.5, top_points: 0.0 }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChromeHitTarget {
    IslandAction(IslandAction),
    IslandRepositionHandle,
    WindowMoveRegion,
    NativeChrome,
    RemoteContent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowChromeCommand {
    BeginMove,
    Minimize,
    ToggleMaximize,
    Close,
    ShowSystemMenu,
}

pub struct ChromeLayouts {
    pub remote: RemoteContentLayout,
    pub overlay: ChromeOverlayLayout,
    pub hit_map: ChromeHitMap,
}

pub struct ChromeGeometrySnapshot {
    pub window_size: PixelSize,
    pub scale_factor: f64,
    pub native: NativeChromeInsets,
}

impl FloatingChromeController {
    pub fn observe_top_sensor(&mut self, inside: bool, remote_input_held: bool, now: Instant);
    pub fn observe_island_union(&mut self, hovered: bool, focused_or_pressed: bool, now: Instant);
    pub fn force_reveal_after_release(&mut self, now: Instant);
    pub fn advance(&mut self, now: Instant) -> bool;
    pub fn next_deadline(&self) -> Option<Instant>;
    pub fn normalized_position(&self) -> (f32, f32);
    pub fn reposition(&mut self, delta_points: egui::Vec2, bounds: egui::Rect);
}
```

Keep this module free of winit windows, native handles, protocol IDs, framebuffers, and renderer resources. Geometry creation returns a complete candidate; callers retain their last valid `ChromeHitMap` on error.

- [ ] **Step 4: Run focused tests**

Run: `cargo test -p frd-shell-desktop floating_chrome -- --nocapture && cargo test -p frd-shell-desktop window_chrome -- --nocapture`

Expected: all state and invariant geometry tests pass.

- [ ] **Step 5: Commit**

```powershell
git add crates/frd-shell-desktop/src/floating_chrome.rs crates/frd-shell-desktop/src/window_chrome.rs crates/frd-shell-desktop/src/lib.rs
git commit -m "feat(shell): add floating chrome state and geometry"
```

---

### Task 3: Accessible control-island renderer and deterministic Material glyphs

**Files:**
- Rename: `crates/frd-ui-egui/src/session_chrome.rs` to `crates/frd-ui-egui/src/control_island.rs`
- Modify: `crates/frd-ui-egui/src/lib.rs`
- Modify: `tools/update-material-symbols-rounded.ps1`
- Modify: `assets/ui-icons/material-symbols-rounded-24-400.ttf`
- Modify: `assets/ui-icons/README.md`

**Interfaces:**
- Consumes: `SessionChromeModel`, `IslandAction`, `IslandWindowCapabilities`, visibility, placement, maximized state, and accessibility focus request.
- Produces: `ControlIslandRenderResult { action, hovered_union, focused_union, pressed, reposition_delta, window_move_requested }` and the exact logical rectangles used to build `ChromeOverlayLayout`.

- [ ] **Step 1: Write failing renderer tests**

```rust
#[test]
fn windows_actions_have_material_glyphs_and_44_point_targets() {
    let semantics = window_action_semantics(IslandWindowCapabilities::WINDOWS, false);
    assert_eq!(semantics.iter().map(|item| item.action).collect::<Vec<_>>(), vec![
        IslandAction::MinimizeWindow,
        IslandAction::ToggleMaximizeWindow,
        IslandAction::CloseWindow,
    ]);
    assert!(semantics.iter().all(|item| item.target_size == 44.0));
    assert!(semantics.iter().all(|item| !item.tooltip.is_empty()));
}

#[test]
fn hidden_renderer_has_only_a_visual_reveal_line() {
    let result = render_island_fixture(IslandVisibility::Hidden, 1200.0);
    assert!(result.action.is_none());
    assert!(result.hit_rects.is_empty());
    assert_eq!(result.reveal_line_alpha, 0.5);
}
```

- [ ] **Step 2: Run the UI tests and verify RED**

Run: `cargo test -p frd-ui-egui control_island -- --nocapture`

Expected: compilation fails because the renderer API and window glyph semantics do not exist.

- [ ] **Step 3: Extend and regenerate the pinned Material subset**

Add the official names/codepoints `remove`, `fullscreen`, `fullscreen_exit`, and `drag_indicator` to `tools/update-material-symbols-rounded.ps1`, run `powershell -ExecutionPolicy Bypass -File tools/update-material-symbols-rounded.ps1`, and record the resulting deterministic subset hash and names in `assets/ui-icons/README.md`. Do not hand-draw these glyphs.

- [ ] **Step 4: Render the line and island in the existing egui pass**

```rust
pub struct ControlIslandRenderInput<'a> {
    pub model: &'a SessionChromeModel,
    pub window_capabilities: IslandWindowCapabilities,
    pub visible: bool,
    pub maximized: bool,
    pub island_rect: egui::Rect,
    pub reveal_line_rect: egui::Rect,
    pub focus_first: bool,
    pub opaque_material: bool,
}

pub struct ControlIslandRenderResult {
    pub action: Option<IslandAction>,
    pub hovered_union: bool,
    pub focused_union: bool,
    pub pressed: bool,
    pub reposition_delta: egui::Vec2,
    pub window_move_requested: bool,
    pub hit_rects: Vec<(egui::Rect, IslandAction)>,
    pub reveal_line_alpha: f32,
}

pub fn show_control_island(ctx: &egui::Context, input: ControlIslandRenderInput<'_>) -> ControlIslandRenderResult;
```

Use a fixed `egui::Area`, a rounded 18--25 percent neutral outer material, compact contrast plates, no blur, and no `Panel::top`. Keep the approved 64-point timing slot and its source-aware tooltip. Collapse unsupported/low-priority capability details before controls overlap.

- [ ] **Step 5: Run UI and deterministic asset tests**

Run: `cargo test -p frd-ui-egui && powershell -ExecutionPolicy Bypass -File tools/update-material-symbols-rounded.ps1 && git diff --exit-code -- assets/ui-icons/material-symbols-rounded-24-400.ttf`

Expected: UI tests pass and the second asset generation produces no binary diff.

- [ ] **Step 6: Commit**

```powershell
git add crates/frd-ui-egui/src/control_island.rs crates/frd-ui-egui/src/lib.rs tools/update-material-symbols-rounded.ps1 assets/ui-icons/material-symbols-rounded-24-400.ttf assets/ui-icons/README.md
git commit -m "feat(ui): render accessible floating control island"
```

---

### Task 4: One-time platform chrome API migration

**Files:**
- Modify: `crates/frd-shell-desktop/src/window_chrome.rs`
- Modify: `crates/frd-shell-desktop/src/platform/windows.rs`
- Modify: `crates/frd-shell-desktop/src/platform/macos.rs`
- Modify: `crates/frd-shell-desktop/src/platform/linux.rs`
- Modify: `crates/frd-shell-desktop/src/lib.rs`
- Modify: `crates/frd-shell-desktop/src/application.rs`

**Interfaces:**
- Consumes: complete `ChromeHitMap`, `WindowChromeCommand`, and `IslandWindowCapabilities`.
- Produces: a `WindowChromeAdapter` that reports platform capabilities, publishes one atomic hit map, and returns `Result<(), WindowChromeError>` for every native command.

- [ ] **Step 1: Write failing command and native-hit tests**

```rust
#[test]
fn windows_maximize_rect_preserves_native_snap_hit() {
    let map = windows_hit_map_fixture();
    assert_eq!(map.hit_test(map.maximize_rect.unwrap().center()), ChromeHitTarget::IslandAction(IslandAction::ToggleMaximizeWindow));
    assert_eq!(windows_native_hit(&map, map.maximize_rect.unwrap().center()), HTMAXBUTTON);
}

#[test]
fn non_windows_adapters_do_not_advertise_windows_caption_actions() {
    assert_eq!(unverified_desktop_capabilities(), IslandWindowCapabilities::NONE);
}
```

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test -p frd-shell-desktop platform -- --nocapture && cargo test -p frd-shell-desktop window_chrome -- --nocapture`

Expected: compilation fails because the adapter still accepts `ChromeHitRegions` and `WindowChromeAction`.

- [ ] **Step 3: Replace the old platform API without aliases**

```rust
pub trait WindowChromeAdapter {
    fn configure(&mut self, window: &winit::window::Window) -> Result<(), WindowChromeError>;
    fn refresh_for_dpi(&mut self, window: &winit::window::Window) -> Result<(), WindowChromeError>;
    fn native_insets(&self, window: &winit::window::Window) -> NativeChromeInsets;
    fn capabilities(&self) -> IslandWindowCapabilities;
    fn publish_hit_map(&mut self, hit_map: ChromeHitMap);
    fn execute(&mut self, window: &winit::window::Window, command: WindowChromeCommand) -> Result<(), WindowChromeError>;
}
```

Windows maps pointer hover/click over the one maximize rectangle to `HTMAXBUTTON` so Windows 11 Snap Layout remains native; keyboard and AccessKit use `WindowChromeCommand::ToggleMaximize`. `BeginMove` calls `Window::drag_window`, `ShowSystemMenu` calls `Window::show_window_menu`, and other commands use the existing winit/Win32 path. macOS/Linux keep system decorations and advertise no copied Windows caption actions in this Windows-first phase.

- [ ] **Step 4: Remove the old production types and hand-drawn control painter**

Delete `ChromeLayout`, `ChromeHitRegions`, `ChromeHit`, `WindowChromeAction`, and `paint_platform_window_controls`. Do not add compatibility aliases or a second hit-test path.

- [ ] **Step 5: Run platform tests and boundary search**

Run: `cargo test -p frd-shell-desktop platform -- --nocapture && cargo test -p frd-shell-desktop window_chrome -- --nocapture && rg -n "ChromeLayout|ChromeHitRegions|enum ChromeHit|WindowChromeAction|paint_platform_window_controls" crates apps`

Expected: tests pass and the search returns no old production symbols.

- [ ] **Step 6: Commit**

```powershell
git add crates/frd-shell-desktop/src/window_chrome.rs crates/frd-shell-desktop/src/platform crates/frd-shell-desktop/src/lib.rs crates/frd-shell-desktop/src/application.rs
git commit -m "refactor(shell): migrate platform chrome to island hit map"
```

---

### Task 5: Application integration, deadlines, input handoff, and AccessKit reveal

**Files:**
- Modify: `crates/frd-shell-desktop/src/application.rs`
- Modify: `crates/frd-shell-desktop/src/input.rs`
- Modify: `crates/frd-shell-desktop/src/repaint.rs`
- Modify: `crates/frd-shell-desktop/src/floating_chrome.rs`

**Interfaces:**
- Consumes: `FloatingChromeController`, renderer output, the atomic hit map, and platform commands.
- Produces: one integrated overlay path, `InputRouter::has_remote_held_input()`, a checked forced-reveal handoff, and a single nearest-deadline `ControlFlow` calculation.

- [ ] **Step 1: Write failing integration tests**

```rust
#[test]
fn visible_island_consumes_pointer_without_changing_remote_viewport() {
    let before = remote_content_fixture();
    let hit = ChromeHitTarget::IslandAction(IslandAction::Disconnect);
    assert_eq!(ownership_for_hit(hit, true), InputOwnership::Ui);
    assert_eq!(before, remote_content_fixture());
}

#[test]
fn forced_reveal_blocks_when_release_all_cannot_be_enqueued() {
    let mut fixture = held_remote_input_fixture_with_closed_command_port();
    assert_eq!(fixture.force_reveal(), Err(ForcedRevealError::ReleaseRejected));
    assert_eq!(fixture.input_gate(), InputGate::Blocked);
    assert_eq!(fixture.chrome_state(), ControlIslandState::Hidden);
}

#[test]
fn hidden_state_has_no_repaint_deadline() {
    assert_eq!(FloatingChromeController::hidden().next_deadline(), None);
}
```

- [ ] **Step 2: Run focused integration tests and verify RED**

Run: `cargo test -p frd-shell-desktop forced_reveal -- --nocapture && cargo test -p frd-shell-desktop island_input -- --nocapture`

Expected: tests fail because remote-held-state inspection and checked handoff are missing.

- [ ] **Step 3: Integrate the overlay without reserving a title row**

Replace `egui::Panel::top("window-session-chrome")` with `show_control_island` inside the existing root `run_ui`. Set `remote_area` from `RemoteContentLayout` only. Connecting/waiting/failure states remain visible for cancel/diagnostics; after the first successful remote frame, the controller begins the 700 ms hide path. Reconnect creates a fresh default top-center placement.

- [ ] **Step 4: Route pointer and keyboard ownership before remote input**

```rust
impl InputRouter {
    pub fn has_remote_held_input(&self) -> bool;
}

fn force_reveal_island(&mut self, now: Instant) -> Result<(), ForcedRevealError> {
    if let Some(release) = self.input.enter_local_chrome() {
        self.sessions.send_command(SessionCommand::Input(release))
            .map_err(|_| ForcedRevealError::ReleaseRejected)?;
    }
    self.floating_chrome.force_reveal_after_release(now);
    Ok(())
}
```

Hover observes only `CursorMoved`; it does not consume click, wheel, or gesture while hidden. Once visible, all island/transient rectangles are local and non-penetrating. `Ctrl+Alt+Home` and the AccessKit root action call the same checked handoff. If command submission fails, keep the island hit map hidden, block the input gate, and show a stable shell diagnostic.

- [ ] **Step 5: Unite timer scheduling**

Add `floating_chrome.next_deadline()` to the existing `about_to_wait()` minimum over repaint, shutdown, metrics, and DPI deadlines. At deadline, call `advance(now)` once and request one redraw only when state/opacity changed. Hidden state returns no deadline.

- [ ] **Step 6: Run shell tests and checks**

Run: `cargo test -p frd-shell-desktop && cargo check -p freeremotedesk-windows && cargo fmt -- --check`

Expected: all shell tests and the Windows application check pass.

- [ ] **Step 7: Commit**

```powershell
git add crates/frd-shell-desktop/src/application.rs crates/frd-shell-desktop/src/input.rs crates/frd-shell-desktop/src/repaint.rs crates/frd-shell-desktop/src/floating_chrome.rs
git commit -m "feat(windows): integrate auto-hiding control island"
```

---

### Task 6: Windows accessibility preference gates and bounded acceptance

**Files:**
- Modify: `crates/frd-shell-desktop/Cargo.toml`
- Modify: `crates/frd-shell-desktop/src/platform/windows.rs`
- Modify: `crates/frd-shell-desktop/src/platform/macos.rs`
- Modify: `crates/frd-shell-desktop/src/platform/linux.rs`
- Modify: `crates/frd-shell-desktop/src/application.rs`
- Create: `docs/validation/windows-floating-control-island.md`
- Modify (primary-agent-only surgical hunk): `README.md`

**Interfaces:**
- Consumes: platform appearance capabilities and the completed island integration.
- Produces: fail-closed high-contrast/reduced-motion policy plus recorded Windows build/manual evidence.

- [ ] **Step 1: Write failing platform-preference tests around injected query results**

```rust
#[test]
fn unknown_preferences_choose_opaque_immediate_rendering() {
    assert_eq!(AppearancePolicy::from_probe(None), AppearancePolicy { opaque_material: true, animate: false });
}

#[test]
fn high_contrast_disables_translucency_and_motion() {
    assert_eq!(AppearancePolicy::from_probe(Some((true, true))), AppearancePolicy { opaque_material: true, animate: false });
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run: `cargo test -p frd-shell-desktop appearance_policy -- --nocapture`

Expected: compilation fails because the appearance probe and policy do not exist.

- [ ] **Step 3: Add the Windows preference probe**

Enable the precise `windows-sys` feature required for `SystemParametersInfoW`, query `SPI_GETHIGHCONTRAST` and `SPI_GETCLIENTAREAANIMATION`, and refresh on Windows setting/theme change. Unknown or failed queries use opaque material and immediate state transitions. macOS/Linux return the same conservative policy until their native adapters are implemented and verified.

- [ ] **Step 4: Run the bounded automated gate**

Run:

```powershell
cargo fmt -- --check
cargo test -p frd-ui-model
cargo test -p frd-app
cargo test -p frd-ui-egui
cargo test -p frd-shell-desktop
cargo test -p freeremotedesk-windows
cargo build --release -p freeremotedesk-windows
git diff --check
```

Expected: every command exits zero.

- [ ] **Step 5: Perform Windows manual acceptance**

Use the existing local test-texture/product launcher, one client process at a time. Verify windowed and maximized behavior at 100%, 150%, and 200% scale; 150 ms hover reveal; 700 ms hide; `Ctrl+Alt+Home`; keyboard focus and tooltips; reposition/clamp; minimize, maximize/restore, close, resize, snap, and system menu; no remote input penetration; unchanged aspect fit; and no persistent redraw while hidden. Touch reveal remains capability-disabled unless a local-from-gesture-start recognizer and touch ledger are both present.

- [ ] **Step 6: Record evidence without overstating other platforms**

Write `docs/validation/windows-floating-control-island.md` with commit hash, release binary SHA-256, commands and counts, tested scale factors, observed native-window behavior, input-isolation result, and unverified macOS/Linux/touch boundaries. Do not claim protocol interoperability from this UI acceptance.

After inspecting the user's existing README diff, the primary agent adds only the Windows floating-control-island state/evidence link to the platform matrix and stages that exact hunk without staging the user's unrelated Apple dual-mode edits. If an isolated hunk cannot be formed safely, keep completion open and report the README conflict instead of committing user work.

- [ ] **Step 7: Commit**

```powershell
git add crates/frd-shell-desktop/Cargo.toml crates/frd-shell-desktop/src/platform crates/frd-shell-desktop/src/application.rs docs/validation/windows-floating-control-island.md
git commit -m "test(windows): validate floating control island"
```

---

## Final integration review

- [ ] Compare every requirement in the approved spec with Tasks 1--6.
- [ ] Verify no protocol crate changed for the island.
- [ ] Verify `RemoteContentLayout` is invariant across hidden, pending, visible, pinned, moved, windowed, and maximized island states.
- [ ] Verify the old chrome type names and persistent `Panel::top("window-session-chrome")` path are absent.
- [ ] Verify the only visible hidden-state artifact is the non-hit-testable 50-percent-alpha green line.
- [ ] Verify one redraw/present path and no hidden-state periodic wake.
- [ ] Request independent specification and code-quality review before claiming completion.
