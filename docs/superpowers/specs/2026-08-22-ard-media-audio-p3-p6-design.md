# ARD P3-P6 Media, Audio, and Audit Design

> **Historical design snapshot — superseded.** Later static and bounded live
> validation recovered the exact AVC/SRTP/AAC-ELD P3/P4 path and disproved the
> P5 premise below: `RemoteMicrophone` mode 4 is consumed through a separate
> Apple AudioChat/IDS call, while the password-authenticated HPSS
> `SSUDPSender` socket does not consume PC-to-Mac RTP. The production
> `--udp-audio-input` entry point therefore fails closed. Treat blocked fields
> and proposed P5 flow in this snapshot as planning history; use
> `ard_re/P4_UDP_EVIDENCE.md`, `docs/ARD_PROTOCOL.md`, and
> `docs/ARD_SESSION_PROTOCOL.md` for current evidence and behavior.

## Scope

This design covers:

- P3: Mac-to-PC audio output.
- P4: Apple AVC media UDP transport.
- P5: PC-to-Mac audio input.
- P6: repository-wide correctness, resource-safety, and semantic-symbol cleanup.

P1 dynamic resolution and P2 MVS remain independent generation-bound state
machines. MVS evidence cannot be reused as proof for AVC media, UDP, SRTP, or
audio codec fields.

## Evidence Rules

Every wire field has one of three states:

1. **Verified**: recovered from a static encoder/consumer or an exact sanitized
   fixture.
2. **Candidate**: a static use is known, but its semantic name is incomplete.
3. **Blocked**: no implementation is permitted until an exact serializer,
   consumer, or differential fixture proves it.

Production code uses named constants and typed records. Numeric values may
appear directly only in byte-exact evidence fixtures whose provenance is stated
beside the fixture.

Credentials, target addresses, usernames, keys, and absolute capture paths are
never stored in this document, source, fixtures, manifests, or command lines.

## Corrected Control Flow

```text
authenticated HPSS session
    -> server InitializeUDPVideoStream
    -> server MediaStream Message 1 (encoding 0x3f2)
    -> client reserves/configures media objects
    -> client message 0x1c MediaStreamConfiguration
    -> server validates and forwards configuration to SSAgent
    -> server MediaStream Message 2 answer (encoding 0x3f2)
    -> negotiated UDP + SRTP/SRTCP data plane
```

Application message `0x08` is `SetServerScaling`, not a UDP capability query.
The observed `3fe6`, `3fed`, and `3fee` byte pairs are prefixes of big-endian
IEEE-754 scale values. Implementations must compare the complete scale field,
never dispatch by those prefixes as pseudo-subtypes.

## P4 Control Plane

### MediaStream Message 1 (verified)

The server sends one zero-sized RFB pseudo-rectangle with encoding `0x3f2`.
The complete wire record is 54 bytes:

| Wire field | Type | Meaning |
|---|---:|---|
| stream id | `u32 BE` | primary stream id `1` |
| rectangle | `4 x u16 BE` | all zero |
| encoding | `i32 BE` | media control encoding `0x3f2` |
| size | `u16 BE` | bytes after the size field, `36` |
| version | `u16 BE` | version `1` |
| kind | `u16 BE` | port announcement `1` |
| flags | `u32 BE` | message flags |
| audio descriptor | `u16 port + u32 flags` | base UDP port |
| video 1 descriptor | `u16 port + u32 flags` | base plus one |
| video 2 descriptor | `u16 port + u32 flags` | base plus two when present |
| reserved tail | 10 bytes | verified zero; semantics blocked |

Descriptor flag bit zero is always set for emitted nonzero-port descriptors.
Bit one tracks the corresponding HDR boolean for video. The exact semantic
name of bit zero remains blocked; presence is determined by the port, and the
parser uses bit zero only to enforce the recovered encoder invariant.

The Rust parser must reject wrong stream ids, non-zero rectangles, wrong
encoding/version/kind, declared-length mismatch, non-zero reserved bytes,
truncation, and trailing data.

### Client message 0x1c (verified header, opaque body)

The daemon consumes:

| Offset | Type | Meaning |
|---:|---:|---|
| `0x00` | `u8` | client message type `0x1c` |
| `0x01` | `u8` | candidate/reserved; blocked |
| `0x02` | `u16 BE` | `messageSize`; wire length is `messageSize + 4` |
| `0x04` | `u16 BE` | version |
| `0x06` | `u32 BE` | message flags |
| `0x0a` | `u16 BE` | audio blob size |
| `0x0c` | `u16 BE` | video 1 blob size |
| `0x0e` | `u16 BE` | video 2 blob size |
| `0x10` | `u16 BE` | video 3 blob size |
| `0x12` | `u16 BE` | video 4 blob size |
| `0x14...` | opaque | stream/network/crypto descriptors |

The verified daemon minimum is:

```text
0xd8 + audio_size + video1_size
     + (video2_size != 0 ? 0x5c + video2_size : 0)
```

The daemon accepts trailing extensions when the declared size is larger. The
old claim that this message has a fixed 96,564-byte minimum is false and must
not be retained in code or documentation.

No Rust serializer for the opaque body is allowed until the client serializer
or a sanitized real fixture proves its field layout.

### Message 2 answer (verified envelope, opaque extension)

The server answer is another `0x3f2` pseudo-rectangle. If the SSAgent answer is
`N` bytes, the RFB record is `N + 16` bytes. The encoder normalizes:

- payload size to `N - 2`;
- version to `2`;
- answer kind to `2`;
- answer flags and three following `u16` fields to network byte order.

Bytes after the verified header remain opaque.

### UDP lifecycle

`MediaTransport` owns all media sockets and state transitions:

```text
Idle -> PortsAnnounced -> LocalSocketsReady -> ConfigSent
     -> AnswerAccepted -> Active -> Closing/Failed
```

- Ports are validated before use; arithmetic uses checked operations.
- Each socket is bound before configuration is sent.
- Audio, video, RTP, and RTCP roles are typed; datagrams stay opaque until
  their framing is proven.
- Timeouts, datagram budgets, replay windows, and queue capacities are named
  policy constants.
- A failed control step closes the entire generation; stale datagrams cannot be
  applied to a later generation.

## SRTP/SRTCP Boundary

The Apple client statically proves, for audio and video 1:

- remote UDP port comes from Message 1;
- SRTP cipher-suite numeric value is `5`;
- SRTCP cipher-suite numeric value is `5`;
- Viewer-to-Server material is the send media key;
- Server-to-Viewer material is the receive media key.

The algorithm name, master-key length, master-salt length, KDF labels, RTP
profile, and SRTCP index format are blocked. A numeric suite value alone is not
enough to select or implement cryptography safely.

## P3 Audio Output

After P4 reaches `Active`, the receive pipeline is:

```text
UDP receive -> framing validation -> replay check -> SRTP open
            -> jitter/reorder buffer -> codec decoder -> PCM format adapter
            -> Windows audio renderer
```

The codec id, sample rate, channel layout, sample format, packet duration, and
timestamp clock are blocked. Strings such as `PCM` in an adjacent daemon path
are not proof that AVC MediaStream audio is LPCM.

Audio output may default on only after a valid negotiated audio descriptor and
complete codec configuration exist. Failure degrades audio only; it must not
corrupt the display generation.

## P5 Audio Input

PC-to-Mac input is explicit opt-in. The send pipeline is:

```text
Windows capture -> PCM format adapter -> negotiated encoder
                -> RTP packetizer -> SRTP seal -> UDP send
```

Capture starts only after the server accepts a send-capable audio
configuration. Muting stops payload transmission and clears queued microphone
samples. No placeholder codec or guessed format is sent to a real target.

## P6 Safety and Symbol Table

The audit fixes are ordered by remote exploitability:

1. pre-authentication parser panics and parameter validation;
2. unbounded allocation and update budgets;
3. encrypted send transaction ordering;
4. MVS generation, rectangle, and decoded-buffer validation;
5. strict ServerState and cursor parsing;
6. persistence/network error propagation;
7. secret input and explicit plaintext downgrade policy;
8. CLI scan bounds, warning cleanup, and documentation parity.

Protocol/resource symbols live in their owning modules. Required names include:

- `RFB_BANNER_LEN` and `parse_rfb_banner_bytes`;
- `APPLE_SRP_PADDED_BYTES`, `validate_apple_srp_group`, and
  `encode_srp_value_padded`;
- `MAX_FRAMEBUFFER_PIXELS`, `MAX_RECTS_PER_UPDATE`, and
  `MAX_UPDATE_RAW_BYTES`;
- `HEARTBEAT_MESSAGE_TYPE` and `HEARTBEAT_MESSAGE_LEN`;
- `PRIMARY_MEDIA_STREAM_ID`, `MEDIA_STREAM_CONTROL_ENCODING`, and typed
  Message 1/Message 2 records;
- generation-bound MVS and dynamic-resolution timeout/budget symbols.

## Acceptance Gates

### Local automated gates

- Python reverse-harness tests, including secret sanitation.
- Rust formatting.
- Rust tests with default and no-default feature sets.
- Rust builds with default and no-default feature sets.
- Clippy for all targets and feature sets with warnings denied.
- Top-level and `hpssview` help output.

### Live gates

- sanitized exact client `0x1c` fixture with provenance;
- accepted Message 2 answer;
- at least one datagram per negotiated active role with event ordering;
- SRTP/SRTCP suite and key-layout proof;
- Mac-to-PC audio rendered with verified format and bounded jitter;
- explicit-opt-in PC-to-Mac capture heard on the target with mute verified;
- reconnect, timeout, malformed control record, stale generation, and teardown
  tests.

Local unit tests cannot satisfy live gates and must never be reported as live
interoperability proof.
