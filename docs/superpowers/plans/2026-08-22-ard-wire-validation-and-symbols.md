# ARD Wire Validation and Semantic Symbols Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the confirmed MVS, RFB, SRP/RSA-SRP, Apple session, HPSS, media-control, JPEG, and RTCP validation gaps while replacing production wire magic numbers with owner-scoped semantic symbols and typed builders.

**Architecture:** Each protocol module owns its own wire types and exports only values shared by consumers. Parsers reject truncation, overflow, wrong generation, and exact-layout mismatches before allocation or state mutation; fixtures remain independent byte arrays under tests. Unknown but verified Apple payloads are labeled opaque/candidate rather than assigned guessed semantics.

**Tech Stack:** Rust 2021, `anyhow`, RFB/Apple private protocols, SRP-6a, RSA PKCS#1 v1.5, MVS/JPEG, unit and loopback tests.

**Spec:** `docs/superpowers/specs/2026-08-22-ard-p3-p4-p6-hardening.md`

## Global Constraints

- Exact type-1 MVS partial decoding remains blocked; never feed partial data to the JPEG path.
- `MVS_TABLE_INITIALIZATION_BYTES` is exactly 129 and table rectangles are exactly zero in all four fields.
- Every numeric wire field in production is referenced through a named constant, enum, typed field, or named opaque evidence blob.
- Standard JPEG Annex K lookup tables, X11 keysym mappings, Apple OUI data, and independent test fixtures remain allowed raw data tables.
- No parser may truncate a length/security type with `as`; use checked conversion before allocation or write.
- This workspace has no Git metadata. Do not initialize Git; record a verification checkpoint after each task.

---

### Task 1: Enforce the exact MVS initialization record contract

**Files:**
- Modify: `src/vnc/mvs.rs:16-43,83-239,335-end`
- Modify: `src/vnc/hpss_viewer.rs:943-1060,2390-2430`
- Modify: `src/vnc/hpss.rs:160-260,650-680,980-1060`
- Test: `src/vnc/mvs.rs` test module and `src/vnc/hpss_viewer.rs` test module

**Interfaces:**
- Consumes: `mvs_stream::MvsRect`, existing `MvsDecodeState` generation checks.
- Produces: `MVS_TABLE_INITIALIZATION_BYTES`, `MvsTables::initialization_parameter`, `MvsRecordKind`, and `classify_mvs_record`.

- [ ] **Step 1: Write RED tests for 128/129/130 bytes and all rectangle fields**

Add:

```rust
#[test]
fn mvs_table_initialization_requires_exact_length_and_preserves_parameter() {
    assert!(parse_tables(&[0; MVS_TABLE_INITIALIZATION_BYTES - 1]).is_err());
    assert!(parse_tables(&[0; MVS_TABLE_INITIALIZATION_BYTES + 1]).is_err());
    let mut exact = [0u8; MVS_TABLE_INITIALIZATION_BYTES];
    exact[MVS_TABLE_INITIALIZATION_PARAMETER_OFFSET] = 0x5a;
    let tables = parse_tables(&exact).unwrap();
    assert_eq!(tables.initialization_parameter, 0x5a);
}

#[test]
fn mvs_table_record_requires_a_completely_zero_rectangle() {
    let payload = [0u8; MVS_TABLE_INITIALIZATION_BYTES];
    let zero = MvsRect { x: 0, y: 0, width: 0, height: 0 };
    assert!(matches!(classify_mvs_record(zero, &payload).unwrap(), MvsRecordKind::Tables(_)));
    for nonzero in [
        MvsRect { x: 1, ..zero },
        MvsRect { y: 1, ..zero },
        MvsRect { width: 1, ..zero },
        MvsRect { height: 1, ..zero },
    ] {
        assert!(classify_mvs_record(nonzero, &payload).is_err());
    }
}
```

Use a raw literal only for the test-only sentinel `0x5a`; do not construct expected bytes from production constants except the public length contract being tested.

- [ ] **Step 2: Run RED tests**

```powershell
cargo test mvs_table_initialization_requires_exact_length_and_preserves_parameter
cargo test mvs_table_record_requires_a_completely_zero_rectangle
```

Expected: the first test fails because 128 and 130 bytes are currently accepted and the parameter is absent; the second fails because no rectangle classifier exists.

- [ ] **Step 3: Implement exact symbols and classifier**

Add:

```rust
pub const MVS_QUANTIZATION_TABLE_BYTES: usize = 64;
pub const MVS_TABLE_INITIALIZATION_PARAMETER_BYTES: usize = 1;
pub const MVS_TABLE_INITIALIZATION_BYTES: usize =
    2 * MVS_QUANTIZATION_TABLE_BYTES + MVS_TABLE_INITIALIZATION_PARAMETER_BYTES;
pub const MVS_TABLE_INITIALIZATION_PARAMETER_OFFSET: usize =
    2 * MVS_QUANTIZATION_TABLE_BYTES;
pub const MVS_FULL_FRAME_SIGNATURE: [u8; 3] = [0x00, 0x0f, 0x19];
pub const MVS_PARTIAL_FRAME_SIGNATURE: [u8; 3] = [0x01, 0x0e, 0x13];

pub struct MvsTables {
    pub luminance: [u8; MVS_QUANTIZATION_TABLE_BYTES],
    pub chrominance: [u8; MVS_QUANTIZATION_TABLE_BYTES],
    pub initialization_parameter: u8,
}

pub enum MvsRecordKind<'a> {
    Tables(&'a [u8]),
    Frame(&'a [u8]),
}
```

`parse_tables` must use `ensure!(init.len() == MVS_TABLE_INITIALIZATION_BYTES, ...)`. `classify_mvs_record` returns `Tables` only when all rectangle fields are zero and the payload length is exact; a partially zero rectangle with table-length payload is an error. All other nonzero-size rectangles return `Frame` and continue through the existing full/partial payload classifier.

- [ ] **Step 4: Route headless and viewer record handling through the classifier**

Replace `width == 0 && height == 0` checks in `hpss.rs`, `hpss_viewer.rs`, and the PNG capture path with `classify_mvs_record`. In the interactive viewer, an invalid candidate table record calls existing `request_full`/rate-limited update logic and returns `RecoveryRequested`; it must not terminate the reader or enter JPEG decode. The headless capture path preserves the complete record as evidence, emits one bounded malformed-table diagnostic, does not increment `table_inits`, and never attempts JPEG decode from it.

- [ ] **Step 5: Run GREEN and P2 regressions**

```powershell
cargo test mvs_table_
cargo test malformed_mvs_
cargo test partial_mvs_
cargo test dynamic_resolution_
```

Expected: all pass and the captured 32748+26572 reassembly behavior remains unchanged.

- [ ] **Step 6: Record checkpoint**

Record exact accepted/rejected MVS shapes and confirm `initialization_parameter` is preserved but unused.

---

### Task 2: Type standard RFB messages and reject RFB 3.3 security truncation

**Files:**
- Modify: `src/vnc/protocol.rs:1-180`
- Modify: `src/vnc/client.rs:285-610,833-end`
- Modify: `src/viewer.rs:53-70`
- Test: `src/vnc/client.rs` and `src/vnc/protocol.rs`

**Interfaces:**
- Produces: `RfbClientMessageType`, `RfbServerMessageType`, `security::APPLE_ARD_39`, `security::requires_apple_account_credentials`, and `msg_set_encodings(...) -> Result<Vec<u8>>`.

- [ ] **Step 1: Add RED tests for security type 257 and credential hints**

Build a loopback server that sends `RFB 003.003\n` followed by `257u32.to_be_bytes()` and assert:

```rust
let error = negotiate(&addr, Duration::from_secs(1)).unwrap_err();
assert!(error.to_string().contains("超出 u8"));
```

Add table-driven tests for Apple types 30, 33, 35, and 36 without credentials:

```rust
for apple_type in [
    protocol::security::APPLE_ARD,
    protocol::security::APPLE_RSA_SRP,
    protocol::security::APPLE_ARD_39,
    protocol::security::APPLE_SRP,
] {
    assert!(protocol::security::requires_apple_account_credentials(apple_type));
    assert!(pick_security(&[apple_type], None, None).is_err());
}
```

- [ ] **Step 2: Run RED tests**

```powershell
cargo test rfb_33_rejects_security_type_that_does_not_fit_u8
cargo test every_apple_account_security_type_requires_credentials
```

Expected: 257 is currently truncated to 1 and type 35 lacks a constant/helper.

- [ ] **Step 3: Define single-owner enums/constants**

Add `#[repr(u8)]` enums for client message types 0/2/3/4/5 and server message types 0/1/2/3, plus named header/padding/count lengths. Keep public builders as the only serialization API. Add:

```rust
pub const APPLE_ARD_39: u8 = 35;

pub const fn requires_apple_account_credentials(value: u8) -> bool {
    matches!(value, APPLE_ARD | APPLE_RSA_SRP | APPLE_ARD_39 | APPLE_SRP)
}
```

Rewrite `security_type_name` and `pick_security` to use only symbols. For 3.3:

```rust
let security_type = u8::try_from(t)
    .context("RFB 3.3 security type 超出 u8 表示范围")?;
vec![security_type]
```

Rewrite `read_server_message` dispatch to use `RfbServerMessageType` conversion, and name the ColourMap padding/entry width, ServerCutText padding and maximum length, RFB pixel width, VNC challenge length, ServerInit pixel-format length, desktop-name budget, and failure-reason budget. Checked multiplication replaces `6 * n as usize`; cursor and Apple-state values continue to come from `hpss` owners.

- [ ] **Step 4: Make SetEncodings length checked**

Change:

```rust
pub fn msg_set_encodings(encodings: &[i32]) -> Result<Vec<u8>> {
    let count = u16::try_from(encodings.len()).context("RFB SetEncodings 数量超过 u16")?;
    // serialize with RfbClientMessageType::SetEncodings and named padding/header constants
}
```

Update every call site to propagate `?`. Add a boundary test using `vec![RAW; usize::from(u16::MAX) + 1]` and assert failure before serialization.

- [ ] **Step 5: Run GREEN**

```powershell
cargo test rfb_33_
cargo test every_apple_account_
cargo test set_encodings_
cargo test --no-default-features
```

Expected: all pass; no raw security type 35 remains in production.

- [ ] **Step 6: Record checkpoint**

Record the enum/constant owners and all changed builder call sites.

---

### Task 3: Replace unchecked SRP and RSA-SRP TLV length casts

**Files:**
- Modify: `src/vnc/srp.rs:32-145,280-350`
- Modify: `src/vnc/rsa_srp.rs:25-165`
- Test: both modules' test sections

**Interfaces:**
- Produces: `SrpTlvBuilder`, checked outer-frame helpers, `RsaSrpFrameVersion`, and named RSA1/response/resource symbols.

- [ ] **Step 1: Add RED boundary tests**

```rust
#[test]
fn srp_tlv_builder_rejects_values_above_wire_widths() {
    let mut builder = SrpTlvBuilder::new();
    assert!(builder.push_sized_u8(&vec![0; usize::from(u8::MAX) + 1]).is_err());
    assert!(builder.push_sized_u16(&vec![0; usize::from(u16::MAX) + 1]).is_err());
}

#[test]
fn srp_tlv_builder_encodes_wire_boundary_lengths() {
    let mut builder = SrpTlvBuilder::new();
    builder.push_sized_u8(&vec![0; usize::from(u8::MAX)]).unwrap();
    builder.push_sized_u16(&vec![0; usize::from(u16::MAX)]).unwrap();
    let encoded = builder.finish().unwrap();
    assert_eq!(u32::from_be_bytes(encoded[..4].try_into().unwrap()) as usize, encoded.len() - 4);
}
```

Add an authentication-construction test with an over-`u16` username and assert no bytes reach the loopback socket before the error.

- [ ] **Step 2: Run RED tests**

```powershell
cargo test srp_tlv_builder_
cargo test oversized_srp_username_fails_before_socket_write
```

Expected: builder API does not exist; current casts would wrap.

- [ ] **Step 3: Implement the checked builder**

```rust
pub(crate) struct SrpTlvBuilder {
    items: Vec<u8>,
}

impl SrpTlvBuilder {
    pub(crate) fn new() -> Self { Self { items: Vec::new() } }

    pub(crate) fn push_sized_u8(&mut self, data: &[u8]) -> Result<()> {
        let len = u8::try_from(data.len()).context("SRP %o 长度超过 u8")?;
        self.items.push(len);
        self.items.extend_from_slice(data);
        Ok(())
    }

    pub(crate) fn push_sized_u16(&mut self, data: &[u8]) -> Result<()> {
        let len = u16::try_from(data.len()).context("SRP %s/%m 长度超过 u16")?;
        self.items.extend_from_slice(&len.to_be_bytes());
        self.items.extend_from_slice(data);
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<Vec<u8>> {
        let len = u32::try_from(self.items.len()).context("SRP TLV 总长超过 u32")?;
        let mut payload = len.to_be_bytes().to_vec();
        payload.extend_from_slice(&self.items);
        Ok(payload)
    }
}
```

Add named helpers for checked `u16/u32` outer lengths. Replace `item_s`, `item_o`, `tlv_payload`, and every `as u16/u32` serializer in both authentication paths.

- [ ] **Step 4: Name RSA-SRP framing values without guessing opaque fields**

Define `RSA1_MAGIC`, `RsaSrpFrameVersion::{PublicKeyRequest, EncryptedSrp}`, `RSA_SRP_RESPONSE_TAG`, `RSA_PUBLIC_KEY_FRAME_MIN_BYTES`, `RSA_PUBLIC_KEY_FRAME_MAX_BYTES`, and `RSA_PUBLIC_KEY_MAX_BITS`. Use `protocol::security::APPLE_RSA_SRP` instead of `0x21`. Keep the 23-byte response tail opaque and length-checked; do not name its content.

- [ ] **Step 5: Run GREEN and cryptographic regressions**

```powershell
cargo test srp_tlv_builder_
cargo test oversized_srp_username_
cargo test srp_
cargo test rsa_srp_
cargo test key_derivation_vector
```

Expected: all pass and no production TLV length cast remains.

- [ ] **Step 6: Record checkpoint**

Record maximum accepted widths and confirm errors occur before socket writes.

---

### Task 4: Serialize Apple session commands from typed records

**Files:**
- Modify: `src/vnc/session.rs:202-290`
- Modify: `src/vnc/hpss.rs:29-60`
- Modify: `src/vnc/media_protocol.rs:1-30`
- Modify: `src/vnc/media_negotiation.rs:16-205`
- Test: `src/vnc/session.rs` and `src/vnc/media_negotiation.rs`

**Interfaces:**
- Produces: `AppleSessionCommand`, `EncryptionCommand`, `EncryptionMethod`, `SessionEncodingProfile::encodings`, and single-owner media-control constants.

- [ ] **Step 1: Preserve current wire bytes as independent RED fixtures**

Move copies of the four current session messages into the test module as literal `EXPECTED_*` fixtures. Add tests that call planned builders:

```rust
assert_eq!(build_select_session(), EXPECTED_SELECT_SESSION);
assert_eq!(build_set_encryption(SessionEncodingProfile::Raw).unwrap(), EXPECTED_AES_RAW);
assert_eq!(build_set_encryption(SessionEncodingProfile::AppleTcpMvs).unwrap(), EXPECTED_AES_APPLE);
assert_eq!(build_set_encryption(SessionEncodingProfile::AppleUdpMedia).unwrap(), EXPECTED_AES_UDP);
assert_eq!(build_encryption_activation(), EXPECTED_ENCRYPTION_ON);
```

The fixture arrays must not reference production constants.

- [ ] **Step 2: Run RED tests**

```powershell
cargo test typed_session_builders_match_independent_wire_fixtures
```

Expected: builder functions do not exist.

- [ ] **Step 3: Implement typed command envelopes and encoding lists**

Define:

```rust
#[repr(u8)]
enum AppleSessionCommand {
    EncodingOrEncryption = 0x12,
    SessionSelect = 0x21,
}

#[repr(u32)]
enum EncryptionMethod { Aes128 = 1 }

#[repr(u32)]
enum EncryptionCommand { Negotiate = 1, Activate = 2 }
```

Create named `APPLE_TCP_MVS_ENCODINGS: &[i32]`; derive the UDP list by prefixing `media_protocol::MEDIA_STREAM_CONTROL_ENCODING`. Serialize counts with checked conversion. Represent the 62-byte SessionSelect payload as `SESSION_SELECT_VERIFIED_OPAQUE_PAYLOAD` with a doc comment that it is `Candidate` evidence from the exact capture and that field semantics are blocked; type and payload length are still serialized by `build_select_session` rather than embedded in one 66-byte production blob.

- [ ] **Step 4: Remove duplicate media/control values**

Delete `MEDIA_STREAM_CONTROL_PRIMARY_ID` and `MEDIA_STREAM_CONTROL_ENCODING` from `media_negotiation.rs`; import `PRIMARY_MEDIA_STREAM_ID` and `MEDIA_STREAM_CONTROL_ENCODING` from `media_protocol.rs`. Move answer version/kind shared by parser and encoder to `media_protocol.rs`. Replace HPSS `FB_REQUEST` and `SERVER_KEEPALIVE` duplicates with `protocol` owners; leave Apple-only query/fence/pasteboard values under `hpss`.

- [ ] **Step 5: Run GREEN**

```powershell
cargo test typed_session_builders_
cargo test media_stream_
cargo test session_encoding_profile_
cargo test --no-default-features
```

Expected: independent fixtures match exactly and all duplicate shared wire definitions are gone.

- [ ] **Step 6: Record checkpoint**

Record which SessionSelect bytes remain explicitly opaque/candidate; do not claim their field semantics were recovered.

---

### Task 5: Type HPSS display, MVS/JPEG, RTP/RTCP, and pixel symbols

**Files:**
- Modify: `src/vnc/hpss.rs:29-60,245-310`
- Modify: `src/vnc/mvs.rs:200-290`
- Modify: `src/vnc/srtp.rs:17-100,220-245`
- Modify: `src/vnc/media_transport.rs:28-50`
- Modify: `src/vnc/ard.rs:20-95`
- Modify: `src/vnc/auth.rs:1-42`
- Modify: `src/framebuffer.rs:1-105`
- Test: the same modules' test sections

**Interfaces:**
- Produces: typed HPSS display builders, standard JPEG segment helpers, RTCP signed-loss helper, and single-owner RTP header classification.

- [ ] **Step 1: Add independent wire tests before renaming**

Keep the existing 308-byte SetDisplayConfiguration and 16-byte display query assertions, but compare against literal test fixtures. Add RTCP signed-loss vectors for `0`, maximum positive, `-1`, and minimum 24-bit value. Add JPEG assertions for SOI/DQT/SOF0/DHT/SOS/EOI and byte stuffing without importing marker constants into expected arrays.

- [ ] **Step 2: Run RED for the new helpers**

```powershell
cargo test rtcp_cumulative_loss_decodes_all_signed_24_bit_boundaries
cargo test jpeg_segment_writer_preserves_mvs_baseline_layout
```

Expected: the signed-loss helper and typed segment writer do not exist.

- [ ] **Step 3: Implement semantic HPSS builders**

Define `HpssClientMessageType::{DisplayQuery, SetDisplayConfiguration}`, `SET_DISPLAY_CONFIGURATION_WIRE_BYTES`, `DISPLAY_NAME_WIRE_CAPACITY`, `DISPLAY_QUERY_WIRE_BYTES`, and named field defaults. Serialize using field methods; no `vec![0x09, ...]`, `push(0x1d)`, `push(0x30)`, or raw final resize length remains in production.

- [ ] **Step 4: Implement JPEG and MVS symbols**

Define `JpegMarker`, `JpegComponent`, quantization/Huffman selector symbols, baseline precision, sampling factors, and scan bounds. Use `append_marker`, `append_segment`, and `append_dht`; retain Annex K arrays as named standard tables. Replace direct MVS prefix arrays with Task 1 signatures.

- [ ] **Step 5: Centralize RTP classification and signed RTCP loss**

Move RTP version/header/marker and RTP/RTCP mux classification to `srtp.rs` (or a focused `src/vnc/rtp.rs` if `srtp.rs` would exceed a coherent responsibility). Export:

```rust
pub enum RtpMuxPacketKind { Rtp, Rtcp }
pub fn classify_rtp_mux_packet(packet: &[u8]) -> Result<RtpMuxPacketKind>;
fn decode_signed_24_be(bytes: [u8; 3]) -> i32;
```

Name the sign bit and extension mask. `media_transport.rs` must call the classifier and delete its duplicate RTP constants.

- [ ] **Step 6: Name framebuffer pixel layout values**

Add `PIXEL_RED_SHIFT`, `PIXEL_GREEN_SHIFT`, `PIXEL_BLUE_SHIFT`, `PNG_ALPHA_OPAQUE`, and `PNG_CHANNEL_BYTES`; use them in `to_rgba` and the PNG helper in `main.rs`.

- [ ] **Step 7: Name ARD30 and VNC authentication field sizes**

Define `ARD_DH_KEY_MIN_BYTES`, `ARD_DH_KEY_MAX_BYTES`, `ARD_PRIVATE_EXPONENT_BYTES`, `ARD_CREDENTIAL_FIELD_BYTES`, `ARD_CREDENTIAL_BLOB_BYTES`, and `AES_128_BLOCK_BYTES` in `ard.rs`. Define `VNC_DES_KEY_BYTES`, `VNC_AUTH_CHALLENGE_BYTES`, and `DES_BLOCK_BYTES` in `auth.rs`; make the challenge type aliases/builders use those symbols. Add compile-time relationships for two credential fields and exact block divisibility. Numeric cryptographic results such as SecurityResult success use a named `RFB_SECURITY_RESULT_OK` owner in `protocol.rs`.

- [ ] **Step 8: Run GREEN**

```powershell
cargo test build_set_display_config
cargo test jpeg_segment_writer_
cargo test rtcp_cumulative_loss_
cargo test rtp_
cargo test framebuffer_
cargo test ard::tests
cargo test auth::tests
```

Expected: all exact fixtures pass and shared RTP constants have one owner.

- [ ] **Step 9: Record checkpoint**

Record the allowed raw-data exceptions: Annex K tables, test fixtures, OUI data, and keysym map.

---

### Task 6: Verify the complete wire/symbol slice

**Files:**
- Modify only to resolve failures introduced by Tasks 1-5.

- [ ] **Step 1: Scan for known duplicate/raw sites**

```powershell
rg -n "types\.contains\(&35\)|vec!\[t as u8\]|data\.len\(\) as u16|data\.len\(\) as u8|items\.len\(\) as u32" src
rg -n "MEDIA_STREAM_CONTROL_ENCODING|MAX_MEDIA_DATAGRAMS_PER_ROLE_PER_POLL" src\vnc
rg -n "\[0x00, 0x0f, 0x19\]|\[0x01, 0x0e, 0x13\]|push\(0x1d\)|vec!\[0x09" src
```

Expected: first and third commands return no production hits; shared symbols have one definition and imports/call sites only.

- [ ] **Step 2: Run both automated matrices**

```powershell
cargo fmt -- --check
cargo test
cargo test --no-default-features
cargo clippy --all-targets --all-features -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
```

Expected: every command exits 0.

- [ ] **Step 3: Compare exact wire fixtures**

Run the named fixture tests for RFB, session commands, MediaStream Message 1/2, display configuration, MVS, SRP, RSA-SRP, RTP/SRTP, and JPEG. Expected bytes must remain literal inside tests.

- [ ] **Step 4: Record plan checkpoint**

List every production numeric literal left in the touched protocol paths and classify it as a named data table, independent algorithm constant, or structural zero/one. Any unclassified protocol literal fails this plan.
