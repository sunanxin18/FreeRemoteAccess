# Windows Standard RFB Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an independent Windows client adapter for standard Linux RFB/VNC servers without importing or changing the Apple HPSS/MVS adapter.

**Architecture:** `frd-protocol-rfb` owns its socket, security negotiation, stateful encodings, canonical CPU framebuffer and RFB writer. It consumes only `frd-wire-rfb`, `frd-protocol-api`, `frd-frame`, and `frd-core`, then publishes the same generation-bound surface/input contracts as Apple. Secure VeNCrypt/TLS is preferred; legacy VNC Authentication is opt-in and visibly marked insecure.

**Tech Stack:** Rust 1.96.0, existing stateless `frd-wire-rfb`, `rustls` 0.23, `rustls-native-certs`, `flate2`, standard RFB 3.3/3.7/3.8, VeNCrypt 0.2, Raw/CopyRect/Hextile/ZRLE/DesktopSize.

**Prerequisite:** Complete `2026-08-27-windows-core-apple-wgpu.md` through Task 12.

**Spec:** `docs/superpowers/specs/2026-08-27-winit-wgpu-windows-first-architecture-design.md`

## Boundaries

- Linux target `Auto` resolves to `org.freeremoteaccess.rfb`; Mac OS never resolves to this adapter.
- `frd-protocol-rfb` must not depend on `frd-protocol-apple`, `frd-protocol-rdp`, minifb, winit, wgpu, egui, CPAL, or any platform crate.
- RFB security, socket, zlib streams, encoding caches, writer ordering and framebuffer are adapter-private.
- `frd-wire-rfb` remains stateless and transport-free; do not move policy or security selection into it.
- Default security policy rejects `None` and plaintext VNC Authentication. The user must explicitly enable the compatibility policy for an insecure server.
- Actual VNC passwords never appear in argv or logs. GUI/provider secrets move into the adapter and are cleared on authentication completion, cancellation or failure.
- Implement only the protocol fixtures named below; no GUI snapshot matrix or synthetic combinatorial suite.

---

### Task 1: Create the independent RFB factory and security policy

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/frd-protocol-rfb/Cargo.toml`
- Create: `crates/frd-protocol-rfb/src/lib.rs`
- Create: `crates/frd-protocol-rfb/src/factory.rs`
- Create: `crates/frd-protocol-rfb/src/config.rs`
- Create: `crates/frd-protocol-rfb/src/security_policy.rs`

**Interfaces:**
- Consumes: `ProtocolFactory`, `ConnectRequest`, `ProtocolRuntime`.
- Produces: `RfbProtocolFactory`, descriptor `org.freeremoteaccess.rfb`, `RfbSecurityPolicy`.

- [ ] **Step 1: Write RED factory tests**

Test the stable descriptor, Linux compatibility, Mac/Windows rejection, default port 5900, required password fields by security policy, and factory construction without network I/O.

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-protocol-rfb factory`

Expected: compile failure until the crate and factory exist.

- [ ] **Step 3: Implement the descriptor and configuration**

```rust
pub struct RfbSecurityPolicy {
    pub allow_insecure_vnc_auth: bool,
    pub allow_unauthenticated: bool,
}

pub struct RfbConnectConfig {
    pub endpoint: Endpoint,
    pub username: Option<String>,
    pub password: SecretBuffer,
    pub security: RfbSecurityPolicy,
}
```

Both insecure flags default to false. `ProtocolFactory::create` validates target/config and returns immediately; `ProtocolSession::run` owns all blocking work.

- [ ] **Step 4: Run GREEN and the dependency guard**

Run: `cargo test -p frd-protocol-rfb factory`

Expected: PASS.

Run: `cargo test -p freeremotedesk-windows --test dependency_boundary`

Expected: PASS and no adapter-to-adapter edge.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/frd-protocol-rfb
git commit -m "feat: add independent RFB protocol factory"
```

### Task 2: Implement version and security negotiation

**Files:**
- Create: `crates/frd-protocol-rfb/src/connection.rs`
- Create: `crates/frd-protocol-rfb/src/security.rs`
- Create: `crates/frd-protocol-rfb/src/tls.rs`
- Modify: `crates/frd-wire-rfb/src/messages.rs`

**Interfaces:**
- Consumes: stateless banner/security codecs.
- Produces: authenticated `RfbConnection`, negotiated version/security diagnostics.

- [ ] **Step 1: Add three core negotiation fixtures**

Use in-memory duplex streams for: RFB 3.8 VeNCrypt/TLS success, RFB 3.8 VNC Authentication rejected by default, and RFB 3.3 fixed security-type failure. Do not add live sockets to unit tests.

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-protocol-rfb security`

Expected: failures for missing negotiation.

- [ ] **Step 3: Implement exact version negotiation**

Support 3.3, 3.7 and 3.8. Reject unsupported major versions, oversized failure reasons, empty type lists and unknown selected types with stable error categories. Apple banner normalization remains a leaf-codec capability but is rejected by this adapter's target policy.

- [ ] **Step 4: Implement secure-first security selection**

Selection order is:

```text
VeNCrypt with verified TLS and credential subtype
  > explicitly allowed VNC Authentication
  > explicitly allowed None
```

Use rustls with the Windows/native root store for X.509 validation and the generic saved-pin/challenge contract established by the core plan. An unknown invalid certificate is observed only in a credential-free preflight that aborts before RFB authentication; approval creates a new connection with an exact-fingerprint verifier. TLS/plain credential subtypes send username/password only inside the verified or exactly pinned TLS tunnel. Legacy type 2 VNC Authentication uses the existing stateless challenge-response primitive only after `allow_insecure_vnc_auth` is true.

- [ ] **Step 5: Clear secret staging buffers**

Transfer password bytes into the selected handshake at the last possible point. Clear temporary plaintext buffers immediately after the authentication write completes. Do not derive `Clone` or `Debug` for connection/config types that contain secrets.

- [ ] **Step 6: Run GREEN**

Run: `cargo test -p frd-protocol-rfb security`

Expected: PASS.

Run: `cargo test -p frd-wire-rfb`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/frd-wire-rfb crates/frd-protocol-rfb
git commit -m "feat: negotiate secure standard RFB sessions"
```

### Task 3: Implement ServerInit, canonical surface and basic updates

**Files:**
- Create: `crates/frd-protocol-rfb/src/framebuffer.rs`
- Create: `crates/frd-protocol-rfb/src/decoder/mod.rs`
- Create: `crates/frd-protocol-rfb/src/decoder/raw.rs`
- Create: `crates/frd-protocol-rfb/src/decoder/copy_rect.rs`
- Create: `crates/frd-protocol-rfb/src/baseline.rs`
- Modify: `crates/frd-wire-rfb/src/server_init.rs`

**Interfaces:**
- Produces: canonical BGRX surface, validated dirty rectangles, baseline coverage.

- [ ] **Step 1: Write RED protocol fixtures**

Cover one 2×2 Raw rectangle with known colors, one overlapping CopyRect, one out-of-bounds rectangle rejection, and a nonincremental response whose coverage establishes a full baseline.

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-protocol-rfb framebuffer`

Expected: failures until surface/decoder exist.

- [ ] **Step 3: Negotiate the canonical pixel format**

After ServerInit, send SetPixelFormat for 32-bit little-endian true-color BGRX and SetEncodings. Convert any accepted server pixel format into `PixelFormat::Bgrx8UnormSrgb` inside this adapter. Reject dimensions or strides that exceed checked allocation limits before allocating.

- [ ] **Step 4: Implement transactional Raw and CopyRect**

Decode each complete rectangle into a temporary bounded patch, validate it, then commit to the canonical surface. CopyRect must be overlap-safe. A malformed rectangle leaves the prior canonical surface unchanged and terminates or resynchronizes according to the exact error class.

- [ ] **Step 5: Publish generation zero and the first baseline**

Call `ProtocolRuntime::begin_generation` after ServerInit. Send a nonincremental full-frame request. Publish each committed rectangle as `Damage`; publish `FrameBoundary::FullBaseline` only after the nonincremental response's validated coverage spans the complete current surface. Until then use `Incremental` and keep app input gated.

- [ ] **Step 6: Run GREEN**

Run: `cargo test -p frd-protocol-rfb framebuffer`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/frd-protocol-rfb crates/frd-wire-rfb
git commit -m "feat: decode baseline RFB framebuffer updates"
```

### Task 4: Add Hextile and ZRLE stateful decoders

**Files:**
- Create: `crates/frd-protocol-rfb/src/decoder/hextile.rs`
- Create: `crates/frd-protocol-rfb/src/decoder/zrle.rs`
- Modify: `crates/frd-protocol-rfb/src/decoder/mod.rs`
- Modify: `crates/frd-protocol-rfb/src/connection.rs`

**Interfaces:**
- Consumes: complete rectangle byte streams.
- Produces: transactional canonical-surface patches.

- [ ] **Step 1: Add one authoritative fixture per encoding**

Use a Hextile tile exercising background/foreground/subrects and a ZRLE rectangle exercising palette RLE across tile boundaries. Expected output is the exact canonical BGRX bytes.

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-protocol-rfb decoder`

Expected: unsupported-encoding failures.

- [ ] **Step 3: Implement Hextile with per-rectangle state**

Track background/foreground only where RFB defines their lifetime. Validate subrect coordinates and lengths before applying. Commit only after the entire rectangle decodes.

- [ ] **Step 4: Implement ZRLE with a session-owned zlib stream**

Keep the zlib stream in `RfbProtocolSession`; enforce compressed and decompressed byte ceilings. Decode raw, solid, packed palette and palette-RLE tile modes. Unknown modes fail closed without partially modifying the canonical surface.

- [ ] **Step 5: Run GREEN**

Run: `cargo test -p frd-protocol-rfb decoder`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/frd-protocol-rfb
git commit -m "feat: decode Hextile and ZRLE updates"
```

### Task 5: Handle framebuffer resize and resynchronization

**Files:**
- Create: `crates/frd-protocol-rfb/src/resync.rs`
- Modify: `crates/frd-protocol-rfb/src/framebuffer.rs`
- Modify: `crates/frd-protocol-rfb/src/connection.rs`

**Interfaces:**
- Produces: exact `DesktopSize` generation transitions and bounded full-refresh requests.

- [ ] **Step 1: Write RED transition tests**

Test DesktopSize creating generation N+1, old damage rejection, canonical allocation replacement, input gating until the new baseline, and mailbox overflow coalescing into one nonincremental refresh request.

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-protocol-rfb resync`

- [ ] **Step 3: Implement atomic resize generation changes**

On a valid DesktopSize pseudo-encoding, allocate the new canonical surface first, then call `begin_generation`, replace decoder state and send exactly one nonincremental full request. Reject zero/oversized dimensions without changing the current generation.

- [ ] **Step 4: Implement the bounded refresh state machine**

At most one full refresh is in flight. Mailbox overflow, decode recovery and generation changes set the same `needs_full_refresh` latch. A complete current-generation baseline clears it; stale responses do not.

- [ ] **Step 5: Run GREEN and adapter suite**

Run: `cargo test -p frd-protocol-rfb resync`

Expected: PASS.

Run: `cargo test -p frd-protocol-rfb`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/frd-protocol-rfb
git commit -m "feat: add RFB resize and resynchronization"
```

### Task 6: Translate normalized input into RFB messages

**Files:**
- Create: `crates/frd-protocol-rfb/src/input.rs`
- Create: `crates/frd-protocol-rfb/src/keysym.rs`
- Create: `crates/frd-protocol-rfb/src/writer.rs`

**Interfaces:**
- Consumes: current `SessionInput`.
- Produces: ordered PointerEvent, KeyEvent and framebuffer requests.

- [ ] **Step 1: Write RED input tests**

Test pointer/button mask order, wheel press/release pairs, physical ASCII/function keys, Unicode text keysyms, ReleaseAll, and rejection of stale session/generation input.

- [ ] **Step 2: Run RED**

Run: `cargo test -p frd-protocol-rfb input`

- [ ] **Step 3: Implement one writer owner**

The writer serializes input and framebuffer requests on the authenticated stream. It tracks held RFB keys/buttons and emits ReleaseAll exactly once when requested. It never receives UI or winit types.

- [ ] **Step 4: Implement key mapping**

Map normalized physical keys to X11 keysyms and `InputEvent::Text` to Unicode keysyms. Advertise only the text/keyboard capabilities actually implemented. Never reuse Apple key conversion.

- [ ] **Step 5: Run GREEN**

Run: `cargo test -p frd-protocol-rfb input`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/frd-protocol-rfb
git commit -m "feat: route normalized input through RFB"
```

### Task 7: Register RFB in the Windows composition root and unified login

**Files:**
- Modify: `apps/freeremotedesk-windows/Cargo.toml`
- Modify: `apps/freeremotedesk-windows/src/main.rs`
- Modify: `crates/frd-ui-model/src/lib.rs`
- Modify: `crates/frd-ui-egui/src/connection.rs`
- Modify: `apps/freeremotedesk-windows/tests/dependency_boundary.rs`

- [ ] **Step 1: Add RED catalog and UI-model tests**

Test Linux/Auto resolution, explicit RFB selection, Mac/RFB rejection, secure/insecure policy presentation, and the unified form requiring the fields declared by the selected security policy.

- [ ] **Step 2: Run RED**

Run: `cargo test -p freeremotedesk-windows rfb`

- [ ] **Step 3: Register only at the composition root**

Add `RfbProtocolFactory` beside the Apple factory in `main.rs`. Core/app/UI/shell packages continue to see only descriptors and factories. CLI prefill and GUI submission both pass through the same `ConnectionDraft` and `AppIntent::Connect` flow.

- [ ] **Step 4: Add the explicit insecure compatibility control**

The control is off by default and visible only for RFB. It states that legacy VNC Authentication does not protect the session with modern transport security. Do not silently enable it after a failed secure negotiation.

- [ ] **Step 5: Run GREEN and dependency checks**

Run: `cargo test -p freeremotedesk-windows rfb`

Run: `cargo test -p freeremotedesk-windows --test dependency_boundary`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add apps/freeremotedesk-windows crates/frd-ui-model crates/frd-ui-egui
git commit -m "feat: expose Linux RFB in unified Windows login"
```

### Task 8: Validate standard RFB interoperability

**Files:**
- Create: `docs/validation/windows-linux-rfb.md`
- Modify only adapter/UI files proven necessary by observed failures.

- [ ] **Step 1: Run the offline matrix**

Run:

```bash
cargo fmt -- --check
cargo test -p frd-wire-rfb -p frd-protocol-rfb -p freeremotedesk-windows
cargo build -p freeremotedesk-windows --release
```

- [ ] **Step 2: Run one bounded mock-server session**

Exercise version/security negotiation, ServerInit, Raw baseline, one compressed incremental update, pointer/key input, DesktopSize and disconnect. Confirm one window and one session worker.

- [ ] **Step 3: Run one authorized Linux server session**

Prefer a stock server offering verified TLS. Verify authentication, full baseline, incremental updates, resize, color, pointer-outside silence, keyboard and clean disconnect. If the available server only offers legacy VNC Authentication, require the explicit compatibility control and record that limitation.

- [ ] **Step 4: Record bounded evidence**

Document server product/version, selected security/encoding, commands, observed behavior and any unverified features without credentials or target secrets.

- [ ] **Step 5: Commit**

```bash
git add crates/frd-protocol-rfb apps/freeremotedesk-windows docs/validation/windows-linux-rfb.md
git commit -m "feat: complete Windows standard RFB interoperability"
```
