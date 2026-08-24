# ARD P1/P2 Evidence-Bounded Redesign

## Goal

Replace the current reactive dynamic-resolution and heuristic MVS paths with:

1. a confirmation-driven display-generation state machine; and
2. exact MVS record reassembly plus conservative full-frame resynchronization
   for partial updates whose field layout is not yet proven.

## Verified Evidence

- Apple exposes dynamic resolution as an explicit session mode. Availability
  requires an AVC session, controlling rather than observing, a non-paused
  session, and a display configuration accepted by
  `dynamicResolutionModeAvailable`.
- ARD zoom operations change behavior while dynamic resolution is active.
  `windowDidResize:` itself only updates UI geometry.
- Resolution/codec changes reallocate the server's MultiVariant buffers and
  call codec-change handling.
- MVS full and partial updates use separate decoder paths. The partial path is
  `DecodeMVSPartialUpdate` and validates the byte sequence `6d 76 73` through
  a bit reader.
- In a captured fragmented full update, the first media record contributes
  32748 payload bytes and the following opaque continuation contributes 26572
  bytes, exactly matching the declared total of 59320 bytes.
- Available offline samples establish the `00 0f 19` full-update prefix. They
  do not contain a trustworthy `01 0e 13` partial fixture.

## Evidence Boundary

- Do not decode a type-1 payload as an independent JPEG.
- Do not invent the partial bit-field layout, reference-coefficient rules, or
  `mvs` marker offset.
- `0x09` is verified to carry display dimensions and start the MVS capture
  path. Using a resized `0x09` as a dynamic-resolution request remains an
  explicit, opt-in protocol experiment until acknowledged by a matching
  `ServerState`.
- A resize is never committed from a mismatching or unsolicited
  `ServerState`.

## P2 Architecture

### Record layer

`MvsRecordAssembler` owns fragmentation before normal media classification.

- `begin(rect, total, first_payload)` starts or completes a record.
- While a record is pending, the next decrypted application frames are opaque
  continuation bytes, not new RFB media headers.
- Completion requires exactly `total` payload bytes.
- Overflow, a second start while pending, zero total, or continuation without
  a pending record is a structural error and aborts the pending record.
- No four-byte prefix is removed from the assembled payload.

### Payload layer

`parse_mvs_payload` recognizes only:

- full update: `[00 0f 19][u32 metadata][JPEG-compatible entropy]`;
- partial update: `[01 0e 13][opaque partial bytes]`.

Everything else is malformed. Table initialization remains the existing
zero-sized MVS rectangle carrying at least 128 quantization-table bytes.

### Decoder-generation layer

`MvsDecodeState` contains:

- current display generation;
- installed quantization tables;
- whether a full reference frame has been applied;
- whether a full-frame resynchronization is outstanding.

Its decisions are:

- `DecodeFull`: generation and tables are valid and the payload is full;
- `RequestFull`: tables/reference are missing, payload is malformed, or a
  partial update is encountered before exact partial support exists;
- `IgnoreStale`: the update belongs to an older display generation.

Only a successful full decode calls `mark_full_applied`. Resynchronization is
rate-limited by the viewer and uses a non-incremental framebuffer request.

## P1 Architecture

### Pure controller

`DynamicResolutionController` has the following states:

- `Unavailable`;
- `Disabled { stable }`;
- `Stable { generation, size }`;
- `Pending { generation, previous, target }`;
- `Switching { generation, previous, target }`.

The controller is enabled only when the user explicitly opts in and the HPSS
capability gate is true.

- A debounced viewport target creates `Pending` and emits a request containing
  the next generation.
- Only a matching `ServerState` changes `Pending` to `Switching` and emits a
  geometry commit.
- The commit increments the display generation exactly once.
- The first successfully applied full frame completes `Switching` to `Stable`.
- Timeout returns to the previous stable size without changing generation.
- Mismatching or unsolicited states are ignored.

### Viewer integration

The viewer owns one shared `DisplaySurface` containing generation, dimensions,
and framebuffer. A confirmed commit performs, in the reader thread:

1. reset `MvsDecodeState` to the new generation;
2. replace `DisplaySurface` under one mutex;
3. send a non-incremental full-frame request for the confirmed dimensions.

The render/input thread always reads dimensions from `DisplaySurface`; it does
not retain startup `w`/`h` values. The minifb window is resizable and uses its
current physical drawable size for rendering and pointer-coordinate mapping.

Viewport changes are debounced for 250 ms. Candidate dimensions are clamped to
`u16`, rounded down to an 8-pixel boundary, and ignored if either axis is below
64 pixels. Dynamic-resolution requests are opt-in through
`--dynamic-resolution`; the default remains off because the resized `0x09`
wire meaning is not yet fully proven.

## Recovery Rules

- Fragment overflow or malformed MVS payload: abort the record and request one
  full update, rate-limited to one request per 200 ms.
- Partial update: never change framebuffer pixels; request a full update.
- Display-generation commit: discard pending fragments and decoder reference
  state before requesting a full frame.
- Stale generation: ignore without mutating decoder or framebuffer.
- Dynamic-resolution timeout after two seconds: return to the previous stable
  state; do not replace the framebuffer.

## Acceptance Criteria

- `cargo test` passes.
- `cargo build --no-default-features` passes.
- Fragment tests reproduce the 32748 + 26572 = 59320 capture.
- Tests prove type-1 payloads never enter the JPEG full-frame path.
- Tests prove mismatching `ServerState` cannot commit a resize.
- Tests prove one matching acknowledgement produces exactly one new
  generation, clears MVS state, and requires a full frame.
- Viewer pointer mapping and rendering use current shared dimensions after a
  confirmed transition.
- No credential or target-machine secret is added to source, documentation,
  tests, commands, or logs.
