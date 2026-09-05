# RDP Platform Orthogonality and Windows Live Closure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep one platform-neutral IronRDP 0.17 protocol implementation, inject the client platform identity from each application composition root, restore the Windows package gate, and prepare a bounded stock-Windows interoperability run.

**Architecture:** `frd-protocol-rdp` continues to own every RDP wire and state-machine concern behind the existing protocol-neutral API. A small RDP protocol value is supplied to `RdpProtocolFactory` by the client application; no target-OS branch or concrete platform service enters the RDP crate. The current Windows composition root supplies the Windows identity, while future client platforms reuse the same factory interface.

**Tech Stack:** Rust 1.96.0, IronRDP 0.17.0, rustls 0.23, winit/egui/wgpu shell, Windows application composition root, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-09-05-rdp-platform-orthogonality-and-live-closure-design.md`

## Global Constraints

- `frd-protocol-rdp` must not depend on a concrete platform crate, winit, egui, wgpu, Apple protocol code, or RFB code.
- IronRDP types remain private to `frd-protocol-rdp`; only existing neutral commands, events, surfaces, media frames, clipboard payloads, capabilities, and stable errors cross its boundary.
- The RDP wire state machine is implemented once. Future client platforms inject platform identity and services; they do not fork RDP negotiation or decoding.
- Client platform identity is application configuration, not a login field, profile value, or server choice.
- Remain on pinned IronRDP 0.17.0. Do not enable RDPGFX, AVC420, AVC444, or an arbitrary upstream `main` snapshot.
- Do not add an unconditional Refresh Rectangle PDU while the server `refreshRectSupport` fact is unavailable.
- Do not change Apple factory behavior, HPSS/MVS, the shared renderer contract, or the protocol-neutral public command/event schema.
- Repair only deterministic test isolation; do not serialize the complete build or globally reduce test concurrency.
- Run focused core tests and the existing Apple/workspace regression gates. Do not add broad UI test suites.
- Windows-to-Windows remains `开发中` until a separate stock Windows target passes login, first frame, incremental refresh, input, and disconnect.

---

### Task 1: Make the Windows package panic-boundary tests deterministic

**Files:**
- Modify: `crates/frd-shell-desktop/src/video_decode_worker.rs`
- Test: `crates/frd-shell-desktop/src/video_decode_worker.rs`

**Interfaces:**
- Consumes: `VideoDecodeWorker`, `VideoRouter` test hooks, router/event retained stream maps.
- Produces: test-only `wait_for_retained_identity_counts(worker, expected)`; production decoder behavior is unchanged.

- [ ] **Step 1: Add the deterministic RED test**

Add a test that uses the existing `set_before_stream_finish` hook to stop a failed stream immediately before `finish_stream`. Assert that one router/event identity remains while the hook is blocked, release the hook, then call the not-yet-defined helper:

```rust
wait_for_retained_identity_counts(&worker, (0, 0));
assert_eq!(retained_identity_counts(&worker), (0, 0));
```

The test must cleanly release the hook and stop the worker even when an assertion fails; do not leave a thread waiting on a channel.

- [ ] **Step 2: Run RED**

Run:

```powershell
cargo test --locked -p frd-shell-desktop video_decode_worker::tests::retained_identity_wait_does_not_complete_before_finish_cleanup -- --exact --nocapture
```

Expected: compilation fails because `wait_for_retained_identity_counts` does not exist. Retain the current GitHub run `33942190561` as the independent pre-fix evidence that supervisor revision progress can precede retained-map cleanup.

- [ ] **Step 3: Implement the bounded condition wait**

Add a test-only helper with this signature:

```rust
fn wait_for_retained_identity_counts(
    worker: &VideoDecodeWorker,
    expected: (usize, usize),
)
```

It must use an absolute one-second deadline, repeatedly inspect both retained maps, and wait/yield only while the observed pair differs. On timeout it must report expected and actual counts. It must not modify production synchronization or sleep in production code.

Replace the `wait_for_supervisor_revisions(&worker, 2)` call immediately before the final retained-count assertion in `assert_panics_release_capacity` with the new condition wait. Do not remove supervisor-revision tests that independently validate supervisor progress.

- [ ] **Step 4: Run GREEN and concurrency regression**

Run:

```powershell
cargo test --locked -p frd-shell-desktop video_decode_worker::tests::retained_identity_wait_does_not_complete_before_finish_cleanup -- --exact --nocapture
cargo test --locked -p frd-shell-desktop video_decode_worker::tests::decoder_boundary_panic_hook_hides_secrets_and_forwards_unguarded_panics -- --exact --nocapture
cargo test --locked -p frd-shell-desktop video_decode_worker::tests::submit_panics_are_terminal_and_four_drained_identities_release_the_cap -- --exact --nocapture
1..10 | ForEach-Object { cargo test --locked -p frd-shell-desktop video_decode_worker::tests -- --test-threads=16 }
```

Expected: every command exits zero; the guarded child still emits only the fixed panic code/media markers and the capacity test ends at `(1, 1)`.

- [ ] **Step 5: Commit**

```powershell
git add crates/frd-shell-desktop/src/video_decode_worker.rs
git commit -m "test: stabilize decoder panic boundary cleanup"
```

---

### Task 2: Inject the RDP client platform identity

**Files:**
- Modify: `crates/frd-protocol-rdp/src/config.rs`
- Modify: `crates/frd-protocol-rdp/src/factory.rs`
- Modify: `crates/frd-protocol-rdp/src/connector.rs`
- Modify: `crates/frd-protocol-rdp/src/upstream.rs`
- Modify: `crates/frd-protocol-rdp/src/lib.rs`
- Modify: `crates/frd-protocol-rdp/src/runtime.rs`
- Modify: `apps/freeremotedesk-windows/src/main.rs`
- Modify: `apps/freeremotedesk-windows/tests/dependency_boundary.rs`
- Test: module-local RDP tests and Windows dependency-boundary tests

**Interfaces:**
- Consumes: `ConnectRequest`, `RdpProtocolFactory`, IronRDP `MajorPlatformType`, existing Windows composition root.
- Produces: public `RdpClientPlatformIdentity`; `RdpProtocolFactory::new(platform)`; `RdpConnectionConfig::try_new(request, platform)`; private exact mapping to IronRDP.

- [ ] **Step 1: Add RED API and mapping tests**

In `config.rs`, add tests that construct all currently approved protocol identities:

```rust
RdpClientPlatformIdentity::Windows
RdpClientPlatformIdentity::Macintosh
RdpClientPlatformIdentity::Ios
RdpClientPlatformIdentity::Unix
RdpClientPlatformIdentity::Android
```

In `connector.rs`, add one table-driven test asserting their exact private IronRDP mappings:

```text
Windows   -> MajorPlatformType::WINDOWS
Macintosh -> MajorPlatformType::MACINTOSH
Ios       -> MajorPlatformType::IOS
Unix      -> MajorPlatformType::UNIX
Android   -> MajorPlatformType::ANDROID
```

Update the factory test to require:

```rust
RdpProtocolFactory::new(RdpClientPlatformIdentity::Windows)
```

Update the dependency-boundary test to require that the Windows composition root uses exactly that constructor and to reject the old unit-struct registration.

- [ ] **Step 2: Run RED**

Run:

```powershell
cargo test --locked -p frd-protocol-rdp factory_exposes_stable_rdp_descriptor
cargo test --locked -p frd-protocol-rdp client_platform
cargo test --locked -p freeremotedesk-windows --test dependency_boundary
```

Expected: compilation or assertions fail because the injected type/constructors do not yet exist and the composition root still uses `RdpProtocolFactory` as a unit struct.

- [ ] **Step 3: Define the protocol-facing identity and consumed config**

In `config.rs`, add:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RdpClientPlatformIdentity {
    Windows,
    Macintosh,
    Ios,
    Unix,
    Android,
}
```

Store this value in `RdpConnectionConfig`. Replace implicit `TryFrom<ConnectRequest>` construction with:

```rust
pub fn try_new(
    request: ConnectRequest,
    client_platform: RdpClientPlatformIdentity,
) -> Result<Self, ProtocolError>
```

Keep credential inspection deferred to `take_connector_credentials`. Update all module-local test constructors and runtime fixtures to pass an explicit identity.

- [ ] **Step 4: Make the factory immutable and explicit**

In `factory.rs`, use:

```rust
pub struct RdpProtocolFactory {
    client_platform: RdpClientPlatformIdentity,
}

impl RdpProtocolFactory {
    pub const fn new(client_platform: RdpClientPlatformIdentity) -> Self {
        Self { client_platform }
    }
}
```

`create` must call `RdpConnectionConfig::try_new(request, self.client_platform)`. Do not add `Default`; a default would reintroduce an implicit platform choice.

- [ ] **Step 5: Move the IronRDP mapping behind the private seam**

In `upstream.rs`, add a crate-private mapper from `RdpClientPlatformIdentity` to `MajorPlatformType`. Pass the explicit value through both calls to `negotiate_enhanced_security` and into `baseline_connector`.

Delete every production `client_platform()` function and its target-OS `cfg` block from `connector.rs`. Test-only Windows socket conditionals in `writer.rs` remain allowed because they validate OS socket semantics and do not choose RDP wire capabilities.

- [ ] **Step 6: Inject Windows identity only at composition**

Export `RdpClientPlatformIdentity` from `lib.rs`. In `apps/freeremotedesk-windows/src/main.rs`, replace the unit factory with:

```rust
let rdp_factory = Arc::new(RdpProtocolFactory::new(
    RdpClientPlatformIdentity::Windows,
)) as Arc<dyn ProtocolFactory>;
```

Do not change the factory order, protocol picker, login fields, profile schema, default port, or Apple construction.

- [ ] **Step 7: Run GREEN and boundary audit**

Run:

```powershell
cargo test --locked -p frd-protocol-rdp
cargo test --locked -p freeremotedesk-windows --test dependency_boundary
cargo test --locked -p frd-ui-model automatic
rg -n "fn client_platform|cfg\(target_os|cfg\(windows" crates/frd-protocol-rdp/src/connector.rs
cargo tree -p frd-protocol-rdp -e normal
```

Expected: tests pass; the source scan has zero matches; the dependency tree has no concrete platform, Apple/RFB, winit, egui, or wgpu crate.

- [ ] **Step 8: Commit**

```powershell
git add crates/frd-protocol-rdp apps/freeremotedesk-windows
git commit -m "refactor: inject RDP client platform identity"
```

---

### Task 3: Refresh status documents and run the delivery gate

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-08-29-windows-native-rdp-design.md`
- Modify: `docs/superpowers/plans/2026-08-29-windows-rdp-adapter.md`
- Modify: `docs/validation/windows-native-rdp.md`
- Modify implementation only if a command below finds an evidence-backed regression

**Interfaces:**
- Consumes: Tasks 1-2 commits and current GitHub workflow definitions.
- Produces: current platform matrix, current offline evidence, release package, and an explicit live-validation blocker or result.

- [ ] **Step 1: Correct stale document status**

Update the 2026-08-29 design header from “implementation not yet started” to a historical implemented/offline-validated status and link the 2026-09-05 specification. Mark the historical plan header as Tasks 1-9 implemented with Task 10 blocked on an independent Windows target; do not mechanically rewrite every historical checkbox.

Update `docs/validation/windows-native-rdp.md` with:

- current branch/commit and IronRDP 0.17.0;
- actual post-change RDP/shell/dependency test counts;
- the explicit platform-identity injection evidence;
- the current traditional Bitmap/RemoteFX scope;
- no RDPGFX/AVC claim;
- the latest Windows package result;
- the exact independent-target blocker if no target is supplied.

Update the README Windows server row in the same change. Keep it `开发中` unless the live gate in Step 5 passes completely.

- [ ] **Step 2: Run formatting, protocol, shell, and composition tests**

```powershell
cargo fmt --all -- --check
cargo test --locked -p frd-protocol-rdp
cargo test --locked -p frd-shell-desktop
cargo test --locked -p freeremotedesk-windows --test dependency_boundary
cargo test --locked -p frd-ui-model -p frd-app
```

Expected: zero failures. Ignored tests must retain their existing fixture reasons.

- [ ] **Step 3: Run workspace and security-boundary checks**

```powershell
cargo check --locked --workspace --all-targets
cargo test --locked
cargo tree -p frd-protocol-rdp -e normal
rg -n "NoCertificateVerification|danger_accept_invalid_certs|SSLKEYLOGFILE|ClearTextPassword|--password" crates/frd-protocol-rdp apps/freeremotedesk-windows
git diff --check
```

Expected: builds/tests pass; secret scan matches only controlled negative tests or provider option names, never a literal password path.

- [ ] **Step 4: Build and verify the Windows package**

```powershell
cargo build --locked --release -p freeremotedesk-windows
pwsh -NoProfile -File tools/stage-windows-package.ps1 -PackageRoot target/package-rdp-orthogonality
pwsh -NoProfile -File tools/verify-windows-package.ps1 -PackageRoot target/package-rdp-orthogonality
```

Run Windows PowerShell 5.1 with Pester 3.4.0 against `tools/tests/windows-package.Tests.ps1`. Expected: all package tests pass and the staged package contains only the approved executable, FFmpeg codec bundle, manifest, and license files.

- [ ] **Step 5: Run the bounded live gate only with a separate target**

Before connecting, confirm the target is not the current Codex host and that the user authorized it. Use the existing GUI and secure store; never place the password on argv. Verify in order:

```text
certificate decision -> NLA -> activation -> FullBaseline present
-> incremental refresh -> pointer -> keyboard -> both wheel axes
-> focus-loss ReleaseAll -> explicit disconnect -> login page
-> known-pin reconnect
```

Record the target/client OS versions, negotiated desktop size, observed legacy codec path, duration, and failures without recording host credentials or certificate bodies. If no separate target exists, record `BLOCKED_LIVE`; do not run localhost RDP.

- [ ] **Step 6: Commit documentation and evidence**

```powershell
git add README.md docs/superpowers/specs/2026-08-29-windows-native-rdp-design.md docs/superpowers/plans/2026-08-29-windows-rdp-adapter.md docs/validation/windows-native-rdp.md
git commit -m "docs: refresh Windows RDP validation status"
```

- [ ] **Step 7: Push the feature branch and monitor both workflows**

```powershell
git push -u origin codex/rdp-platform-orthogonality
gh run list --repo sunanxin18/FreeRemoteAccess --branch codex/rdp-platform-orthogonality
```

Wait for both `CI` and `Windows package` to reach terminal success. If either fails, read the exact failing log, repair only the evidence-backed cause, repeat its focused local gate, and push the fix. Do not merge or overwrite `main` without a new explicit user request.
