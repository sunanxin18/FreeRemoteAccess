# Windows Product Cutover and Installer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut the Windows product fully over to the single winit/egui/wgpu application, remove minifb and obsolete product entry points, register the isolated Apple/RFB/RDP adapters, and produce verified x64 EXE, portable ZIP and MSI artifacts.

**Architecture:** `apps/freeremotedesk-windows` is the only product composition root and only installed executable. It owns optional CLI-prefill parsing, platform services, factory registration and the winit event loop; it never owns protocol internals. The old root package remains a headless reverse-engineering/protocol lab and is excluded from product artifacts. Packaging contains no server helper, service, daemon, driver or relay.

**Tech Stack:** Rust 1.96.0, Windows MSVC x64, winit/wgpu/egui versions pinned by the architecture spec, WiX Toolset v4 CLI, PowerShell packaging scripts, GitHub Actions `windows-2022`.

**Prerequisites:** Complete the core/Apple, RFB and RDP plans, including their live validation gates.

**Spec:** `docs/superpowers/specs/2026-08-27-winit-wgpu-windows-first-architecture-design.md`

## Product Rules

- Product display name and installed executable are `FreeRemoteDesk` and `FreeRemoteDesk.exe`; repository/remote naming does not create a second product binary.
- One winit window provides login, connection state, remote desktop and errors. CLI options only prefill that form and may request `--connect` after validation.
- The MSI and ZIP contain only the product executable, license/notices and required runtime data. The headless protocol lab and minifb comparison tool are never shipped.
- No literal password option exists. No credentials, target addresses, captures, TLS keys or local test configuration enter an artifact.
- No custom program is installed on any remote server. The Windows installer installs no service, scheduled task, driver, firewall rule or background updater.
- Historical packaging files under `.worktrees/five-platform-client/packaging/windows` may be consulted read-only for safe cleanup, canonical archive and MSI lifecycle checks. Do not merge that branch or restore its Flutter/two-binary layout.
- Add only packaging manifest/archive/lifecycle tests; do not add GUI snapshots.

---

### Task 1: Finalize the three-adapter composition root and unified launch contract

**Files:**
- Modify: `apps/freeremotedesk-windows/Cargo.toml`
- Modify: `apps/freeremotedesk-windows/src/main.rs`
- Modify: `apps/freeremotedesk-windows/src/cli.rs`
- Create: `apps/freeremotedesk-windows/src/catalog.rs`
- Modify: `crates/frd-app/src/controller.rs`
- Modify: `crates/frd-ui-model/src/lib.rs`
- Modify: `crates/frd-ui-egui/src/connection.rs`

**Interfaces:**
- Produces: complete protocol catalog, one `LaunchOptions → ConnectionDraft → AppIntent` path.

- [ ] **Step 1: Write RED composition tests**

Test exact Auto mappings (`Mac OS → Apple HPSS/MVS`, `Windows → RDP`, `Linux → RFB`), explicit incompatible pair rejection, one stable descriptor per adapter, empty/partial CLI opening the form, complete CLI prefill without auto-connect, and complete `--connect` issuing one intent.

- [ ] **Step 2: Run RED**

Run: `cargo test -p freeremotedesk-windows catalog`

- [ ] **Step 3: Implement the one catalog**

Only `apps/freeremotedesk-windows/src/main.rs` constructs `AppleProtocolFactory`, `RfbProtocolFactory` and `RdpProtocolFactory`. `catalog.rs` exposes their protocol-neutral descriptors to `frd-app`; no lower package imports a concrete adapter.

- [ ] **Step 4: Finalize CLI prefill behavior**

Support optional `--target-system`, `--address`, `--port`, `--protocol`, credential-provider selectors and `--connect`. No `--password` or other literal-secret argument exists. Provider failure/missing values annotate the form and do not terminate startup. GUI submission and auto-connect both construct the same `ConnectionSubmission`.

- [ ] **Step 5: Run GREEN**

Run: `cargo test -p freeremotedesk-windows catalog`

Run: `cargo run -p freeremotedesk-windows -- --help`

Expected: PASS; help contains no literal password argument.

- [ ] **Step 6: Commit**

```bash
git add apps/freeremotedesk-windows crates/frd-app crates/frd-ui-model crates/frd-ui-egui
git commit -m "feat: finalize unified Windows protocol catalog"
```

### Task 2: Enforce one process, one window and one session

**Files:**
- Modify: `crates/frd-platform-windows/src/single_instance.rs`
- Modify: `crates/frd-shell-desktop/src/application.rs`
- Modify: `crates/frd-session/src/coordinator.rs`
- Create: `apps/freeremotedesk-windows/tests/single_instance.rs`

- [ ] **Step 1: Write RED lifecycle tests**

Test first-process mutex ownership, second-process stable rejection, disconnect waiting for all worker joins, reconnect only after `Closed`, and window close releasing input/session/surface in order.

- [ ] **Step 2: Run RED**

Run: `cargo test -p freeremotedesk-windows single_instance`

- [ ] **Step 3: Implement the final process/session gates**

Acquire a per-user named mutex before creating EventLoop. A second invocation exits with `windows_instance_already_running` and cannot start a worker. Within the process, `ActiveSessionSlot` prevents a second worker until the previous session published `Closed` and joined.

- [ ] **Step 4: Implement deterministic shutdown order**

Stop input, emit one ReleaseAll, cancel session, close protocol writer/socket, join reader/decoder/audio, discard frame/media mailboxes, destroy wgpu Surface, release its window lease, then release the mutex. Do not force-kill worker threads during ordinary shutdown.

- [ ] **Step 5: Run GREEN**

Run: `cargo test -p freeremotedesk-windows single_instance`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/frd-platform-windows crates/frd-shell-desktop crates/frd-session apps/freeremotedesk-windows/tests/single_instance.rs
git commit -m "fix: enforce one Windows client process and session"
```

### Task 3: Remove minifb and obsolete product viewer paths

**Files:**
- Modify: root `Cargo.toml`
- Modify: root `src/main.rs`
- Modify: root `src/vnc/mod.rs`
- Delete after migrated parity: `src/viewer.rs`
- Delete after migrated parity: `src/keysym.rs`
- Delete after extraction: `src/pointer_input.rs`
- Delete after extraction: `src/vnc/hpss_viewer.rs`
- Delete: `tools/frd-legacy-minifb-lab/`
- Update: `README.md`
- Update: `AGENTS.md`

- [ ] **Step 1: Record the final legacy parity gate**

Verify the current Windows product has already passed Apple picture/color/type-1/input/dynamic-resolution/audio parity and that RFB/RDP adapters passed their gates. Record the exact validation document commits before deletion.

- [ ] **Step 2: Remove product viewer dispatch and feature coupling**

Remove root `view`/`hpssview`, the `viewer` feature, minifb dependency, minifb key mapping and CPAL coupling. Keep only headless protocol/reverse-engineering commands that still serve documented development workflows. Platform audio remains in `frd-platform-windows` behind its own feature.

- [ ] **Step 3: Delete the temporary comparison package**

Remove `frd-legacy-minifb-lab` after the parity record exists. Do not move its renderer/input loop elsewhere.

- [ ] **Step 4: Verify complete Flutter/minifb removal from active product inputs**

Run:

```powershell
rg -n "Flutter|flutter|dart:ffi|flutter_rust_bridge|minifb|hpss_viewer" Cargo.toml Cargo.lock src crates apps packaging .github
```

Expected: no matches in active product code, dependencies, build scripts or tests. Historical reverse notes/specs may retain factual mentions and are not build inputs.

- [ ] **Step 5: Build both product and headless lab**

Run: `cargo build -p freeremotedesk-windows --release`

Run: `cargo build -p freeremotedesk --no-default-features`

Expected: both PASS; `cargo tree -p freeremotedesk-windows` contains neither minifb nor the root package.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src crates apps tools README.md AGENTS.md
git commit -m "refactor: remove legacy minifb product paths"
```

### Task 4: Add Windows executable metadata and release profile

**Files:**
- Modify: root `Cargo.toml`
- Modify: `apps/freeremotedesk-windows/Cargo.toml`
- Modify: `apps/freeremotedesk-windows/src/main.rs`
- Create: `apps/freeremotedesk-windows/build.rs`
- Create: `packaging/windows/windows_app.manifest`
- Create: `packaging/windows/version-info.toml`

- [ ] **Step 1: Define the product binary**

Set the binary name to `FreeRemoteDesk`. On Windows release builds use the Windows GUI subsystem while retaining CLI argument parsing. Debug builds may keep a console for diagnostics. Embed the supportedOS/longPathAware/per-monitor-v2 DPI manifest and package version/company/file description metadata.

- [ ] **Step 2: Configure release behavior**

Use workspace release settings with LTO, one codegen unit, panic abort and symbol stripping only after crash diagnostics remain available through structured local logs. Do not enable CPU target features that prevent running on supported x64 Windows machines.

- [ ] **Step 3: Build and inspect PE metadata**

Run: `cargo build -p freeremotedesk-windows --release --locked`

Inspect the PE header/resources with a PowerShell parser or `dumpbin /headers`. Verify GUI subsystem, x64 machine type, manifest, version and no unexpected DLL dependency outside supported Windows/system/runtime libraries.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock apps/freeremotedesk-windows packaging/windows
git commit -m "build: add Windows product metadata"
```

### Task 5: Generate a deterministic product manifest and notices

**Files:**
- Create: `packaging/package_manifest.py`
- Create: `packaging/windows/prepare-stage.ps1`
- Create: `packaging/windows/verify-stage.ps1`
- Create: `packaging/THIRD_PARTY.toml`
- Create: `LICENSES/`
- Modify: `.gitignore`

- [ ] **Step 1: Define the artifact manifest schema**

Include product version, target triple, rustc version, git commit, artifact names, sizes and SHA-256 hashes. Sort paths and JSON keys deterministically. Reject paths outside the exact staging/dist roots and reject reparse points before cleanup or archive creation.

- [ ] **Step 2: Generate third-party notices**

Create license/notice output from `Cargo.lock`, including source-offer/license obligations for the selected AAC-ELD implementation. Adapt the historical FDK supply-chain checks only if the final Apple audio dependency still requires them; verify exact crate checksum/source and fail packaging when obligations cannot be met.

- [ ] **Step 3: Stage only the product payload**

`prepare-stage.ps1` copies `FreeRemoteDesk.exe`, project license and generated notices into `target/package/windows/stage/FreeRemoteDesk`. It must reject the headless lab executable, minifb files, credentials, captures, `.dmg`, reverse-engineering binaries and local configuration.

- [ ] **Step 4: Test manifest/stage safety**

Run the scripts against a temporary fixture under `target/package/windows/tests`; verify path traversal, reparse point, missing notice, hash mismatch and unexpected-file cases fail closed.

- [ ] **Step 5: Commit**

```bash
git add packaging LICENSES .gitignore
git commit -m "build: add deterministic Windows package staging"
```

### Task 6: Build and verify the portable ZIP

**Files:**
- Create: `packaging/windows/portable-archive.ps1`
- Create: `packaging/windows/build-portable.ps1`
- Create: `packaging/windows/test-portable-archive.ps1`

- [ ] **Step 1: Implement canonical archive construction**

Normalize separators to `/`, reject absolute/traversal/reserved/duplicate/case-colliding entry names, sort entries ordinally and set deterministic timestamps. Never follow symlinks/reparse points.

- [ ] **Step 2: Implement the portable build**

Build the locked release binary, prepare the verified stage, and emit:

```text
dist/windows/FreeRemoteDesk-<version>-windows-x64.exe
dist/windows/FreeRemoteDesk-<version>-windows-x64-portable.zip
dist/windows/artifact-manifest.json
```

- [ ] **Step 3: Run archive safety tests**

Run: `powershell -NoProfile -File packaging/windows/test-portable-archive.ps1 -FixtureRoot target/package/windows/archive-tests`

Expected: canonical fixture accepted; traversal, duplicates and unexpected roots rejected.

- [ ] **Step 4: Extract and smoke test**

Extract to a clean temporary directory, verify artifact hashes/notices, launch exactly one GUI process, confirm the unified login window appears without credentials, then close it normally.

- [ ] **Step 5: Commit**

```bash
git add packaging/windows
git commit -m "build: produce verified Windows portable archive"
```

### Task 7: Build and verify the WiX MSI

**Files:**
- Create: `packaging/windows/verify-wix-tool.ps1`
- Create: `packaging/windows/build-msi.ps1`
- Create: `packaging/windows/verify-package.ps1`
- Create: `packaging/windows/wix/main.wxs`

- [ ] **Step 1: Pin and verify WiX v4**

`verify-wix-tool.ps1` requires an exact approved WiX v4 CLI version and prints the official installation command when absent; it does not silently download tools. Record a stable UpgradeCode once in `main.wxs`; ProductCode changes per release.

- [ ] **Step 2: Define the MSI payload**

Install `FreeRemoteDesk.exe` and notices under `ProgramFilesFolder\FreeRemoteDesk`, create one Start Menu shortcut and one Add/Remove Programs entry, and support major upgrades. Install no service, driver, scheduled task, firewall rule, protocol handler or auto-start entry.

- [ ] **Step 3: Build MSI from the verified stage**

Emit `dist/windows/FreeRemoteDesk-<version>-windows-x64.msi`, then update the artifact manifest with its hash. Use exact checked paths; safe cleanup is limited to `dist/windows` and `target/package/windows` after absolute-path/reparse-point validation.

- [ ] **Step 4: Verify administrative extraction and payload identity**

Use `msiexec /a` into the verified temporary root. Confirm the extracted executable hash equals the standalone/ZIP payload and the installer contains no unexpected files/custom actions.

- [ ] **Step 5: Verify install, launch, upgrade and uninstall on a clean runner**

Install a lower-version fixture, upgrade to current, launch from the Start Menu, confirm one unified-login window, uninstall, and verify shortcut, install directory and ARP registration are removed. Cleanup failures must be reported separately from the primary failure.

- [ ] **Step 6: Commit**

```bash
git add packaging/windows
git commit -m "build: produce verified Windows MSI installer"
```

### Task 8: Add reproducible Windows CI artifacts

**Files:**
- Create: `.github/workflows/windows-release.yml`
- Create: `.github/workflows/windows-ci.yml`

- [ ] **Step 1: Add pull-request CI**

On `windows-2022`, install Rust from `rust-toolchain.toml`, restore only safe Cargo caches, run format, workspace tests, dependency-boundary tests, headless build and Windows release build. Never expose local credentials or run live target tests.

- [ ] **Step 2: Add tagged artifact build**

For version tags, install the pinned WiX tool from its official source, run locked packaging scripts, verify manifest hashes, then upload EXE/ZIP/MSI/manifest as GitHub Actions artifacts. Release publishing remains a separately authorized operation.

- [ ] **Step 3: Verify workflow locally and on one branch run**

Validate YAML, push the implementation branch, inspect one successful Windows run and download its artifacts. Verify hashes match `artifact-manifest.json`.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows
git commit -m "ci: build verified Windows installers"
```

### Task 9: Run the final Windows product acceptance gate

**Files:**
- Create: `docs/validation/windows-product-release.md`
- Update: `README.md`

- [ ] **Step 1: Run the complete offline matrix**

Run:

```bash
cargo fmt -- --check
cargo test --workspace
cargo build --workspace --no-default-features
cargo build -p freeremotedesk-windows --release --locked
powershell -NoProfile -File packaging/windows/build-msi.ps1
powershell -NoProfile -File packaging/windows/verify-package.ps1 -DistDir dist/windows
```

- [ ] **Step 2: Audit product dependency and artifact boundaries**

Verify product dependencies contain winit/wgpu/egui and three adapters but no minifb/Flutter/root-lab edge. Verify every package contains one product executable and no server companion, lab executable, credentials or captures.

- [ ] **Step 3: Re-run bounded native-server smoke sessions**

From the packaged executable, verify the unified login plus one successful Apple HPSS/MVS, Linux RFB and Windows RDP session against authorized stock services. Run each serially and close it before the next; never launch two clients.

- [ ] **Step 4: Verify unified login fallback**

Launch with no args, partial non-secret args, complete prefill without `--connect`, and complete provider-backed `--connect`. Confirm all cases use the same window/form/session flow and no password can be supplied in argv.

- [ ] **Step 5: Record exact evidence and known limits**

Document commit, tool versions, artifact hashes, install lifecycle, protocol gates, current capability omissions and any unverified long-duration behavior. Do not claim Android/macOS/Linux/HarmonyOS client packages from this Windows milestone.

- [ ] **Step 6: Commit**

```bash
git add README.md docs/validation/windows-product-release.md
git commit -m "docs: record Windows product release acceptance"
```
