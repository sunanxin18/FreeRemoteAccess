# Compact Login Window Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the large card-on-canvas login page with one compact, movable, closable Windows login window whose aligned fields and lifecycle reuse the existing winit/egui/WGPU application.

**Architecture:** `frd-ui-egui` publishes the compact form metrics and renders one single-surface form plus a local window bar. `frd-shell-desktop` owns an explicit `CompactLocal`/`RemoteDesktop` presentation controller and applies mode transitions to the existing Winit window without recreating its WGPU surface or exposing window state to protocol crates.

**Tech Stack:** Rust 2021, egui, winit, wgpu, Win32 `WM_NCHITTEST`, AccessKit, Material Symbols Rounded.

**Spec:** `docs/superpowers/specs/2026-09-03-compact-login-window-design.md`

## Global Constraints

- Keep one Winit window, one egui integration, one WGPU surface, and one application controller.
- Windows is the only implementation target in this plan; macOS and Linux retain capability-shaped native-shell boundaries.
- Do not change Apple, RDP, RFB, credential-store, decoder, framebuffer, or transport behavior.
- Username and password fields are exactly equal in outer width and height; password visibility remains inside the password field.
- Field identity remains visible through floating labels and accessible metadata; placeholder-only identification is prohibited.
- Compact local pages use a single platform-adaptive surface, not a transparent window or an inner shadowed card.
- Enter in the password field continues to submit through the existing one-shot path and remains blocked for IME composition, repeat, or an active connection attempt.
- Local move/close input is never forwarded to the remote session.
- Do not add exhaustive screenshot or combinatorial tests; keep tests at the UI contract, state transition, hit-map, and lifecycle boundaries.
- The worktree already contains approved uncommitted work in several target files. Task workers must not stage, commit, reset, or revert files. The root agent reviews each exact diff and owns any later selective commit.

---

### Task 1: Compact Form Metrics and Aligned Floating-Label Fields

**Files:**
- Modify: `crates/frd-ui-egui/src/connection.rs:17-197`
- Modify: `crates/frd-ui-egui/src/connection.rs:340-487`
- Modify: `crates/frd-ui-egui/src/lib.rs:88-260`

**Interfaces:**
- Consumes: existing `ConnectionForm`, `SecretTextBuffer`, `ConnectTriggerInput`, `ProtocolCatalog`, and `LoginIcon` APIs.
- Produces: `pub struct CompactLoginMetrics`, `pub const fn compact_login_metrics() -> CompactLoginMetrics`, and the existing `show_connection_form_with_state(...) -> Option<AppIntent>` rendered in compact single-surface form.

- [ ] **Step 1: Write failing metric and field-state tests**

Add pure contract tests in `crates/frd-ui-egui/src/lib.rs`:

```rust
#[test]
fn compact_login_metrics_align_identity_fields_and_meet_hit_targets() {
    let metrics = compact_login_metrics();
    assert_eq!(metrics.username_width, metrics.password_width);
    assert_eq!(metrics.field_height, 52.0);
    assert!(metrics.field_height >= 44.0);
    assert!(metrics.password_trailing_target >= 44.0);
    assert_eq!(metrics.window_bar_height, 44.0);
}

#[test]
fn floating_field_label_persists_when_identity_must_remain_visible() {
    assert!(!floating_label_is_raised(false, false, false));
    assert!(floating_label_is_raised(true, false, false));
    assert!(floating_label_is_raised(false, true, false));
    assert!(floating_label_is_raised(false, false, true));
}
```

The second test proves the field label is raised for focus, content/autofill, or
invalid state and is never replaced by a disappearing placeholder after input.

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```powershell
cargo test -p frd-ui-egui compact_login_metrics_align_identity_fields_and_meet_hit_targets
cargo test -p frd-ui-egui floating_field_label_persists_when_identity_must_remain_visible
```

Expected: both fail because `CompactLoginMetrics`, `compact_login_metrics`, and
`floating_label_is_raised` do not exist.

- [ ] **Step 3: Add the shared compact metrics contract**

Replace `LoginCardMetrics` with a compact presentation contract in
`connection.rs`:

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompactLoginMetrics {
    pub initial_window_width: f64,
    pub initial_window_height: f64,
    pub minimum_window_width: f64,
    pub minimum_window_height: f64,
    pub content_max_width: f32,
    pub content_inset: f32,
    pub field_height: f32,
    pub username_width: f32,
    pub password_width: f32,
    pub password_trailing_target: f32,
    pub window_bar_height: f32,
}

pub const fn compact_login_metrics() -> CompactLoginMetrics {
    CompactLoginMetrics {
        initial_window_width: 520.0,
        initial_window_height: 600.0,
        minimum_window_width: 480.0,
        minimum_window_height: 520.0,
        content_max_width: 460.0,
        content_inset: 24.0,
        field_height: 52.0,
        username_width: 412.0,
        password_width: 412.0,
        password_trailing_target: 44.0,
        window_bar_height: 44.0,
    }
}

pub const fn floating_label_is_raised(
    focused: bool,
    has_text: bool,
    invalid: bool,
) -> bool {
    focused || has_text || invalid
}
```

Use `content_max_width` as a maximum rather than a fixed width at narrow user
resizes. Compute the actual identity-field outer width once from the available
content width and pass the same value to both render paths; the fixed
`username_width` and `password_width` fields are the default 520-point-window
contract tested by the shell.

- [ ] **Step 4: Render one surface instead of a card on a canvas**

In `show_connection_form_with_state`:

- remove the estimated 560/680-point vertical centering and the filled,
  stroked, shadowed `Frame`;
- retain a vertical `ScrollArea` for text scale/localization overflow;
- center only the bounded content column with 24-point insets;
- keep target/protocol and address/port paired when the computed content width
  fits, otherwise stack them in their current logical order;
- reduce the decorative header spacing without removing the product name;
- merge the secure-store explanation into the same row as the save checkbox.

The central-panel fill remains `ui.visuals().window_fill()`, so light, dark, and
high-contrast themes use one surface with no white exterior layer.

- [ ] **Step 5: Render username and password through one outlined-field shell**

Add a private helper that allocates one outer frame for each field and places
the icon, frameless `TextEdit`, and optional trailing action inside it:

```rust
fn outlined_field_frame(
    ui: &mut Ui,
    width: f32,
    label: &'static str,
    focused: bool,
    has_text: bool,
    error: Option<&str>,
    add_contents: impl FnOnce(&mut Ui) -> Response,
) -> Response
```

The helper uses `floating_label_is_raised(focused, has_text, error.is_some())`,
draws the label within the outline, gives focus/invalid states a non-color-only
stroke change, and keeps `WidgetInfo::labeled` on the actual editable response.
Render both outer frames with the same computed width and 52-point height. Give
the password visibility control a 44-by-44 trailing allocation inside the
frame, retain its tooltip/accessibility name, and restore the password edit
focus after a visibility toggle.

- [ ] **Step 6: Run UI tests and verify GREEN**

Run:

```powershell
cargo test -p frd-ui-egui compact_login_metrics_align_identity_fields_and_meet_hit_targets
cargo test -p frd-ui-egui floating_field_label_persists_when_identity_must_remain_visible
cargo test -p frd-ui-egui password_enter_
cargo test -p frd-ui-egui
cargo fmt --all -- --check
```

Expected: all focused tests and the complete `frd-ui-egui` suite pass; no new
test inspects password contents, command-line arguments, or snapshots.

- [ ] **Step 7: Root diff checkpoint**

Run `git diff -- crates/frd-ui-egui/src/connection.rs crates/frd-ui-egui/src/lib.rs`.
The root agent verifies that only compact-form rendering and tests were added,
records the checkpoint, and leaves the working tree unstaged.

---

### Task 2: Local Window Bar, Close Action, and Disjoint Native Hit Geometry

**Files:**
- Create: `crates/frd-ui-egui/src/local_window_bar.rs`
- Modify: `crates/frd-ui-egui/src/lib.rs:1-105`
- Modify: `crates/frd-shell-desktop/src/floating_chrome.rs:200-505`
- Modify: `crates/frd-shell-desktop/src/floating_chrome.rs:700-770`
- Modify: `crates/frd-shell-desktop/src/platform/windows.rs:260-308`
- Modify: `crates/frd-shell-desktop/src/platform/windows.rs:580-635`

**Interfaces:**
- Consumes: Task 1 `compact_login_metrics().window_bar_height`, existing `IslandAction::CloseWindow`, Material `close` U+E5CD, `ChromeRect`, `ChromeHitMap`, and `WindowChromeCapabilities`.
- Produces: `show_local_window_bar(...) -> LocalWindowBarResult`, `ChromeOverlayLayout::local_close_rect`, and local-page hit maps whose close and move regions never overlap.

- [ ] **Step 1: Write failing local chrome geometry tests**

Extend the existing local-page layout test in `floating_chrome.rs`:

```rust
#[test]
fn local_page_layout_separates_close_from_native_move_region() {
    let layouts = windows_snapshot(1.5)
        .local_page_layouts(ControlIslandPlacement::default())
        .expect("valid local chrome");
    let close = layouts.overlay.local_close_rect.expect("close capability");
    let drag = layouts.overlay.window_move_region.expect("begin-move capability");
    assert_eq!(close.width, 66);
    assert_eq!(close.height, 66);
    assert!(!overlaps(Some(close), Some(drag)));
    assert_eq!(drag.y, 0);
}
```

Add a Windows hit test proving the close region stays client-owned while its
neighbor remains caption-owned:

```rust
#[test]
fn local_close_is_client_input_and_neighboring_bar_is_caption() {
    let map = local_page_hit_map_with_close();
    assert_eq!(windows_native_hit(&map, local_close_center()), HTCLIENT);
    assert_eq!(windows_native_hit(&map, local_move_center()), HTCAPTION);
}
```

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```powershell
cargo test -p frd-shell-desktop local_page_layout_separates_close_from_native_move_region
cargo test -p frd-shell-desktop local_close_is_client_input_and_neighboring_bar_is_caption
```

Expected: fail because local chrome has no dedicated close rectangle and the
existing move rectangle consumes the full safe width.

- [ ] **Step 3: Add the local window bar renderer**

Create `local_window_bar.rs` with:

```rust
pub struct LocalWindowBarResult {
    pub hit_rects: Vec<(egui::Rect, IslandAction)>,
    pub action: Option<IslandAction>,
}

pub fn show_local_window_bar(
    context: &egui::Context,
    bar_rect: egui::Rect,
    close_rect: Option<egui::Rect>,
) -> LocalWindowBarResult
```

Render a low-emphasis centered `FreeRemoteDesk` title and, when supplied, one
44-point close target using the existing Material Symbols Rounded font and
U+E5CD. The close response exposes tooltip and accessible label `关闭窗口`,
returns `IslandAction::CloseWindow` only on activation, and reports its exact
hit rectangle. The bar background is noninteractive; native caption movement
continues to come from `ChromeHitMap`, not an egui drag gesture.

- [ ] **Step 4: Partition local chrome geometry**

Add `local_close_rect: Option<ChromeRect>` to `ChromeOverlayLayout`. For local
pages on a platform with `close` capability, reserve the platform-safe trailing
44 logical points for close and shorten `window_move_region` before creating
the hit map. Keep resize edges outside this map and retain the existing
candidate overlap rejection.

Populate `ChromeHitMap` with `(local_close_rect, IslandAction::CloseWindow)` and
the disjoint `WindowMoveRegion`. Do not change remote control-island geometry.

- [ ] **Step 5: Run chrome and UI tests and verify GREEN**

Run:

```powershell
cargo test -p frd-shell-desktop local_page_layout_
cargo test -p frd-shell-desktop local_close_
cargo test -p frd-shell-desktop windows_native_hit
cargo test -p frd-ui-egui
cargo fmt --all -- --check
```

Expected: local close is `HTCLIENT`, local drag is `HTCAPTION`, resize still
wins at the edge, and remote island tests remain unchanged.

- [ ] **Step 6: Root diff checkpoint**

Run `git diff -- crates/frd-ui-egui/src/local_window_bar.rs crates/frd-ui-egui/src/lib.rs crates/frd-shell-desktop/src/floating_chrome.rs crates/frd-shell-desktop/src/platform/windows.rs`.
The root agent rejects any protocol dependency, remote-island behavior change,
or overlapping local hit region and leaves the working tree unstaged.

---

### Task 3: Pure Window Presentation State Machine

**Files:**
- Create: `crates/frd-shell-desktop/src/window_presentation.rs`
- Modify: `crates/frd-shell-desktop/src/lib.rs:1-45`

**Interfaces:**
- Consumes: Task 1 `CompactLoginMetrics`; it does not consume `AppPage`, a native window, a protocol session, or a GPU handle.
- Produces: `WindowPresentationMode`, `LogicalWindowExtent`, `WindowPresentationTransition`, and `WindowPresentationController` methods used by Task 4.

- [ ] **Step 1: Write failing state-machine tests**

Create `window_presentation.rs` with its tests first:

```rust
#[test]
fn stays_compact_until_first_complete_remote_frame() {
    let mut controller = WindowPresentationController::new(
        compact_extent(),
        LogicalWindowExtent::new(1100.0, 720.0),
    );
    assert_eq!(controller.mode(), WindowPresentationMode::CompactLocal);
    assert_eq!(controller.observe_local_or_connecting(), None);
    assert_eq!(
        controller.observe_first_complete_remote_frame(),
        Some(WindowPresentationTransition::RemoteDesktop {
            extent: LogicalWindowExtent::new(1100.0, 720.0),
        })
    );
}

#[test]
fn cleanup_return_restores_compact_without_forgetting_remote_size() {
    let mut controller = connected_controller();
    controller.record_user_resize(LogicalWindowExtent::new(1440.0, 900.0));
    assert_eq!(
        controller.observe_cleanup_returned_local(),
        Some(WindowPresentationTransition::CompactLocal {
            extent: compact_extent(),
        })
    );
    assert_eq!(controller.last_remote_extent(), LogicalWindowExtent::new(1440.0, 900.0));
    assert_eq!(controller.observe_cleanup_returned_local(), None);
}
```

Add one test that `record_user_resize` is ignored in `CompactLocal`, ensuring a
small login resize never overwrites the remembered remote window size.

- [ ] **Step 2: Run the state tests and verify RED**

Run:

```powershell
cargo test -p frd-shell-desktop window_presentation::tests
```

Expected: fail because the module and controller types are not implemented.

- [ ] **Step 3: Implement the pure controller**

Implement these exact public contracts:

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LogicalWindowExtent {
    pub width: f64,
    pub height: f64,
}

impl LogicalWindowExtent {
    pub const fn new(width: f64, height: f64) -> Self;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowPresentationMode {
    CompactLocal,
    RemoteDesktop,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowPresentationTransition {
    CompactLocal { extent: LogicalWindowExtent },
    RemoteDesktop { extent: LogicalWindowExtent },
}

pub struct WindowPresentationController {
    mode: WindowPresentationMode,
    compact_extent: LogicalWindowExtent,
    last_remote_extent: LogicalWindowExtent,
}
```

Methods are `new`, `mode`, `last_remote_extent`,
`observe_local_or_connecting`, `observe_first_complete_remote_frame`,
`observe_cleanup_returned_local`, and `record_user_resize`. Observation methods
return `None` when no mode change is required. Reject non-finite or nonpositive
resize extents before recording them.

- [ ] **Step 4: Run state tests and verify GREEN**

Run:

```powershell
cargo test -p frd-shell-desktop window_presentation::tests
cargo fmt --all -- --check
```

Expected: all presentation-controller tests pass without constructing a Winit
window or WGPU device.

- [ ] **Step 5: Root diff checkpoint**

Run `git diff -- crates/frd-shell-desktop/src/window_presentation.rs crates/frd-shell-desktop/src/lib.rs`.
The root agent verifies that the controller has no protocol, renderer, native
handle, or `AppPage` dependency and leaves the working tree unstaged.

---

### Task 4: Integrate Compact/Remote Modes into the Existing Application

**Files:**
- Modify: `crates/frd-shell-desktop/src/application.rs:2260-2665`
- Modify: `crates/frd-shell-desktop/src/application.rs:3040-3370`
- Modify: `crates/frd-shell-desktop/src/application.rs:3480-3665`
- Modify: `crates/frd-shell-desktop/src/application.rs:4040-4160`
- Modify: `crates/frd-shell-desktop/src/application.rs:4540-4620`
- Modify: `crates/frd-shell-desktop/src/application.rs:6400-6775`

**Interfaces:**
- Consumes: Task 1 `compact_login_metrics`, Task 2 `show_local_window_bar` and local chrome rectangles, Task 3 `WindowPresentationController` and `WindowPresentationTransition`, plus existing first-frame, cleanup, DPI, compositor resize, and window-command paths.
- Produces: one installed Windows application that starts compact, stays compact through local flows, expands only after the first complete remote frame, and returns compact after cleanup.

- [ ] **Step 1: Write failing application-boundary tests**

Add tests beside the existing first-frame and local-page tests:

```rust
#[test]
fn presentation_transition_is_driven_by_existing_first_frame_gate() {
    let mut presentation = test_window_presentation();
    assert_eq!(transition_after_frame(&mut presentation, false, true, false), None);
    assert_eq!(transition_after_frame(&mut presentation, true, true, true), None);
    assert!(matches!(
        transition_after_frame(&mut presentation, false, true, true),
        Some(WindowPresentationTransition::RemoteDesktop { .. })
    ));
}

#[test]
fn local_cleanup_transition_waits_for_controller_local_page() {
    let mut presentation = connected_window_presentation();
    assert_eq!(transition_after_cleanup(&mut presentation, false), None);
    assert!(matches!(
        transition_after_cleanup(&mut presentation, true),
        Some(WindowPresentationTransition::CompactLocal { .. })
    ));
}
```

Add a geometry-routing test asserting a local close activation maps exactly to
`WindowChromeCommand::Close`, while the drag strip remains geometry-driven and
does not create an application intent.

- [ ] **Step 2: Run the application tests and verify RED**

Run:

```powershell
cargo test -p frd-shell-desktop presentation_transition_is_driven_by_existing_first_frame_gate
cargo test -p frd-shell-desktop local_cleanup_transition_waits_for_controller_local_page
cargo test -p frd-shell-desktop local_close_activation_maps_to_native_close
```

Expected: fail because application integration helpers and local-bar rendering
do not yet exist.

- [ ] **Step 3: Initialize the window in compact mode**

In `initialize_window`, obtain `let compact = frd_ui_egui::compact_login_metrics()`
and replace the hardcoded 1100-by-720 initial size/minimum with its compact
logical size/minimum. Initialize `WindowPresentationController` in
`CompactLocal`, retaining 1100 by 720 as the first remote default.

Add one method on `DesktopWindowState`:

```rust
fn apply_window_presentation_transition(
    &mut self,
    transition: WindowPresentationTransition,
) -> bool
```

It updates resizable/minimum constraints appropriate to the requested mode and
calls `request_inner_size(LogicalSize::new(extent.width, extent.height))` once.
It returns whether Winit accepted an immediate size. It never mutates physical
surface size directly and never recreates presentation resources.

- [ ] **Step 4: Drive mode changes from established lifecycle boundaries**

Where `first_remote_frame_presented` is already computed, call
`observe_first_complete_remote_frame` and apply its returned transition after
the accepted frame transaction. Do not expand on Connect, launch completion,
authentication, certificate decision, or an incomplete frame.

After `handle_cleanup_finished` has successfully updated the controller, check
that the page is local (`ConnectionForm`, authentication/certificate decision,
or `Failed`) before calling `observe_cleanup_returned_local`. Do not shrink
during `Disconnecting` or while cleanup remains in flight.

In `WindowEvent::Resized`, convert the accepted size to logical units and call
`record_user_resize` only when the presentation controller is
`RemoteDesktop`. Leave the existing `commit_window_resize`, DPI transition,
remote content layout, and input transform path unchanged.

- [ ] **Step 5: Render the local bar and publish one atomic hit map**

Replace the empty local top panel with `show_local_window_bar`. Convert the
precomputed local bar and close rectangles from physical to logical using the
same scale snapshot as the visible controls. Append its returned close hit
rectangle to `ChromeHitMap`, route `IslandAction::CloseWindow` through existing
`window_command_for_island_action`, and leave caption movement under native
`HTCAPTION` hit testing.

The local page content begins below exactly the same 44-point bar used by the
hit geometry. Remote-session rendering keeps the existing floating control
island and must not call the local bar renderer.

- [ ] **Step 6: Run focused and complete crate tests and verify GREEN**

Run:

```powershell
cargo test -p frd-shell-desktop presentation_transition_
cargo test -p frd-shell-desktop local_cleanup_transition_
cargo test -p frd-shell-desktop local_close_
cargo test -p frd-shell-desktop
cargo test -p frd-ui-egui
cargo fmt --all -- --check
git diff --check
```

Expected: all tests pass; PowerShell line-ending notices may be reported but no
whitespace error is accepted.

- [ ] **Step 7: Root integration checkpoint**

The root agent reviews the complete UI/shell diff and explicitly verifies:

- no protocol, credential, framebuffer, decoder, or transport behavior changed;
- local close and move hits never enter `SessionInput`;
- first-frame expansion and cleanup return each have one owner;
- compact resize never becomes remote dynamic-resolution negotiation;
- existing remote floating-island and focus restoration changes remain intact;
- no extra window, surface, renderer, or event loop was added.

Leave the working tree unstaged.

---

### Task 5: Windows Release, Package, and Manual Acceptance

**Files:**
- Verify only: `target/release/freeremotedesk-windows.exe`
- Verify only: staged/installed Windows package produced by existing tools
- Update only if behavior materially changes: `README.md` platform feature matrix

**Interfaces:**
- Consumes: Tasks 1--4 complete and reviewed.
- Produces: fresh release/package evidence and one running installed Windows client for user acceptance.

- [ ] **Step 1: Run the bounded repository verification matrix**

Run sequentially to avoid repeating the prior machine-pressure failure:

```powershell
cargo fmt --all -- --check
cargo test -p frd-ui-egui -p frd-shell-desktop
cargo build --release -p freeremotedesk-windows
git diff --check
```

Expected: every command exits zero. Do not start duplicate Cargo builds or
multiple desktop clients.

- [ ] **Step 2: Stage and verify the Windows package**

Use the repository's existing Windows package staging and verification scripts.
Verify that the executable, FFmpeg/runtime DLLs, licenses, application icon,
and hashes are present. Packaging success is build evidence only, not GUI or
protocol interoperability proof.

- [ ] **Step 3: Install without duplicate clients**

Resolve all `freeremotedesk-windows.exe` processes first. Close only processes
whose executable path is the trusted installed or just-built FreeRemoteDesk
binary. Install the newly staged package, verify the installed files, and start
exactly one normal client without credentials on its command line.

- [ ] **Step 4: Perform Windows local-page acceptance**

Without entering or automating a password, verify:

- default 520-by-600-class compact window and no inner card-on-whiteboard layer;
- equal username/password outer widths and in-field floating labels;
- password visibility stays inside the field and focus remains usable;
- whole safe local bar moves the window, close works, edge resize wins, and no
  second client is started;
- light/dark and 100/150/200-percent scale remain legible when available;
- local controls never create remote input.

- [ ] **Step 5: Perform bounded Mac regression acceptance when authorized credentials are available**

Use the local credential provider, never argv or logs. Verify one selected Apple
mode can authenticate, present a complete frame, expand the same window, accept
mouse/keyboard input, disconnect, complete cleanup, and restore the compact
login window. Record this separately from local UI/build evidence. Do not add or
modify any program on the Mac.

- [ ] **Step 6: Final root review and handoff**

Report exact commands, pass counts, installed executable hash/path, running PID,
local GUI observations, and whether live Mac regression was run or remains
unverified. Do not commit or push implementation changes unless the user
explicitly requests it after reviewing the result.
