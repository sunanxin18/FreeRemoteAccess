# Apple HP 1440p Decode Throughput Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver ARD-compatible 2560x1440/60-Hz Apple HP first choice, automatic 30-Hz overload fallback, and a bounded SIMD-enabled cross-platform FFmpeg software decoder.

**Architecture:** Apple source-load negotiation stays in `frd-protocol-apple`; decoder scheduling stays inside the FFmpeg plugin; platform packaging only supplies the pinned FFmpeg libraries. The existing protocol-neutral media API and wgpu renderer remain unchanged in this phase.

**Tech Stack:** Rust 2021, FFmpeg 8.1.2/libavcodec, C11 bridge, NASM on Windows x86_64, Cargo `cc`, PowerShell build and package verification.

**Spec:** `docs/superpowers/specs/2026-09-04-apple-hp-1440p-decode-throughput-design.md`

## Global Constraints

- Do not modify Standard/MVS or RDP wire behavior.
- Do not block UDP reads or use PLI as steady-state backpressure.
- Keep FFmpeg threading and assembly platform-neutral above build adapters.
- Preserve FFmpeg LGPL dynamic-link and corresponding-source requirements.
- Do not commit or push the dirty worktree; use exact diff snapshots.
- Add only focused core protocol/backend tests.

---

### Task 1: Apple HP explicit 2560x1440 60/30-Hz profiles

**Files:**
- Modify: `crates/frd-protocol-apple/src/hpss.rs`
- Modify: `crates/frd-protocol-apple/src/runtime.rs`
- Modify: `crates/frd-protocol-apple/src/media_runtime.rs`
- Modify: `crates/frd-protocol-apple/src/media_transport.rs`

**Interfaces:**
- Consumes: recovered 308-byte `0x1d` layout and HP-only runtime identity.
- Produces: HP startup size 2560x1440 pixels/points (1x scale) plus explicit 60-Hz and 30-Hz mode records; initial runtime uses 60 Hz and its `0x1c` advertises stream-1 60-FPS support, while 30 Hz/non-HP keep the bit clear.

- [ ] Add failing wire-layout and HP-only startup tests for both refresh profiles.
- [ ] Run the focused tests and confirm expected 1280x720/single-profile assertions fail.
- [ ] Implement 2560x1440 primary mode, explicit 60/30-Hz encoding for every fallback mode, and 60-Hz initial runtime selection.
- [ ] Add HP-60, HP-30, and non-HP integration tests for exact `0x1c` flags and propagate the explicit tier without changing UDP/PLI behavior.
- [ ] Run `cargo test -p frd-protocol-apple high_performance --lib` and the full Apple crate tests.
- [ ] Record the exact no-index diff in the SDD report without committing.

### Task 2: Cross-platform bounded FFmpeg threading and SIMD build

**Files:**
- Modify: `crates/frd-video-ffmpeg-plugin/src/decoder.rs`
- Modify: `crates/frd-video-ffmpeg-plugin/src/ffmpeg_bridge.c`
- Modify: `crates/frd-video-ffmpeg-plugin/build.rs`
- Modify: `crates/frd-video-ffmpeg-plugin/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `tools/build-ffmpeg-windows.ps1`

**Interfaces:**
- Consumes: coded width/height already present in `FrdVideoConfig`.
- Produces: internal `SoftwareDecodeThreadPolicy` and native bridge creation arguments; no public media API change.

- [ ] Add failing Rust policy tests for sub-1440p, 1440p, and single-core cases.
- [ ] Add a failing native integration assertion for requested/active thread settings.
- [ ] Implement bounded two-frame-thread creation through the C bridge.
- [ ] Replace the Windows-only native bridge build with `cc`-based Windows/macOS/Linux support.
- [ ] Require NASM, remove `--disable-x86asm`, and verify `HAVE_X86ASM 1` in the Windows FFmpeg build.
- [ ] Run plugin unit/integration tests and rebuild the fixed FFmpeg bundle.
- [ ] Record the exact no-index diff in the SDD report without committing.

### Task 3: Sustained-overload 60-to-30-Hz controller

**Files:**
- Modify: `crates/frd-protocol-api/src/lib.rs`
- Create: `crates/frd-shell-desktop/src/video_rate_fallback.rs`
- Modify: `crates/frd-shell-desktop/src/lib.rs`
- Modify: `crates/frd-shell-desktop/src/video_decode_worker.rs`
- Modify: `crates/frd-shell-desktop/src/application.rs`
- Modify: `crates/frd-protocol-apple/src/runtime.rs`
- Modify: `crates/frd-protocol-rdp/src/runtime.rs`

**Interfaces:**
- Consumes: presentation timing, decoder queue depth, active protocol identity, and Task 1 refresh profiles.
- Produces: one idempotent 30-Hz source-rate command scoped to Apple HP.

- [ ] Add failing controller tests for the 200-ms/four-AU/two-second conjunction, transient recovery, and one-shot behavior.
- [ ] Add a protocol-isolation test proving Standard/MVS and RDP never receive the fallback action.
- [ ] Implement queue-depth observation without exposing encoded payloads or blocking UDP receive.
- [ ] Implement the exact recovered active-session `RFBSetDisplayConfiguration`/0x1d 30-Hz write; do not restart authentication or copy credentials.
- [ ] Run focused shell/protocol tests and record the exact no-index diff.

### Task 4: Measurement gate and copy optimization decision

**Files:**
- Modify: `docs/validation/apple-hp-latency-20260904.md`
- Modify if evidence requires: `crates/frd-video-ffmpeg-plugin/src/decoder.rs`

**Interfaces:**
- Consumes: Task 1 source profiles, Task 2 optimized decoder, and Task 3 fallback controller.
- Produces: measured decode/copy/queue evidence; optionally a plugin-local reusable plane pool without ABI changes.

- [ ] Build and install the exact release package after stopping any existing client process.
- [ ] Run one bounded 60-Hz target session and record accepted size, AU rate, decode rate, queue depth, latency percentiles, and whether fallback fired.
- [ ] If allocation/copy remains material, add a failing pool-reuse test and implement only a plugin-local bounded plane pool.
- [ ] Re-run focused and full verification; document external limitations precisely.

### Task 5: Whole-change review and verification

**Files:**
- Modify: `README.md`
- Modify: `docs/validation/apple-hp-latency-20260904.md`

**Interfaces:**
- Consumes: Tasks 1-4 diffs and evidence.
- Produces: current platform matrix and review verdict.

- [ ] Run a task-scoped review after each implementation task.
- [ ] Run a final cross-layer review for Apple isolation, ABI safety, latency, and cross-platform build behavior.
- [ ] Run formatting, Apple/FFmpeg/shell tests, release build, and Windows package verification.
- [ ] Leave all changes uncommitted and report the exact remaining manual platform-validation limits.
