# Mac-Baseline Windows RDP Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the verified Windows winit/wgpu Apple HPSS/MVS client the immutable baseline, then integrate the independent IronRDP adapter without regressing Mac display, input, audio, title-bar, DPI, or protocol selection.

**Architecture:** Freeze the exact verified Mac working tree first, then create an isolated integration branch from that commit. Transplant RDP in protocol-owned slices, adapt the USB-HID key contract inside the RDP adapter, and integrate only the protocol-neutral lazy-audio and composition-root changes by hand. Move local `main` only after offline, architectural, UI, and bounded Mac live gates pass.

**Tech Stack:** Rust 1.96 stable, Cargo workspace, winit 0.30.13, wgpu 30.0.1, egui 0.36.1, AccessKit, IronRDP 0.17.0, Windows Credential Manager/DPAPI, Apple HPSS/MVS.

**Spec:** `docs/superpowers/specs/2026-08-29-mac-baseline-rdp-integration-design.md`

## Global Constraints

- The exact verified `windows-client-rearchitecture` source and asset state is the Mac product baseline.
- Concrete protocol types are imported only by `apps/freeremotedesk-windows/src/main.rs`.
- `frd-protocol-apple` and `frd-protocol-rdp` never depend on or fall back to one another.
- Mac automatic selection resolves only to Apple HPSS/MVS; Windows automatic selection resolves only to RDP.
- `PhysicalKeyCode` remains a USB HID usage; Windows Set-1/E0 translation belongs only to `frd-protocol-rdp::input`.
- Preserve the verified title bar, AccessKit, native chrome, DPI transition, content rectangle, pointer mapping, and Apple input behavior.
- Preserve lazy audio: no-media and video-only sessions do not open an output; the first supported PCM frame opens the device and is delivered exactly once.
- Do not add RDP-specific pages, title-bar controls, profile fields, public feature schemas, or cross-protocol fallback.
- Run focused core protocol and boundary tests; do not add broad low-value GUI test duplication.
- Use Cargo's normal parallelism; do not add serial or low-concurrency build policy.
- Keep Windows RDP at `开发中` until an independent authorized stock Windows target passes its live gate.
- Do not delete or overwrite unrelated untracked files in `D:\FreeRemoteDesk`.
- Do not push local `main` without separate explicit user authorization.

---

### Task 1: Freeze the verified Mac product baseline

**Files:**
- Modify: the existing 31 tracked files in `D:\FreeRemoteDesk\.worktrees\windows-client-rearchitecture`
- Create: the existing 36 untracked product files under `apps/`, `assets/`, `crates/`, `docs/`, and `tools/`
- Verify: `docs/validation/windows-apple-wgpu-parity.md`

**Interfaces:**
- Consumes: approved spec commit `bb790bd` and the already live-validated working tree.
- Produces: one immutable source commit whose tree exactly builds the verified Apple HPSS/MVS winit/wgpu product.

- [ ] **Step 1: Record the exact baseline inventory without changing it**

Run:

```powershell
$wt = 'D:\FreeRemoteDesk\.worktrees\windows-client-rearchitecture'
git -C $wt status --short
git -C $wt diff --check
git -C $wt diff --stat
git -C $wt ls-files --others --exclude-standard
```

Expected: 31 modified tracked files, 36 untracked files, no staged files, and no whitespace errors.

- [ ] **Step 2: Fail closed on credentials and generated output**

Run a path-only and content scan over the files that would be committed:

```powershell
$wt = 'D:\FreeRemoteDesk\.worktrees\windows-client-rearchitecture'
$files = @(git -C $wt diff --name-only)
$files += @(git -C $wt ls-files --others --exclude-standard)
$files | Where-Object { $_ -match 'CREDENTIALS\.local|target/|ard_capture|\.superpowers/' }
$files | ForEach-Object {
    $path = Join-Path $wt $_
    if (Test-Path -LiteralPath $path -PathType Leaf) {
        rg -l --fixed-strings '192.168.131.64' $path
    }
}
```

Expected: no listed credential, target, capture, task-runtime, or hard-coded authorized-target file. Review all binary paths against the approved font/icon asset directories.

- [ ] **Step 3: Re-run the baseline build gates**

Run:

```powershell
cargo +stable fmt --all -- --check
cargo +stable test --workspace
cargo +stable test --workspace --no-default-features
cargo +stable build -p freeremotedesk-windows --release
```

Expected: all commands exit 0. Existing explicitly ignored local-fixture tests may remain ignored.

- [ ] **Step 4: Stage the complete baseline and verify the index**

Run:

```powershell
git add --all
git diff --cached --check
git diff --cached --name-status
git status --short
```

Expected: the integration spec and plan commits are already in history; only the verified product snapshot is staged. No credential or generated-output path appears.

- [ ] **Step 5: Commit the immutable baseline**

Run:

```powershell
git commit -m "feat: establish verified Apple desktop baseline"
git status --short
```

Expected: commit succeeds and the worktree becomes clean.

---

### Task 2: Preserve asset provenance omitted from the verified snapshot

**Files:**
- Create: `assets/app-icon/source/app-icon-master-reference.png`
- Create: `assets/app-icon/source/portal-foreground-matte.png`
- Create: `assets/app-icon/source/remote3.jpeg`
- Create: `tools/frd-icon-assets/tests/exports.rs`
- Modify only if evidence requires it: `assets/app-icon/README.md`
- Modify only if evidence requires it: `tools/frd-icon-assets/src/lib.rs`

**Interfaces:**
- Consumes: immutable Mac baseline and historical asset commit `62f7233`.
- Produces: deterministic source/provenance files and export coverage without replacing the verified runtime assets.

- [ ] **Step 1: Create an isolated integration branch and worktree**

Run from `D:\FreeRemoteDesk` after the baseline commit is known as `$baseline`:

```powershell
git worktree add 'D:\FreeRemoteDesk\.worktrees\mac-baseline-rdp-integration' -b codex/mac-baseline-rdp-integration $baseline
git -C 'D:\FreeRemoteDesk\.worktrees\mac-baseline-rdp-integration' status --short
```

Expected: the new worktree is clean and starts exactly at the immutable baseline commit.

- [ ] **Step 2: Prove which historical assets are absent**

Run:

```powershell
git diff --name-status HEAD 62f7233 -- assets/app-icon/source tools/frd-icon-assets/tests
```

Expected: the four declared paths are the historical provenance/export additions requiring transplantation; do not replace already verified generated assets.

- [ ] **Step 3: Transplant only the declared provenance paths**

Run:

```powershell
git checkout 62f7233 -- assets/app-icon/source/app-icon-master-reference.png assets/app-icon/source/portal-foreground-matte.png assets/app-icon/source/remote3.jpeg tools/frd-icon-assets/tests/exports.rs
```

If `exports.rs` requires a symbol absent from the baseline, copy only that symbol and its direct unit test from `62f7233`; do not replace the complete icon tool library.

- [ ] **Step 4: Run deterministic asset tests**

Run:

```powershell
cargo +stable test -p frd-icon-assets
cargo +stable test -p freeremotedesk-windows --test icon_resource
```

Expected: both commands pass and runtime/icon package assets remain byte-compatible with the baseline.

- [ ] **Step 5: Commit the provenance slice**

Run:

```powershell
git add assets/app-icon/source tools/frd-icon-assets
git diff --cached --check
git commit -m "chore: preserve product asset provenance"
```

---

### Task 3: Transplant secure RDP connection and graphics foundations

**Files:**
- Create: `crates/frd-protocol-rdp/Cargo.toml`
- Create: `crates/frd-protocol-rdp/src/{lib,factory,config,upstream,server_identity,tls,error,connector,runtime,active_session,surface,baseline}.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Create/Modify: `docs/superpowers/specs/2026-08-29-windows-native-rdp-design.md`
- Create/Modify: `docs/superpowers/plans/2026-08-29-windows-rdp-adapter.md`

**Interfaces:**
- Consumes: `ProtocolFactory`, `ProtocolRuntime`, `SurfaceUpdate`, `ProtocolId::rdp()`, and the immutable Mac baseline.
- Produces: an independent RDP crate through activation and bounded BGRX graphics publication, without product registration or keyboard input.

- [ ] **Step 1: Replay the protocol-owned foundation commits in order**

Run:

```powershell
$commits = @('a8b9c73','87c9f59','456cbc7','93b2abd','7e6d17c','4d04f23','8b82a3a','cc7d245','fc48991')
foreach ($commit in $commits) { git cherry-pick $commit; if ($LASTEXITCODE -ne 0) { break } }
```

For manifest or lockfile conflicts, keep the baseline's AccessKit and title-bar dependencies, add the RDP member/dependencies, remove conflict markers, run `cargo +stable generate-lockfile`, then continue the cherry-pick. Do not resolve source conflicts with whole-file `ours` or `theirs`.

- [ ] **Step 2: Verify the adapter dependency boundary**

Run:

```powershell
cargo +stable tree -p frd-protocol-rdp -e normal
rg -n "frd-protocol-apple|frd-wire-rfb|frd-shell-desktop|frd-platform-|winit|wgpu|egui" crates/frd-protocol-rdp/Cargo.toml crates/frd-protocol-rdp/src
```

Expected: the tree builds; the search finds no forbidden concrete dependency or import.

- [ ] **Step 3: Run the secure-connection and graphics tests**

Run:

```powershell
cargo +stable test -p frd-protocol-rdp server_identity
cargo +stable test -p frd-protocol-rdp connector
cargo +stable test -p frd-protocol-rdp surface
cargo +stable test -p frd-protocol-rdp baseline
```

Expected: all focused tests pass.

- [ ] **Step 4: Record the resulting commit range**

Run:

```powershell
git log --oneline --decorate -12
git status --short
```

Expected: all replayed commits are present and the worktree is clean.

---

### Task 4: Reconcile USB HID input with RDP Set-1/E0 input

**Files:**
- Create/Modify: `crates/frd-protocol-rdp/src/input.rs`
- Create/Modify: `crates/frd-protocol-rdp/src/writer.rs`
- Modify: `crates/frd-protocol-rdp/src/active_session.rs`
- Modify: `crates/frd-protocol-rdp/src/runtime.rs`

**Interfaces:**
- Consumes: `PhysicalKeyCode::usb_hid_usage() -> u16`, `InputEvent`, and IronRDP `Scancode`/`Database`.
- Produces: `fn set1_scancode_from_hid_usage(usage: u16) -> Option<u16>` and ordered RDP input operations.

- [ ] **Step 1: Replay the original RDP input commits**

Run:

```powershell
git cherry-pick 53438b5
git cherry-pick 9812749
```

Expected: the RDP input files exist. Compilation may fail because the verified neutral key type intentionally hides its raw field.

- [ ] **Step 2: Write RED HID-to-RDP mapping tests**

Add focused cases in `crates/frd-protocol-rdp/src/input.rs`:

```rust
#[test]
fn hid_keys_map_to_set1_and_e0_scancodes() {
    assert_eq!(set1_scancode_from_hid_usage(0x04), Some(0x001e)); // A
    assert_eq!(set1_scancode_from_hid_usage(0x1e), Some(0x0002)); // 1
    assert_eq!(set1_scancode_from_hid_usage(0x28), Some(0x001c)); // Enter
    assert_eq!(set1_scancode_from_hid_usage(0xe0), Some(0x001d)); // Left Ctrl
    assert_eq!(set1_scancode_from_hid_usage(0xe4), Some(0xe01d)); // Right Ctrl
    assert_eq!(set1_scancode_from_hid_usage(0x52), Some(0xe048)); // Up
    assert_eq!(set1_scancode_from_hid_usage(0x58), Some(0xe01c)); // Keypad Enter
    assert_eq!(set1_scancode_from_hid_usage(0xffff), None);
}
```

Update existing input fixtures to construct keys with
`PhysicalKeyCode::from_usb_hid_usage(...)`. Add one translation test proving a
HID `A` press/release becomes RDP Set-1 `0x1e` press/release and one existing
`ReleaseAll` test remains green.

- [ ] **Step 3: Run RED**

Run:

```powershell
cargo +stable test -p frd-protocol-rdp input
```

Expected: fail because `set1_scancode_from_hid_usage` is absent and direct field/constructor access is invalid.

- [ ] **Step 4: Implement the private mapping**

Implement `set1_scancode_from_hid_usage` as an explicit match table for the
supported USB Keyboard/Keypad usages. In `operations_for_event`, replace direct
numeric access with:

```rust
let scancode = set1_scancode_from_hid_usage(code.usb_hid_usage())
    .map(Scancode::from_u16)
    .ok_or(RdpInputError::InvalidScancode)?;
```

Do not modify the neutral HID type or the Apple adapter.

- [ ] **Step 5: Run GREEN and commit the reconciliation**

Run:

```powershell
cargo +stable test -p frd-protocol-rdp input
cargo +stable test -p frd-protocol-rdp lifecycle
git add crates/frd-protocol-rdp
git commit -m "fix: translate USB HID keys for RDP input"
```

Expected: all focused input/lifecycle tests pass.

---

### Task 5: Integrate protocol-neutral lazy audio into the verified shell

**Files:**
- Modify: `crates/frd-shell-desktop/src/application.rs`

**Interfaces:**
- Consumes: `AudioOutputFactory`, media receiver, video/PCM `MediaFrame` variants.
- Produces: `run_audio_worker` that opens output only for the first supported PCM frame and enqueues that frame exactly once.

- [ ] **Step 1: Add the two RED shell regressions from the RDP history**

Port the focused fixtures and assertions represented by commits `2fc832c` and
`873140b` into the verified shell:

```rust
#[test]
fn audio_worker_opens_device_only_after_first_frame() {
    // A closed no-media publisher leaves the open count at zero.
    // A first PCM frame opens the output exactly once.
}

#[test]
fn audio_worker_ignores_video_until_first_pcm_frame() {
    // Video does not open audio.
    // The following PCM frame is the first recorded sample payload.
}
```

Reuse the original test bodies from current `main`; do not copy the complete
pre-title-bar `application.rs`.

- [ ] **Step 2: Run RED**

Run:

```powershell
cargo +stable test -p frd-shell-desktop audio_worker_opens_device_only_after_first_frame
cargo +stable test -p frd-shell-desktop audio_worker_ignores_video_until_first_pcm_frame
```

Expected: at least one test fails because the baseline opens the device before receiving media.

- [ ] **Step 3: Implement minimal lazy open in the existing worker**

Adapt `run_audio_worker` so it reads until the first supported PCM frame,
returns `Closed` if the sender closes first, then opens the device, enqueues the
saved PCM frame once, and continues processing. Video and unsupported media do
not open audio. Preserve the baseline's title-bar, AccessKit, DPI, renderer,
cleanup, and fatal-exit code unchanged.

- [ ] **Step 4: Run GREEN plus Apple audio regression**

Run:

```powershell
cargo +stable test -p frd-shell-desktop audio_worker_opens_device_only_after_first_frame
cargo +stable test -p frd-shell-desktop audio_worker_ignores_video_until_first_pcm_frame
cargo +stable test -p frd-protocol-apple audio
```

Expected: all commands pass.

- [ ] **Step 5: Commit the shared correction**

Run:

```powershell
git add crates/frd-shell-desktop/src/application.rs
git commit -m "fix: open remote audio on first PCM frame"
```

---

### Task 6: Register RDP through the unchanged unified product UI

**Files:**
- Modify: `apps/freeremotedesk-windows/Cargo.toml`
- Modify: `apps/freeremotedesk-windows/src/main.rs`
- Modify: `apps/freeremotedesk-windows/tests/dependency_boundary.rs`
- Modify: `Cargo.toml`
- Modify: `crates/frd-ui-model/src/lib.rs`

**Interfaces:**
- Consumes: `AppleProtocolFactory`, `RdpProtocolFactory`, `ProtocolCatalog`, `DesktopApplication::new_product`, and the existing connection form.
- Produces: exactly two registered adapters; Mac automatic → Apple and Windows automatic → RDP.

- [ ] **Step 1: Add RED selection and dependency tests**

Port the assertions from `21cdb28` while preserving the chrome module:

```rust
#[test]
fn automatic_protocol_for_windows_is_resolved_before_submission() {
    // Register apple_hpss_mvs and rdp; assert Windows resolves to rdp.
}

#[test]
fn automatic_protocol_for_mac_remains_apple_with_rdp_registered() {
    // Register apple_hpss_mvs and rdp; assert Mac resolves to apple_hpss_mvs.
}
```

Update `dependency_boundary.rs` to expect `frd-protocol-apple` and
`frd-protocol-rdp` only in the Windows application composition root.

- [ ] **Step 2: Run RED**

Run:

```powershell
cargo +stable test -p frd-ui-model automatic_protocol
cargo +stable test -p freeremotedesk-windows --test dependency_boundary
```

Expected: Windows automatic selection and/or the two-factory boundary fails before registration.

- [ ] **Step 3: Register both factories only in `main.rs`**

Use the baseline `main.rs` and add:

```rust
let apple_factory = Arc::new(AppleProtocolFactory) as Arc<dyn ProtocolFactory>;
let rdp_factory = Arc::new(RdpProtocolFactory) as Arc<dyn ProtocolFactory>;
let factories = [apple_factory, rdp_factory];
let catalog = ProtocolCatalog::new(factories.iter().map(|factory| factory.descriptor().id));
```

Pass `factories` to `DesktopApplication::new_product`. Preserve the baseline
window icon, `WindowChromeFailed`, platform stores, audio factory, and proxy.

- [ ] **Step 4: Run GREEN and boundary checks**

Run:

```powershell
cargo +stable test -p frd-ui-model automatic_protocol
cargo +stable test -p freeremotedesk-windows --test dependency_boundary
rg -n "frd_protocol_(apple|rdp)" crates apps
```

Expected: tests pass and concrete imports are confined to the composition root and dedicated adapter crates/tests.

- [ ] **Step 5: Commit product registration**

Run:

```powershell
git add Cargo.toml apps/freeremotedesk-windows crates/frd-ui-model/src/lib.rs
cargo +stable generate-lockfile
git add Cargo.lock
git commit -m "feat: register RDP beside the Apple adapter"
```

---

### Task 7: Replay remaining RDP-internal capabilities and fixes

**Files:**
- Modify: `crates/frd-protocol-rdp/src/*.rs`
- Create: `crates/frd-protocol-rdp/src/{display,clipboard,audio}.rs`
- Modify: `README.md`
- Modify: `docs/validation/windows-native-rdp.md`
- Modify: RDP design/plan documents already transplanted in Task 3

**Interfaces:**
- Consumes: integrated RDP activation, graphics, HID input, composition root, and existing protocol-neutral viewport/clipboard/media ports.
- Produces: the reviewed RDP state through `a57032d`, while Windows live status remains `开发中`.

- [ ] **Step 1: Replay the remaining protocol-owned commits in order**

Run:

```powershell
$commits = @('3142d1f','50367e4','28e6c94','234f51a','5b9a4c8','01c8a5e','6994ea3','7b24dd3','a57032d')
foreach ($commit in $commits) { git cherry-pick $commit; if ($LASTEXITCODE -ne 0) { break } }
```

Resolve documentation conflicts by retaining the Mac/title-bar evidence and
adding the RDP rows. Resolve lockfile conflicts only by regenerating after final
manifests. Do not accept changes to Apple, renderer, compositor, app controller,
or title-bar files from these commits.

- [ ] **Step 2: Run focused RDP capability tests**

Run:

```powershell
cargo +stable test -p frd-protocol-rdp display
cargo +stable test -p frd-protocol-rdp clipboard
cargo +stable test -p frd-protocol-rdp audio
cargo +stable test -p frd-protocol-rdp lifecycle
cargo +stable test -p frd-protocol-rdp server_identity
```

Expected: all focused tests pass.

- [ ] **Step 3: Verify excluded features and product status**

Run:

```powershell
rg -n "gateway|smart.?card|printer|usb|rdpdr|audin|file.?contents" crates/frd-protocol-rdp
rg -n "Windows.*RDP|开发中|BLOCKED_LIVE" README.md docs/validation/windows-native-rdp.md
```

Expected: excluded capabilities are not enabled; documentation states the exact live blocker and does not claim Windows interoperability.

- [ ] **Step 4: Confirm Apple sources did not change in the RDP slice**

Run using the commit before Task 7 as `$task7Base`:

```powershell
git diff --name-only "$task7Base..HEAD" -- crates/frd-protocol-apple crates/frd-wire-rfb crates/frd-render-wgpu crates/frd-compositor-wgpu crates/frd-app
```

Expected: no output.

---

### Task 8: Run complete integration and bounded Mac compatibility gates

**Files:**
- Modify: `docs/validation/windows-apple-wgpu-parity.md`
- Modify: `docs/validation/windows-native-rdp.md` only to record fresh offline evidence
- Modify: `README.md` only if the platform matrix evidence date/path needs alignment

**Interfaces:**
- Consumes: complete integrated product release.
- Produces: reproducible offline evidence, fresh Mac live evidence, and an exact remaining Windows live blocker.

- [ ] **Step 1: Run the complete automated gate with normal parallelism**

Run:

```powershell
cargo +stable fmt --all -- --check
cargo +stable test --workspace
cargo +stable test --workspace --no-default-features
cargo +stable build --workspace --no-default-features
cargo +stable build -p freeremotedesk-windows --release
cargo +stable clippy -p freeremotedesk-windows -p frd-shell-desktop -p frd-ui-egui -p frd-ui-model -p frd-icon-assets -p frd-protocol-apple -p frd-protocol-rdp --all-targets --no-deps -- -D warnings
```

Expected: every command exits 0; explicitly ignored local-fixture tests remain documented rather than silently enabled or removed.

- [ ] **Step 2: Run static isolation checks**

Run:

```powershell
cargo +stable tree -p frd-protocol-rdp -e normal
rg -n "frd-protocol-apple|frd-wire-rfb|frd-shell-desktop|frd-platform-|winit|wgpu|egui" crates/frd-protocol-rdp/Cargo.toml crates/frd-protocol-rdp/src
git diff --check
```

Expected: no forbidden RDP dependency/import and no whitespace errors.

- [ ] **Step 3: Launch exactly one release client for the bounded Mac gate**

Load the authorized endpoint and credentials only from the ignored local
credential provider into the child environment; do not put secrets in argv,
source, logs, or this validation record. Confirm no existing
`freeremotedesk-windows` process before launch.

- [ ] **Step 4: Verify observable Mac behavior**

Verify in order:

1. Mac automatic selection resolves to Apple HPSS/MVS and authenticates.
2. The first complete frame has correct color and begins below the title bar.
3. One mouse click opens or changes a visible remote control.
4. One keyboard key changes a visible remote selection.
5. The change refreshes through the existing MVS session without reconnecting.
6. Moving outside or unfocusing remote content prevents further injection and releases held input.
7. Disconnect returns to the centered login form.
8. Closing the app leaves no client process.

- [ ] **Step 5: Record evidence without secrets**

Update `docs/validation/windows-apple-wgpu-parity.md` with the integration
commit, toolchain, command results, release size/SHA-256, and the eight bounded
observations. Update `docs/validation/windows-native-rdp.md` with fresh offline
commands/hash and retain `BLOCKED_LIVE`.

- [ ] **Step 6: Commit validation records**

Run:

```powershell
git add README.md docs/validation/windows-apple-wgpu-parity.md docs/validation/windows-native-rdp.md
git diff --cached --check
git commit -m "test: validate Mac compatibility after RDP integration"
```

---

### Task 9: Final review and move local main without losing user files

**Files:**
- No product source changes unless final review finds a concrete defect.
- Git refs: `main`, `codex/mac-baseline-rdp-integration`, and a recovery branch for old main.

**Interfaces:**
- Consumes: reviewed integration head and clean worktrees.
- Produces: local `main` at the integrated Mac-baseline/RDP result, with old main retained as a recovery ref.

- [ ] **Step 1: Run a final whole-branch review**

Review the range from the immutable Mac baseline to integration `HEAD` for spec
compliance, protocol isolation, keyboard mapping, audio behavior, documentation
accuracy, security, and unintended Apple/UI changes. Fix only validated
Critical/Important findings and re-run their focused gates.

- [ ] **Step 2: Verify the main worktree can change refs safely**

Run:

```powershell
git -C 'D:\FreeRemoteDesk' status --short
git -C 'D:\FreeRemoteDesk' diff --check
```

Expected: no tracked modifications. The four known top-level untracked user
font/image files may remain and must not be deleted or overwritten.

- [ ] **Step 3: Preserve old main and advance local main**

After removing the now-unused integration worktree or detaching it from the
integration branch, run from `D:\FreeRemoteDesk`:

```powershell
$oldMain = git rev-parse main
git branch codex/pre-mac-baseline-main-$($oldMain.Substring(0,7)) $oldMain
git switch --detach $oldMain
git branch --force main codex/mac-baseline-rdp-integration
git switch main
```

Expected: branch movement succeeds without touching the known untracked user
files. The recovery branch points to the old `a57032d` lineage.

- [ ] **Step 4: Verify final identity and cleanliness**

Run:

```powershell
git branch --show-current
git log -1 --oneline
git status --short
git rev-parse main
git rev-parse codex/mac-baseline-rdp-integration
```

Expected: current branch is `main`; both refs resolve to the integrated head;
only the preserved known top-level user files remain untracked.

- [ ] **Step 5: Do not push**

Report the new local main commit, recovery branch, complete verification
evidence, and the remaining Windows RDP live blocker. Wait for separate user
authorization before any `git push` or remote-main update.
