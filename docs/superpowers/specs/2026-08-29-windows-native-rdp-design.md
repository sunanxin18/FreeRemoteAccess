# Windows Native RDP Client Design

**Date:** 2026-08-29

**Status:** Historical approved design; Tasks 1-9 are implemented and offline-validated as of
2026-09-05, while stock-Windows live interoperability remains blocked on an independent target

**Current closure specification:**
[`2026-09-05-rdp-platform-orthogonality-and-live-closure-design.md`](2026-09-05-rdp-platform-orthogonality-and-live-closure-design.md)

**Target:** FreeRemoteDesk Windows client to unmodified Windows Remote Desktop Services

## 1. Goal and compatibility baseline

Add a production-oriented Windows RDP client path without changing the behavior
of the existing Apple HPSS/MVS adapter. The first supported server baseline is:

- Windows 10/11 Pro or Enterprise;
- Windows Server 2016 or newer;
- TLS and CredSSP/NLA required;
- username forms `user`, `DOMAIN\user`, and `user@domain`;
- no fallback to Standard RDP Security or another protocol after failure.

The product remains client-only. It must connect to stock Windows Remote Desktop
Services and must not install a FreeRemoteDesk service, relay, daemon, driver, or
companion program on the server.

## 2. Selected approach

Create an independent `frd-protocol-rdp` crate around IronRDP's low-level public
connector, session, graphics, and input state machines. Pin the stable IronRDP
0.17.0 release and commit `Cargo.lock`. Do not embed IronRDP's viewer, CLI,
software renderer, server, command-line password behavior, or permissive TLS
backend.

The alternatives are rejected for this implementation:

- FreeRDP provides broader mature coverage but adds a large C/FFI and packaging
  surface across every future client platform.
- A new RDP implementation would duplicate security-sensitive TLS, CredSSP,
  licensing, compression, graphics, and virtual-channel work.

IronRDP types are confined to a private `upstream.rs` seam so upstream API
changes do not propagate into application, renderer, platform, or Apple code.

## 3. Architecture and dependency boundaries

```text
connection UI / secure stores / AppController
                     |
        ProtocolFactory / ProtocolRuntime
              /                      \
 frd-protocol-apple             frd-protocol-rdp
 Apple auth, HPSS/MVS           TLS, NLA, RDP graphics/input
              \                      /
        SessionEvent / SurfaceUpdate / MediaFrame
                     |
              FrameMailbox -> wgpu
```

Only the Windows application composition root registers concrete factories.
Protocol-neutral crates operate on descriptors and interfaces, not concrete
adapter types.

Mandatory boundaries:

- `frd-protocol-rdp` must not depend on `frd-protocol-apple`, `frd-wire-rfb`,
  winit, wgpu, egui, minifb, or a platform crate.
- Apple and RDP do not share sockets, authentication objects, writers, decoded
  images, held-input state, generation controllers, or errors.
- Mac OS automatic selection continues to choose only Apple HPSS/MVS. Windows
  automatic selection chooses only `ProtocolId::rdp()` and port 3389.
- A failed RDP connection does not fall back to Apple or RFB.
- Apple MVS rules, the resized `0x09` experiment, nonblack diagnostics,
  SRTP/SRTCP, and AAC-ELD remain private to `frd-protocol-apple`.
- RDP publishes canonical BGRX surfaces and protocol-neutral events. No RDP PDU
  or IronRDP type crosses the adapter boundary.
- The adapter returns one `ProtocolExit`; the existing shell remains the sole
  producer of the terminal `SessionEvent::Closed`.

Dependency tests must prove that concrete protocol crates are imported only by
composition roots and never by one another or by neutral layers.

## 4. Reused public contracts

Phase 1 does not change these public interfaces:

- `ConnectRequest`, `ProtocolFactory`, and `ProtocolSession`;
- `ProtocolRuntime`, `SessionCommand`, and `SessionEvent`;
- `SessionCapabilities` and `ProtocolExit`;
- `SurfaceUpdate`, `PixelPatch`, `FrameMailbox`, and presentation receipts;
- `AppController`, `SessionCoordinator`, and the wgpu renderer.

RDP normalizes decoded output to `PixelFormat::Bgrx8UnormSrgb`. Input arrives as
the existing protocol-neutral `SessionInput` and is translated inside the RDP
adapter into scan-code, Unicode, pointer-button, movement, and wheel events.

The common login form remains unchanged. The RDP adapter parses `DOMAIN\user`
and UPN forms rather than adding a domain field to shared profile and credential
schemas. Saved profiles remain keyed by endpoint, protocol ID, and username, so
Apple and RDP records cannot collide.

## 5. Server identity and credential security

IronRDP 0.17.0's example/default TLS paths accept invalid server certificates.
They are reference transport code only and are a release blocker for
FreeRemoteDesk. The product path must implement the following sequence:

1. Open a credential-free TCP/TLS preflight connection.
2. Validate the system trust chain, server name, validity period, and server
   authentication usage.
3. Continue automatically only when ordinary validation succeeds, or when an
   exact saved endpoint/protocol SHA-256 pin matches and the leaf still passes
   server-name, validity, and server-auth/EKU checks. A pin may override only
   the untrusted-issuer class.
4. Only for an untrusted issuer whose leaf passes those independent checks,
   publish the existing generic server identity challenge before any CredSSP
   write. Wrong-host, expired/not-yet-valid, invalid-purpose/EKU, malformed,
   revoked, and all other non-issuer failures are not interactive overrides.
5. On TrustOnce or TrustAndRemember, close the preflight connection and open a
   new connection that accepts only the exact approved fingerprint.
6. Persist only an explicit TrustAndRemember decision through the existing
   DPAPI-backed identity store.
7. Fail closed on a saved-pin mismatch. A generic retry must not override it.
8. Start CredSSP/NLA only after identity validation completes.

Passwords are loaded from the existing secure store or in-memory secret buffer.
They must not enter argv, ordinary configuration, profile metadata, logs,
diagnostics, captures, screenshots, or error strings. Production builds must
not export TLS session secrets through `SSLKEYLOGFILE`. Upstream credential
allocations are created once where possible and dropped immediately at session
termination.

## 6. Phase 1: baseline RDP desktop

Phase 1 delivers the agreed minimum:

- TLS plus CredSSP/NLA and RDP licensing;
- one remote display with a fixed initial remote size;
- raw bitmap, Interleaved RLE, RDP 6.0 bitmap compression, and RemoteFX;
- mouse movement, buttons, extra buttons, vertical and horizontal wheel;
- scan-code keys, extended keys, Unicode text, physical modifier keys, and
  `ReleaseAll` on focus loss, pointer disarm, disconnect, or generation change;
- the existing login UI, secure credential store, certificate decision page,
  connection lifecycle, frame mailbox, and wgpu renderer;
- local system cursor only; server cursor shape fidelity is deferred;
- audio, clipboard, disk/device redirection, dynamic resolution, EGFX, AVC420,
  and AVC444 disabled.

Caps Lock, Num Lock, and Scroll Lock state synchronization is explicitly
deferred. The current protocol-neutral `Modifiers` contract carries no lock
state bits, so this branch does not invent state or add a public input/UI
schema. Existing physical modifier-key events and all other input behavior are
unchanged; lock synchronization requires a future approved protocol-neutral
input-contract change.

The new crate is divided by responsibility:

```text
frd-protocol-rdp/
  factory.rs          descriptor and nonblocking session construction
  config.rs           immutable connection configuration
  upstream.rs         pinned IronRDP public seam
  server_identity.rs  validation and generic challenge mapping
  tls.rs              verified TLS transport
  connector.rs        negotiation, CredSSP, licensing, activation
  runtime.rs          command/socket selection and shutdown
  active_session.rs   active-stage and reactivation driver
  surface.rs          BGRX dirty-region extraction
  baseline.rs         generation coverage and snapshot recovery
  input.rs            protocol-neutral input translation
  writer.rs           single ordered RDP writer
  error.rs            stable credential-free error mapping
```

### Graphics publication

The adapter owns IronRDP's canonical decoded image. A graphics update copies
only the affected rows into an owned, non-cloneable `PixelPatch`; it never
converts the complete desktop into a new `Vec<u32>` for every update. Each
revision has one boundary. The first boundary whose coverage includes the whole
current surface is `FullBaseline`; earlier boundaries are `Incremental`.

When the bounded mailbox requests a full snapshot, the adapter republishes from
its canonical image. Large surfaces are split into bounded patches and only the
final recovery boundary is `FullBaseline`. A negotiated size that cannot fit
renderer or mailbox budgets is rejected before allocation.

### Lifecycle

Blocking protocol work starts only inside `ProtocolSession::run`. One ordered
writer owns all outbound RDP traffic. DNS, TCP, TLS, NLA, activation, active,
reactivation, and shutdown states all consume Disconnect. The adapter never
sends synthetic anti-idle mouse input.

`TransportReady` means RDP activation completed, not merely TCP or TLS success.
The remote page becomes interactive only after the current generation's GPU-
confirmed `FullBaseline`. Fatal failure returns immediately with a stable RDP
error; shell cleanup produces the single terminal event.

## 7. Existing-capability follow-up

The current RDP project must not add a new public feature schema, advanced RDP
settings page, title-bar group, or protocol-specific UI control. After the
baseline works, it may implement only capabilities that already have
FreeRemoteDesk protocol-neutral contracts and UI:

- DeactivateAll/reactivation remains an internal RDP lifecycle concern. It uses
  the existing generation and baseline contracts and adds no UI.
- Display Control may consume the existing `ViewportChanged` command and expose
  only the existing `dynamic_resolution` capability after successful
  negotiation. Server confirmation remains required before a new generation.
- CLIPRDR text may use the existing clipboard read/write capabilities,
  `ClipboardPayload`, command, event, and current title-bar control.
- RDPSND may publish through the existing media port and expose only the current
  `remote_audio` state/control. The adapter never opens a platform audio device.
- IronRDP graphics improvements, including EGFX, ZGFX, or AVC420, may be added
  internally only when they continue to publish the current BGRX
  `SurfaceUpdate` contract and require no new product UI.

IronRDP 0.17.0 does not implement AVC444/AVC444v2 decoding. FreeRemoteDesk must
not advertise or claim those codecs. Any future implementation requires a
separate approved specification and direct interoperability evidence.

The shell's audio device is opened lazily on the first media frame so a
no-media RDP session cannot fail or downgrade a later Apple audio session. This
shared host correction must pass the existing Apple audio regression gate.

## 8. Explicitly excluded from current development

The following capabilities remain hidden and receive no new UI, command, event,
profile field, or public interface in the current RDP work:

- RDP gateway and gateway-specific credentials;
- server cursor bitmap fidelity, because no current cursor contract exists;
- file clipboard transfer and drive redirection;
- client microphone input;
- printer, smart-card, USB, COM plugin, and generic device redirection;
- new multi-monitor configuration, per-monitor DPI controls, or RDP-specific
  performance/codec selectors;
- Caps Lock, Num Lock, and Scroll Lock state synchronization until a future
  protocol-neutral input contract represents those states;
- any other IronRDP feature not representable by the existing product contracts.

Their presence in IronRDP source or Cargo features does not make them a
FreeRemoteDesk capability. Reopening one requires a separate design and user
approval. Apple HPSS/MVS must not receive placeholder commands for excluded RDP
features.

## 9. Cross-platform compatibility

`frd-protocol-rdp` remains platform-neutral, but the current implementation and
validation target is the Windows client only. Later macOS, Linux, Android, and
HarmonyOS NEXT clients may reuse the same adapter through their platform shell
and secure-store implementations; they must not fork RDP wire behavior or add
platform conditions inside the RDP crate. No additional client-platform work is
part of this implementation plan.

## 10. Error model

Only stable, credential-free codes cross the adapter boundary, including:

- `rdp_dns_failed`, `rdp_tcp_failed`, `rdp_tls_failed`;
- `rdp_server_identity_changed`, `rdp_nla_failed`, `rdp_logon_failed`;
- `rdp_license_failed`, `rdp_activation_failed`, `rdp_graphics_failed`;
- `rdp_session_closed`.

Raw IronRDP, TLS, certificate, or operating-system diagnostics remain internal
and are sanitized before any user-visible mapping. An RDP error never mutates
Apple capability, session, or retry state.

## 11. Validation gates

Automated tests remain focused on core protocol and boundaries:

- certificate chain/pin/challenge/reject/cancel state transitions;
- proof that certificate preflight performs no CredSSP credential write;
- BGRX colour, stride, inclusive rectangle, and dirty-row extraction;
- baseline coverage and bounded full-snapshot recovery;
- scan-code, physical modifier, Unicode, pointer, wheel, stale generation, and
  `ReleaseAll`; lock-state synchronization is not current acceptance;
- cancellation and exactly one terminal lifecycle outcome;
- dependency graph isolation and composition-root-only registration;
- all existing Apple authentication, MVS, generation, input, frame, audio, and
  presentation tests.

Compilation, package generation, local UI execution, protocol implementation,
and live interoperability are recorded separately in `README.md`.

The current Windows host is Windows Pro with Remote Desktop Services running and
port 3389 listening. It can be used for bounded reachability, TLS, and protocol
tests. A loopback full login may lock or switch the active console and interrupt
Codex, so full first-frame and input validation is performed serially while the
user is present, or against another authorized Windows host/VM. The project must
not create accounts, VMs, or change system policy without separate authority.

## 12. Delivery order

1. Pin and compile the upstream seam and independent factory.
2. Implement verified TLS identity and NLA activation.
3. Implement traditional graphics, baseline, and backpressure recovery.
4. Implement input and lifecycle.
5. Register RDP in the unified Windows login.
6. Run focused RDP tests and the complete Apple regression gate.
7. Build the Windows release and perform bounded stock-Windows interoperability.
8. Update the platform matrix with the exact evidence level.
9. Stop after the existing-capability scope. Any excluded IronRDP feature needs
   a separate approved design before implementation.
