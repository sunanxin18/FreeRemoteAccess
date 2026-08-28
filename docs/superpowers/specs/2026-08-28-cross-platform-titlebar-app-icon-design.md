# Cross-Platform Session Title Bar and App Icon Design

Date: 2026-08-28

## Status

Approved direction; implementation requires a separate reviewed plan. The
generated image is a visual candidate, not yet a packaged production asset.

## Goals

- Give the remote framebuffer all client space below one title-bar row.
- Keep FreeRemoteDesk session controls geometrically centered while preserving
  native Windows, macOS, and Linux window conventions.
- Replace persistent title-bar text controls with recognizable glyphs without
  sacrificing diagnostics, keyboard access, or accessibility.
- Turn `D:\FreeRemoteDesk\remote3.jpeg` into one recognizable cross-platform
  icon family and install the Windows icon first.

## Non-Goals

- This work does not change any remote-desktop protocol, authentication,
  decoder, framebuffer, audio, or clipboard wire behavior.
- It does not implement the macOS or Linux application packages yet.
- It does not place persistent controls over the remote framebuffer.
- It does not claim that one pre-rounded bitmap is a valid source for every
  platform.

## Shared Architecture

The title bar has three logical zones: platform-leading controls, a
FreeRemoteDesk session cluster anchored to the window's geometric center, and
platform-trailing controls. Leading and trailing widths must not shift the
session cluster; the center is calculated from the full window width.

`frd-ui-model` provides a protocol-neutral chrome model containing connection
state, audio availability/state, clipboard capabilities, and permitted session
actions. `frd-ui-egui` renders only the FreeRemoteDesk glyph cluster and its
tooltips. `frd-shell-desktop` owns layout, hit testing, the remote-content
rectangle, and platform window actions. Platform-specific implementations own
native window integration. Protocol adapters publish capabilities and events
through existing contracts and never call window APIs.

The login form and a required server-identity decision may use the client area.
Once a connection attempt starts, connection progress, diagnostics, cancel,
connected capabilities, and disconnect move into title-bar chrome. A failure
returns to the login surface and presents the sanitized failure there. The
remote-content area contains only the remote surface and temporary pointer
feedback produced by the remote interaction itself.

## Platform Chrome

### Windows

The Windows shell implements an extended/client title bar with controls on the
right and the session cluster at the true window center. It preserves drag,
double-click maximize/restore, minimize, maximize/restore, close, DPI behavior,
edge resize, keyboard system menu behavior, and Windows 11 snap affordances.
The first implementation must not silently trade these behaviors for a merely
frameless window.

### macOS

The macOS shell uses a transparent, full-size native title bar and retains the
native traffic-light controls on the left. The FreeRemoteDesk cluster occupies
the centered safe title-bar region and does not cover the traffic lights.

### Linux

The Linux shell uses a client-side title bar when the active X11/Wayland window
manager cannot host application widgets in server-side decorations. Window
controls follow the desktop environment's configured side when discoverable;
otherwise the adapter uses the conventional right side. Move, resize,
maximize, close, tiling, and compositor behavior remain platform-adapter
responsibilities.

## Session Glyphs

The shared glyph vocabulary is resolution-independent and visually consistent:

- connection: distinct connecting, connected, disconnecting, and failed shapes;
- remote audio: speaker-active and speaker-unavailable shapes;
- clipboard: clipboard-enabled and clipboard-unavailable shapes;
- disconnect: a disconnect/unplug action shape.

The compact row shows no permanent explanatory text. Every glyph has a concise
Simplified-Chinese tooltip and accessible name, participates in keyboard focus,
and distinguishes state through shape as well as color. Unsupported audio or
clipboard glyphs remain visible but disabled so the title-bar geometry does not
jump when capabilities change. Diagnostics appear in the connection-state
glyph's tooltip and accessible description, not in a second toolbar.

## Remote Geometry and Input

The platform adapter reports one physical-pixel title-bar inset. The renderer
aspect-fits the remote surface into the rectangle below that inset. Pointer
mapping and dynamic-resolution viewport reporting use the exact same rectangle.
Title-bar hits are UI-owned and never produce remote pointer or keyboard input.
Changing DPI, maximizing, restoring, or switching decoration mode recomputes
the inset and remote viewport atomically.

## App Icon Asset Model

The source identity is the deep-navy field with a bright oval remote portal and
central spiral/star. Production artwork removes the gray photographic canvas,
baked drop shadow, perspective, and baked rounded mask. Fine star dust is
simplified so the mark remains legible at 16 pixels.

The canonical artwork is separated into:

1. a full-bleed navy background layer;
2. a transparent portal/spiral foreground layer;
3. a simplified monochrome foreground glyph.

Platform exports are generated from those canonical layers rather than by
re-editing platform-specific bitmaps:

- Apple: 1024-by-1024 square unmasked layers for Icon Composer/asset catalogs;
- Android: 108-by-108 logical foreground/background layers with essential
  artwork inside the centered 66-by-66 safe zone, plus a monochrome layer;
- Google Play: flattened 512-by-512 32-bit PNG within the store size budget;
- Windows: a multi-resolution alpha-capable ICO plus a runtime window icon;
- Linux: hicolor PNG exports at standard launcher sizes and a scalable source.

The current Windows application sets both the executable resource icon and the
winit window icon so Explorer, taskbar, Alt-Tab, and the window surface agree.
Later packages consume the same canonical layers through platform-native asset
pipelines.

## Implementation Boundaries

- Keep shared state and glyph semantics out of platform code.
- Keep window messages, AppKit title-bar configuration, and X11/Wayland
  decoration behavior behind platform adapters.
- Use compact vector/path glyphs for session actions; do not reuse raster app
  icon artwork as interface controls.
- Do not add a fallback toolbar beneath the title bar.
- Do not let title-bar failures terminate an otherwise valid network session;
  report a stable shell error before exposing a malformed remote viewport.

## Verification

Focused automated coverage must prove:

- the center cluster remains centered with asymmetric native controls;
- every session state maps to a distinct glyph and accessible label;
- unsupported controls remain geometry-stable and cannot dispatch actions;
- the title-bar inset and renderer/input viewport are identical at multiple DPI
  scales and after maximize/restore;
- title-bar pointer and keyboard events never reach the remote input route;
- Windows window actions map to the expected platform commands;
- icon assets have expected dimensions, alpha policy, safe-zone bounds, and
  required Windows ICO sizes.

Windows manual acceptance checks cover taskbar/Alt-Tab/Explorer icon identity,
drag, resize, snap, minimize, maximize/restore, close, 100/150/200 percent DPI,
and a live Apple session with no remote-content occlusion. macOS and Linux gain
equivalent platform acceptance gates when their shells are implemented.

## Normative Asset References

- Apple Human Interface Guidelines, App icons:
  <https://developer.apple.com/design/human-interface-guidelines/app-icons>
- Android Developers, Adaptive icons:
  <https://developer.android.com/develop/ui/compose/system/icon_design_adaptive>
- Google Play Console Help, preview asset requirements:
  <https://support.google.com/googleplay/android-developer/answer/9866151>
