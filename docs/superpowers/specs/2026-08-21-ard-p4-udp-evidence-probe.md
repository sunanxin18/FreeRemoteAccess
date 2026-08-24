# ARD P4 UDP Evidence Probe

> **Superseded correction (2026-08-22):** This document records the original
> evidence-harness phase. The verified Message 1 parser and generation-bound
> UDP socket foundation now exist in Rust; Message2, the opaque `0x1c` body,
> SRTP/SRTCP details, and audio formats remain blocked. The current boundary is
> `2026-08-22-ard-media-audio-p3-p6-design.md`.

## Goal

Build an evidence-first reverse-engineering harness that can establish the
client/server control messages and UDP socket parameters required by Apple's
AVC media path without moving guessed fields into the Rust implementation.

The first milestone is a reproducible, decrypted Apple-client `0x1c` fixture.
The second is at least one captured UDP datagram whose session preconditions
and control-message provenance are recorded.

## Verified Evidence

- `-[SSSession startAVCMediaStreams]` generates separate audio and video media
  configurations, reads `audioStreamUDPPort` and video-stream UDP ports, sets
  their remote addresses, and installs bidirectional SRTP/SRTCP media keys.
- The server dispatches application message `0x1c` to
  `HandleServerMediaStreamConfiguration`. The recovered path validates a
  declared message size, byte-swaps media fields, obtains local and remote
  socket addresses, creates a server configuration, invokes another service,
  and emits an answer.
- The twelve-byte client message beginning `08 00 3f e6` is a
  `SetServerScaling` message: byte `0x08`, one reserved byte, one big-endian
  IEEE-754 `f64` scale, then two reserved bytes. `0x3fe6`, `0x3fed`, and
  `0x3fee` are prefixes of different scale values, not protocol subtypes.
- Server decompilation identifies application message `0x08` as
  `HandleSetServerScalingMessage`. Captured `ServerState` responses echo the
  complete eight-byte scale plus the two reserved bytes.
- Server decompilation of `InitializeUDPVideoStream` and
  `EncodeRFBMediaStreamMessage1` proves the fixed 54-byte port-announcement
  layout (encoding `0x3f2`): audio uses the base UDP port, the first video
  stream uses the next port, and the client replies with application message
  `0x1c` only after receiving this announcement.
- No saved artifact currently contains a trustworthy client `0x1c` message.

## Evidence Boundary

- `0x08` is scaling control and is not part of UDP capability negotiation.
  Any probe or implementation that dispatches it by a `3fe6`/`3fed`/`3fee`
  pseudo-subtype is invalid and must fail its regression tests.
- The Message 1 port-announcement layout is statically verified. Message 2
  (`0x1c`) fields, its answer, RTP/RTCP framing, codecs, and SRTP key derivation
  remain gated on exact static recovery or sanitized fixtures.
- Absence of audio bytes in one TCP capture does not prove that a usable UDP
  audio session was negotiated. Static evidence only establishes that the AVC
  media implementation has a UDP/SRTP path.
- A zero-filled `0x1c` rejection cannot identify which field or session
  precondition failed.
- AVC/HEVC, MVS, P1 dynamic resolution, P3 audio output, and P5 audio input
  remain separate state machines and evidence ledgers.
- No inferred `0x1c`, `0x08`, UDP, SRTP, or reliable-UDP field is added to
  `src/` during this phase.

## Architecture

### Decrypted fixture layer

`ard_re/udp_probe.py` owns deterministic, sanitized fixture persistence and
strict parsing of the verified control records.

- `parse_server_scaling_message(frame)` accepts only the exact twelve-byte
  `[08 00][big-endian f64 scale][00 00]` frame and rejects non-finite,
  non-positive, truncated, or non-zero-reserved values.
- `parse_media_stream_message1(frame)` accepts only the verified 54-byte
  envelope, declared size, version, kind, three port/flag descriptors, and
  zero reserved tail.
- `FixtureStore.record(direction, payload, classification)` writes one binary
  file and one JSON manifest entry with sequence, length, SHA-256, direction,
  classification, and relative filename.
- Manifests must never contain IP addresses, usernames, passwords, command
  lines, encryption keys, or absolute paths.
- Decrypted application frames are recorded exactly; a 96 KB record may span
  several captured frames and is not reassembled using an inferred header.

### Response-profile layer

`ServerScalingResponder` selects an explicit scaling-response profile.

- `observe-only` records queries and sends no `0x08` reply.
- `captured-known-only` answers only a complete ten-byte scale-plus-reserved
  echo found in a captured response template.
- A scale that merely shares its first two bytes with a captured value does
  not match and fails closed.
- A later `evidence-fixture` profile may load a response only from a JSON
  descriptor that names the exact ten-byte echo, template file, echo offset,
  provenance, and evidence status.
- Template substitution validates all lengths and replaces only the complete
  ten-byte scale-plus-reserved echo at the declared offset.

### Fake-server integration

`ard_re/fake_ss.py` delegates exact query parsing, response selection, and
fixture writing to `udp_probe.py`.

- Each decrypted client frame is recorded before classification.
- Exact `0x08` scaling frames are queued once and answered once.
- After the display-configuration response, the server sends a statically
  verified Message 1 using the locally reserved consecutive UDP port block,
  then waits for an exact client `0x1c` fixture.
- An unknown scale can never fall through to another scale's template.
- The server password is read from the non-echoing
  `FRD_FAKE_SS_PASSWORD` environment variable, never from argv.
- The response profile and fixture output directory are non-secret CLI
  options.

### UDP capture layer

`ard_re/udp_sink.py` binds a UDP socket and records raw datagrams through the
same `FixtureStore` format.

- Port zero is supported so the OS can select an available port.
- The selected port is printed for an operator but is not injected into any
  protocol field automatically.
- Timeout and maximum-datagram controls are explicit and conservative.
- Datagram payloads remain opaque; this phase does not claim audio/video or
  reliable-UDP framing.

### Static-trace layer

Headless Ghidra scripts produce reproducible candidates for:

- scalar `0x3fe6` instruction uses and containing functions;
- references to `audioStreamUDPPort`, both video UDP-port properties, their
  setters, `startAVCMediaStreams`, and media-configuration serialization;
- server callees used by the `0x1c` handler to allocate a port and construct
  its reply.

Tool output is evidence only when it includes program identity, architecture,
address, containing function, and the instruction or reference path. A missing
reference in one Mach-O slice is recorded as such rather than generalized.

## Acceptance Gates

1. **Scaling semantic gate (complete):** server static evidence and exact
   response-template comparison identify `0x08` as a full-precision scaling
   message and falsify the pseudo-subtype/UDP-query hypothesis.
2. **Port-announcement gate (static complete):** the server encoder establishes
   the exact Message 1 envelope, descriptors, flags, and consecutive ports.
3. **Configuration gate:** the Apple client reproducibly sends a complete
   `0x1c` under a named response profile.
4. **Transport gate:** at least one UDP datagram is captured with the exact
   control profile and event ordering recorded.
5. Only after gates 1-4 may a separate Rust P4 transport specification be
   proposed. P3/P5 decoding and capture remain later specifications.

## Verification

- Python unit tests cover exact scaling parsing, full-echo fail-closed behavior,
  Message 1 serialization/parsing, template validation, sanitized fixture
  manifests, and UDP capture over real localhost sockets.
- Fake-server tests exercise real helper behavior without opening a target
  connection.
- Ghidra scripts run headlessly and record slice-specific results.
- The existing Rust formatting, tests, and builds remain green after the
  reverse-tool changes.
