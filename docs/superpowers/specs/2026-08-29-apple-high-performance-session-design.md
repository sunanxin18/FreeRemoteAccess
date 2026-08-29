# Apple High Performance Session Design

## Status

Approved in conversation on 2026-08-29. This specification replaces any
product interpretation in which an authenticated Apple HPSS/MVS transport is
treated as proof that a High Performance virtual-display session is active.

## Goal

Make the Windows FreeRemoteDesk client establish one strict Apple High
Performance virtual display through the stock macOS service. When the same
currently logged-in Mac account is used, macOS owns blanking the Mac hardware
displays while the client continues to show and control the virtual desktop.

## Authoritative Product Semantics

- The Mac product route is `Apple High Performance (HPSS/MVS)` only.
- Standard screen sharing, ordinary VNC, shared-console mirroring, and Curtain
  or Lock Screen are not fallbacks for this route.
- Hardware-display blanking is a server-side macOS effect. FreeRemoteDesk must
  never synthesize a black remote frame to imitate it.
- The remote surface is the confirmed virtual display, not a capture of the
  hardware display after macOS blanks it.
- A connection that cannot prove the virtual-display transition fails with
  `apple_high_performance_unavailable`; it does not silently continue with a
  different display mode.
- The first release supports one virtual display. Multiple virtual displays are
  outside this change and must not be inferred from unverified fields.

Apple documents the same-user High Performance behavior at
<https://support.apple.com/guide/mac-help/screen-sharing-type-options-mchl1883115d/mac>.
Apple separately documents shared-display and virtual-display choices at
<https://support.apple.com/guide/remote-desktop/choose-how-to-control-and-observe-apd4f46319e/mac>
and Curtain/Lock Screen at
<https://support.apple.com/en-gb/guide/remote-desktop/apd2450a787/mac>.

## ARD 3.10 Evidence Boundary

The wire implementation remains bounded by the repository's ARD 3.10 evidence:

1. Require the authenticated `AppleConnection` to be encrypted before any HPSS
   success event or application-frame write.
2. Send the existing literal 308-byte `0x1d SetDisplayConfiguration` request.
3. Preserve the established `0x1d`, `0x09`, non-incremental framebuffer request
   ordering and encrypted writer ownership.
4. Treat only a strictly parsed `0x451 ServerState` received after the `0x1d`
   write as the virtual-display acknowledgement.
5. Use the width and height in that acknowledged `ServerState` as the initial
   public surface geometry.
6. Request a new non-incremental MVS baseline for that confirmed geometry.

No new flag, packet, retry message, subtype, or undocumented field is added.
The implementation must continue to use the literal builder regression in
`crates/frd-protocol-apple/src/hpss.rs` as the wire-layout gate.

## Current Defect

`run_authenticated_session_inner` currently publishes `TransportReady` and
`NetworkReaderRuntime::new` publishes generation 1 before sending or receiving
the virtual-display configuration exchange. The app therefore knows only that
authentication and HPSS transport succeeded; it does not know that macOS
accepted the High Performance virtual display.

The same early generation also permits physical-display transition pixels to
enter the public framebuffer. If macOS blanks the hardware display before the
virtual display is ready, those transition pixels can be mistaken for the
remote desktop.

## Selected Architecture

### Strict startup gate

Add an Apple-private `HighPerformanceStartupGate`. It has three states:

- `AwaitingServerState`: `0x1d` has been written, but no valid post-request
  `ServerState` has been accepted.
- `Confirmed`: the first valid post-request `ServerState` has supplied the
  single virtual display geometry.
- `Failed`: the acknowledgement was malformed, exceeded the product resource
  budget, or was absent until the bounded product deadline.

The deadline is five seconds from the successful `0x1d` write. It is a local
fail-closed product deadline, not an inferred ARD wire timeout, and it sends no
extra packet. A ServerState observation carries its observation time into the
gate; the gate checks the deadline and the geometry atomically, so a message
observed at or after the deadline cannot confirm the session merely because the
previous 100 ms reader tick happened before it.

### Deferred public generation

Authentication may construct private receive and decode state, but it must not
call `ProtocolRuntime::begin_generation`, publish `TransportReady`, publish a
frame, or enable input before the startup gate confirms the virtual display.
An unencrypted authenticated compatibility path is not eligible for the
product High Performance route and must fail before `0x1d` is written. The
product security selector therefore requires the encrypted Apple security type;
legacy unencrypted authentication remains research-only and cannot be selected
by `AppleProtocolFactory`.

`AppleSurfacePublisher` therefore starts pending. On the first accepted
`ServerState`, the Apple adapter performs one transactional startup commit:

1. strictly parse and validate the confirmed size against the existing Apple
   framebuffer budget;
2. allocate replacement generation-1 receiver, request, CPU surface, dynamic
   state, and exact full-request bytes without changing public state;
3. preserve only the timestamp of the earlier full write for wire-rate safety;
   discard its in-flight/table/generation bookkeeping and all pre-confirmation
   MVS assembly, decoder, pixels, and viewport targets;
4. successfully write one existing non-incremental MVS request for the
   confirmed size;
5. install the prepared private state and call
   `ProtocolRuntime::begin_generation` exactly once;
6. return one private confirmation outcome; the runtime then publishes
   `TransportReady`, the existing capabilities, and any audio-start state
   before reading the requested baseline response.

All fallible private preparation and the confirmed-size wire write occur before
public generation activation. A public event/frame port failure during
`begin_generation` keeps the existing terminal, no-rollback API contract and
must not publish readiness. A duplicate matching startup `ServerState` is
idempotent. After the startup commit, matching geometry is unchanged and later
changed geometry is handled only by the existing generation-bound transaction;
the startup gate is not reused to reject a valid later resize.

### Confirmation-before-frame rule

MVS records that arrive before confirmation may be reassembled only to preserve
the encrypted application-frame boundary, then are discarded. They must not be
decoded, install tables, mutate the CPU surface or dynamic-resolution evidence,
run recovery, publish a `SurfaceUpdate`, or send another full/incremental
request. Structural pre-confirmation MVS failures reset private assembly and
continue waiting for the confirmation deadline without a recovery write. The
startup commit installs fresh decoder state and explicitly asks for a new full
MVS record. While pending, the normal incomplete-MVS and dynamic-resolution
tick is disabled; only the five-second startup deadline is serviced.

After confirmation, the existing Apple invariant remains unchanged: only a
complete, current-generation, non-black type-0 transaction can publish the
initial `FullBaseline`. Type-1 and incomplete type-0 records cannot establish
the initial baseline.

### UI and input gate

The protocol-neutral stages keep their existing meaning:

- `Connecting`: authentication or High Performance display confirmation is in
  progress.
- `TransportReady`: the Apple virtual display has been confirmed and generation
  1 has been published.
- `RemoteSession`: the compositor has actually presented the current
  generation's `FullBaseline`.

The current `frd-app` presentation reducer and desktop `InputGate` remain
protocol-neutral. They continue to enable input only after that presented full
baseline. Apple-specific state must not be added to `frd-app`, the renderer, or
the RDP adapter.

### Geometry and aspect ratio

The acknowledged `ServerState` size is the only initial remote size exposed to
the shell. `SurfaceGenerationChanged`, `SurfaceUpdate::Reset`,
`RemoteBinding`, the wgpu texture, `ContentViewport::fit_in`, and inverse pointer
mapping must all carry that same size and generation.

The renderer uses aspect-preserving contain fit and may letterbox. It must not
stretch the remote image or substitute the client window size for the confirmed
virtual-display size.

## Failure Behavior

- Missing or malformed initial `ServerState`:
  `apple_high_performance_unavailable`.
- Invalid or over-budget geometry: `apple_high_performance_unavailable`.
- Peer close before confirmation: `apple_high_performance_unavailable`; peer
  close after confirmation retains the ordinary `Closed` result.
- Failure after the startup commit: retain the existing typed Apple runtime
  failure behavior.
- No failure path starts Standard, VNC, Curtain, or a second Apple session.
- Fatal exit blocks input immediately and preserves the stable error code in
  the visible diagnostics.

## Protocol and Platform Isolation

- All startup-gate state and ARD parsing remain in `frd-protocol-apple`.
- `frd-protocol-api` receives no Apple-specific enum or flag.
- `frd-app`, `frd-shell-desktop`, `frd-render-wgpu`, and
  `frd-compositor-wgpu` retain protocol-neutral session, frame, and input
  contracts.
- `frd-protocol-rdp` and future RFB adapters are unchanged.

## Evidence and Product-Policy Boundary

ARD 3.10 evidence fixes the existing literal `0x1d`, `0x09`, framebuffer
request bytes/order and the strict `0x451` geometry envelope. Treating the first
strict post-`0x1d` ServerState as product-level virtual-display confirmation,
the five-second deadline, discarding pre-confirmation MVS, the fresh confirmed
baseline request, the stable failure code, and delayed public generation are
local fail-closed product policies. They do not claim an undocumented Apple
flag or redefine the wire protocol.

## Verification

Automated verification is intentionally limited to the core protocol contract:

1. no public generation, `TransportReady`, frame, or input-capable state before
   a valid post-`0x1d` `ServerState`;
2. confirmation publishes exactly one generation with the acknowledged size
   and requests a fresh full MVS baseline;
3. a pre-confirmation MVS record cannot become the product baseline;
4. malformed or missing confirmation produces
   `apple_high_performance_unavailable` without fallback;
5. a current-generation presented `FullBaseline` remains the sole input gate.

The bounded Windows-to-Mac gate must verify separately:

1. same-account connection blanks the Mac hardware displays;
2. the Windows client shows a complete, continuously updating virtual desktop;
3. the remote surface has the server-confirmed aspect ratio;
4. pointer, keyboard, and wheel input operate only while the remote surface and
   app are focused;
5. disconnect restores the hardware display through stock macOS behavior;
6. no Standard/VNC/Curtain fallback occurs.

## Completion Boundary

This specification is complete only when the strict gate and focused tests are
implemented and the bounded live gate records its actual result. Compilation or
offline tests alone do not prove hardware-display blanking or virtual-display
interoperability.
