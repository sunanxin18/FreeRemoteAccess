# RDP Platform Orthogonality and Windows Live Closure

**Date:** 2026-09-05

**Status:** Approved approach; written specification pending user review

**Base:** `b576cd7`

## 1. Goal

Continue the existing IronRDP client implementation until a Windows client can
log in to an unmodified Windows Remote Desktop Services host, display and
refresh one desktop, send keyboard, mouse, and wheel input, and disconnect
cleanly. At the same time, remove the remaining compile-target decision from
the RDP protocol crate so future Windows, macOS, Linux, Android, and HarmonyOS
NEXT clients reuse one RDP wire implementation.

This work extends the approved 2026-08-29 Windows native RDP design. It does
not replace the Apple protocol, renderer, application controller, or platform
service interfaces.

## 2. Confirmed current state

`frd-protocol-rdp` already contains a real IronRDP 0.17 client path:

- credential-free TLS identity preflight, generic certificate decision, exact
  pin reconnect, and CredSSP/NLA-only authentication;
- licensing, secure settings, capability exchange, activation, DeactivateAll
  reactivation, and one ordered writer;
- legacy Bitmap Update and RemoteFX output through IronRDP `DecodedImage`,
  converted to bounded BGRX dirty patches and the shared framebuffer contract;
- scan-code and Unicode keyboard input, pointer buttons, X1/X2, both wheel
  axes, and `ReleaseAll`;
- single-display Display Control, Unicode text CLIPRDR, and 48 kHz stereo PCM
  RDPSND through existing protocol-neutral contracts;
- factory registration in the Windows composition root alongside the isolated
  Apple factories.

These are implementation and offline-test facts. There is no stock-Windows
login, first-frame, refresh, input, or disconnect interoperability evidence.
The README status therefore remains `开发中`.

## 3. Mandatory architecture boundary

The dependency direction is:

```text
apps/freeremotedesk-<platform>
    |-- selects host platform services
    |-- constructs protocol factories
    v
frd-protocol-api / frd-platform-api
    ^                         ^
    |                         |
frd-protocol-rdp        frd-platform-<platform>
    |
    v
IronRDP private seam
```

The following rules are binding:

1. `frd-protocol-rdp` must not depend on a concrete platform crate, winit,
   egui, wgpu, Apple protocol code, or RFB code.
2. IronRDP types remain private to `frd-protocol-rdp`; only existing neutral
   commands, events, surfaces, media frames, clipboard payloads, capabilities,
   and stable errors cross its boundary.
3. Platform shells own credential-store selection, system UI, window/input
   capture, audio output, clipboard integration, packaging, and application
   lifecycle.
4. The RDP wire state machine is implemented once. Future client platforms
   inject platform identity and services; they do not fork RDP negotiation or
   decoding.
5. Server operating system and client runtime platform are independent axes.
   Choosing a Windows server selects RDP; it does not select a Windows-only
   protocol implementation.

## 4. Platform identity injection

The current `connector.rs` chooses IronRDP's `MajorPlatformType` through
`cfg(target_os)`. Replace that compile-time branch with an immutable value
provided when the concrete `RdpProtocolFactory` is constructed.

The RDP crate owns a small protocol-facing value such as
`RdpClientPlatformIdentity`. It represents only values carried on the RDP wire
and does not expose Rust target triples or platform APIs. The application
composition root maps its host platform to this value:

- Windows -> IronRDP Windows platform value;
- macOS/iOS -> the corresponding Apple client value;
- Linux/Android/HarmonyOS NEXT -> the approved protocol value selected by that
  platform's future composition specification.

The Windows application constructs the factory with the Windows identity.
Tests construct it explicitly. No default based on `cfg(target_os)` is allowed
inside `frd-protocol-rdp`, because an implicit default would recreate the
coupling this change removes.

This does not add a field to the login form, connection profile, or
`ConnectRequest`: client platform identity is application configuration, not a
user or server choice.

## 5. Delivery sequence

### 5.1 Restore a green delivery baseline

The current `main` CI workflow is green, but its Windows package run exposed
two shell decoder panic-hook tests that interfere when the test harness runs
them concurrently. Repair only their deterministic test isolation. Do not
change decoder panic handling or serialize the complete build.

### 5.2 Make client platform identity explicit

Introduce the injected identity at the RDP factory/config/upstream seam, update
the Windows composition root, and add dependency tests proving the RDP crate
has no concrete platform dependency or target-conditional platform choice.
Apple factory construction and automatic Mac selection must be byte-for-byte
unaffected at their public boundary.

### 5.3 Close the existing Windows Phase 1 path on a real target

Use a separate, authorized Windows 10/11 Pro or Enterprise or Windows Server
2016+ target with stock Remote Desktop Services. The bounded sequence is:

1. endpoint resolution and credential-free TLS identity decision;
2. verified TLS reconnect, CredSSP/NLA, licensing, and activation;
3. negotiated desktop size and first GPU-confirmed `FullBaseline`;
4. at least one incremental refresh with correct color and geometry;
5. mouse movement/buttons, vertical and horizontal wheel, physical keyboard,
   Unicode text, focus-loss `ReleaseAll`, and input after refocus;
6. explicit disconnect, worker cleanup, and return to the login page;
7. one known-pin reconnect.

The current Codex host must not be used for a full loopback login because RDP
can lock or switch its active console. No account, VM, firewall rule, service,
or policy may be created or changed without separate authorization.

### 5.4 Fix only evidence-backed interoperability failures

Live failures are routed to their owning layer:

- TLS/NLA/licensing/activation -> `frd-protocol-rdp` connector or stable error;
- decoded RDP graphics -> RDP active stage, baseline, or surface publisher;
- neutral surface presentation -> existing frame/renderer contracts;
- platform credential, clipboard, audio, or window behavior -> the concrete
  client-platform adapter;
- UI state -> protocol-neutral app/UI model.

An RDP fix must not add commands to Apple, modify MVS/HPSS, or introduce a
protocol-specific window path.

## 6. Graphics readiness and Refresh Rectangle

The current client enters the remote page only after exact full-surface
coverage is presented as `FullBaseline`. This is conservative and remains the
correct input gate.

Do not yet add an unconditional Refresh Rectangle PDU. IronRDP 0.17 consumes
the server General Capability Set during activation and does not expose the
server's `refreshRectSupport` in `ConnectionResult`. Sending the PDU without
that negotiated fact can cause a conforming server to reject it.

If live evidence shows activation succeeds but full coverage never arrives,
the next design amendment must choose one of these evidence-backed options:

1. update to an IronRDP release that exposes the server capability;
2. contribute a minimal upstream capability exposure and pin its reviewed
   release/commit;
3. add a private seam patch with the same narrow exposure, without duplicating
   activation parsing.

Any such amendment must advertise only capabilities the decoder actually
implements and add an initial-frame deadline with a stable
`rdp_graphics_failed` outcome. Absence of a frame must not be treated as proof
of a particular codec.

## 7. IronRDP and EGFX policy

Remain on the pinned IronRDP 0.17 release for this Phase 1 closure. Do not pin
an arbitrary upstream `main` snapshot.

Client-side EGFX integration is upstream work in progress across connector
flags, the dynamic graphics channel, surface composition, codec coverage, and
capability correctness. FreeRemoteDesk must not advertise RDPGFX, AVC420, or
AVC444 until the exact pinned upstream set can decode every capability it
advertises and publish the result through the current BGRX surface interface.

EGFX, ClearCodec, Progressive RemoteFX, AVC420, AVC444/AVC444v2, multi-monitor,
network reconnect, cursor bitmaps, drive/device redirection, and client
microphone remain separate future specifications.

## 8. Error and secret handling

- Keep user-visible errors stable and credential-free.
- Distinguish graphics readiness/session closure from activation failures when
  live evidence exercises those paths; do not expose raw IronRDP errors.
- Preserve identity validation before password extraction.
- Passwords remain absent from argv, profiles, logs, diagnostics, captures,
  and UI snapshots.
- Explicit secret-memory zeroization is a follow-up security task unless a
  live failure requires changing credential ownership. It must not be mixed
  into graphics or platform-identity work.

## 9. Focused validation

Automated tests are limited to core contracts:

- each injected client platform identity produces the exact IronRDP platform
  capability and no compile-target branch remains in the RDP crate;
- Windows composition injects the Windows identity and automatic Windows
  selection remains RDP/3389;
- Apple selection, factory registration, and protocol tests remain unchanged;
- the two panic-hook tests are deterministic under the normal concurrent test
  harness;
- `frd-protocol-rdp`, dependency-boundary, app/UI model, shell, workspace
  compile, Windows release/package, and existing Apple regression gates pass.

Live evidence and automated tests are recorded separately. A green build does
not upgrade Windows-to-Windows support from `开发中`.

## 10. Acceptance

This specification is complete when:

1. the Windows package pipeline is green without global test serialization;
2. `frd-protocol-rdp` contains no client-OS compile branch or concrete platform
   dependency;
3. Windows composition explicitly injects the RDP Windows platform identity;
4. all Apple and neutral regression gates remain green;
5. a separate stock Windows target completes login, first frame, incremental
   refresh, input, and disconnect; and
6. README/validation records name the observed codec and exact validation
   scope without claiming EGFX or untested platforms.

If no separate Windows target is available, items 1-4 may complete, while
items 5-6 remain explicitly blocked. The implementation must not use same-host
RDP or inferred protocol behavior to manufacture closure.
