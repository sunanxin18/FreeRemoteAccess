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
4. Handle a valid Media Message 1 port announcement immediately, including
   before any optional `0x451 ServerState`, and reply with the existing exact
   version-3 `0x1c` configuration.
5. Treat a valid Media Message 2 answer as the media-control and UDP transport
   milestone. In the media-first ordering, only then may the selected startup
   geometry activate generation 1. Every ordering publishes `TransportReady`,
   capabilities, and audio-starting state only at this milestone.
6. If `ServerState` arrives first, keep the existing strict geometry commit. If
   it arrives later, treat it as an optional geometry/consistency update; it
   must never block Message 1 or Message 2.

No new flag, packet, retry message, subtype, or undocumented field is added.
The implementation must continue to use the literal builder regression in
`crates/frd-protocol-apple/src/hpss.rs` as the wire-layout gate.

## Current Defect

The previous product runtime cached Message 1 until a `ServerState` activated
the surface. Live A/B evidence disproved that prerequisite: the legacy direct
path received authenticated video RTP while
`virtual_display_confirmed=false`. Because the product path withheld `0x1c`,
the server could not send Message 2 and both sides waited indefinitely. ARD's
`handleAVCMediaEncoding` path likewise derives the client configuration from
Message 1 without a `ServerState` precondition.

## Selected Architecture

### Media-control startup gate

The product HP identity owns an Apple-private media startup gate with three
states:

- `AwaitingMessage2`: `0x1d` has been written, but the Message 1 → `0x1c` →
  Message 2 media-control exchange is not complete.
- `Confirmed`: `ViewerMediaState::handle_answer` has accepted Message 2 and
  activated the generation-bound UDP/SRTP transport.
- `Failed`: the acknowledgement was malformed, exceeded the product resource
  budget, or was absent until the bounded product deadline.

The deadline is five seconds from the successful `0x1d` write. It is a local
fail-closed product deadline, not an inferred ARD wire timeout, and it sends no
extra packet. A valid Message 2 completes this deadline; `ServerState` alone
does not. Standard/TCP-MVS retains its independent ServerState gate and buffered
media behavior.

### Deferred public generation

Authentication may construct private receive and decode state, but Message 1
must not call `ProtocolRuntime::begin_generation`, publish `TransportReady`,
publish a frame, or enable input. It only binds the generation-bound media
sockets and sends the existing exact `0x1c` configuration.
An unencrypted authenticated compatibility path is not eligible for the
product High Performance route and must fail before `0x1d` is written. The
product security selector therefore requires the encrypted Apple security type;
legacy unencrypted authentication remains research-only and cannot be selected
by `AppleProtocolFactory`.

`AppleSurfacePublisher` therefore starts pending. After a valid Message 2, the
Apple adapter performs one transactional media startup commit:

1. use the already selected, resource-checked startup geometry for the private
   scratch generation; do not fabricate a `ServerState`;
2. call `ViewerMediaState::handle_answer` to accept Message 2 and activate the
   generation-bound UDP/SRTP transport;
3. activate the already admitted generation 1 exactly once, without sending an
   extra MVS full request;
4. publish
   `TransportReady`, the existing capabilities, and any audio-start state
   exactly once; then service the active UDP transport.

If a strict `ServerState` arrives before Message 2, the existing geometry commit
and its exact full request remain valid, but they do not publish
`TransportReady`. Message 2 still completes the media gate. A matching late
`ServerState` is idempotent; changed late geometry uses the existing
generation-bound geometry transaction. A public port failure retains the
terminal, no-rollback API contract and must not publish readiness.

### Confirmation-before-frame rule

The HP media gate does not use MVS traffic as proof of Message 2 and does not
cache or swallow Message 1/Message 2 behind the MVS/ServerState state machine.
When Message 2 arrives before `ServerState`, startup generation activation sends
no additional MVS full request. Exact AVC/HEVC decode and compositor present,
not a port announcement or transport event, remain the application Ready/input
evidence.

Standard/TCP-MVS retains its independent pre-confirmation discard/recovery
behavior. Only a complete, current-generation, non-black type-0 transaction can
publish its initial `FullBaseline`; type-1 and incomplete type-0 records cannot
establish that baseline.

### UI and input gate

The protocol-neutral stages keep their existing meaning:

- `Connecting`: authentication or the High Performance media-control exchange
  is in progress.
- `TransportReady`: Message 2 has activated Apple UDP/SRTP transport and
  generation 1 has been published. Message 1 alone never reaches this stage.
- `RemoteSession`: the compositor has actually presented the current
  generation's exact decoded frame.

The current `frd-app` presentation reducer and desktop `InputGate` remain
protocol-neutral. They continue to enable input only after that presented full
baseline. Apple-specific state must not be added to `frd-app`, the renderer, or
the RDP adapter.

### Geometry and aspect ratio

The selected startup geometry is provisional until the decoded AVC/HEVC frame
supplies exact geometry. `SurfaceGenerationChanged`, `SurfaceUpdate::Reset`,
`RemoteBinding`, the decoder surface, the wgpu texture,
`ContentViewport::fit_in`, and inverse pointer mapping must converge on the same
current generation and exact decoded size. An optional `ServerState` may supply
a consistent geometry update, but it is not the media-control prerequisite.

The renderer uses aspect-preserving contain fit and may letterbox. It must not
stretch the remote image or substitute the client window size for the confirmed
virtual-display size.

## Failure Behavior

- Missing or malformed required Message 1/Message 2 media control:
  `apple_high_performance_unavailable`.
- Invalid or over-budget geometry: `apple_high_performance_unavailable`.
- Peer close before Message 2 transport activation:
  `apple_high_performance_unavailable`; peer close after activation retains the
  ordinary `Closed` result.
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
request bytes/order, Message 1 → version-3 `0x1c`, Message 2 handling, and the
strict `0x451` geometry envelope. Live A/B evidence proves that authenticated
video RTP does not require prior `virtual_display_confirmed` evidence. The
five-second deadline, delayed generation activation, and exact-present UI/input
gate remain local fail-closed product policies; they do not redefine Apple wire
semantics.

## Verification

Automated verification is intentionally limited to the core protocol contract:

1. Message 1 before optional `ServerState` immediately produces one exact
   legal `0x1c`, without public generation, `TransportReady`, or an extra
   framebuffer request;
2. valid Message 2 activates UDP/SRTP, activates generation 1 if it is still
   pending, and publishes `TransportReady`, capabilities, and audio-starting
   state exactly once;
3. ServerState-first and ServerState-late ordering cannot block or duplicate the
   media-control milestone;
4. malformed or missing Message 2 produces
   `apple_high_performance_unavailable` without fallback;
5. a current-generation exact decoded frame presented by the compositor remains
   the sole UI Ready and input gate.

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
