# Canonical Frame Recovery Design

## Status

Approved as part of the 2026-08-29 High Performance correction. This
specification addresses the independent product-frame loss that left black
rectangles and then stopped visible updates during the Mac display transition.

## Goal

Guarantee that a bounded frame-mailbox overflow cannot leave the wgpu texture
with stale black rectangles when the Apple decoder's canonical CPU surface
already contains the restored pixels.

## Evidence and Root Cause

The current Apple decode path commits type-0 and type-1 pixels to
`DisplaySurface` before it publishes a `SurfaceUpdate`. The canonical CPU
surface therefore remains correct even if the downstream mailbox rejects the
publication.

`FrameMailbox`, however, clears all queued damage and boundaries for the
current generation on overflow and returns `NeedsFullSnapshot`. The Apple path
currently treats `NeedsFullSnapshot` as though the decoder lacked a full
baseline: it calls `receiver.request_full()` and waits for another server full
record. This violates the existing architecture contract that the owner of the
canonical CPU surface republishes its current full snapshot.

The product also starts the protocol worker before `LiveSessionPorts` become
active on the application thread. Events and frames can accumulate before the
first drain, increasing the probability of that overflow. The legacy minifb
control continuously drains and did not reproduce the black rectangles.

No evidence currently shows that wgpu overwrites a correct damage patch. This
change therefore does not replace the renderer or alter MVS decoding.

## Selected Architecture

### Separate recovery meanings

Keep the two recovery outcomes distinct:

- `NeedsFullBaseline`: the Apple decoder/publisher has no independently
  presentable full baseline. Reset decoder receive state and request a full MVS
  update from the Mac.
- `NeedsFullSnapshot`: the current canonical CPU surface is valid, but the
  downstream queue discarded a publication. Repackage that local surface as a
  newer full snapshot; do not wait for the Mac and do not put the decoder into
  awaiting-full state.

### Local canonical snapshot

On `NeedsFullSnapshot`, `AppleSurfacePublisher` must:

1. retain the current session and generation;
2. allocate the next strictly greater revision;
3. extract the complete current `DisplaySurface` as full-width BGRX horizontal
   bands no larger than 1 MiB each;
4. publish each band as a strictly increasing `Damage` plus matching
   `FrameBoundary` revision;
5. mark intermediate boundaries `Incremental` and only the final boundary
   `FullBaseline`;
6. mark the baseline established only after the final boundary is accepted;
7. continue the normal incremental request sequence.

Banding prevents the Apple surface budget from being incorrectly constrained
to the product mailbox's 64 MiB aggregate budget. Every band has the complete
surface stride, and the bands cover the surface exactly without overlap or
gaps. If one complete row exceeds the band budget, or recovery overflows again
before the consumer drains, the adapter fails closed with the existing
frame-port error. It must not recurse, allocate an unbounded queue, or silently
publish a partial baseline.

The snapshot copy happens only on explicit recovery. Normal type-0/type-1
updates keep the current dirty-rectangle path and its existing copy budget.

### Baseline validity

An overflow invalidates the queued presentation chain, not the canonical CPU
surface. The publisher must not allow another incremental publication to
pretend recovery succeeded before a full snapshot boundary has been accepted.

If overflow occurs before any complete canonical baseline exists, recovery is
`NeedsFullBaseline`, not a local snapshot of incomplete pixels.

### Session launch barrier

Add a protocol-neutral start barrier to the desktop session host:

1. the background launch creates the command/event/frame ports and worker;
2. the protocol worker waits on a private, single-owner RAII start gate;
3. `accept_launch_outcome` installs `LiveSessionPorts` as active;
4. only then does it release the protocol worker;
5. ordinary cancellation drops the gate and uses the existing asynchronous
   cleanup/join path;
6. fatal exit or an ignored late outcome drops the gate through ownership only
   and returns the already-latched fatal immediately, without waiting for
   launch completion, cleanup, or join.

This prevents every adapter from filling a mailbox before the application can
drain it. Gate release is non-blocking and cannot introduce a new public error
or leave a half-installed active session. The barrier contains no Apple, RDP,
MVS, or wire-protocol branch.

### Explicitly deferred amplifiers

Wake coalescing, per-update GPU error-scope reduction, and compositor
acquisition retry are not changed in this correction. They remain performance
or resilience candidates, but the current evidence does not require them to
restore lost canonical pixels. They may be reconsidered only after revision
watermarks show a remaining loss or stall.

## Isolation

- Canonical BGRX snapshot generation and Apple recovery decisions remain in
  `frd-protocol-apple`.
- The generic mailbox keeps its bounded drop-and-request contract.
- The launch barrier remains in `frd-shell-desktop` and applies uniformly to
  Apple and RDP.
- No Apple rule is added to wgpu, the compositor, app state, or RDP.

## Observability

Recovery logs are edge-triggered rather than per-frame. One recovery line may
record session, generation, lost revision, replacement revision, and snapshot
bytes. Secrets, host credentials, pixels, and raw protocol payloads are never
logged.

## Verification

Focused tests must prove:

1. an established baseline followed by mailbox overflow republishes the latest
   canonical surface at a greater revision;
2. local snapshot recovery does not call `request_full` or enter decoder
   awaiting-full state;
3. no unrelated decoder incremental is published between overflow and the
   replacement full baseline; snapshot bands themselves use ordered
   incremental boundaries until the final full boundary;
4. overflow before a canonical baseline still requests a server full update;
5. the protocol worker cannot publish before the application accepts and
   installs its ports;
6. cancellation before acceptance does not leak or start a protocol session.

The live Mac gate must include a black-to-restored display transition and prove
that the Windows client reaches the restored desktop without residual black
rectangles or a stopped revision stream.

## Completion Boundary

Offline tests establish recovery correctness but not the original live failure.
The README platform matrix may move from `开发中` to `受限验证` only after a
bounded Windows-to-Mac run confirms continuous restored frames and records the
evidence date.
