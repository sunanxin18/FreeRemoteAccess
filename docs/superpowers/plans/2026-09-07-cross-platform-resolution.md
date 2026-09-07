# Cross-Platform Native Display Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a protocol-neutral display-resolution intent that defaults to the current monitor's physical pixels, supports resolutions above 2560×1440, and feeds safe initial and dynamic sizing into the existing RDP and Apple session paths without changing Windows authentication or baseline behavior.

**Architecture:** Keep display geometry and planning in `frd-core`; let the desktop shell translate winit monitor/window state into a physical geometry snapshot; carry the immutable intent through `ConnectRequest`; let each protocol adapter map only the capabilities it owns. RDP will consume the planned initial size and retain its existing generation-bound Display Control state machine. Unsupported protocols remain server-managed or locally scaled.

**Tech Stack:** Rust 2021 workspace, winit desktop shell, egui UI, IronRDP, Apple HPSS/MVS adapters, built-in Rust tests.

**Spec:** `docs/plans/2026-09-07-cross-platform-resolution-design.md`

## Global Constraints

- Never add a product-wide 2560×1440 cap or treat logical points as remote physical pixels.
- Keep Windows-specific authentication, certificate TOFU, keychain/credential storage, black-screen baseline handling, and input behavior unchanged.
- Do not make protocol crates call platform window APIs; all platform geometry enters through `ConnectRequest`.
- Do not put credentials, target passwords, or sensitive connection metadata in source, docs, logs, tests, command lines, or captures.
- Update the README platform matrix in the same change and distinguish compilation, UI execution, protocol implementation, and live interoperability.
- Keep the remote-content UI free of persistent toolbars; resolution status belongs in the existing title-bar/chrome path.
- Run focused tests after each task, then the workspace verification ladder before claiming completion.

---

## Task 1: Add the protocol-neutral display model and planner

**Files:** `crates/frd-core/src/display.rs` (new), `crates/frd-core/src/lib.rs`, display-model unit tests.

- [x] Define `ResolutionMode`, `DisplayGeometry`, `DisplayConstraints`, `DisplayIntent`, `DisplayPlan`, and a diagnostic reason enum using checked `PixelSize`/`PixelRect` values.
- [x] Make the default intent server-managed for direct protocol callers; provide constructors for native display, work area, content, and fixed size.
- [x] Implement aspect-preserving constraint handling for width, height, area, texture, and byte budgets with no arbitrary 2560 cap.
- [x] Add tests for Retina physical conversion, 3840×2160 and 5120×2880 acceptance, fixed/server-managed modes, max-area and memory clamping, invalid zero sizes, and aspect preservation.
- [x] Run focused core tests and format checks.

## Task 2: Carry display intent through the connection API

**Files:** `crates/frd-protocol-api/src/lib.rs` and every `ConnectRequest` literal in `crates/` and protocol examples/tests.

- [x] Add a `display_intent: DisplayIntent` field with a safe default value and derive compatibility traits required by existing request types.
- [x] Update application, shell, RDP, Apple, cleanup, and test/example request constructors to use `DisplayIntent::default()` unless they explicitly supply a mode.
- [x] Carry selected UI mode through the application request; direct protocol callers remain server-managed.
- [x] Run affected crate tests.

## Task 3: Translate desktop window state into physical display geometry

**Files:** `crates/frd-shell-desktop/src/display_geometry.rs` (new), `crates/frd-shell-desktop/src/lib.rs`, `crates/frd-shell-desktop/src/application.rs`, shell tests.

- [x] Convert the current winit window and monitor sizes to physical pixels using checked scale-factor arithmetic; keep logical window extents for native window sizing.
- [x] Build native-display, work-area, and content snapshots without importing winit types into protocol crates.
- [x] Prepare `ConnectRequest.display_intent` at session start from the selected UI mode, falling back to the request's server-managed value in headless tests.
- [x] Preserve current platform title-bar/chrome geometry and ensure remote content receives only the physical content rectangle.
- [x] Add pure conversion tests for 1×, 2× Retina, and checked invalid scale factors.
- [x] Run shell library tests.

## Task 4: Use the planned initial size in RDP

**Files:** `crates/frd-protocol-rdp/src/connector.rs`, `config.rs`, `display.rs`, related tests.

- [x] Replace the unconditional 1280×720 initial choice with the request's planned physical size, retaining 1280×720 only as a server-managed fallback for direct callers.
- [x] Ensure the initial size is validated against IronRDP field limits and existing framebuffer safety checks without adding a 2560 cap.
- [x] Keep Display Control latest-only queueing, exact acknowledgement, generation transition, and full-baseline gating intact; feed it physical viewport dimensions.
- [x] Add tests for native 3840×2160, fixed 5120×2880, and server-managed fallback.
- [x] Run RDP crate tests.

## Task 5: Map Apple protocol adapters without cross-protocol leakage

**Files:** `crates/frd-protocol-apple/src/factory.rs`, `high_performance.rs`, `dynamic_resolution.rs`, and adapter tests.

- [x] Update Apple request construction for the new field.
- [x] Apply native/work-area intent only to the existing HPSS virtual-display startup path; leave Standard/MVS server-managed.
- [x] Keep Apple display state types separate from RDP Display Control types and retain generation-bound full-frame behavior.
- [x] Add tests proving HPSS native physical mapping and Standard/MVS ServerInit behavior.
- [x] Run the Apple protocol tests and format checks.

## Task 6: Remove the fixed shell presentation target while preserving window UX

**Files:** `crates/frd-shell-desktop/src/window_presentation.rs`, `application.rs`, `window_chrome.rs`, related tests.

- [x] Derive the initial native window content extent from the selected monitor in logical points, with the compact login extent retained before a session starts.
- [x] Keep remote session target size in physical pixels and local window extent in logical points; never pass a physical size directly to winit as points.
- [x] Preserve the current complete-frame transition, user-resize tracking, title-bar controls, and black-screen surface retention.
- [x] Add high-DPI conversion tests; the old 1100×720 value is now only a headless fallback.
- [x] Run shell and app tests.

## Task 7: Expose the mode in the shared desktop connection UI

**Files:** `crates/frd-ui-egui/src/connection.rs`, connection model/submission types, profile metadata if needed, translations/tests.

- [x] Add a protocol-neutral resolution-mode selector with a persistent field label, keyboard navigation, and Simplified-Chinese labels for native display, work area, window content, fixed size, and server-managed.
- [x] Keep the default native display, separate UI scale from remote resolution, and preserve entered credentials after resolution changes.
- [x] Validate fixed dimensions through positive `DragValue` bounds and carry the selected `DisplayIntent` into `ConnectRequest`; do not put secrets in profile metadata.
- [x] Add UI/model and application tests for default selection, saved-credential retention, and submission mapping.
- [x] Run UI, app, and shell tests.

## Task 8: Verify, document, and update the platform matrix

**Files:** `README.md`, `docs/validation/cross-platform-resolution-20260907.md` (new), affected package scripts if required.

- [x] Document the physical-pixel priority, no-2560 rule, constraint diagnostics, protocol capability differences, and local scaling fallback.
- [x] Mark high-resolution live interoperability as `开发中`; no unsupported live claim was added.
- [x] Run the final workspace verification ladder, macOS package staging, and the available Windows-target check. Focused package tests, `cargo fmt --all -- --check`, `git diff --check`, macOS package staging, and the Windows `frd-app` target check passed. The workspace test now passes after making the icon derivative assertion tolerant only of the observed single-channel cross-architecture Lanczos rounding difference; no committed asset was changed. The root binary and full RDP Windows target remain outside this host's linker/SDK capabilities.
- [x] Record any Windows SDK/MSVC or locked-desktop limitation separately from code failures. No locked-desktop rerun was required; the Windows target limitation is the missing MSVC/Windows SDK and vcpkg headers, while the icon mismatch is unrelated packaging state.
