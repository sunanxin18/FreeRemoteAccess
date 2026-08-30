# Apple Current Desktop and High Performance Isolation Design

## Status

Approved in conversation on 2026-08-30 and corrected after new ARD 3.10 static
evidence. The earlier Raw-only Candidate A is rejected: ARD command 1 is an
encryption-level command, not an encoding advertisement, and the recovered ARD
encoding arrays do not establish Raw encoding 0 for the current-console route.

The first Shared direction is now an **ARD framework-equivalent current-console
candidate**. Its product descriptor remains disabled until the exact
client-to-server transcript, ARD selection semantics, and required decoder
coverage gates in this specification close. It must not be described as
implemented or verified before then.

The High Performance startup, geometry, baseline, and failure invariants in
`2026-08-29-apple-high-performance-session-design.md` remain normative inside
that adapter.

## Goal

Provide two explicit local-username/password Apple modes without mixing their
post-authentication protocols:

- **Apple 当前桌面:** an evidence-complete implementation of the ARD 3.10
  framework's current-console route;
- **Apple High Performance:** the strict HPSS/MVS virtual-display route.

They may reuse identical stateless authentication and protocol-neutral
application contracts, but never share post-authentication state or fall back
into one another.

## Non-Goals

- Do not choose Raw, MVS, CopyRect, or any quality profile for Current Desktop
  before the exact ARD transcript and selection mapping prove it.
- Do not reinterpret `_RFBSetEncryptionLevel` as `_RFBSetViewerEncodings`.
- Do not infer wire semantics from old successful pixels, input, or physical
  display blanking/non-blanking.
- Do not implement or register the historical no-`0x1d` MVS candidate in this
  phase.
- Do not invent an Apple exclusive bit or synthesize black pixels to imitate
  physical-display blanking.
- Do not make ordinary generic VNC, Curtain/Lock Screen, Apple ID, IDS, APNs, or
  a Mac helper part of either mode.
- Do not add Apple mode state to the renderer, compositor, platform shell, or
  generic session API.

## Corrected ARD 3.10 Static Evidence

### Authentication and ClientInit

The recovered ownership boundary is:

- the ARD main application selects Apple security type 36;
- with `shared = 1`, it calls `_RFBAuthenticate`;
- the framework constructs ClientInit as bit 0 for shared, bit 7
  (`enhanced | 0x80`), and bit 6 (`sessionSelectInfo | 0x40`), yielding the
  typical `0xC1` value when all three predicates are active.

`0xC1` is therefore a statically supported typical combination, not a universal
constant for every control/display choice. The live transcript must establish
the exact byte for each selected matrix cell.

### Command 1 is encryption level, not encoding selection

`_RFBSetEncryptionLevel` emits a fixed 12-byte command. Level 1 is exactly:

```text
12 00 00 01 00 01 00 01 00 00 00 01
```

This command does not advertise Raw or any framebuffer encoding. Its function,
fixed length, and level field must remain separate from the variable encoding
list owned by `_RFBSetViewerEncodings`.

### Display-configuration boundary

The main session entry accepts control types 0, 1, and 2. Its recovered display
selection separates the no-`0x1d` and `0x1d` routes:

- display type 0, compatibility: skips `_RFBSetDisplayConfiguration` and sends
  no `0x1d` at that branch;
- display type 3, remote Mac display/current display: also skips
  `_RFBSetDisplayConfiguration` and sends no `0x1d` at that branch;
- display types 1 and 2, virtual-display/High Performance routes: call
  `_RFBSetDisplayConfiguration` and send `0x1d`.

This establishes a static branch boundary, not the complete pre-first-frame
transcript. In particular, “no `0x1d`” alone does not choose an encoding or
prove a shared/current-console product session.

### Viewer encoding arrays

The recovered named arrays are:

| ARD array | Ordered values |
| --- | --- |
| `SSFull` | `[6, 16]` |
| `SSLow` | `[1000, 6, 16]` |
| `SSMedium` | `[1001, 6, 16]` |
| `SSHigh` | `[1011, 1002, 6, 16]` |
| `SSPro` | `[1010, 1011, 1002, 6, 16]` |

`_RFBSetViewerEncodings` also appends Apple pseudo encodings according to its
own predicates. Encoding 0 is absent from the recovered arrays above; the
specification therefore makes no Raw claim.

Compatibility plus `SSFull [6, 16]` is a static candidate worth testing, but it
is not the product default. It may become a default only if ARD evidence maps
that exact display/control/UI or saved-profile choice to `SSFull`, including the
ordered pseudo-encoding suffix and all writes before the first frame.

## Historical Evidence That Must Not Be Misattributed

- The old stable snapshot `35e5962` and historical `hpssview` send `0x1d`.
  They cannot prove either no-`0x1d` current-console branch, regardless of their
  stable pixels or input.
- Historical `e2de9eb` contains a no-`0x1d` HPSS/MVS current-console candidate,
  but it is not an ancestor of the current implementation line and has no
  recorded stock-Mac live acceptance for that behavior. It remains a research
  candidate only and is not registered or implemented in this phase.
- A physical display remaining visible or becoming blank cannot identify
  compatibility, current Mac display, virtual display, Raw, MVS, shared, or
  exclusive wire semantics. Blanking is a macOS-owned target behavior with its
  own live gate.

Command names, UI labels, and working pixels are not enough. Exact call
ownership and bytes before the first frame are the evidence boundary.

## Current Desktop Evidence and Enablement Gates

### Gate 1: exact client-to-server transcript

The implementation direction cannot choose startup bytes until a bounded ARD
3.10 live trace records every client write through the first framebuffer update
for the required matrix below. At minimum it must establish:

- selected security type and exact ClientInit byte;
- exact `_RFBSetEncryptionLevel` invocation and bytes;
- exact `_RFBSetViewerEncodings` ordered base array and appended Apple pseudo
  encodings;
- exact `_RFBSetMode` invocation and bytes;
- presence or absence and bytes of `_RFBSetDisplayConfiguration`;
- every other `_WriteSocketData` write before the first frame;
- exact first full-update request and subsequent incremental request cadence;
- input writes for the same selected session path.

The transcript is C-to-S authoritative. A nearby function, static array, or
server response cannot substitute for the actual ordered writes.

### Gate 2: selection semantics

The reverse record must map ARD UI/profile choices to the exact:

- control type 0/1/2 semantics;
- observe, shared-control, and exclusive product labels;
- display type 0 compatibility, display type 3 current Mac display, and display
  types 1/2 virtual/High Performance semantics;
- `shared`, `enhanced`, and `sessionSelectInfo` ClientInit predicates;
- quality/profile selection that chooses `SSFull`, `SSLow`, `SSMedium`,
  `SSHigh`, or `SSPro`;
- predicates and order for every appended Apple pseudo encoding.

No FreeRemoteDesk default is selected until that mapping proves which ARD
choice represents the intended current desktop.

### Gate 3: decoder coverage

After Gates 1 and 2 select one exact current-console transcript, the adapter may
enable only when FreeRemoteDesk implements every real framebuffer encoding and
Apple pseudo-encoding behavior required before and during first-frame/current
incremental operation. An unknown or unimplemented encoding cannot be silently
dropped, replaced with Raw, or delegated to the High Performance MVS decoder.

The descriptor remains disabled until the selected transcript's decoder,
geometry, baseline, incremental, and input contracts pass focused fixtures and
the stock-Mac gate below.

## Minimum ARD Live Trace Matrix

Run the following display choices against each control choice:

| Display choice | Observe | Shared control | Exclusive |
| --- | --- | --- | --- |
| Compatibility, display type 0 | required | required | required |
| Current Mac display, display type 3 | required | required | required |
| Virtual/High Performance, display types 1/2 | required | required | required |

For every cell, hook and correlate:

- `_RFBSetEncryptionLevel`;
- `_RFBSetViewerEncodings`;
- `_RFBSetMode`;
- `_RFBSetDisplayConfiguration`;
- `_WriteSocketData`.

Capture exact bytes, call order, control/display/quality inputs, ClientInit, and
all writes through the first framebuffer update. The virtual/High Performance
cells are the positive `0x1d` control; compatibility and current-Mac-display
cells must be checked for the statically predicted absence of that call rather
than assumed from it.

The trace is read-only instrumentation of the authorized client/stock service.
It installs no product server component and cannot by itself mark FreeRemoteDesk
interoperable.

## Product Descriptors

The generic catalog is designed for two stable identities:

- **Apple 当前桌面:** disabled until the exact transcript, selection semantics,
  decoder coverage, and stock-Mac product gates pass; after enablement it starts
  only the framework-equivalent current-console adapter.
- **Apple High Performance:** the existing encrypted HPSS/MVS virtual-display
  adapter with strict `0x1d`, post-request `0x451 ServerState`, generation gate,
  and MVS type-0/type-1 contract. macOS-owned physical-display blanking is a
  target product behavior subject to a live gate, not a fact established by
  authentication, `0x1d`, `0x451`, or MVS alone.

Use generic `ProtocolId` descriptors/factories rather than adding an Apple mode
enum to `frd-protocol-api`, `frd-app`, the UI model, renderer, or platform shell.
A saved profile retains the exact descriptor identity.

## Authentication-First, Single-Ownership Split

Both modes may reuse the stateless Apple connection/authentication
implementation. After authentication returns `AppleAuthenticated`, ownership
moves exactly once into the descriptor selected before the socket opened:

```text
Apple endpoint + local Mac credentials
              |
              v
Apple banner/security/SRP/AES authentication
              |
              v
        AppleAuthenticated
          /             \
         v               v
AppleCurrentDesktop   AppleHighPerformance
ARD transcript       0x1d + HPSS/MVS
equivalent
```

The selected adapter owns the stream, crypto state, buffered bytes, serial
writer, runtime, publisher, and closure. Failure cannot transfer those objects
to the sibling adapter or reinterpret buffered bytes with its parser.

## Allowed Shared Components

The adapters may share only components whose semantics are identical:

- endpoint resolution, TCP connection, banner and Apple security negotiation;
- local Mac username/password retrieval and secret zeroization;
- Apple SRP/AES establishment and encrypted serial-writer primitive;
- stateless common message codecs proved identical in both selected transcripts;
- protocol-neutral command, event, frame-port, mailbox, `FrameTransaction`, and
  presentation interfaces;
- at most a protocol-neutral revision/frame-port helper owning no decoder,
  geometry, baseline, request, mode, or recovery state;
- input encoding only where exact ARD/live bytes establish identity;
- renderer, compositor, viewport mapping, presentation receipt, input gate,
  secure profile store, and generic capability model.

Each adapter owns a distinct publisher and all mutable publication state.
`AppleSurfacePublisher` cannot be shared across adapters or become a
mode-switching object. There is no global request, generation, decoder,
framebuffer, baseline, timing, or recovery state.

## Prohibited Sharing

The adapters must not share or call through each other's:

- post-authentication startup or readiness gate;
- viewer-encoding list, pseudo-encoding predicates, or framebuffer request
  policy;
- `_RFBSetMode` or display-selection state;
- `0x1d`, `0x09`, `0x451`, HPSS, MVS, UDP/media, or dynamic-resolution state;
- selected current-desktop versus MVS decoder/cache/table/assembly state;
- geometry, first-baseline proof, request timing, generation controller,
  publisher, revision, capabilities, timeout, or terminal error state.

Current Desktop consumes only the transcript and decoders approved by its
gates. High Performance cannot accept that framebuffer path as a fallback.
Parser failure invokes only the selected adapter's bounded failure policy.

## Mode-Specific Runtime Contracts

### `AppleCurrentDesktop`

The adapter is not specified at wire-byte level until the gates choose one
matrix cell and transcript. Its eventual implementation must:

1. reproduce that cell's type-36 authentication, ClientInit, encryption-level,
   viewer-encoding, mode, and request bytes exactly;
2. omit `_RFBSetDisplayConfiguration`/`0x1d` only when the selected ARD branch
   does so;
3. own a decoder covering every encoding and pseudo-encoding required by the
   selected transcript;
4. own its canonical surface, publisher, geometry, generation, timing, and
   failure state;
5. publish and present a complete current-generation baseline before input;
6. send only the exact full/incremental request cadence proved for that branch;
7. expose only capabilities proved for the selected current-console session.

It cannot substitute Raw for `[6, 16]`, choose `SSFull` merely because it is
smallest, or initialize MVS because an old no-`0x1d` branch did so.

The implementation registers exactly one stable shared terminal code,
provisionally `apple_current_desktop_failed`, after error-registry review. It is
one code, not an open-ended family.

### `AppleHighPerformance`

This adapter retains the strict approved contract:

1. require encrypted `AppleAuthenticated` state;
2. send the captured literal `0x1d SetDisplayConfiguration` in established
   order;
3. accept only a strictly parsed post-request `0x451 ServerState` as the
   virtual-display geometry confirmation;
4. defer public generation, readiness, frames, and input until confirmed
   startup succeeds;
5. request and present a complete MVS baseline before input;
6. keep MVS, dynamic resolution, media, publisher, recovery, and later
   generation state inside this adapter;
7. fail with `apple_high_performance_unavailable` when virtual-display
   confirmation cannot be proved.

macOS-owned physical-display blanking remains an intended, separately
live-gated target. Neither `0x1d`, `0x451`, successful MVS, nor a previous
blanked/unblanked observation proves it for the current build.

### Historical no-`0x1d` MVS candidate

The `e2de9eb` current-console MVS path remains research evidence only. This
phase does not register its descriptor, implement/reuse its runtime, or route
automatic selection to it. Reopening it requires a new design based on current
ancestry, exact ARD selection/transcript evidence, decoder isolation, and its
own stock-Mac live acceptance.

## Selection, Profiles, and Explicit Reconnect

Mac automatic selection resolves to Apple High Performance. Before all Current
Desktop gates pass, its descriptor and saved profiles remain disabled with an
explicit unavailable reason.

After enablement, a Current Desktop profile resolves only to
`AppleCurrentDesktop`; a High Performance profile resolves only to
`AppleHighPerformance`. Credentials may be reused, but descriptor identity
cannot change silently.

If automatic/High Performance fails, only the login/error decision page may
offer “以共享当前桌面重新连接.” The action remains disabled while Current Desktop
is gated. Once enabled, it closes the failed session and creates a new session,
socket, authentication exchange, adapter, publisher, generation, and input
gate. It is never in-stream or automatic fallback.

The connected-session control island cannot select a descriptor, reconnect, or
create a connection.

## Input, Surface, and Failure Contracts

Both implemented adapters ultimately use the same protocol-neutral surface,
`FrameTransaction`, presentation receipt, focus, and input-gate contracts. This
downstream reuse does not permit a shared publisher, decoder, baseline latch,
request timer, generation, or canonical surface.

- While Current Desktop is gated, selection starts no runtime.
- High Performance never starts Current Desktop implicitly.
- After enablement, Current Desktop uses one registered stable terminal code
  and never sends High Performance setup after failure.
- Any adapter failure blocks input, closes only that session, preserves
  sanitized diagnostics/login fields, and cannot mutate sibling state.
- No failure requests Apple ID, installs a server component, starts generic
  VNC, guesses an encoding, or switches parser.

## Protocol and Client-Platform Isolation

- `frd-protocol-apple` owns evidence-approved factories/adapters, their distinct
  publishers, `AppleAuthenticated`, and post-authentication state.
- `frd-protocol-api` exposes generic descriptors, capabilities, commands,
  events, surfaces, and timings only.
- frame/renderer/compositor crates cannot inspect which Apple adapter produced a
  frame.
- `frd-ui-model` renders descriptor/capability state without importing the Apple
  crate.
- the platform shell owns focus/window behavior, never Apple wire order.
- RDP and future generic RFB adapters never reuse Apple auth or mode state.
- Windows, macOS, Linux, Android, and HarmonyOS shells select through the generic
  catalog; native window handles never enter an Apple adapter.

## Focused Verification

Before Current Desktop enablement, automated coverage is limited to:

1. the descriptor remains disabled;
2. command 1 level 1 is exactly
   `12 00 00 01 00 01 00 01 00 00 00 01` and is not parsed as encodings;
3. the recovered named encoding arrays retain exact order and no invented Raw;
4. display types 0/3 take the static no-`0x1d` branch and types 1/2 take the
   positive `0x1d` control in the recovered selector;
5. High Performance retains its strict confirmation and no fallback.

After the transcript selects a Current Desktop route, add only focused tests for
its exact ordered startup bytes, required decoder set, complete-baseline input
gate, and adapter-state isolation. Do not add exhaustive cross-product protocol
fixtures or duplicate decoder/renderer tests.

Bounded live acceptance is separate:

1. complete the 3-by-3 hook matrix and preserve exact pre-first-frame bytes;
2. prove the selected Current Desktop session shows and controls the stock Mac
   current console with continuous updates and no unrecorded fallback;
3. prove High Performance virtual-display confirmation and separately observe
   its macOS-owned physical-display blanking target for the current build;
4. forcing High Performance unavailable creates no second connection until the
   user explicitly chooses Current Desktop on the login/error page.

## Normative Dependencies and Invariants

- `FrameTransaction`, batching, receipts, and latency acceptance are defined
  only by `2026-08-30-frame-transaction-render-latency-design.md`; neither Apple
  adapter may specialize them.
- Control-island state is defined only by
  `2026-08-30-desktop-floating-control-island-design.md`; the island cannot own
  mode selection or reconnection.
- The 2026-08-29 strict High Performance design remains normative inside
  `AppleHighPerformance`; Current Desktop is a separate evidence-gated adapter.

## Completion Boundary

Current Desktop implementation cannot be specified completely until the exact
ARD transcript, selection semantics, and decoder set are closed. Its descriptor
then remains disabled until focused and stock-Mac gates pass. High Performance
physical-display blanking likewise remains a target until its current-build live
gate passes. Compilation and static evidence prove neither live behavior.
README must record separate evidence dates and limitations; success in one mode
is never evidence for the other.
