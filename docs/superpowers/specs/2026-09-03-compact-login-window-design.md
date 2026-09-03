# Compact Login Window Design

## Status

Approved in conversation on 2026-09-03. The implementation target is the
Windows desktop client. The state model and UI contracts are specified for the
future macOS and Linux shells so the shared login UI does not acquire
Windows-only window assumptions.

## Goal

Replace the current large white canvas containing a centered login card with
one compact native application window whose entire client area is the login
surface. Username and password fields must be visually aligned, the window must
be easy to move and close, and the existing credential, protocol-selection,
validation, and submission behavior must remain unchanged.

The login, authentication, failure, and remote-desktop experiences continue to
use one winit window, one egui integration, and one WGPU presentation surface.
This design does not create a second GUI or a second native window.

## Non-Goals

- Do not create a transparent, shaped, frameless floating card.
- Do not create a second login window, child window, swapchain, render loop, or
  duplicated form implementation.
- Do not change credential storage, protocol selection, authentication,
  connection lifecycle, or password ownership.
- Do not make the whole form a drag target; text selection, IME, buttons,
  checkboxes, and selectors remain ordinary local controls.
- Do not copy the Windows close-button placement onto macOS or Linux.
- Do not add exhaustive visual snapshot tests. Automated coverage remains
  limited to layout, state transition, hit testing, and existing form behavior.

## Selected Architecture

### One window with two presentation modes

`frd-shell-desktop` owns an explicit platform-neutral window presentation
mode:

```text
CompactLocal
  | connection/authentication/certificate/failure: remain CompactLocal
  | first complete remote frame is presented
  v
RemoteDesktop
  | disconnect/session cleanup completes and the controller returns local
  v
CompactLocal
```

The mode is shell state, not protocol state. Apple Shared Desktop, Apple High
Performance, RDP, and future RFB adapters continue to publish the same generic
application pages and remote-frame events. No protocol module may resize the
native window or depend on either presentation mode.

`DesktopWindowState` stores the current presentation mode and the most recent
user-selected remote logical size. A single idempotent
`apply_window_presentation_mode` entry point changes Winit size constraints and
requests a new logical size only when the desired mode changes. Rendering must
not request the same size on every frame.

The transition to `RemoteDesktop` occurs only when the existing first-complete-
remote-frame gate succeeds. Clicking Connect, authenticating, accepting a
certificate, or receiving incomplete remote pixels does not expand the window.
The transition back to `CompactLocal` occurs only after session cleanup has
completed and the application controller has returned to a local page. It must
not shrink while a remote surface or cleanup operation is still visible.

### Surface and GPU lifetime

Both modes retain the same `Window`, `PresentationSurface`, GPU context, egui
renderer, remote renderer, decoder ownership, and event loop. A mode change uses
the existing `WindowEvent::Resized` to resize the compositor. It must not detach
or recreate the WGPU surface, clear a video generation, or restart a protocol
session.

All requested sizes are logical units. The existing DPI transition remains the
only authority for converting them to physical pixels and committing surface
size. A pending scale-factor transition defers a presentation-mode size request
until the current DPI transition settles.

## Compact Window Geometry

The Windows first implementation starts at approximately 520 by 600 logical
points. The exact height may be adjusted during visual acceptance, but the
default must display the ordinary form without a large empty exterior canvas.
The compact surface uses:

- 24 logical points of horizontal content inset;
- 20--24 logical points of vertical section spacing;
- a 44-logical-point local window bar;
- a 440--460-logical-point maximum form width;
- a DPI-aware minimum size that keeps the title bar, primary action, and one
  focused field reachable;
- vertical scrolling only when text scaling, localization, or a small user
  resize makes it necessary.

The entire client area uses one platform-adaptive surface color. The former
outer canvas plus inner filled, stroked, shadowed card is removed. Native window
shadow and border remain shell responsibilities. Resizing the compact window
may add breathing room inside the single surface, but it must never recreate a
second card-on-canvas visual layer.

`RemoteDesktop` restores the last remote logical size, or the existing 1100 by
720 default on the first successful session. User resizes in remote mode update
only the saved remote size. Local compact resizes must not overwrite it.

## Login Form Layout

The form keeps one responsive vertical hierarchy:

1. compact product mark, product name, and one-line purpose;
2. optional recent-profile selector;
3. target system and protocol selectors on one row when space permits;
4. address with a compact port field on one row when space permits;
5. full-width username field;
6. full-width password field;
7. one combined secure-save row;
8. inline error or unsupported-path explanation when applicable;
9. one full-width primary Connect button.

At narrow widths the paired rows stack without changing field order. The form
must not horizontally scroll.

Username and password fields have identical outer width, 52-logical-point
height, and 8--12-logical-point corner radius. Their leading glyph and text
baseline align. The password visibility action occupies a trailing slot inside
the password field and has a 44-by-44 logical-point hit target; it must not
reduce the password field's outer width or move the field relative to the
username field.

The separate labels currently rendered above Username and Password are
removed. They are replaced by an outlined floating-label field:

- empty and unfocused: the field name appears inside the field;
- focused, nonempty, autofilled, or invalid: the field name remains visible as
  a smaller label at the outline;
- optional placeholder text describes format or context, such as
  `远程设备账户`, and never becomes the sole field identity;
- errors use outline/state shape plus a nearby Simplified-Chinese explanation,
  not color alone;
- widget metadata exposes the persistent accessible name, description, and
  invalid state independently of the visual label.

This preserves the user's requested compact appearance while following
Material 3 outlined-field structure and Apple's warning that disappearing
placeholder text cannot reliably identify a field after entry.

The password remains backed by `SecretTextBuffer`. Toggling visibility retains
focus and caret, does not submit, and never copies the password into ordinary UI
state, logs, snapshots, or diagnostics. Enter in the focused password field
continues to invoke exactly the same single submission path as Connect and
remains disabled during IME composition, key repeat, or an active attempt.

The `在此设备上保存登录信息` control and the secure-store explanation become
one compact row. Passwords continue to use the operating-system credential
store; this layout change cannot introduce a configuration-file fallback.

## Local Window Bar and Native Behavior

The compact window exposes a visually recognizable 44-logical-point window bar
instead of the current empty drag strip. Drawing and hit testing consume the
same geometry snapshot.

On Windows:

- a dedicated close action occupies the rightmost 44-by-44 logical points;
- the remaining safe bar is `WindowMoveRegion` and maps to native `HTCAPTION`;
- close routes through the existing safe shutdown/`WM_CLOSE` path;
- edge resize takes precedence over caption hit testing;
- Snap, double-click maximize/restore, system menu, taskbar, DPI, and focus
  behavior remain native;
- no text field, selector, checkbox, primary action, tooltip, or close target is
  included in the move region.

The close action uses the vendored Material Symbols Rounded family, with a
Simplified-Chinese tooltip, accessible name, keyboard focus, and distinct hover,
pressed, and focus states. Escape does not close the main window.

Future macOS uses the native left-side traffic lights and native frame drag
behavior. Future Linux uses the active window manager's native decoration when
available. The shared egui form receives only safe content insets and generic
close capability; it never imports Win32, AppKit, X11, or Wayland APIs and never
hardcodes platform button placement.

## Local Pages and Failure Behavior

Connection, authentication, certificate decision, and recoverable failure pages
all remain `CompactLocal`. Each page uses the same single-surface shell and
window bar. Long certificate details or diagnostic text scroll inside the
content area instead of enlarging the window without a user action.

Recoverable errors preserve address, username, protocol choice, save choice,
and valid non-secret form state. Password lifetime continues to follow the
existing security policy. Field errors appear adjacent to the affected field;
a toast or tooltip cannot replace persistent error content.

If native move or close execution fails, the shell reports a stable window
diagnostic and keeps local input ownership. A window-command failure is never
reported as a protocol or authentication error.

If the shell cannot compute safe compact geometry, it retains the last valid
layout and keeps controls local. It must not publish an overlapping hit map or
route a click in uncertain local geometry to a remote session.

## Module Ownership

- `frd-ui-egui` owns compact form composition, floating-label field rendering,
  visual tokens, accessible widget metadata, and layout metrics.
- `frd-shell-desktop` owns presentation-mode state, logical window sizing,
  mode transitions, chrome geometry, and native command routing.
- platform window adapters own native close/move/resize semantics and safe
  insets independently.
- `frd-ui-model` retains form values, validation, protocol-neutral intents, and
  secret ownership; it gains no native window state.
- protocol, decoder, framebuffer, and renderer-core modules remain unaware of
  compact login geometry and platform chrome.

The UI crate exports a compact layout contract or metrics object so the shell
does not duplicate card widths, input heights, or window-bar constants. The
shell may add platform insets and DPI conversion, but it must not infer form
height by reimplementing the form.

## Focused Verification

Automated verification is intentionally limited to core contracts:

1. username and password fields receive the same outer rectangle, while the
   password visibility target remains inside that rectangle;
2. floating labels persist for focused, nonempty, autofilled, and invalid
   fields, with stable accessible names;
3. password Enter still submits once and rejects IME composition, repeat, and
   active-attempt input;
4. `CompactLocal` remains active through connect/authentication/failure, changes
   to `RemoteDesktop` only after the first complete presented frame, and returns
   only after cleanup reaches a local page;
5. repeated synchronization is idempotent and remote/local sizes are stored
   independently;
6. compact move, close, and resize rectangles are disjoint at 100, 150, and 200
   percent scale, with native resize precedence;
7. a presentation-mode change resizes the existing compositor without
   recreating GPU, video, or protocol ownership.

Windows manual acceptance covers:

- light, dark, and high-contrast appearance at 100, 150, and 200 percent scale;
- equal field widths, visible floating labels, password visibility focus, Tab
  order, password Enter, inline validation, tooltips, and keyboard-only use;
- compact default size with no card-on-whiteboard layer;
- moving from the local window bar, edge resizing, Snap, double-click, system
  menu, taskbar, and close during idle, connecting, and failure states;
- one-window transition into the remote desktop and back, with the accepted
  remote aspect ratio, input mapping, frame latency, and decoder ownership
  unchanged.

Compilation and unit tests do not prove visual quality or native window
behavior. macOS and Linux remain separately gated by their native shell and
live visual acceptance when implemented.

## Completion Boundary

The Windows change is complete only when the installed release package passes
the focused automated checks and manual acceptance confirms the compact login
window, field alignment, native movement/close behavior, one-window transition,
and no regression to remote rendering or input. This specification does not
claim macOS, Linux, Android, or HarmonyOS runtime validation.
