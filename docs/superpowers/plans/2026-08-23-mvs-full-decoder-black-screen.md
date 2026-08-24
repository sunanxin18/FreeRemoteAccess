# Apple MVS Full Decoder Black-Screen Repair Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use `superpowers:test-driven-development` for every behavior change, `superpowers:systematic-debugging` for unexpected failures, and `superpowers:verification-before-completion` before any completion claim. If the user explicitly authorizes delegated execution, use `superpowers:subagent-driven-development`; otherwise execute inline.

**Goal:** Replace the incorrect JPEG interpretation of Apple MVS type-0 records with an evidence-based native decoder, so a password-authenticated ARD/Screen Sharing session can render a non-black screen without installing or changing anything on the Mac.

**Architecture:** Parse the Apple wire contract separately from bounded bit reading and tile decoding. Decode each type-0 record transactionally into a record-local RGB rectangle and a staged codec-state snapshot; mutate the generation-scoped decoder state only after the framebuffer rectangle has been accepted. Keep type-1 partial records fail-closed and request a full refresh. Route both the live viewer and offline capture path through the same decoder.

**Tech stack:** Rust 2021, built-in Rust tests, existing `anyhow`, `png`, `minifb`, HPSS/MVS capture support, Windows release build, client-only live testing against the authorized Mac.

**Approved design:** `docs/superpowers/specs/2026-08-23-mvs-full-decoder-black-screen-design.md`

## Global constraints

- Do not add a Mac helper, daemon, agent, launch item, audio device, or any other server-side component.
- Authenticate only with the Mac username/password through the existing local credential provider. Never put credentials in source, docs, command lines, logs, or captures.
- Do not use Apple ID credentials.
- Treat ARD 3.10 reverse-engineering evidence as the protocol authority. Do not infer type-1 fields from type-0 behavior.
- Type-0 means a codec full update for its rectangle; it does **not** imply the rectangle covers the entire display.
- Type-1 remains opaque and must trigger the existing rate-limited full resync without entering the JPEG or type-0 decoder.
- A malformed record must not partially mutate the framebuffer, tile cache, previous-index state, coefficient state, generation, or P1 complete-surface evidence.
- This working tree has no `.git` metadata. The checkpoint steps below replace commits with explicit diff inspection and test evidence; do not initialize a repository.
- Keep the current diagnostic improvements in `ard_re/run_live_hpss.py` and `ard_re/test_run_live_hpss.py`.
- Keep the local capture under `.superpowers/` local-only. Never copy it into source, docs, or a tracked fixture directory.

## Target API and ownership

The implementation should converge on these responsibilities; exact visibility may be reduced when a symbol is module-private.

```rust
// src/vnc/mvs_wire.rs
pub struct MvsTables {
    pub luminance: [u8; 64],
    pub chrominance: [u8; 64],
}

pub struct MvsFullRecord<'a> {
    pub scale_threshold_a: u8,
    pub scale_threshold_b: u8,
    pub mode_stream: &'a [u8],
    pub data_stream: &'a [u8],
}

pub enum MvsWirePayload<'a> {
    Tables(MvsTables),
    Full(MvsFullRecord<'a>),
    Partial(&'a [u8]),
}

pub fn parse_payload(payload: &[u8]) -> anyhow::Result<MvsWirePayload<'_>>;
```

```rust
// src/vnc/mvs_bitstream.rs
pub struct BitReader<'a> { /* bounded MSB-first cursor */ }

impl<'a> BitReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self;
    pub fn read_bits(&mut self, count: u8) -> anyhow::Result<u32>;
    pub fn read_u8(&mut self) -> anyhow::Result<u8>;
}

pub fn decode_repeat_count(reader: &mut BitReader<'_>) -> anyhow::Result<usize>;
```

```rust
// src/vnc/mvs_full.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedMvsRect {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

#[derive(Clone)]
pub struct MvsFullDecoder { /* tables + generation-scoped codec state */ }

pub struct PreparedMvsFull {
    decoded: DecodedMvsRect,
    next_decoder: MvsFullDecoder,
}

impl MvsFullDecoder {
    pub fn new(tables: MvsTables) -> Self;
    pub fn prepare(
        &self,
        record: &MvsFullRecord<'_>,
        width: usize,
        height: usize,
    ) -> anyhow::Result<PreparedMvsFull>;
    pub fn decoded(prepared: &PreparedMvsFull) -> &DecodedMvsRect;
    pub fn commit(&mut self, prepared: PreparedMvsFull) -> DecodedMvsRect;
}
```

`MvsDecodeState` in `src/vnc/mvs.rs` owns the generation-scoped `MvsFullDecoder`. The live viewer asks it to prepare a record, applies the returned RGB rectangle, and only then commits the prepared decoder state. Generation reset discards all tables, caches, coefficients, and previous cache index.

---

## Task 1: Correct and lock the Apple MVS wire contract

**Files:**

- Create: `src/vnc/mvs_wire.rs`
- Modify: `src/vnc/mod.rs`
- Modify: `src/vnc/mvs.rs`

### Step 1: Add failing parser tests

Write tests in `src/vnc/mvs_wire.rs` before implementation:

```rust
#[test]
fn parses_exact_type_two_tables() {
    let mut payload = vec![2];
    payload.extend(0u8..64);
    payload.extend(64u8..128);
    let MvsWirePayload::Tables(tables) = parse_payload(&payload).unwrap() else {
        panic!("expected tables");
    };
    assert_eq!(tables.luminance[0], 0);
    assert_eq!(tables.luminance[63], 63);
    assert_eq!(tables.chrominance[0], 64);
    assert_eq!(tables.chrominance[63], 127);
}

#[test]
fn type_two_rejects_non_129_byte_payload() {
    assert!(parse_payload(&[2; 128]).is_err());
    assert!(parse_payload(&[2; 130]).is_err());
}

#[test]
fn type_zero_uses_big_endian_u24_data_offset() {
    let payload = [0, 15, 25, 0, 0, 8, 0xaa, 0xbb, 0xcc, 0xdd];
    let MvsWirePayload::Full(full) = parse_payload(&payload).unwrap() else {
        panic!("expected full record");
    };
    assert_eq!(full.scale_threshold_a, 15);
    assert_eq!(full.scale_threshold_b, 25);
    assert_eq!(full.mode_stream, &[0xaa, 0xbb]);
    assert_eq!(full.data_stream, &[0xcc, 0xdd]);
}

#[test]
fn type_zero_rejects_offsets_before_header_or_after_record() {
    assert!(parse_payload(&[0, 1, 2, 0, 0, 5]).is_err());
    assert!(parse_payload(&[0, 1, 2, 0, 0, 7]).is_err());
}

#[test]
fn type_one_is_kept_opaque() {
    let payload = [1, 0x6d, 0x76, 0x73, 0xaa];
    assert!(matches!(
        parse_payload(&payload).unwrap(),
        MvsWirePayload::Partial(bytes) if bytes == payload
    ));
}
```

### Step 2: Run the RED test

Run:

```powershell
cargo test mvs_wire -- --nocapture
```

Expected: compilation/test failure because the module and parser do not exist.

### Step 3: Implement the smallest bounded parser

- Type 2 must be exactly 129 bytes: tag plus 64 luminance bytes and 64 chrominance bytes.
- Type 0 must be at least 6 bytes.
- Interpret bytes 3..=5 as an unsigned big-endian 24-bit offset from the beginning of the record.
- `mode_stream` is bytes `6..offset`; `data_stream` is bytes `offset..`.
- Reject an empty mode stream, offsets below 6, offsets beyond the payload, and unknown record tags.
- Return type 1 unchanged. Do not inspect or validate its internal fields.

### Step 4: Remove obsolete wire assumptions

Delete or rewrite tests in `src/vnc/mvs.rs` that assert:

- a 7-byte type-0 header,
- a 32-bit metadata field,
- a `00 0f 19` magic signature,
- a 64+64+trailing-parameter type-2 table layout.

Replace them with tests that call the new parser. Do not touch decoding yet.

### Step 5: Verify and inspect checkpoint

Run:

```powershell
cargo fmt -- --check
cargo test mvs_wire -- --nocapture
cargo test mvs::tests -- --nocapture
```

Inspect:

```powershell
git diff -- src/vnc/mod.rs src/vnc/mvs.rs src/vnc/mvs_wire.rs
```

If `git` reports that this is not a repository, use:

```powershell
Get-Content src\vnc\mvs_wire.rs
rg -n "u32|JPEG|jpeg|00 0f 19|header" src\vnc\mvs.rs src\vnc\mvs_wire.rs
```

Expected: parser tests pass and no active type-0 parsing code retains the old JPEG/u32 contract.

---

## Task 2: Implement the bounded MSB-first reader and repeat code

**Files:**

- Create: `src/vnc/mvs_bitstream.rs`
- Modify: `src/vnc/mod.rs`

### Step 1: Add RED tests for bit boundaries

Add tests covering:

```rust
#[test]
fn reads_msb_first_across_byte_boundary() {
    let mut bits = BitReader::new(&[0b1011_0010, 0b0110_0001]);
    assert_eq!(bits.read_bits(3).unwrap(), 0b101);
    assert_eq!(bits.read_bits(6).unwrap(), 0b100100);
    assert_eq!(bits.read_bits(7).unwrap(), 0b1100001);
}

#[test]
fn exhaustion_is_an_error_without_advancing() {
    let mut bits = BitReader::new(&[0x80]);
    assert!(bits.read_bits(9).is_err());
    assert_eq!(bits.read_bits(1).unwrap(), 1);
}

#[test]
fn rejects_bit_counts_above_word_width() {
    let mut bits = BitReader::new(&[0xff; 8]);
    assert!(bits.read_bits(33).is_err());
}
```

Transcribe `_GetRepeatCount_at_1c894dc84.c` into table-driven tests before implementing it. Include every branch boundary observed in the decompile, the maximum accepted count, and one truncated encoding for each prefix length. Name the cases by encoded bit pattern and expected repeat count; do not create a guessed generalized code.

### Step 2: Run RED

```powershell
cargo test mvs_bitstream -- --nocapture
```

Expected: missing module/symbol failures.

### Step 3: Implement reader and exact repeat decoder

- Read MSB first, matching `_BitReadGetBits_at_1c894dc14.c`.
- Bounds-check before mutating cursor state.
- Accept only `0..=32` in `read_bits`; `0` returns zero.
- Preserve enough cursor information in errors to identify stream name and bit offset once wrapped by the full decoder.
- Implement repeat decoding as a literal translation of the verified branch tree in `_GetRepeatCount`, with checked arithmetic and a practical tile-count upper bound supplied by the caller.

### Step 4: GREEN and checkpoint

```powershell
cargo fmt -- --check
cargo test mvs_bitstream -- --nocapture
```

Expected: all boundary and truncation tests pass; no unchecked slice access or shift remains in the reader.

---

## Task 3: Decode non-transform tile modes transactionally

**Files:**

- Create: `src/vnc/mvs_full.rs`
- Modify: `src/vnc/mod.rs`
- Test: `src/vnc/mvs_full.rs`

### Step 1: Define literal fixture helpers in tests only

Create a small `TestBitWriter` inside `#[cfg(test)]` that writes MSB-first fields. It is a test constructor, not a second production encoder. Add helpers for an 8x8 solid tile, a 16x8 two-tile rectangle, and the `0x6d` terminal byte.

### Step 2: Add failing mode tests

Cover the verified modes independently:

- Mode 0: emit a white 8x8 tile.
- Mode 1: copy the tile immediately to the left; reject it in column zero.
- Mode 2: copy the tile immediately above; reject it in row zero.
- Mode 3: decode a black/white bitmap and verify bit-to-pixel ordering.
- Mode 4: decode one-color and two-color bitmaps and verify both palette ordering and bitmap ordering.
- Non-multiple dimensions: a 10x9 rectangle must decode the clipped edges without writing outside the RGB vector.
- Terminal handling: missing, wrong, or trailing-incomplete `0x6d` markers must fail.
- Transactionality: after any failure, a second valid decode on the original decoder must produce the same pixels and cache/index state as a fresh decoder.

Each synthetic record must contain separate mode and data streams and terminate **both** with `0x6d` in the exact positions expected by ARD.

### Step 3: Run RED

```powershell
cargo test mvs_full::tests -- --nocapture
```

Expected: missing decoder/types or unimplemented mode errors.

### Step 4: Implement tile traversal and modes 0-4

- Traverse 8x8 tiles left-to-right and top-to-bottom.
- Keep the decoded working surface in RGB24 (`width * height * 3`) with checked multiplication.
- Clip right/bottom writes at the record rectangle bounds.
- Translate the mode and payload branch order literally from `_DecodeMVSUpdate_at_1c884ee70.c`.
- Keep mode repeat expansion bounded by remaining tile count.
- Use a cloned decoder state and record-local RGB buffer. `prepare` returns staged state; it does not mutate `self`.
- Require the exact `0x6d` terminal from both readers after the declared tile count. Reject premature terminal, missing terminal, or extra non-padding bits where the decompile rejects them.

### Step 5: GREEN and checkpoint

```powershell
cargo fmt -- --check
cargo test mvs_full::tests -- --nocapture
```

Expected: modes 0-4 and all transaction/failure tests pass without mode 5, 6, or 7 being silently accepted.

---

## Task 4: Add cache-index modes 6 and 7

**Files:**

- Modify: `src/vnc/mvs_full.rs`

### Step 1: Add RED tests for verified cache behavior

Tests must prove:

- Mode 6 reads an explicit 16-bit cache index and reproduces that cached 8x8 tile.
- Mode 7 uses `previous_cache_index + 1` with checked wrap/limit behavior.
- Mode 7 before any valid previous index fails closed.
- An out-of-range or uninitialized index fails without state mutation.
- Cache update/eviction order matches `_Cache_UpdateTile_at_1c894ed1c.c`; transcribe at least one collision/eviction sequence from the decompile into expected indices.
- A prepared decode changes no state until `commit`; dropping it leaves the decoder unchanged.

### Step 2: Run RED

```powershell
cargo test mvs_full::tests::cache -- --nocapture
```

If Rust's name filter does not select all new cases, run the full module filter.

### Step 3: Implement literal cache semantics

- Translate cache sizing, hashing/index selection, update order, and previous-index behavior from `_Cache_UpdateTile` and the mode 6/7 branches.
- Store complete RGB or decoded YCbCr tile material exactly as required by the Apple ordering; do not introduce an unrelated LRU policy.
- Include cache state in the staged decoder clone.

### Step 4: GREEN and checkpoint

```powershell
cargo fmt -- --check
cargo test mvs_full::tests -- --nocapture
```

Expected: cache modes pass and malformed cache references remain transactional.

---

## Task 5: Implement Rice coefficient expansion, inverse DCT, and color conversion

**Files:**

- Modify: `src/vnc/mvs_full.rs`
- Reference only: `ard_re/decomp/_ExpandDCRice_at_1c8852280.c`
- Reference only: `ard_re/decomp/_ExpandBlockRice_at_1c894dea0.c`
- Reference only: `ard_re/decomp/_PerformInverseDCT8By8_at_1c88525f8.c`
- Reference only: `ard_re/decomp/_InitializeIDCT_at_1c88515e0.c`
- Reference only: `ard_re/decomp/_ycc_xrgb_convert20to32Pixel_at_1c894eccc.c`

### Step 1: Add RED tests for signed Rice values

Before implementation, extract literal input/output vectors from `_ExpandDCRice` and `_ExpandBlockRice`:

- zero DC delta;
- smallest positive and negative deltas;
- fixed DC and AC unary-guard boundaries recovered from instruction-level evidence;
- AC end-of-block;
- AC zero run that lands on coefficient 63;
- zero run beyond coefficient 63;
- truncated unary prefix and truncated remainder;
- coefficient overflow/underflow.

Assert the full 64-element coefficient array in zig-zag order, not just a checksum.

### Step 2: Implement exact DC/AC expansion

- Use checked signed arithmetic.
- Preserve Apple zig-zag and predictor order.
- Reject out-of-range quotient, run, coefficient index, or truncated field.
- Keep luma and chroma Rice limits separate and sourced from type-0 bytes 1 and 2.

Run:

```powershell
cargo test mvs_full::tests::rice -- --nocapture
```

### Step 3: Add RED tests for IDCT and YCbCr conversion

Use deterministic vectors derived from the decompiled constants and integer operations:

- all-zero coefficients produce the neutral block expected by Apple;
- DC-only positive and negative blocks produce uniform pixels;
- one AC impulse validates row/column orientation;
- saturation clamps at 0 and 255;
- neutral chroma preserves luma;
- extreme Cb/Cr vectors verify RGB channel order and clipping.

Keep expected pixel arrays as explicit literals calculated independently from the Rust implementation. Do not snapshot the implementation's own first output.

### Step 4: Implement the Apple integer transform path

- Port initialization constants and operation order from `_InitializeIDCT` and `_PerformInverseDCT8By8`.
- Preserve integer widths, rounding points, shifts, saturation, and row/column order.
- Port `_ycc_xrgb_convert20to32Pixel` semantics, but emit RGB24 for the existing framebuffer application path.
- Avoid floating-point IDCT unless reverse evidence proves Apple used it.

Run:

```powershell
cargo test mvs_full::tests::idct -- --nocapture
cargo test mvs_full::tests::color -- --nocapture
```

### Step 5: Add and implement mode 5 integration tests

Construct synthetic mode-5 records containing:

- a DC-only 8x8 tile;
- two adjacent tiles whose DC predictor proves left-to-right update order;
- a 10x9 clipped rectangle;
- a malformed AC run after a valid first component, proving no partial commit;
- luma/chroma table values that produce visibly different expected pixels.

Implement mode 5 using the installed type-2 tables, persistent coefficient/predictor state where the decompile requires it, three component blocks, inverse DCT, and YCbCr conversion.

Run:

```powershell
cargo fmt -- --check
cargo test mvs_full::tests -- --nocapture
```

Expected: all seven verified modes pass synthetic vector tests and every malformed path remains fail-closed.

---

## Task 6: Replace the JPEG decoder orchestration in `mvs.rs`

**Files:**

- Modify: `src/vnc/mvs.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`

### Step 1: Add RED state-machine tests

Refactor tests around an outcome shaped like:

```rust
pub enum MvsDecodeDecision {
    Prepared(PreparedGenerationMvs),
    RequestFull(MvsResyncReason),
    IgnoreStale,
    TablesInstalled,
}
```

Test these sequences:

1. A type-0 record before type-2 tables requests full resync.
2. Type-2 installs tables but does not emit pixels.
3. A valid type-0 subrectangle prepares pixels even while complete-surface evidence is false.
4. The state remains unchanged before prepared-record commit.
5. Commit updates codec state and marks only codec synchronization.
6. A type-1 record requests full resync and never reaches `MvsFullDecoder`.
7. A malformed type-0 record requests resync and preserves previous committed codec state.
8. Generation change invalidates tables, cache, previous index, coefficients, pending preparation, and sync evidence.
9. A stale-generation prepare/commit is rejected.
10. Only an exact `x=0, y=0, width=surface_width, height=surface_height` committed type-0 record can report complete-surface evidence to P1.

### Step 2: Run RED

```powershell
cargo test mvs::tests -- --nocapture
```

Expected: old decision/decoder API does not satisfy the tests.

### Step 3: Integrate the new parser and decoder state

- Delete `decode_mvs_block` and all JPEG wrapper construction.
- Remove the `jpeg-decoder` direct dependency after verifying it has no other users:

```powershell
rg -n "jpeg_decoder|jpeg-decoder" src Cargo.toml Cargo.lock
```

- Own `MvsFullDecoder` inside the generation-bound receive state.
- Distinguish codec synchronization from full-display evidence.
- Preserve existing rate limiting for resync writes, but never suppress the internal transition to “needs full.”
- Make prepared-record commit generation-checked.

### Step 4: GREEN and dependency verification

```powershell
cargo fmt -- --check
cargo test mvs::tests -- --nocapture
cargo test --no-default-features mvs::tests -- --nocapture
rg -n "jpeg_decoder|jpeg-decoder|decode_mvs_block" src Cargo.toml
```

Expected: focused tests pass in both configurations and the obsolete JPEG path is absent.

---

## Task 7: Integrate transactional decode with the live viewer

**Files:**

- Modify: `src/vnc/hpss_viewer.rs`
- Test: `src/vnc/hpss_viewer.rs`

### Step 1: Add viewer regression tests first

Use the synthetic record builders from the MVS test support through public test helpers or small duplicated payload literals. Add tests proving:

- A valid type-0 subrectangle is decoded and applied while the receiver is awaiting complete-surface evidence.
- The rectangle uses the MVS record's `x`, `y`, `width`, and `height`, and clipping/geometry validation occurs before framebuffer mutation.
- Framebuffer apply failure does not commit codec state.
- Decoder failure leaves the framebuffer byte-for-byte unchanged and requests resync.
- A type-1 record leaves the framebuffer unchanged and requests resync.
- A valid complete-surface record commits pixels, codec state, and then P1 complete-surface evidence in that order.
- A stale prepared record cannot be committed after a dynamic-resolution generation change.
- Rendering and pointer mapping still use the current committed `DisplaySurface` generation.

### Step 2: Run RED

```powershell
cargo test hpss_viewer::tests -- --nocapture
```

Expected: the current “reject non-complete surface before decode” path fails at least the subrectangle test.

### Step 3: Reorder the receive pipeline

Implement the live path in this order:

1. Reassemble and classify the MVS record.
2. Validate generation and rectangle geometry.
3. Install type-2 tables, reject type-1, or prepare type-0.
4. Apply prepared RGB to the current-generation framebuffer rectangle.
5. Commit the staged codec state only if step 4 succeeds.
6. Update codec sync status.
7. Update complete-surface/P1 evidence only for an exact full-display rectangle.
8. Request a full record on any decode/apply failure without replacing a previously visible surface with black.

Improve logs so they distinguish:

- table installation;
- type-0 rectangle decoded/applied;
- type-1 unsupported/resync;
- malformed type-0/resync;
- exact complete-surface evidence;
- stale generation rejection.

Do not log payload bytes that may contain session data.

### Step 4: GREEN and checkpoint

```powershell
cargo fmt -- --check
cargo test hpss_viewer::tests -- --nocapture
cargo test mvs::tests mvs_full::tests -- --nocapture
```

If Cargo accepts only one filter, run each filter in a separate invocation.

Expected: the viewer can display valid subrectangles without weakening P1's exact complete-surface gate.

---

## Task 8: Route offline capture decoding through the same state machine

**Files:**

- Modify: `src/main.rs`
- Modify or create tests near the offline MVS capture reader

### Step 1: Add a failing sequential replay test

Create a deterministic in-memory `FRDMVS01` capture containing:

1. a type-2 table record;
2. a valid type-0 full rectangle;
3. a type-0 subrectangle that depends on committed codec/cache state;
4. a type-1 record that is reported as unsupported but does not corrupt the last decoded image.

Assert that offline replay:

- processes records in capture order;
- uses the same `MvsDecodeState`/`MvsFullDecoder` as the viewer;
- commits each successful rectangle to an offline surface;
- exports the final surface rather than selecting the largest entropy payload;
- reports counts of tables, decoded type-0, rejected type-1, malformed, and stale records.

### Step 2: Run RED

```powershell
cargo test offline_mvs -- --nocapture
```

Expected: current “largest entropy then JPEG decode” behavior fails.

### Step 3: Refactor offline decoding

- Extract a small sequential replay function callable from tests and the `hpss --png` branch.
- Reuse the production parser, generation state, transactional decoder, and rectangle application rules.
- Do not add a second MVS decoder or a special capture-only interpretation.
- Preserve explicit rejection of legacy capture files.

### Step 4: GREEN

```powershell
cargo fmt -- --check
cargo test offline_mvs -- --nocapture
cargo test --no-default-features offline_mvs -- --nocapture
```

Expected: deterministic offline replay produces the expected final RGB surface in both configurations.

---

## Task 9: Replay the authorized local capture as a non-source acceptance test

**Files:**

- Local input only: `.superpowers/sdd/mvs-full-fixture-current/current-full.mvs`
- Local output only: `.superpowers/sdd/mvs-full-fixture-current/current-full.png`
- Modify tests only if needed to expose an `#[ignore]` environment-driven replay acceptance test.

### Step 1: Add an ignored, environment-driven acceptance test if no offline CLI accepts a capture path

The test must:

- read `FRD_MVS_CAPTURE` and `FRD_MVS_PNG` only when explicitly invoked;
- remain `#[ignore]` in normal test runs;
- never embed an absolute path;
- assert at least one type-2 and one successfully decoded type-0 record;
- assert the output has at least two distinct RGB colors and is not all black;
- write the PNG only to the caller-supplied local path.

### Step 2: Run the local replay

```powershell
$env:FRD_MVS_CAPTURE = (Resolve-Path '.superpowers\sdd\mvs-full-fixture-current\current-full.mvs').Path
$env:FRD_MVS_PNG = (Join-Path (Split-Path $env:FRD_MVS_CAPTURE) 'current-full.png')
cargo test replay_external_mvs_capture -- --ignored --nocapture
Remove-Item Env:FRD_MVS_CAPTURE
Remove-Item Env:FRD_MVS_PNG
```

Expected:

- replay succeeds;
- output dimensions match negotiated/captured geometry handled by the capture;
- decoded type-0 count is greater than zero;
- output is not uniformly black;
- no type-1 record is claimed as decoded.

### Step 3: Visually inspect the PNG

Use the local image viewer to verify that the image contains coherent Mac desktop content rather than random color blocks, transposed tiles, channel-swapped pixels, or an all-black surface. If the image is structurally wrong, return to Task 5 vector tests and compare the first divergent tile against the ARD decompile; do not tune constants by appearance.

---

## Task 10: Full verification, latest release build, and live Mac validation

**Files:**

- No new source files expected
- Local logs/output under `.superpowers/sdd/` only

### Step 1: Run the full static/test matrix

Run each command separately and preserve its exit status:

```powershell
cargo fmt -- --check
cargo test
cargo test --no-default-features
cargo build
cargo build --no-default-features
cargo build --release
cargo run -- --help
cargo run -- hpssview --help
python -m unittest ard_re.test_run_live_hpss
```

Expected baseline:

- no formatting diff;
- all non-ignored Rust tests pass in both feature configurations;
- both build configurations pass;
- top-level and `hpssview` help render;
- diagnostic Python tests pass.

Do not compare only against the earlier 256/190 counts; report the new exact counts because decoder tests will increase them.

### Step 2: Audit forbidden regressions

Run:

```powershell
rg -n "jpeg_decoder|jpeg-decoder|decode_mvs_block|AppleID|Apple ID|frida-server" src Cargo.toml docs\superpowers\specs docs\superpowers\plans
rg -n "192\.168\.|nswd|password\s*[:=]" src docs ard_re --glob '!CREDENTIALS.local.md' --glob '!*.bin' --glob '!*.mvs'
```

Expected: no obsolete JPEG decoder path, no server helper requirement, and no credential/IP leakage introduced by this work. Review matches rather than assuming every documentation word is a secret.

### Step 3: Build and identify the exact release artifact

```powershell
cargo build --release
Get-FileHash .\target\release\freeremotedesk.exe -Algorithm SHA256
Get-Item .\target\release\freeremotedesk.exe | Select-Object FullName,Length,LastWriteTimeUtc
```

Use the exact reported artifact for live testing. Do not launch an older `.codex-target` executable unless its hash matches the new build.

### Step 4: Launch the client-only live viewer

Use the existing non-echoing local credential provider and `ard_re/run_live_hpss.py`/existing launcher. Never place the password on the command line. Connect to the already authorized Mac, with no software or settings changed on that Mac.

Collect locally:

- release executable hash;
- negotiated surface dimensions;
- table installation count;
- successfully applied type-0 rectangles;
- rejected/resynced type-1 count;
- decode/apply errors by category;
- current generation transitions;
- a screenshot of the Windows viewer window.

### Step 5: Live acceptance gates

All must hold before claiming the black-screen fix complete:

- The Windows viewer opens at the negotiated display size or the correctly scaled current surface.
- The window shows coherent, changing Mac desktop pixels rather than black.
- At least one type-0 record is decoded and applied by the native MVS path.
- No log contains `MVS JPEG 解码失败` or invokes the removed JPEG path.
- Type-1 remains explicitly unsupported and requests resync; it is not falsely reported as decoded.
- Pointer mapping uses the visible current surface dimensions.
- Resizing or a generation change cannot commit a stale prepared frame.
- No Mac companion/server program was installed or run.
- Authentication used only the existing Mac username/password flow.

If the viewer is still black, keep the task open and capture the first failing record index, rectangle, mode, tile index, stream bit offsets, and error category without dumping raw session payload. Reproduce that record through the local replay test, add a RED regression, then fix the smallest verified divergence.

### Step 6: Completion report

Report:

- files changed;
- exact test/build commands and pass counts;
- release path, timestamp, and SHA-256;
- offline replay decoded/rejected counts and non-black proof;
- live negotiated dimensions and type-0 apply evidence;
- whether type-1 remains blocked;
- any remaining P1/P2 limitations separately from the black-screen result.

Do not claim P2 type-1 incremental support. The intended result of this plan is native Apple type-0/full MVS rendering plus conservative type-1 resync.
