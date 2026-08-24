# ARD Documentation, Dependencies, and Completion Audit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish the authoritative semantic wire index, reconcile all ARD documents with implemented evidence, audit dependencies reproducibly, and prove the completed P3/P4/P6 slices through automated and live gates without misreporting P5.

**Architecture:** Code symbols remain authoritative in their owner modules; `ARD_WIRE_SYMBOLS.md` is a cross-reference with evidence status, not a duplicate constant source. Two PowerShell checks make symbol-policy and OSV audits repeatable on the Windows-first workspace. The final audit maps every approved-spec requirement to current source/test/live evidence and leaves the overall goal active until a separate P5 architecture is approved and implemented.

**Tech Stack:** Markdown, PowerShell 7, Cargo metadata/lockfile, OSV API, Rust build/test/Clippy, Windows and authorized macOS live validation.

**Spec:** `docs/superpowers/specs/2026-08-22-ard-p3-p4-p6-hardening.md`

## Global Constraints

- Never write credentials, target addresses, usernames, SSH key paths, raw session keys, or unredacted captures into source, docs, scripts, test output, or command lines.
- `CREDENTIALS.local.md` remains the only local secret source and remains ignored.
- Documentation distinguishes `Verified`, `Candidate`, and `Blocked`; local tests never count as live interoperability proof.
- The P5 HPSS path remains fail-closed. Document the IDS/QuickRelay/AVConference boundary and ask for a separate architecture choice after P3/P4/P6 close.
- Dependency findings are calibrated by active target and called operation; an unpatched advisory is documented, not falsely labeled fixed.
- This workspace has no Git metadata. Do not initialize Git; record verification checkpoints instead.

---

### Task 1: Create the authoritative ARD wire symbol index

**Files:**
- Create: `docs/ARD_WIRE_SYMBOLS.md`
- Read: all touched owner modules and `ard_re/P4_UDP_EVIDENCE.md`

**Interfaces:**
- Consumes: final symbol names from the media, wire, and runtime plans.
- Produces: one searchable row per production protocol/policy symbol.

- [ ] **Step 1: Create the document skeleton with fixed evidence vocabulary**

Start with:

```markdown
# ARD/RFB Wire Semantic Symbol Index

This file indexes production symbols; numeric definitions remain in their owner modules.

Evidence status:
- `Verified`: static producer/consumer, sanitized exact fixture, RFC, or bounded live differential proof.
- `Candidate`: position/behavior is stable but the private semantic name is incomplete.
- `Blocked`: serializer/consumer/layout proof is insufficient; production generation is forbidden.

| Owner | Code symbol | Wire value/layout | Direction | Status | Evidence |
|---|---|---|---|---|---|
```

- [ ] **Step 2: Populate standard RFB and Apple session rows**

Include banner lengths/versions, every RFB client/server message enum, all security types, ClientInit flags, SelectSession envelope/opaque candidate body, SetEncryption commands/methods, encoding profiles, EncryptionInfo, and encrypted-frame lengths/masks. Link evidence to RFC 6143 or exact sections in `docs/ARD_PROTOCOL.md` / `docs/ARD_SESSION_PROTOCOL.md`.

- [ ] **Step 3: Populate HPSS/MVS/media rows**

Include display configuration/query, MVS/cursor/ServerState encodings, MVS signatures/129-byte layout, capture file magics, MediaStream Message 1/2 fields, client `0x1c`, role descriptors, SRTP/SRTCP lengths/KDF labels/profile, RTP/RTCP fields, AAC-ELD/RFC3640 fields, budgets, and timeouts. Mark MVS type-1 partial layout and P5 AudioChat data plane `Blocked`.

- [ ] **Step 4: Add allowed raw-data exceptions**

List JPEG Annex K tables, X11 keysym map, Apple OUI list, and independent exact fixtures. State why each is a named table/evidence artifact rather than a production magic number.

- [ ] **Step 5: Check every code symbol resolves**

Run a script-free first pass:

```powershell
$symbols = Select-String -Path docs\ARD_WIRE_SYMBOLS.md -Pattern '`([A-Z][A-Z0-9_]+)`' -AllMatches |
  ForEach-Object { $_.Matches.Value.Trim('`') } | Sort-Object -Unique
$missing = foreach ($symbol in $symbols) {
  if (-not (rg -l --fixed-strings $symbol src)) { $symbol }
}
$missing
```

Expected: no output except rows explicitly labeled document-only status names.

- [ ] **Step 6: Record checkpoint**

Record row counts by `Verified`/`Candidate`/`Blocked` and resolve every missing code symbol.

---

### Task 2: Reconcile protocol docs, evidence notes, and repository guidance

**Files:**
- Modify: `docs/ARD_PROTOCOL.md`
- Modify: `docs/ARD_SESSION_PROTOCOL.md`
- Modify: `ard_re/P4_UDP_EVIDENCE.md`
- Modify: `AGENTS.md`
- Modify: `docs/superpowers/specs/2026-08-22-ard-media-audio-p3-p6-design.md`

**Interfaces:**
- Consumes: final behavior and live evidence.
- Produces: one non-contradictory current roadmap and explicit historical/superseded markers.

- [ ] **Step 1: Update P3/P4 behavior and remaining live gaps**

Document typed UDP outcomes, discard counters, per-role quota/rotation, SRTP replay semantics, audio late-discard/concealment, audio degraded state, and exact live tests performed. Keep the 256-packet capture as bounded evidence only; do not imply it crossed a full 16-bit rollover.

- [ ] **Step 2: Correct P2 and session descriptions**

Change every table statement to exact 129 bytes and zero `x/y/w/h`; include the preserved unknown initialization parameter as `Candidate`. Replace raw session-message examples used as definitions with links to semantic symbols, leaving literal bytes only in clearly labeled capture/fixture blocks.

- [ ] **Step 3: Publish the P5 native boundary**

Add a section that states:

```text
Verified boundary: password-authenticated HPSS SSUDPSender is server-to-viewer.
Verified native control plane: ScreenSharing.framework uses IDS services, QuickRelay,
AVConference AudioOnly invite/accept dictionaries, and private entitlements.
Blocked: a Windows implementation of Apple identity/push/relay/media and the exact
AudioChat data-plane wire layout.
Alternative requiring approval: an explicit custom Mac companion.
```

Reference sanitized decompilation filenames, never local credentials or raw keys.

- [ ] **Step 4: Update AGENTS current state and symbol policy**

Update P3/P4 implementation status only after tests/live gates prove it. Keep P5 pending/fail-closed. Add the owner-scoped symbol policy and allowed table/fixture exceptions. Preserve the warning that Ghidra headless works but MCP connectivity is unproven.

- [ ] **Step 5: Keep obsolete design visibly historical**

Retain the historical banner in `2026-08-22-ard-media-audio-p3-p6-design.md` and add a direct link to the approved hardening spec. Do not delete contradictory historical evidence; label it superseded and point readers to current docs.

- [ ] **Step 6: Search for contradicted claims**

```powershell
rg -n "3fe6.*UDP|固定.*96564|96,?564|需 128B|至少 128|HPSS.*PC.?→.?Mac|RemoteMicrophone.*SSUDPSender.*接收" docs AGENTS.md ard_re\P4_UDP_EVIDENCE.md
```

Expected: no current claim retains a disproved UDP query/fixed-size/client-to-Mac HPSS interpretation; historical occurrences are explicitly marked false/superseded.

- [ ] **Step 7: Record checkpoint**

Record the current authoritative documents and historical-only documents.

---

### Task 3: Add a reproducible semantic-symbol policy check

**Files:**
- Create: `scripts/check_wire_symbols.ps1`
- Modify: `AGENTS.md`
- Test: execute the script from repository root

**Interfaces:**
- Produces: a deterministic nonzero exit for known raw-wire regressions and duplicate owners.

- [ ] **Step 1: Write the check with explicit patterns and allowlists**

The script must:

1. require repository root files `Cargo.toml`, `src/vnc/protocol.rs`, and `docs/ARD_WIRE_SYMBOLS.md`;
2. scan production portions of protocol modules (content before their first top-level `#[cfg(test)]` where tests are tail modules);
3. reject known unsafe patterns: `types.contains(&35)`, `vec![t as u8]`, wire `len() as u8/u16/u32`, direct MVS signatures, direct HPSS display message templates, and duplicated definitions of shared media/quota symbols;
4. exempt named JPEG Annex K arrays, `keysym.rs`, `APPLE_OUIS`, and `#[cfg(test)]` fixtures;
5. print file/line/pattern and exit 1 on violations.

Use PowerShell regex objects, not shell-generated command strings. End with:

```powershell
if ($violations.Count -ne 0) {
    $violations | ForEach-Object { Write-Error $_ }
    exit 1
}
Write-Output "wire-symbol-policy=pass"
```

- [ ] **Step 2: Prove the check detects a synthetic violation without editing source**

Factor scanning into a function accepting text. In a Pester-free self-test mode (`-SelfTest`), feed `let x = vec![t as u8];` and assert one violation, then feed a named fixture/table example and assert zero.

Run:

```powershell
pwsh -NoProfile -File scripts\check_wire_symbols.ps1 -SelfTest
pwsh -NoProfile -File scripts\check_wire_symbols.ps1
```

Expected: self-test and repository audit both exit 0 and print pass markers.

- [ ] **Step 3: Document the check**

Add the exact command to `AGENTS.md` verification matrix and explain that it guards known wire-magic regression classes; it does not replace human classification of new algorithm literals.

- [ ] **Step 4: Record checkpoint**

Record scanned files, allowlist categories, and violation count.

---

### Task 4: Make the dependency audit reproducible and calibrated

**Files:**
- Create: `scripts/audit_cargo_osv.ps1`
- Create: `docs/DEPENDENCY_AUDIT.md`
- Modify: `Cargo.toml` and `Cargo.lock` only if a compatible verified update removes an active issue

**Interfaces:**
- Produces: OSV batch results keyed by exact Cargo.lock package/version and a human calibration record.

- [ ] **Step 1: Implement exact Cargo metadata to OSV batch querying**

Use `cargo metadata --format-version 1 --locked` and build POST batches for `https://api.osv.dev/v1/querybatch` with ecosystem `crates.io`. The script writes no package data to disk by default and emits stable rows:

```text
crate<TAB>version<TAB>advisory-id<TAB>summary
```

It must fail on cargo/JSON/HTTP errors, deduplicate package/version pairs, and support `-OutputPath` for a sanitized local artifact. No credentials are read or transmitted.

- [ ] **Step 2: Run the audit and target graphs**

```powershell
pwsh -NoProfile -File scripts\audit_cargo_osv.ps1
cargo tree -i instant --target x86_64-pc-windows-msvc
cargo tree -i instant --target all
cargo tree -i rsa
```

Expected baseline: `instant 0.1.13` appears only through `minifb 0.28.0` in the all-target graph, not the active Windows graph; `rsa 0.9.10` is direct.

- [ ] **Step 3: Evaluate a compatible minifb update without forcing it**

Run `cargo update -p minifb --dry-run` if supported; otherwise copy `Cargo.toml`/`Cargo.lock` to a temporary directory created by `New-Item` and evaluate the newest compatible minifb there. Accept a real dependency edit only when all of these are true:

- the selected version supports the project's Rust/platform constraints;
- `cargo tree -i instant --target all` no longer contains `instant` or the new graph has a documented equivalent risk;
- both feature build/test/Clippy matrices pass;
- viewer help and local window construction tests pass.

If no compatible version meets all conditions, leave manifests unchanged and document the non-active-target unmaintained risk.

- [ ] **Step 4: Calibrate the RSA advisory by operation**

Record [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html): the vulnerable operation is private-key RSA decryption; production uses `RsaPublicKey::encrypt`, while private/decrypt operations occur only in mock tests. Add a source guard:

```powershell
rg -n "RsaPrivateKey|\.decrypt\(" src
```

Expected: test-only hits or none. Because no patched stable release is listed, document `unpatched, production path not reached`, not `fixed`.

- [ ] **Step 5: Document target-specific instant risk**

Record [RUSTSEC-2024-0384](https://rustsec.org/advisories/RUSTSEC-2024-0384.html), active/ all-target tree evidence, accepted upgrade result, and residual risk.

- [ ] **Step 6: Record checkpoint**

Record audit date, exact versions, active target, advisory disposition, commands, and whether manifests changed.

---

### Task 5: Run the complete automated completion audit

**Files:**
- Create: `docs/P3_P4_P6_COMPLETION_AUDIT.md`
- Modify only code/docs that fail a proved requirement.

**Interfaces:**
- Consumes: approved spec and all three implementation plans.
- Produces: requirement-by-requirement evidence, contradiction, or missing status.

- [ ] **Step 1: Build the audit matrix before running commands**

Use columns:

```markdown
| Requirement | Authoritative evidence | Current result | Status | Remaining action |
|---|---|---|---|---|
```

Include every numbered RED test, every source guard, every automated command, every live gate, P5 fail-closed, allowed magic-number exceptions, dependency dispositions, and document deliverable.

- [ ] **Step 2: Run format/test/Clippy/build/help gates fresh**

```powershell
cargo fmt -- --check
cargo test
cargo test --no-default-features
cargo clippy --all-targets --all-features -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo build --all-features
cargo build --no-default-features
cargo run -- --help
cargo run -- hpssview --help
pwsh -NoProfile -File scripts\check_wire_symbols.ps1
pwsh -NoProfile -File scripts\audit_cargo_osv.ps1
```

Record actual totals and exit codes; do not copy the old 136-test baseline as a new result.

- [ ] **Step 3: Audit source state, not just green tests**

Run all source guards from the three plans, inspect every hit, and verify the tests genuinely exercise their requirement. Mark indirect or missing evidence `Not proven` and fix or add a test before continuing.

- [ ] **Step 4: Verify documentation links and evidence labels**

Check every local path in `ARD_WIRE_SYMBOLS.md`, all `Verified/Candidate/Blocked` entries, and all superseded claims. No missing source or capture reference is allowed.

- [ ] **Step 5: Record checkpoint**

The automated audit may mark only automated requirements complete. Live requirements remain `Not proven` until Task 6.

---

### Task 6: Execute sanitized live P3/P4 validation

**Files:**
- Append sanitized results to: `docs/P3_P4_P6_COMPLETION_AUDIT.md`
- Update: `ard_re/P4_UDP_EVIDENCE.md`

**Interfaces:**
- Consumes: the authorized local credential provider and final release binary.
- Produces: live event ordering and counters without secrets.

- [ ] **Step 1: Validate the private test configuration without echoing it**

Use `CREDENTIALS.local.md` or the existing non-echoing local helper. Check only booleans for target, username, credential, and SSH-key presence; never print their values. Confirm the target is reachable before launching the viewer.

- [ ] **Step 2: Build release and start Mac→PC audio/video**

```powershell
cargo build --release --all-features
```

Launch `hpssview` through a local wrapper that supplies target and credential environment without literal secrets in the command. Enable UDP media, leave PC→Mac input disabled, and play a known non-silent sound on the Mac.

Expected event order: authenticated session → Message 1 → client `0x1c` → Message 2 → Active → authenticated SRTP audio/video → non-silent decoded PCM.

- [ ] **Step 3: Inject bounded UDP noise and loss/reordering**

Using only the negotiated local test sockets or the test proxy, inject wrong-source, bad-tag, duplicate, and controlled late audio packets. Record discard/late/concealment counters. Screen/control must continue and the next valid packet must be accepted.

- [ ] **Step 4: Validate audio-device degradation**

Start one run with the Windows output device unavailable or deliberately selected to fail through the test seam. Confirm one bounded audio-degraded diagnostic while screen/control remain interactive. Restore the device, reconnect, and confirm audio initializes in the new generation.

- [ ] **Step 5: Cross a full RTP sequence cycle**

Run continuously long enough to observe at least 65,537 accepted RTP packets for each active role being claimed. Record first/last extended sequence, ROC transition, replay/loss counts, and clean teardown. If a role's packet rate cannot cross the cycle in a practical run, mark that role `Not proven`; do not substitute the old 256-packet capture.

- [ ] **Step 6: Sanitize and publish results**

Publish only timestamps relative to session start, role names, counts, state transitions, and hashed/redacted fixture IDs. Do not publish endpoints, usernames, absolute secret paths, session IDs, SSRCs if linkable, or key material.

- [ ] **Step 7: Record plan checkpoint and preserve full goal**

Mark P3/P4/P6 complete only if every corresponding audit row is proved. Explicitly leave the overall thread goal incomplete because P5 still requires its separate architecture decision and implementation.
