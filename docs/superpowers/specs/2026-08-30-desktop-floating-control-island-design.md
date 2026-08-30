# Desktop Floating Control Island Design

## Status

Approved in conversation on 2026-08-30. This replaces the persistent
FreeRemoteDesk title-bar session cluster only after the frame-transaction
latency gate in `2026-08-30-frame-transaction-render-latency-design.md` passes.
The current implementation target is Windows; macOS and Linux behavior is
specified now so the shared UI cannot encode a Windows-only window model.

## Goal

Maximize the stable remote-desktop surface by rendering one movable,
auto-hiding, accessible rounded control island inside the main window. When
hidden, only a 50-percent-alpha green line remains at the page top. The island
uses the existing WGPU presentation, never creates a second GUI/window, and
never lets local control input reach the remote session.

## Non-Goals

- Do not create a child/native utility window, second swapchain, separate render
  thread, or continuously animated overlay.
- Do not resize, translate, or renegotiate the remote surface when the island
  appears, hides, or moves.
- Do not copy Windows window controls onto macOS or Linux.
- Do not add protocol-specific buttons or let UI code inspect Apple, RDP, MVS,
  Raw, or CopyRect messages.
- Do not use backdrop blur in the first implementation or trade accepted frame
  latency for decoration.
- Do not add exhaustive screenshot tests; automated coverage is limited to
  state, layout invariance, command routing, and input ownership.

## Selected Architecture

### One window, one swapchain, two independent layouts

The island is an egui overlay encoded by the existing compositor into the same
winit window, WGPU surface texture, command encoder, submit, and present as the
remote desktop. It cannot own a second event loop or request a remote redraw.

`RemoteContentLayout` remains the sole remote rendering and input rectangle. It
is derived from the window's physical size and effective native platform insets
and remains byte-for-byte stable while the island changes state or position.
Aspect fit, letterboxing, pointer inversion, and dynamic-resolution viewport
all consume this same rectangle.

`ChromeOverlayLayout` separately computes DPI-aware physical rectangles for:

- the hidden reveal line and hover sensor;
- the visible rounded island and its `IslandRepositionHandle`;
- a distinct `WindowMoveRegion` where the current platform supports moving the
  native window;
- status, latency, capability, disconnect, and window controls;
- tooltips, popovers, focus outlines, and platform safe areas.

Overlay geometry can cover part of `RemoteContentLayout` while visible, but it
never subtracts an inset or changes the remote texture. Resize, DPI change,
maximize/restore, or native-inset change recomputes both layouts from one window
snapshot and publishes the new `ChromeHitMap` atomically.

### `ControlIslandState`

This specification is the single owner of the shared control-island state
machine:

```text
Hidden
  -> RevealPending(deadline)
  -> Visible
  -> HidePending(deadline)
  -> Hidden

Visible/HidePending
  -> Pinned(focus | tooltip | popover | pressed | island-reposition | window-move)
  -> Visible
```

`FloatingChromeController` owns `ControlIslandState`, deadlines, focus/hover
pinning, and one session-local clamped position. It consumes pointer/focus/timer
observations and emits only overlay invalidation and generic chrome intents. It
has no protocol, framebuffer, renderer-resource, or native-window handle.

`RevealPending` may begin from hover only when the shell input tracker reports
no remotely held input. Keyboard, AccessKit, and touch-edge forced reveal use
the explicit `ReleaseAll` ownership transition defined below.

State transitions are deadline-driven. The shell schedules the nearest
deadline through the wait-based winit event loop and redraws only for a short
opacity/position transition. `Hidden` never forces a 60 Hz redraw.

### Hidden reveal affordance

The default connected-session state renders one centered green line at the top
edge of the effective remote content:

- thickness: one to two logical points, rounded at both ends;
- opacity: 50 percent;
- width: bounded and responsive rather than spanning the full window;
- horizontal anchor: follows the island's clamped center;
- DPI and native-safe-area aware.

The top 10--12 logical points form a hover sensor. Entering starts a 150 ms
reveal deadline; leaving before it expires cancels the transition. Hidden
sensor code may observe pointer-move coordinates only, without reserving a
layout row or claiming pointer down/up, wheel, or gesture events. The green line
is visual-only and never clickable or hit-testable.

The line is not the accessibility control. The platform shell exposes a
documented keyboard action and an AccessKit reveal/focus action on the window's
accessible root. The Windows keyboard route follows remote-desktop convention
with local `Ctrl+Alt+Home`. A touch-capable shell exposes a deliberate top-edge
swipe gesture; it must not depend on hover and its edge recognizer is local from
gesture start, so it cannot duplicate touch input to the remote session.

### Visible island and auto-hide

The rounded island initially appears at the top center and is clamped inside
the platform safe area. `IslandRepositionHandle` moves only the island within
the main client area; `WindowMoveRegion` asks the platform adapter to move the
native OS window. The regions are disjoint, controls never double as either
handle, and dragging one can never invoke the other. Island position is
normalized and kept for the current session only. It is not saved with
credentials or sent to a server. The reveal line retains the island's
horizontal anchor, and revealing restores its last valid clamped position.

Hovering or keyboard-focusing the island, its tooltip/popover, a pressed
control, or an active drag pins it visible. When pointer and focus leave that
union, a 700 ms hide deadline begins. Re-entry cancels the deadline. At expiry
the island disappears and only the line remains. Escape dismisses a reversible
popover or an unpinned visible island; it never disconnects by itself.

Windowed and maximized Windows sessions use the same hidden/reveal behavior.
Maximizing does not restore a persistent custom title bar. The island remains
available through the top line, pointer hover, and keyboard route.

## Appearance and Accessibility

The outer island material is deliberately highly transparent: approximately
18--25 percent neutral surface tint, a thin adaptive border, and restrained
shadow. Real-time backdrop blur is excluded because it adds another sampling or
render pass to the accepted latency-critical compositor.

Arbitrary remote pixels cannot be trusted as a readable background. Each
glyph/text group therefore uses a compact contrast plate whose computed
foreground/background pair meets at least 4.5:1 for text and 3:1 for large
glyph/state boundaries. The outer island remains highly transparent while the
interactive content remains legible. System high-contrast or reduced-motion
preference replaces translucency/movement with an opaque system-color surface
and immediate transitions.

FreeRemoteDesk actions use only the vendored Material Symbols Rounded subset.
Every icon-only action retains:

- a concise Simplified-Chinese tooltip and accessible name;
- keyboard focus and visible focus treatment;
- distinct hover, pressed, selected, disabled, warning, and failed states;
- a minimum 44-by-44 logical-point desktop target;
- a shape/outline/opacity distinction so state is not color-only.

Status and frame-response latency remain concise but available to assistive
technology. Unsupported audio or clipboard is capability-driven and cannot
dispatch an action. A stable failure remains visible in the connection detail;
a tooltip or toast never replaces it.

## Contents and Collapse Priority

The island contains:

1. connection state and diagnostic access;
2. frame-response latency;
3. remote-audio capability/action;
4. clipboard capability/action;
5. disconnect;
6. window commands supplied by the current platform adapter.

It does not contain protocol selection or “以共享当前桌面重新连接.” Starting a
new Apple mode is allowed only on the login/error decision page after the
previous session has closed.

At narrow widths, low-priority capability details collapse into one accessible
overflow surface before controls overlap. Connection/failure state, disconnect,
and required platform safety controls remain reachable. Collapsing changes only
`ChromeOverlayLayout`, never `RemoteContentLayout`.

## `ChromeHitMap` and Input Ownership

This specification is the single owner of `ChromeHitMap`. It is generated from
the same `ChromeOverlayLayout` used for drawing and resolves hits in this strict
order:

1. active accessibility/focus surface, tooltip, popover, or menu;
2. visible island controls and `IslandRepositionHandle`;
3. `WindowMoveRegion` and platform-native chrome/resize/safe-area regions;
4. remote content.

Every pointer down/up, wheel, gesture, drag, text, key, or shortcut owned by
levels 1--3 is consumed locally and never enters `SessionInput`. A visible
island is never click-through, even where its material is highly transparent.
The green line has no hit entry. The hidden hover sensor observes pointer move
only; it does not capture, duplicate, or delay remote clicks.

Remote input remains additionally gated by application focus and remote-surface
focus. Moving outside the remote surface or FreeRemoteDesk window continues to
release/block remote input. Hiding the island can restore remote focus only
after all locally held buttons and keys have been released or handled locally;
it cannot leave a remote button or modifier stuck.

Hover reveal is deferred while the input tracker reports any remotely held
mouse button, key, modifier, or active touch. The physical release must first be
forwarded through the existing remote input route; only then may the 150 ms
hover deadline start. Hover alone never synthesizes a release.

A forced keyboard or AccessKit reveal uses a stricter ownership handoff. The
shell first emits the existing `ReleaseAll` for the active remote binding. Only
after that command is successfully accepted by the session command port may it
publish the island's local `ChromeHitMap` and focus the island. If `ReleaseAll`
cannot be accepted, the shell latches the input gate as blocked before exposing
any failure surface; it must not activate a local hit map while remote held
state is uncertain. Touch edge reveal uses the same handoff once its local edge
gesture is recognized.

## Window Command Boundary

This specification is the single owner of generic island window commands:

```text
WindowChromeCommand::BeginMove
WindowChromeCommand::Minimize
WindowChromeCommand::ToggleMaximize
WindowChromeCommand::Close
WindowChromeCommand::ShowSystemMenu
```

`WindowChromeAdapter` is the only component allowed to translate these commands
into Win32, AppKit, X11, or Wayland behavior. It also owns native insets, DPI
refresh, resize borders, and safe-area reporting. `FloatingChromeController`
emits commands but never executes a native API. Protocol, decoder, frame,
renderer-core, and UI-model crates contain no native handle.

`IslandRepositionHandle` never emits `WindowChromeCommand::BeginMove`; it updates
only `FloatingChromeController` position. Only `WindowMoveRegion` maps to
`BeginMove`.

### One-time chrome API migration

The implementation replaces the existing chrome types in one
dependency-ordered change. Old and new representations must not coexist behind
aliases or parallel hit-test paths:

| Existing owner/type | Replacement | Migration rule |
| --- | --- | --- |
| `frd-shell-desktop::ChromeLayout` | `RemoteContentLayout` plus `ChromeOverlayLayout` | One geometry snapshot creates both; remove the old type after callers move |
| `ChromeHitRegions` | `ChromeHitMap` | Platform adapters receive only the new atomic map |
| `ChromeHit` | `ChromeHitTarget` | Replace every match arm; no compatibility conversion |
| `WindowChromeAction` | `WindowChromeCommand` | One-time rename/reuse of existing semantic variants; no second enum or alias |
| `frd-ui-model::SessionChromeAction` | `frd-ui-model::IslandAction` | Generic user intent only; `frd-ui-egui` returns it and the shell routes it |

`IslandAction` belongs in `frd-ui-model` because it describes
protocol-neutral user intent and must be renderable by later client shells.
Its semantic variants cover cancel/disconnect, connection details, audio,
clipboard, minimize, maximize/restore, close, and system menu. Begin-window-move
is geometry-driven and is not an `IslandAction`. Session variants map to
existing application intents/commands. Window variants are exposed only when
the shell advertises that capability and are mapped by `frd-shell-desktop` to
`WindowChromeCommand`; the UI model never imports the desktop shell or a native
API.

`ChromeHitTarget` distinguishes an overlay control carrying `IslandAction`,
island reposition, window move, native chrome/resize, and remote content. It
does not contain a reveal-line target.

### Windows first implementation

Minimize, maximize/restore, and close are rendered inside the island. The
Windows adapter must still preserve edge resize, snap layouts, drag, ordinary
double-click maximize/restore where applicable, Alt+Space/system menu, DPI
changes, taskbar behavior, and native hit-test expectations. A visually absent
persistent title bar is not permission to break the Windows window model.

### macOS

Retain native traffic lights, native title-bar/full-screen behavior, and safe
areas. The island contains FreeRemoteDesk session controls only and never draws
Windows minimize/maximize/close replicas. `WindowChromeCommand` capabilities
determine which generic window actions, if any, the island renders.

### Linux

Retain the active desktop/window manager's native controls and decoration
conventions. The island contains session controls only unless a future adapter
proves a complete client-side-decoration contract for the target compositor.

### Future Android and HarmonyOS

These shells may reuse control semantics, capability states, and overlay/input
ownership, but not desktop window commands or hover-only discovery. Touch/back,
safe-area, and accessibility behavior must be supplied by their native shell.

## Approved Remote-Content UI Boundary Exception

The repository normally prohibits application controls over the remote
framebuffer. This approved design creates one narrow exception:

- the 50-percent-alpha reveal line may remain at the top edge;
- the island may temporarily overlay remote content only while revealed,
  focused, dragged, or showing one of its transient surfaces;
- neither element reserves a toolbar row or changes the remote content
  rectangle;
- no other persistent button, status box, diagnostic panel, advertisement, or
  protocol-specific widget may use this exception;
- the visible island and its transient surfaces own local input and are
  non-penetrating; the visual-only line has no hit region and claims no input.

This supersedes only the no-overlay clause in the earlier title-bar design. It
does not relax native macOS/Linux conventions, accessibility rules, or the
separation between platform shell and renderer/protocol code.

## Failure Behavior

- Invalid overlay geometry retains the last valid `ChromeHitMap`; if no safe map
  exists, local controls remain hidden and remote input is blocked rather than
  routed through unknown regions.
- A non-critical transparency, shadow, or transition failure falls back to the
  opaque accessible material and does not terminate a healthy remote session.
- A native window-command failure shows a stable shell diagnostic and cannot be
  reinterpreted as a protocol failure.
- Fatal session/presentation failure blocks remote input immediately, prints
  its stable error content, and keeps disconnect/close accessible.
- Island rendering cannot start an extra present loop, change server viewport,
  or bypass the accepted frame-transaction path.

## Protocol and Client-Platform Isolation

- `frd-ui-model` owns generic status/capability presentation data and
  `IslandAction`, not native commands or protocol adapters.
- `frd-ui-egui` renders the island from model plus `ChromeOverlayLayout`; it does
  not resolve endpoints or parse protocol state.
- `frd-shell-desktop` owns `FloatingChromeController`, `ChromeHitMap`, window
  command routing, timer scheduling, and desktop input focus.
- platform adapters implement capability-shaped window behavior independently;
  Windows constants or handles cannot enter macOS/Linux code.
- all server protocols publish generic capabilities and frames. Apple Shared
  Desktop, Apple High Performance, RDP, and future RFB cannot alter overlay
  geometry or input precedence.
- future non-desktop shells can replace window-command and hover policy while
  retaining generic session controls and protocol isolation.

## Focused Verification

Automated verification is intentionally limited to:

1. `ControlIslandState` reveal, cancellation, held-remote-input deferral,
   forced `ReleaseAll` handoff, pinning, hide, and drag deadlines;
2. island visibility/movement leaves `RemoteContentLayout`, aspect-fit input
   transform, and viewport generation unchanged;
3. `ChromeHitMap` excludes the visual-only line, distinguishes
   `IslandRepositionHandle` from `WindowMoveRegion`, and never forwards a local
   region as remote input;
4. Windows island controls map to the exact generic `WindowChromeCommand` and
   unsupported macOS/Linux commands are absent rather than fake;
5. hidden state does not schedule continuous redraw and one island render stays
   in the existing submit/present;
6. old `ChromeLayout`/`ChromeHitRegions`/`ChromeHit` and
   `WindowChromeAction` have no remaining production path or compatibility
   alias after the one-time migration, and `IslandAction` has no native handle.

Do not add exhaustive visual snapshots, timer cross-products, protocol tests,
or platform mocks beyond these contracts.

Windows manual acceptance, after the latency gate, covers:

- 100, 150, and 200 percent scale; windowed and maximized states;
- pointer and keyboard reveal, 150 ms reveal and 700 ms hide behavior;
- AccessKit and touch-edge reveal, including remote-held-input handoff failure;
- movement/clamping, tooltips, focus, light/dark/high-contrast appearance;
- minimize, maximize/restore, close, drag, resize, snap, and system menu;
- no local-to-remote input penetration and no stuck remote input;
- stable aspect ratio and viewport while the island shows/hides;
- no measurable regression from the accepted batch latency run.

macOS and Linux require equivalent native-control and safe-area acceptance when
their shells are implemented; Windows behavior is not proof for those clients.

## Normative Dependencies and Invariants

- Frame batching, presentation receipts, and the prerequisite latency gate are
  defined only by `2026-08-30-frame-transaction-render-latency-design.md`.
- Apple Shared Desktop and High Performance descriptors/adapters are defined
  only by `2026-08-30-apple-shared-high-performance-isolation-design.md`. The
  island may display generic current-session capability state, but it cannot
  select a descriptor, reconnect, create a session, or combine adapter state.
- `RemoteContentLayout` is invariant under every `ControlIslandState`; this
  remains true for every server protocol and client platform.

## Completion Boundary

Compilation and state tests do not prove a usable overlay or native window
behavior. The Windows work is complete only after the frame-latency gate has
passed and manual acceptance confirms accessibility, native window conventions,
non-penetrating input, stable remote geometry, and no latency regression.
macOS, Linux, Android, and HarmonyOS remain separately gated by their native
shell validation.
