# Windows Native RDP Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a secure Windows-to-Windows RDP client path that logs into stock Windows Remote Desktop Services, displays the live desktop through the existing wgpu path, and sends keyboard and pointer input without changing Apple HPSS/MVS behavior.

**Architecture:** A new `frd-protocol-rdp` crate drives IronRDP 0.17.0 behind the existing `ProtocolFactory`, `ProtocolSession`, `ProtocolRuntime`, `SurfaceUpdate`, and `SessionInput` contracts. Only the Windows composition root imports the concrete adapter. IronRDP types, TLS/CredSSP state, decoded images, input state, and virtual channels remain private to the RDP crate.

**Tech Stack:** Rust 1.96.0, IronRDP 0.17.0, rustls 0.23, rustls-platform-verifier 0.7.0, Tokio, winit, egui, wgpu, Windows Credential Manager/DPAPI.

**Spec:** `docs/superpowers/specs/2026-08-29-windows-native-rdp-design.md`

## Global Constraints

- Support Windows 10/11 Pro or Enterprise and Windows Server 2016 or newer.
- Require TLS and CredSSP/NLA; do not implement Standard RDP Security fallback.
- Keep `ProtocolId::rdp()` equal to `"rdp"` and use default port 3389.
- Accept `user`, `DOMAIN\user`, and `user@domain` through the existing username field.
- Do not add a new public feature schema, advanced RDP settings page, title-bar group, or protocol-specific UI control.
- Do not claim Caps Lock, Num Lock, or Scroll Lock synchronization: the current protocol-neutral `Modifiers` contract has no lock bits. Keep physical modifier keys unchanged and defer lock-state synchronization to a future approved public input-contract task.
- Do not depend on `frd-protocol-apple`, `frd-wire-rfb`, winit, wgpu, egui, minifb, or a platform crate from `frd-protocol-rdp`.
- Do not copy IronRDP's permissive TLS verifier, viewer, CLI password argument, server, software renderer, or per-update full-frame output conversion.
- Validate server identity before any CredSSP credential write. Only an untrusted issuer with an otherwise valid leaf uses the existing challenge; a saved-pin mismatch and every other certificate failure fail closed.
- Do not place credentials, certificate bodies, TLS secrets, target secrets, or raw upstream errors in argv, configuration, logs, captures, fixtures, or UI strings.
- Publish only `PixelFormat::Bgrx8UnormSrgb` and current generation-bound `SurfaceUpdate` values.
- Return one `ProtocolExit`; the shell remains the only producer of `SessionEvent::Closed`.
- Run focused core protocol tests only. Keep all existing Apple authentication, MVS, generation, input, media, and presentation tests green.
- Update `README.md` in the same change whenever implementation or live-validation status changes.

## File Map

```text
crates/frd-protocol-rdp/
  Cargo.toml             exact upstream and neutral dependencies
  src/lib.rs             public RdpProtocolFactory export
  src/factory.rs         descriptor and nonblocking construction
  src/config.rs          consumed connection configuration and username parsing
  src/upstream.rs        private IronRDP API seam
  src/server_identity.rs certificate classification and decisions
  src/tls.rs             preflight and verified TLS transport
  src/connector.rs       negotiation, CredSSP, licensing, activation
  src/runtime.rs         socket/command loop and shutdown
  src/active_session.rs  IronRDP active stage and reactivation
  src/surface.rs         dirty rectangle to BGRX patches
  src/baseline.rs        generation coverage and full snapshot recovery
  src/input.rs           SessionInput to IronRDP input operations
  src/writer.rs          one ordered network writer
  src/display.rs         existing ViewportChanged to Display Control
  src/clipboard.rs       existing text clipboard contract
  src/audio.rs           RDPSND to existing MediaFrame contract
  src/error.rs           stable credential-free error mapping
```

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
- Test: `crates/frd-protocol-rdp/src/factory.rs`

**Interfaces:**
- Consumes: `ProtocolFactory`, `ConnectRequest`, `ProtocolRuntime`, `ProtocolId::rdp()`.
- Produces: `RdpProtocolFactory`, `RdpProtocolSession`, `RdpConnectionConfig`, and a compile-checked private IronRDP seam.

- [ ] **Step 1: Repair and verify the pinned Rust toolchain**

Run:

```powershell
rustup toolchain install 1.96.0 --profile minimal --component rustfmt
rustc --version
cargo --version
```

Expected: both tools report 1.96.0. A missing-manifest error is a failed prerequisite, not an RDP test failure.

- [ ] **Step 2: Add RED factory tests**

Add tests that assert the stable descriptor and nonblocking construction:

```rust
#[test]
fn factory_exposes_stable_rdp_descriptor() {
    let descriptor = RdpProtocolFactory.descriptor();
    assert_eq!(descriptor.id, ProtocolId::rdp());
    assert_eq!(descriptor.default_port, 3389);
    assert!(descriptor.credential_requirements.username);
    assert!(descriptor.credential_requirements.password);
}

#[test]
fn username_parser_accepts_local_domain_and_upn_forms() {
    assert_eq!(ParsedUsername::parse("alice").unwrap().account(), "alice");
    assert_eq!(ParsedUsername::parse("ACME\\alice").unwrap().domain(), Some("ACME"));
    assert_eq!(ParsedUsername::parse("alice@acme.test").unwrap().upn(), Some("alice@acme.test"));
}
```

- [ ] **Step 3: Run the RED test**

Run:

```powershell
cargo test -p frd-protocol-rdp factory
```

Expected: fail because the package/types do not exist.

- [ ] **Step 4: Add exact dependencies and the minimal factory**

Add the workspace member and pin:

```toml
ironrdp = { version = "=0.17.0", default-features = false, features = ["connector", "session", "graphics", "input"] }
rustls = { version = "0.23", default-features = false, features = ["std", "tls12", "aws_lc_rs"] }
rustls-platform-verifier = "=0.7.0"
tokio = { version = "1", features = ["io-util", "net", "rt", "sync", "time"] }
sha2 = "0.10"
zeroize = "1"
```

Implement the public shape:

```rust
pub struct RdpProtocolFactory;

impl ProtocolFactory for RdpProtocolFactory {
    fn descriptor(&self) -> ProtocolDescriptor {
        ProtocolDescriptor::from(ProtocolId::rdp())
    }

    fn create(
        &self,
        request: ConnectRequest,
        runtime: ProtocolRuntime,
    ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
        let config = RdpConnectionConfig::try_from(request)?;
        Ok(Box::new(RdpProtocolSession { config, runtime }))
    }
}
```

`create` may parse and move values but must not resolve DNS, connect, block, or spawn.

- [ ] **Step 5: Compile-check the upstream seam**

`upstream.rs` privately imports the exact 0.17.0 types used by later tasks, including `ClientConnector`, `ConnectionResult`, `DecodedImage`, `ActiveStageBuilder`, `ActiveStageOutput`, and `ironrdp_input::Database`. Do not re-export them.

- [ ] **Step 6: Run GREEN and dependency inspection**

Run:

```powershell
cargo test -p frd-protocol-rdp factory
cargo tree -p frd-protocol-rdp -e normal
```

Expected: tests pass; no Apple, RFB, UI, renderer, shell, or platform crate appears under the RDP package.

- [ ] **Step 7: Commit**

```powershell
git add Cargo.toml Cargo.lock crates/frd-protocol-rdp
git commit -m "feat: add independent IronRDP protocol factory"
```

### Task 2: Implement credential-free TLS identity verification

**Files:**
- Create: `crates/frd-protocol-rdp/src/server_identity.rs`
- Create: `crates/frd-protocol-rdp/src/tls.rs`
- Create: `crates/frd-protocol-rdp/src/error.rs`
- Modify: `crates/frd-protocol-rdp/src/lib.rs`
- Test: `crates/frd-protocol-rdp/src/server_identity.rs`

**Interfaces:**
- Consumes: endpoint, `saved_server_pin`, `ResolveServerIdentity`, and current session ID.
- Produces: `VerifiedTlsTransport` or stable `ProtocolError::adapter(ProtocolId::rdp(), code)`.

- [ ] **Step 1: Add RED identity-policy tests**

Define and test this internal decision type:

```rust
enum IdentityDisposition {
    SystemTrusted { fingerprint: [u8; 32] },
    PinMatched { fingerprint: [u8; 32] },
    Challenge { fingerprint: [u8; 32], subject: String, issuer: String },
    PinMismatch,
}
```

Tests must cover system trust, exact saved pin, untrusted-issuer challenge, pin mismatch, Reject, stale challenge ID, Disconnect while waiting, bounded sanitized subject/issuer text, and deterministic wrong-host, expired, not-yet-valid, invalid-EKU, and malformed/non-issuer failures. Only the untrusted-issuer class may reach TrustOnce or TrustAndRemember.

- [ ] **Step 2: Run RED**

```powershell
cargo test -p frd-protocol-rdp server_identity
```

Expected: fail because policy and transport are absent.

- [ ] **Step 3: Implement certificate fingerprint and classification**

Use SHA-256 of the complete leaf DER bytes and preserve the rustls certificate-failure class. Exact saved-pin comparison happens before presenting a new challenge, but a matching pin overrides only `UnknownIssuer`; server name, validity, server-auth/EKU, malformed, revoked, and other non-issuer failures remain fail-closed. A mismatch returns `rdp_server_identity_changed` and cannot be overridden by retry, including a leaf change between preflight and the verified reconnect.

```rust
fn fingerprint_sha256(leaf_der: &[u8]) -> [u8; 32];

fn classify_identity(
    saved_pin: Option<[u8; 32]>,
    fingerprint: [u8; 32],
    platform_validation: Result<(), PlatformValidationFailure>,
    names: SanitizedCertificateNames,
) -> IdentityDisposition;
```

- [ ] **Step 4: Implement the credential-free preflight**

The preflight verifier may capture the presented chain only in a transport state that cannot call the RDP connector or access credentials. It completes the TLS handshake, records the leaf and platform failure class, performs no application write, and closes. Before treating `UnknownIssuer` as challengeable, independently validate server name, validity, and server-auth/EKU so a chain-policy precedence result cannot hide a non-overridable leaf defect.

The second connection uses either:

- `rustls_platform_verifier::ConfigVerifierExt::with_platform_verifier()` for ordinary system trust and hostname verification; or
- an exact-pin verifier that checks the complete leaf fingerprint and delegates TLS 1.2/1.3 handshake-signature verification to the configured rustls crypto provider.

Do not use IronRDP `NoCertificateVerification`, `danger_accept_invalid_certs(true)`, or an assertion-only handshake-signature verifier.

- [ ] **Step 5: Publish and resolve the existing generic challenge**

Publish one `ServerIdentityChallenge` with protocol `rdp`, endpoint, fingerprint, sanitized names, and `Unknown` validation. Poll only the current session's commands until TrustOnce, TrustAndRemember, Reject, or Disconnect. Persistence remains owned by `frd-app`; the adapter receives only the decision.

- [ ] **Step 6: Run GREEN and secret scan**

```powershell
cargo test -p frd-protocol-rdp server_identity
rg -n "NoCertificateVerification|danger_accept_invalid_certs|SSLKEYLOGFILE|password.*debug" crates/frd-protocol-rdp
```

Expected: tests pass; none of the prohibited patterns exist.

- [ ] **Step 7: Commit**

```powershell
git add crates/frd-protocol-rdp
git commit -m "feat: verify RDP server identity before credentials"
```

### Task 3: Establish TLS, CredSSP/NLA, licensing, and activation

**Files:**
- Create: `crates/frd-protocol-rdp/src/connector.rs`
- Create: `crates/frd-protocol-rdp/src/runtime.rs`
- Modify: `crates/frd-protocol-rdp/src/factory.rs`
- Modify: `crates/frd-protocol-rdp/src/config.rs`
- Modify: `crates/frd-protocol-rdp/src/error.rs`
- Test: `crates/frd-protocol-rdp/src/connector.rs`

**Interfaces:**
- Consumes: `VerifiedTlsTransport`, parsed credentials, endpoint, initial desktop size, and `ProtocolRuntime`.
- Produces: activated IronRDP `ConnectionResult` and an owned ordered transport.

- [ ] **Step 1: Add RED connector configuration tests**

Test that the connector requires enhanced RDP security, enables TLS/CredSSP, requests 32-bit graphics and server pointer support, uses one primary monitor, and never enables Standard RDP Security, clipboard, sound, device, gateway, or COM plugin channels in the baseline.

- [ ] **Step 2: Run RED**

```powershell
cargo test -p frd-protocol-rdp connector
```

Expected: fail because connector configuration is missing.

- [ ] **Step 3: Build connector configuration inside the worker**

Move the password out of the adapter config only inside `ProtocolSession::run`. Construct IronRDP username/password credentials once, without cloning the enclosing config. Configure the client desktop size from the current product viewport or the conservative default 1280x720.

```rust
struct ActivatedRdpSession {
    connection: ConnectionResult,
    transport: VerifiedTlsTransport,
}

async fn connect_and_activate(
    config: &mut RdpConnectionConfig,
    runtime: &mut ProtocolRuntime,
) -> Result<ActivatedRdpSession, ProtocolError>;
```

- [ ] **Step 4: Drive the official 0.17.0 sequence**

Adapt the state-machine order from the tagged `ironrdp/examples/screenshot.rs`: X.224 negotiation, verified TLS upgrade, CredSSP/NLA, MCS/capability exchange, licensing, finalization, and activation. Replace its TLS handling with Task 2. Map failures to fixed codes:

```text
rdp_dns_failed
rdp_tcp_failed
rdp_tls_failed
rdp_nla_failed
rdp_logon_failed
rdp_license_failed
rdp_activation_failed
```

Never include the upstream error string in the public error.

- [ ] **Step 5: Publish truthful connection stages**

Publish `Connecting` before network work and `TransportReady` only after activation. Do not publish capabilities or begin a surface generation before activation has returned the negotiated desktop geometry.

- [ ] **Step 6: Make every stage cancellable**

The adapter-owned Tokio current-thread runtime selects bounded network/time work with repeated checks of `runtime.try_next_command()`. Disconnect closes transport and returns `ProtocolExit::Closed`; a fatal protocol error returns `ProtocolExit::Failed` immediately.

- [ ] **Step 7: Run GREEN**

```powershell
cargo test -p frd-protocol-rdp connector
cargo test -p frd-protocol-rdp lifecycle
```

Expected: connector and cancellation tests pass.

- [ ] **Step 8: Commit**

```powershell
git add crates/frd-protocol-rdp
git commit -m "feat: activate NLA-protected RDP sessions"
```

### Task 4: Publish traditional RDP graphics as bounded BGRX damage

**Files:**
- Create: `crates/frd-protocol-rdp/src/active_session.rs`
- Create: `crates/frd-protocol-rdp/src/surface.rs`
- Create: `crates/frd-protocol-rdp/src/baseline.rs`
- Modify: `crates/frd-protocol-rdp/src/runtime.rs`
- Test: `crates/frd-protocol-rdp/src/surface.rs`
- Test: `crates/frd-protocol-rdp/src/baseline.rs`

**Interfaces:**
- Consumes: activated `ConnectionResult`, IronRDP `DecodedImage`, and graphics regions.
- Produces: current generation `Reset`, BGRX `Damage`, and one boundary per revision.

- [ ] **Step 1: Add RED dirty-rectangle tests**

Use a 4x3 decoded image with distinct B/G/R values and a 2x2 inner update. Assert only 16 bytes are copied, the output rect and stride are exact, X bytes are 255, out-of-bounds inclusive rectangles are rejected, and no full-image allocation occurs for the inner update.

- [ ] **Step 2: Add RED baseline tests**

Test incomplete coverage, overlapping regions, exact full coverage, new generation reset, stale generation rejection, and a mailbox recovery split into bounded patches with only the final boundary marked `FullBaseline`.

- [ ] **Step 3: Run RED**

```powershell
cargo test -p frd-protocol-rdp surface
cargo test -p frd-protocol-rdp baseline
```

- [ ] **Step 4: Begin the negotiated surface generation**

After activation, validate nonzero negotiated dimensions against mailbox and renderer budgets and call:

```rust
runtime.begin_generation(
    session_id,
    1,
    negotiated_size,
    PixelFormat::Bgrx8UnormSrgb,
)?;
```

The adapter owns `DecodedImage` as its canonical current surface.

- [ ] **Step 5: Extract and publish only changed rows**

Implement:

```rust
fn extract_bgrx_patch(
    image: &DecodedImage,
    region: InclusiveRectangle,
) -> Result<PixelPatch, RdpSurfaceError>;
```

For each IronRDP graphics update, publish `Damage`, update coverage, then publish exactly one `FrameBoundary`. Do not use `ironrdp-client::RdpOutputEvent::Image` or collect the whole image into `Vec<u32>`.

- [ ] **Step 6: Recover from mailbox backpressure**

Treat `ProtocolError::NeedsFullSnapshot` as a recoverable request. Rebuild bounded patches from the adapter-owned canonical image and finish with a new `FullBaseline`. Other frame-port errors remain terminal.

- [ ] **Step 7: Run GREEN and allocation grep**

```powershell
cargo test -p frd-protocol-rdp surface
cargo test -p frd-protocol-rdp baseline
rg -n "RdpOutputEvent::Image|Vec<u32>|collect::<Vec<u32>>" crates/frd-protocol-rdp
```

Expected: tests pass and no per-update full-frame conversion exists.

- [ ] **Step 8: Commit**

```powershell
git add crates/frd-protocol-rdp
git commit -m "feat: publish bounded RDP desktop damage"
```

### Task 5: Route keyboard, pointer, and lifecycle events

**Files:**
- Create: `crates/frd-protocol-rdp/src/input.rs`
- Create: `crates/frd-protocol-rdp/src/writer.rs`
- Modify: `crates/frd-protocol-rdp/src/active_session.rs`
- Modify: `crates/frd-protocol-rdp/src/runtime.rs`
- Test: `crates/frd-protocol-rdp/src/input.rs`
- Test: `crates/frd-protocol-rdp/src/runtime.rs`

**Interfaces:**
- Consumes: current generation `SessionInput` and `Disconnect`.
- Produces: ordered IronRDP fast-path input, release events, reactivation, and one `ProtocolExit`.

- [ ] **Step 1: Add RED input tests**

Cover scan-code press/release, E0 extended keys, physical modifier keys, Unicode text, pointer movement, left/middle/right/X1/X2 buttons, vertical/horizontal wheel, stale generation rejection, and `ReleaseAll` after focus loss/disconnect. Lock-state synchronization is not part of this task's acceptance and must not add public schema.

- [ ] **Step 2: Run RED**

```powershell
cargo test -p frd-protocol-rdp input
```

- [ ] **Step 3: Translate through IronRDP input state**

Own one `ironrdp_input::Database` per session and translate normalized events into `Operation` values. Apply each operation once and enqueue the resulting fast-path events through the single writer. No winit key type enters the RDP crate.

```rust
fn translate_input(
    database: &mut ironrdp_input::Database,
    event: InputEvent,
) -> Result<Vec<FastPathInputEvent>, RdpInputError>;
```

- [ ] **Step 4: Complete the active loop and reactivation**

Select socket input and current commands. Feed server PDUs to `ActiveStage`, write every `ResponseFrame` through the ordered writer, publish graphics outputs, and process DeactivateAll through IronRDP reactivation. A reactivated desktop starts a new generation and empty coverage state.

- [ ] **Step 5: Implement shutdown invariants**

On Disconnect or fatal failure: stop accepting input, release held input when transport is writable, close transport, drop credential/config allocations, stop Tokio, and return one `ProtocolExit`. Do not publish `Closed` from the adapter and do not send anti-idle pointer movement.

- [ ] **Step 6: Run GREEN**

```powershell
cargo test -p frd-protocol-rdp input
cargo test -p frd-protocol-rdp lifecycle
cargo test -p frd-protocol-rdp
```

- [ ] **Step 7: Commit**

```powershell
git add crates/frd-protocol-rdp
git commit -m "feat: route RDP input and session lifecycle"
```

### Task 6: Prevent no-media RDP sessions from affecting Apple audio

**Files:**
- Modify: `crates/frd-shell-desktop/src/application.rs`
- Test: `crates/frd-shell-desktop/src/application.rs`

**Interfaces:**
- Consumes: the existing media receiver and `AudioOutputFactory`.
- Produces: lazy platform audio open on the first media frame without protocol-specific branching.

- [ ] **Step 1: Add the RED lazy-audio test**

Create a protocol fixture that publishes no media and assert the audio factory open count remains zero through launch and disconnect. Create a second fixture that publishes one valid media frame and assert open occurs exactly once after that frame.

- [ ] **Step 2: Run RED**

```powershell
cargo test -p frd-shell-desktop audio_worker_opens_device_only_after_first_frame
```

Expected: no-media case fails because the current worker opens immediately.

- [ ] **Step 3: Move device open behind the first frame**

Change `run_audio_worker` so it blocks on the media receiver first, then opens the platform output and processes the first and following frames. The worker must exit quietly if the sender closes before a frame.

```rust
let first = match media.recv() {
    Ok(frame) => frame,
    Err(_) => return AudioWorkerExit::Closed,
};
let Ok(mut output) = factory.open() else {
    return AudioWorkerExit::Failed;
};
if let MediaFrame::Pcm {
    sample_rate_hz,
    channels,
    samples,
} = first
{
    if output
        .enqueue_pcm(sample_rate_hz, channels, samples)
        .is_err()
    {
        return AudioWorkerExit::Failed;
    }
}
```

- [ ] **Step 4: Run GREEN and Apple audio regression**

```powershell
cargo test -p frd-shell-desktop audio_worker_opens_device_only_after_first_frame
cargo test -p frd-protocol-apple audio
```

- [ ] **Step 5: Commit**

```powershell
git add crates/frd-shell-desktop/src/application.rs
git commit -m "fix: open remote audio output on first media frame"
```

### Task 7: Register RDP in the unified Windows product

**Files:**
- Modify: `apps/freeremotedesk-windows/Cargo.toml`
- Modify: `apps/freeremotedesk-windows/src/main.rs`
- Modify: `apps/freeremotedesk-windows/tests/dependency_boundary.rs`
- Test: `crates/frd-ui-model/src/lib.rs`

**Interfaces:**
- Consumes: `RdpProtocolFactory` and the existing dynamic protocol catalog/login form.
- Produces: Windows/Automatic and explicit RDP launch through the same `AppIntent::Connect` path as Apple.

- [ ] **Step 1: Add RED composition tests**

Update the dependency test to require exactly two concrete product dependencies, sorted as `frd-protocol-apple` and `frd-protocol-rdp`, while still requiring that only `apps/freeremotedesk-windows/src/main.rs` imports concrete adapters. Add UI-model coverage for Windows/Automatic resolving to RDP and Mac OS/Automatic remaining Apple.

- [ ] **Step 2: Run RED**

```powershell
cargo test -p freeremotedesk-windows --test dependency_boundary
cargo test -p frd-ui-model automatic
```

- [ ] **Step 3: Register both factories only in `main.rs`**

Use separate values and one protocol-neutral vector:

```rust
let apple_factory = Arc::new(AppleProtocolFactory) as Arc<dyn ProtocolFactory>;
let rdp_factory = Arc::new(RdpProtocolFactory) as Arc<dyn ProtocolFactory>;
let factories = [apple_factory, rdp_factory];
let catalog = ProtocolCatalog::new(
    factories.iter().map(|factory| factory.descriptor().id),
);
```

Pass the same factories to `DesktopApplication::new_product`. Do not add an RDP-specific page, field, status bar, title-bar button, or fallback.

- [ ] **Step 4: Publish only negotiated current capabilities**

Baseline RDP publishes `text_input=true` after active input is available and leaves `dynamic_resolution`, clipboard read/write, and remote audio false. Apple capability publication remains unchanged.

- [ ] **Step 5: Run GREEN**

```powershell
cargo test -p freeremotedesk-windows --test dependency_boundary
cargo test -p frd-ui-model
cargo test -p frd-app
```

- [ ] **Step 6: Commit**

```powershell
git add apps/freeremotedesk-windows crates/frd-ui-model
git commit -m "feat: expose RDP through the unified Windows login"
```

### Task 8: Adapt only existing optional capabilities

**Files:**
- Create: `crates/frd-protocol-rdp/src/display.rs`
- Create: `crates/frd-protocol-rdp/src/clipboard.rs`
- Create: `crates/frd-protocol-rdp/src/audio.rs`
- Modify: `crates/frd-protocol-rdp/src/active_session.rs`
- Modify: `crates/frd-protocol-rdp/src/runtime.rs`
- Test: corresponding module-local test modules

**Interfaces:**
- Consumes: existing `ViewportChanged`, `ClipboardWrite`, clipboard events, `MediaFrame`, and fixed `SessionCapabilities` fields.
- Produces: Display Control, CLIPRDR text, and RDPSND only when negotiated; no new public API or UI.

- [ ] **Step 1: Add focused RED capability tests**

Test that an unnegotiated channel leaves its current capability false and rejects or ignores its command without affecting graphics. Test Display Control confirmation before generation change, text-only CLIPRDR read/write, and RDPSND conversion into the existing media format.

- [ ] **Step 2: Run RED**

```powershell
cargo test -p frd-protocol-rdp display
cargo test -p frd-protocol-rdp clipboard
cargo test -p frd-protocol-rdp audio
```

- [ ] **Step 3: Implement Display Control through the existing viewport command**

Advertise `dynamic_resolution=true` only after the server channel reports support. Coalesce viewport requests and create a new generation only after the server confirms/reactivates at the requested size. No multi-monitor or RDP-specific layout control is added.

- [ ] **Step 4: Implement text-only CLIPRDR**

Map only Unicode text formats to `ClipboardPayload`. Leave file descriptors and file contents disabled. Publish clipboard read/write capabilities independently from actual negotiated direction.

- [ ] **Step 5: Implement RDPSND through the existing media port**

Decode/normalize negotiated audio into the existing `MediaFrame` contract and use `try_publish_optional_media`; media backpressure stops/degrades audio without terminating graphics. Do not import or call platform audio APIs.

- [ ] **Step 6: Run GREEN and absence checks**

```powershell
cargo test -p frd-protocol-rdp display
cargo test -p frd-protocol-rdp clipboard
cargo test -p frd-protocol-rdp audio
rg -n "gateway|smart.?card|printer|usb|rdpdr|audin|file.?contents" crates/frd-protocol-rdp
```

Expected: only deliberate comments/negative tests may match excluded features; no implementation or UI bridge exists.

- [ ] **Step 7: Commit**

```powershell
git add crates/frd-protocol-rdp
git commit -m "feat: adapt existing RDP display clipboard and audio ports"
```

### Task 9: Complete offline verification and Windows release build

**Files:**
- Modify: `README.md`
- Create: `docs/validation/windows-native-rdp.md`
- Modify only implementation files proven necessary by failures

**Interfaces:**
- Consumes: the complete adapter and product composition.
- Produces: a release executable and evidence that distinguishes tests/build from live interoperability.

- [ ] **Step 1: Run focused formatting and adapter tests**

```powershell
cargo fmt -- --check
cargo test -p frd-protocol-rdp
cargo test -p frd-shell-desktop
cargo test -p freeremotedesk-windows --test dependency_boundary
```

- [ ] **Step 2: Run the complete workspace regression gate**

```powershell
cargo test --workspace
cargo test --workspace --no-default-features
```

Expected: all Apple and neutral tests remain green. Do not reduce the workspace gate to get RDP passing.

- [ ] **Step 3: Build the product configurations**

```powershell
cargo build -p freeremotedesk-windows --release
cargo build --no-default-features
```

- [ ] **Step 4: Audit dependency and secret boundaries**

```powershell
cargo tree -p frd-protocol-rdp -e normal
rg -n "NoCertificateVerification|danger_accept_invalid_certs|SSLKEYLOGFILE|--password|ClearTextPassword" crates/frd-protocol-rdp apps/freeremotedesk-windows
git diff --check
```

- [ ] **Step 5: Record exact offline evidence**

In `docs/validation/windows-native-rdp.md`, record toolchain, dependency versions, commands, pass/fail counts, release path/hash, implemented capabilities, and excluded capabilities. Do not record credentials, certificate DER, host secrets, or session keys. README remains `开发中` until live validation.

- [ ] **Step 6: Commit**

```powershell
git add README.md docs/validation/windows-native-rdp.md crates apps Cargo.toml Cargo.lock
git commit -m "test: verify Windows RDP product integration"
```

### Task 10: Run bounded stock-Windows interoperability

**Files:**
- Modify: `docs/validation/windows-native-rdp.md`
- Modify: `README.md`
- Modify implementation only for evidence-backed interoperability failures

**Interfaces:**
- Consumes: the release executable and an authorized stock Windows Remote Desktop Services target.
- Produces: bounded live evidence for login, desktop, input, optional negotiated capabilities, and cleanup.

- [ ] **Step 1: Reconfirm target safety and prerequisites**

Read-only checks:

```powershell
Get-Service TermService
Get-NetTCPConnection -LocalPort 3389 -State Listen
```

Do not create users, enable services, change firewall/policy, or start a local loopback login while the user is absent. A same-host full login may lock/switch the console and interrupt Codex.

- [ ] **Step 2: Run one bounded connection with credentials supplied through the secure UI/store**

Verify in order: unknown-certificate prompt before credentials, explicit trust decision, NLA activation, first full baseline, incremental refresh, correct colours, pointer, keyboard, wheel, focus-loss release, disconnect, and process cleanup. Never pass a password on the command line.

- [ ] **Step 3: Verify the known-pin path and mismatch fail-closed path**

Reconnect to the same authorized endpoint and confirm the saved exact pin avoids a second unknown prompt. Exercise mismatch only with a deterministic test certificate fixture or another authorized endpoint; do not modify the live server certificate.

- [ ] **Step 4: Verify negotiated existing optional capabilities**

When the stock server negotiates them, verify dynamic resize, text clipboard, and remote audio through the existing controls. An unavailable channel must remain disabled/hidden and must not affect desktop input or Apple behavior.

- [ ] **Step 5: Measure the damage path**

Record remote dimensions and one small dirty region's dimensions and copied byte count. Confirm renderer uploads the patch rather than a complete desktop copy.

- [ ] **Step 6: Record evidence and update the matrix truthfully**

Record client/server OS versions, IronRDP version, certificate decision path, negotiated capabilities, observed results, duration, and known limits. Change Windows-to-Windows from `开发中` to `受限验证` only if login, first frame, refresh, pointer, keyboard, and disconnect all pass.

- [ ] **Step 7: Commit**

```powershell
git add README.md docs/validation/windows-native-rdp.md crates/frd-protocol-rdp
git commit -m "test: validate stock Windows RDP interoperability"
```
