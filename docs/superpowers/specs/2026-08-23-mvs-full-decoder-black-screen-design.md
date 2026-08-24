# Apple MVS Full Decoder Black-Screen Repair Design

## Goal

Replace the incorrect JPEG-wrapper implementation with an evidence-bounded,
stateful decoder for Apple type-0 Multi Variant Stream updates so that the
Windows HPSS viewer displays the remote Mac instead of leaving its initial
framebuffer black.

## Scope

This repair implements type-0 full MVS records only. Type-1 partial records
remain opaque and continue to request a rate-limited type-0 refresh. The work
does not add a Mac companion, modify the stock Mac server, require Apple ID
credentials, or change the UDP/audio protocols.

The acceptance target is the existing username/password HPSS client path.
Dynamic-resolution evidence remains fail-closed and requires a successfully
applied record that covers the complete current surface.

## Verified Evidence

Apple Remote Desktop 3.10 links
`/System/Library/PrivateFrameworks/ScreenSharing.framework/Versions/A/ScreenSharing`.
The relevant decoder is `DecodeMVSUpdate` in
`RFBViewerLib/DecodeMultiVariant.c`, recovered from the local
`ScreenSharing.framework.current` Ghidra project.

The current authorized-Mac capture contains 421 complete `FRDMVS01` records:

- one type-2 table record;
- 361 type-0 records;
- 59 type-1 records.

The first type-2 record is exactly 129 bytes and begins with `0x02`. Apple
copies bytes 1 through 64 into the luminance table and bytes 65 through 128
into the chrominance table.

A type-0 record has this verified header:

```text
byte 0      update type = 0x00
byte 1      luminance/Rice parameter
byte 2      chrominance/Rice parameter
bytes 3..5  big-endian u24 image-stream offset
byte 6..    mode stream
byte offset.. image/data stream
```

The offset is relative to the beginning of the record. Apple initializes one
bit reader at byte 6 and a second at the u24 offset. It decodes 8-by-8 tiles,
requires an `0x6d` terminal byte from each stream, and does not pass the record
to a standalone JPEG decoder.

Type-0 means a self-contained codec update, not necessarily a rectangle that
covers the whole display. The live capture contains both a `1456x1080` type-0
record and many smaller type-0 rectangles while the negotiated surface is
`1920x1080`.

## Root Cause

The current implementation treats `00 0f 19` as a three-byte signature,
reads the following four bytes as metadata, starts entropy at byte 7, and
wraps the remainder with synthetic JPEG headers and Annex K Huffman tables.
This consumes the first data byte as metadata and feeds a proprietary
dual-stream block format to a baseline JPEG decoder. Full-record decode then
fails with `failed to decode huffman code`, no pixels are applied, and the
initial zero-filled framebuffer remains black.

The viewer also conflates a type-0 codec update with complete-surface
coverage. While awaiting synchronization it rejects valid type-0 subrectangles
before classification. That rule must be split: a valid type-0 record may be
rendered, while complete-surface coverage remains a separate P1 evidence bit.

## Selected Approach

Implement a pure Rust decoder that follows the recovered Apple type-0 data
flow. Do not retain the JPEG-wrapper fallback: it is contradicted by the wire
grammar and would hide future protocol errors.

Two alternatives were rejected:

- forcing standard RFB encoding would bypass HPSS/MVS and break the basis for
  P1, P2, and later media work;
- changing the entropy start from byte 7 to byte 6 would still send the custom
  tile stream to a JPEG decoder.

## Components

### `src/vnc/mvs_wire.rs`

Own strict wire parsing only.

- `MvsTables::parse` accepts exactly `0x02` followed by two 64-byte tables.
- `MvsFullRecord::parse` accepts type `0x00`, reads the two parameters and u24
  offset, and returns bounded mode/data slices.
- The offset must be at least 6 and no greater than the payload length.
- Type-1 remains represented as opaque encoded bytes.

No pixel decoding, generation state, or framebuffer mutation belongs here.

### `src/vnc/mvs_bitstream.rs`

Own an MSB-first bounded bit reader.

- Reads 1 through 32 bits without reading beyond its assigned stream.
- Decodes Apple's repeat-count prefix exactly.
- Provides explicit remaining-byte/bit state for terminal-marker validation.
- Every truncation returns an error; no synthetic zero padding is permitted.

### `src/vnc/mvs_full.rs`

Own type-0 tile decoding and generation-scoped codec state.

- Iterate the rectangle in 8-by-8 tiles, including clipped right/bottom
  edges.
- Decode a three-bit tile mode and its repeat count from the mode stream.
- Decode image parameters, cache indices, or Rice/DCT coefficients from the
  data stream.
- Decode into a record-local RGB buffer. Publish no pixels until the complete
  record and both terminal markers validate.
- Maintain only the Apple state needed by type-0: quantization tables, tile
  cache, the last coefficient block, and the last cache index.

The verified modes are:

| Mode | Behavior |
| --- | --- |
| 0 | Fill tile white |
| 1 | Copy the tile immediately to the left |
| 2 | Copy the tile immediately above |
| 3 | Decode a black/white bitmap |
| 4 | Decode a one-color or two-color bitmap |
| 5 | Decode Rice-compressed DCT coefficients, inverse-transform, convert to RGB |
| 6 | Load an explicit 16-bit tile-cache index |
| 7 | Load the cache index following the previously used index |

Mode 5 uses the recovered JPEG natural coefficient order, separate luminance
and chrominance quantization tables, a deterministic integer 8-by-8 IDCT, and
clamped YCbCr-to-RGB conversion. It does not reconstruct a JPEG file.

### `src/vnc/mvs.rs`

Retain generation/readiness orchestration and remove JPEG construction.

- Install parsed type-2 tables into the current generation.
- Route type-0 records to `MvsFullDecoder`.
- Route type-1 records to `RequestFull(UnsupportedPartial)`.
- Reset tables, cache, coefficient references, and readiness on generation
  change.
- Mark a full codec reference only after an entire type-0 record succeeds.

### `src/vnc/hpss_viewer.rs`

Separate codec completeness from display coverage.

- Classify the record before applying any awaiting-full geometry rule.
- Permit a successfully decoded type-0 subrectangle to update the current
  surface.
- Preserve the old surface on any decode failure.
- Continue using exact complete-surface coverage for dynamic-resolution
  evidence and generation switching.
- Type-1 remains unable to mutate the surface and requests a full refresh.

The offline `hpss --png` path must call the same decoder as the viewer so that
capture replay and live rendering cannot drift.

## Data Flow

```text
encrypted application frames
  -> exact MVS record reassembly
  -> type 0/1/2 wire classification
  -> generation readiness
  -> type-0 dual-stream tile decode into temporary RGB rectangle
  -> terminal-marker and exact-layout validation
  -> atomic framebuffer rectangle application
  -> complete-surface evidence (only when geometry exactly matches)
```

Transport reassembly never interprets MVS bits. Wire parsing never mutates
decoder state. The decoder never mutates the shared framebuffer. The viewer is
the sole owner of committing a fully decoded rectangle.

## Error Handling and Resource Bounds

The decoder fails closed for:

- table records with a wrong type, rectangle, or length;
- full headers shorter than six bytes;
- offsets below six or beyond the payload;
- bit reads beyond either assigned stream;
- repeat counts exceeding the remaining tile count;
- left/top copy modes without a valid source tile;
- cache misses or out-of-range cache indices;
- Rice values, coefficient counts, or shifts outside recovered bounds;
- missing or wrong `0x6d` terminal markers;
- rectangle or decoded RGB lengths outside existing protocol budgets.

On failure, the record-local buffer is dropped, the last displayed surface is
preserved, the decoder enters awaiting-full state, and the existing rate-limited
non-incremental refresh path is used. No malformed record may partially update
the cache, coefficient reference, or framebuffer; state changes are committed
only with successful record completion.

## Testing Strategy

Every production behavior is implemented with a witnessed RED/GREEN cycle.
Hand-derived literal fixtures, not helper-generated expectations, cover:

- type-2 table indexing and exact length;
- type-0 parameters, u24 offset, and stream slicing;
- bounded bit reads and repeat-count boundaries;
- deterministic pixels for modes 0 through 4 and 6 through 7;
- Rice coefficient reconstruction, quantization, integer IDCT, and RGB output
  for mode 5;
- clipped tiles for rectangles not divisible by eight;
- offset, stream, repeat, cache, and terminal-marker failures;
- transactional decoder/cache state on failure;
- generation reset;
- type-0 subrectangle application while awaiting full;
- complete-surface evidence remaining false for a subrectangle;
- type-1 continuing to request full without entering the type-0 decoder.

The current 14 MB live capture remains a local-only acceptance artifact under
`.superpowers/sdd`; it must not be added to source, docs, or generated test
fixtures. Offline acceptance replays it through the decoder and checks exact
RGB lengths, bounded execution, and non-black output. It does not establish
pixel correctness by itself; hand-derived fixtures own that proof.

## Verification Matrix

- `cargo fmt -- --check`
- focused RED/GREEN tests for each decoder layer
- `cargo test`
- `cargo test --no-default-features`
- `cargo build --release` with an isolated `CARGO_TARGET_DIR`
- `cargo build --release --no-default-features` with the same isolated target
- top-level and `hpssview` help output
- offline replay of the local current-Mac capture
- bounded live username/password HPSS viewer test
- Windows window inspection showing non-black pixels
- sanitized logs containing no JPEG/Huffman failure and no credentials

## Live Acceptance Criteria

- The current Mac remains unmodified and runs only stock Screen Sharing.
- The latest isolated release authenticates with the local Mac username and
  password provider.
- A non-black remote image appears within the bounded test interval.
- Valid type-0 subrectangles can update the display before complete-surface
  evidence is available.
- Type-1 refresh handling does not erase already displayed pixels.
- No `MVS JPEG` or `failed to decode huffman code` diagnostic remains.
- Dynamic-resolution generation, surface dimensions, pointer mapping, and
  existing audio/UDP behavior do not regress.

## Non-Goals

- Type-1 partial MVS field recovery or decoding.
- AVC/HEVC video transport changes.
- UDP framing, Mac-to-PC audio, or PC-to-Mac audio changes.
- A server-side helper, daemon, injected library, Frida server, or privileged
  Mac process.
- Apple ID or IDS invitation support.
