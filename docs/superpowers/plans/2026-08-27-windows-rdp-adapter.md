# Windows Native RDP Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a high-performance, independent Windows-to-Windows RDP adapter that connects to the stock Windows Remote Desktop service with NLA/TLS, publishes dirty rectangles rather than per-update full-frame copies, and consumes the common input/session contracts.

**Architecture:** `frd-protocol-rdp` drives IronRDP's public connector/session state machines directly. It deliberately does not embed `ironrdp-client::RdpOutputEvent::Image`, because IronRDP client 0.1.0 converts the entire decoded image into a new `Vec<u32>` for every `GraphicsUpdate`. The adapter retains IronRDP's `DecodedImage`, extracts only the reported `InclusiveRectangle` rows into one owned `PixelPatch`, and publishes them through `FrameMailbox`.

**Pinned upstream baseline (verified 2026-08-27):** `ironrdp` 0.17.0, `ironrdp-connector` 0.10, `ironrdp-session` 0.11, `ironrdp-input` 0.7, `ironrdp-pdu` 0.9, `ironrdp-tokio` 0.10, `ironrdp-tls` 0.2.2. API references: [IronRDP repository](https://github.com/Devolutions/IronRDP), [IronRDP architecture](https://github.com/Devolutions/IronRDP/blob/master/ARCHITECTURE.md), and the shipped `ironrdp` 0.17 `screenshot.rs` example.

**Prerequisite:** Complete `2026-08-27-windows-core-apple-wgpu.md` through Task 12. The RFB plan is independent and may be executed before or after this plan.

**Spec:** `docs/superpowers/specs/2026-08-27-winit-wgpu-windows-first-architecture-design.md`

## Boundaries

- Windows target `Auto` resolves to `org.freeremoteaccess.rdp`; neither Mac OS nor Linux may resolve to it.
- `frd-protocol-rdp` must not depend on Apple, RFB, winit, wgpu, egui, minifb, CPAL or a platform crate.
- Do not copy IronRDP's GUI, software renderer, CLI password argument, certificate-verification bypass or per-update full-frame conversion.
- The adapter owns its Tokio runtime, socket/TLS/CredSSP, IronRDP state machines, decoded image and input database.
- NLA/CredSSP is enabled by default. A server certificate that is neither chain-valid nor already pinned must pause in an explicit server-identity challenge; it is never silently accepted.
- This plan advertises RDP graphics, pointer, keyboard, Unicode text and display resize only. Clipboard and RDPSND remain false until separately integrated through platform/media ports.
- Actual credentials never appear in argv, logs, error strings or captures. Because upstream credential structs own ordinary `String` values, never clone the connector/config; drop it immediately when the session exits and document that upstream allocations are not guaranteed to be zeroized.
- Add only the connector, damage, input and lifecycle fixtures named here.

---

### Task 1: Pin IronRDP and create the independent factory

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Create: `crates/frd-protocol-rdp/Cargo.toml`
- Create: `crates/frd-protocol-rdp/src/lib.rs`
- Create: `crates/frd-protocol-rdp/src/factory.rs`
- Create: `crates/frd-protocol-rdp/src/config.rs`
- Create: `crates/frd-protocol-rdp/src/upstream.rs`

**Interfaces:**
- Consumes: protocol/session/core contracts.
- Produces: `RdpProtocolFactory`, descriptor `org.freeremoteaccess.rdp`, compile-checked upstream type aliases.

- [ ] **Step 1: Add exact dependencies and RED factory tests**

Pin the versions above, use only the features required for direct TLS/NLA graphics, and disable optional clipboard/sound/gateway/plugin features. Test the descriptor, Windows compatibility, default port 3389, required username/password/domain fields, and nonblocking factory construction.

- [ ] **Step 2: Compile the public upstream seam**

`upstream.rs` imports and type-checks the exact public types used by the adapter:

```rust
use ironrdp_connector::{ClientConnector, ConnectionResult, Credentials};
use ironrdp_input::{Database as InputDatabase, Operation};
use ironrdp_session::image::DecodedImage;
use ironrdp_session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput};
```

Do not expose these types outside `frd-protocol-rdp`.

- [ ] **Step 3: Run RED then GREEN**

Run: `cargo test -p frd-protocol-rdp factory`

Expected before implementation: missing factory failures. Expected after minimal implementation: PASS.

Run: `cargo tree -p frd-protocol-rdp -e normal`

Expected: IronRDP protocol crates present; Apple/RFB/UI/render/platform crates absent.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock crates/frd-protocol-rdp
git commit -m "feat: add independent IronRDP protocol factory"
```

### Task 2: Implement RDP certificate validation and identity challenges

**Files:**
- Create: `crates/frd-protocol-rdp/src/server_identity.rs`
- Modify: `crates/frd-protocol-rdp/src/config.rs`

**Interfaces:**
- Consumes: the generic `ServerIdentityChallenge`, saved-pin snapshot and decision contract from the core plan.
- Produces: RDP certificate-chain/pin validation and a credential-free preflight challenge.

- [ ] **Step 1: Write RED state-machine tests**

Test chain-valid acceptance, known endpoint/fingerprint pin acceptance, unknown self-signed challenge, Reject, cancellation while waiting, and a mismatched stored pin failing closed.

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-protocol-rdp server_identity`

- [ ] **Step 3: Implement chain and exact-pin validation**

Use native roots for ordinary chain validation. If a saved endpoint/protocol pin exists, require an exact fingerprint match. A pin mismatch fails immediately and cannot be overridden by a generic retry.

- [ ] **Step 4: Implement credential-free certificate preflight**

For an unknown invalid/self-signed certificate, perform a preflight TLS exchange that records the presented chain and intentionally aborts before CredSSP or any credential write. Emit the generic challenge and wait for its current decision. On approval, create a new TCP/TLS connection whose verifier accepts only the exact approved fingerprint; never continue the preflight connection.

- [ ] **Step 5: Run GREEN**

Run: `cargo test -p frd-protocol-rdp server_identity`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/frd-protocol-rdp
git commit -m "feat: validate RDP server identity"
```

### Task 3: Drive TLS, CredSSP and the IronRDP connector

**Files:**
- Create: `crates/frd-protocol-rdp/src/connection.rs`
- Create: `crates/frd-protocol-rdp/src/tls.rs`
- Create: `crates/frd-protocol-rdp/src/connector.rs`
- Create: `crates/frd-protocol-rdp/src/runtime.rs`

**Interfaces:**
- Consumes: RDP config, server identity decision, cancellation.
- Produces: `ConnectionResult` and upgraded framed transport owned by the adapter worker.

- [ ] **Step 1: Add a connector transcript fixture**

Use one bounded captured/mock transcript that reaches activation with NLA enabled and one certificate challenge cancellation case. Redact credentials and TLS secrets; do not commit session keys.

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-protocol-rdp connector`

- [ ] **Step 3: Build the connector config inside the worker**

Consume `SecretBuffer` only after the worker starts. Construct one `ironrdp_connector::Config` with `Credentials::UsernamePassword`, NLA/CredSSP enabled, TLS enabled, 32-bit bitmap support, current viewport size, Windows major platform and server pointer support. Never derive or call `Clone` on the containing adapter config.

- [ ] **Step 4: Implement direct transport and certificate validation**

Adapt the public sequence from IronRDP 0.17's `screenshot.rs`: `ClientConnector`, `connect_begin`, TLS upgrade, public-key extraction, `connect_finalize`. Replace its `NoCertificateVerification` example with native-root verification plus Task 2 pin/challenge handling. Disable TLS resumption as required by CredSSP. Bound DNS, connect, handshake and cancellation waits without inventing cross-protocol retry/fallback.

- [ ] **Step 5: Publish connection stages and clean exit**

Map DNS/TCP/TLS/CredSSP/activation to existing structured `ConnectionStage` values. On cancellation, close transport, drop connector/config, join the Tokio runtime and publish one `Closed`; never return to another protocol.

- [ ] **Step 6: Run GREEN**

Run: `cargo test -p frd-protocol-rdp connector`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/frd-protocol-rdp
git commit -m "feat: establish NLA protected RDP sessions"
```

### Task 4: Decode and publish dirty rectangles without full-frame copies

**Files:**
- Create: `crates/frd-protocol-rdp/src/active_session.rs`
- Create: `crates/frd-protocol-rdp/src/surface_publisher.rs`
- Create: `crates/frd-protocol-rdp/src/baseline.rs`
- Modify: `crates/frd-protocol-rdp/src/runtime.rs`

**Interfaces:**
- Consumes: `ActiveStageOutput`, `DecodedImage`.
- Produces: generation-bound RGBA/BGRX `SurfaceUpdate` patches.

- [ ] **Step 1: Write RED damage tests**

Use a 4×3 `DecodedImage` fixture and an inner 2×2 `InclusiveRectangle`. Verify row-by-row extraction produces only 16 pixel bytes, not the whole image or stride gaps. Add bounds rejection and baseline coverage tests.

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-protocol-rdp damage`

- [ ] **Step 3: Build and own the active stage**

Construct `ActiveStage` from `ConnectionResult` exactly once. On `ResponseFrame`, write through the adapter-owned writer. On `GraphicsUpdate(region)`, validate the region against `DecodedImage`, copy only its rows into one non-Clone `PixelBuffer`, publish `Damage`, then `FrameBoundary::Incremental`.

- [ ] **Step 4: Establish an exact baseline**

Call `begin_generation` with the negotiated desktop size before active graphics. Maintain a tile/coverage tracker for current-generation graphics regions. Publish `FullBaseline` only when the decoded canonical image has current-generation coverage for the whole surface; reset coverage on reactivation/resize. Do not apply Apple's nonblack diagnostic to RDP.

- [ ] **Step 5: Handle pointer outputs without contaminating framebuffer ownership**

Translate `PointerDefault`, `PointerHidden`, `PointerPosition` and `PointerBitmap` into protocol-neutral cursor events. The shell/compositor draws the cursor overlay; do not bake the pointer into every remote damage upload unless IronRDP negotiated software pointer rendering.

- [ ] **Step 6: Run GREEN and allocation check**

Run: `cargo test -p frd-protocol-rdp damage`

Expected: PASS.

Inspect the adapter path with `rg "Vec<u32>|\.data\(\).*collect|RdpOutputEvent::Image" crates/frd-protocol-rdp`; expected: no per-update full-frame conversion.

- [ ] **Step 7: Commit**

```bash
git add crates/frd-protocol-rdp
git commit -m "feat: publish IronRDP dirty rectangles"
```

### Task 5: Implement RDP input and display resize

**Files:**
- Create: `crates/frd-protocol-rdp/src/input.rs`
- Create: `crates/frd-protocol-rdp/src/writer.rs`
- Create: `crates/frd-protocol-rdp/src/display.rs`
- Modify: `crates/frd-protocol-rdp/src/active_session.rs`

**Interfaces:**
- Consumes: current `SessionInput`, viewport changes.
- Produces: IronRDP fast-path input and display-control messages.

- [ ] **Step 1: Write RED input tests**

Test scancode press/release, extended keys, Unicode text, pointer buttons/wheel/movement, ReleaseAll, stale generation rejection and resize generation pairing.

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-protocol-rdp input`

- [ ] **Step 3: Use `ironrdp_input::Database` for held-state correctness**

Translate normalized events into `Operation` values, call `Database::apply`, and send resulting `FastPathInputEvent` values through the active stage. `ReleaseAll` calls the database's release operation once. No winit key enum crosses into this crate.

- [ ] **Step 4: Add dynamic display control when negotiated**

Advertise dynamic resize only when IronRDP negotiates the display-control channel. Coalesce window changes, send the server layout, and commit a new application generation only after the RDP reactivation/desktop-size result. Before the new FullBaseline, stale input is rejected by both app and adapter.

- [ ] **Step 5: Run GREEN**

Run: `cargo test -p frd-protocol-rdp input`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/frd-protocol-rdp
git commit -m "feat: route input and resize through IronRDP"
```

### Task 6: Complete the RDP worker lifecycle

**Files:**
- Modify: `crates/frd-protocol-rdp/src/runtime.rs`
- Modify: `crates/frd-protocol-rdp/src/active_session.rs`
- Modify: `crates/frd-protocol-rdp/src/writer.rs`

- [ ] **Step 1: Write RED lifecycle tests**

Test normal disconnect, cancellation during DNS/TLS/NLA/active state, server DeactivateAll reactivation, mailbox overflow snapshot publication, and exactly one terminal event.

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-protocol-rdp lifecycle`

- [ ] **Step 3: Implement one adapter-owned Tokio runtime**

Create it inside `ProtocolSession::run`, not on the winit thread. Select over socket input, SessionCommand and cancellation. Keep exactly one ordered network writer. Do not use IronRDP's anti-idle fake mouse movement; remote input occurs only from current user/app events.

- [ ] **Step 4: Implement reactivation and shutdown**

On `DeactivateAll`, execute IronRDP's reactivation sequence, create the next surface generation, reset coverage and await FullBaseline. Shutdown stops accepting commands, sends graceful close when possible, releases held input, closes transport and runtime, then emits one `Closed`.

- [ ] **Step 5: Run GREEN and adapter suite**

Run: `cargo test -p frd-protocol-rdp lifecycle`

Run: `cargo test -p frd-protocol-rdp`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/frd-protocol-rdp
git commit -m "feat: complete bounded RDP session lifecycle"
```

### Task 7: Register RDP in the unified Windows login

**Files:**
- Modify: `apps/freeremotedesk-windows/Cargo.toml`
- Modify: `apps/freeremotedesk-windows/src/main.rs`
- Modify: `crates/frd-ui-model/src/lib.rs`
- Modify: `crates/frd-ui-egui/src/connection.rs`
- Modify: `apps/freeremotedesk-windows/tests/dependency_boundary.rs`

- [ ] **Step 1: Add RED catalog tests**

Test Windows/Auto resolution, explicit RDP selection, non-Windows rejection, domain/username/password field declaration, certificate challenge UI, and CLI prefill falling back to the same form when any field/provider is missing.

- [ ] **Step 2: Run RED**

Run: `cargo test -p freeremotedesk-windows rdp`

- [ ] **Step 3: Register only in the composition root**

Add `RdpProtocolFactory` beside Apple and RFB factories in `main.rs`. Concrete IronRDP types must not appear outside `frd-protocol-rdp`. The same `ConnectionDraft` and `AppIntent::Connect` path serves CLI prefill and manual GUI entry.

- [ ] **Step 4: Display truthful capabilities**

Enable graphics/input/text/dynamic resize only after negotiation. Keep clipboard and remote audio controls absent/disabled because they are not implemented in this plan.

- [ ] **Step 5: Run GREEN and graph checks**

Run: `cargo test -p freeremotedesk-windows rdp`

Run: `cargo test -p freeremotedesk-windows --test dependency_boundary`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add apps/freeremotedesk-windows crates/frd-ui-model crates/frd-ui-egui
git commit -m "feat: expose native RDP in unified Windows login"
```

### Task 8: Validate stock Windows RDP interoperability and performance

**Files:**
- Create: `docs/validation/windows-native-rdp.md`
- Modify only files proven necessary by observed failures.

- [ ] **Step 1: Run the offline matrix**

Run:

```bash
cargo fmt -- --check
cargo test -p frd-protocol-rdp -p freeremotedesk-windows
cargo build -p freeremotedesk-windows --release
```

- [ ] **Step 2: Run one authorized stock Windows target**

Verify NLA authentication, explicit certificate trust/pin behavior, first FullBaseline, incremental desktop refresh, correct colors, remote pointer, keyboard/text, focus-loss/pointer-outside release, dynamic resize when negotiated, disconnect and one-session enforcement.

- [ ] **Step 3: Measure the damage path**

Record remote dimensions, dirty-region dimensions and bytes copied for a small UI update. Confirm the adapter copies only reported rows and the renderer uploads only `PixelPatch` rectangles; no `Vec<u32>` full-frame conversion occurs per update.

- [ ] **Step 4: Record bounded evidence**

Write client/server OS versions, IronRDP versions, certificate decision path, negotiated capabilities, test commands and observed results. Exclude credentials, certificate private data and target secrets.

- [ ] **Step 5: Commit**

```bash
git add crates/frd-protocol-rdp apps/freeremotedesk-windows docs/validation/windows-native-rdp.md
git commit -m "feat: complete Windows native RDP interoperability"
```
