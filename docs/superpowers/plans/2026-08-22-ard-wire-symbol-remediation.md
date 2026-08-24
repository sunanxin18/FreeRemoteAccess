# ARD Wire Symbol Gate Remediation Addendum

**Goal:** Close the bounded production-wire literal gaps confirmed by the independent Task 6 review, then rerun the complete Wire plan acceptance gate.

**Basis:** This addendum does not change protocol behavior or evidence claims. It completes the already-approved requirement that every production wire field use an owner-scoped semantic symbol, typed field/builder, or named opaque evidence blob.

**Global constraints:** Preserve all reviewed Tasks 1-5 behavior; exact type-1 MVS partial decoding remains blocked; SessionSelect stays opaque Candidate evidence; SRTCP replay/fairness/runtime changes stay in later plans; byte-exact expected fixtures remain literal and independent; no Git initialization; fresh temporary Cargo targets only.

## Task 7: MVS and HPSS framing owners

- Name the full-MVS signature/metadata/header relationship.
- Parse capture headers through named fields/offsets.
- Parse the fixed media rectangle and media-control discriminator through typed/named owners, consuming the shared primary stream ID.
- Replace the inline raw full-refresh request with the standard RFB typed builder.
- Preserve capture bytes, media dispatch, and MVS recovery fixtures.

## Task 8: RFB, pixel, and pointer owners

- Use the shared SecurityResult success owner and negotiated pixel-width/channel owners in the standard RFB client.
- Add a single protocol owner for pointer button/wheel masks and use it in both viewers.
- Share RGB input-channel offsets and framebuffer output shifts across main/viewer paths.
- Preserve pointer/wheel behavior and exact pixel conversion results.

## Task 9: SRP and RSA-SRP framing owners

- Name/import Apple security type, challenge bounds/layout, nonce, padded width, proof/tag/tail/status fields, and SecurityResult.
- Share common SRP capacities with RSA-SRP and replace raw response/public-key/challenge slices with named or typed parsing.
- Preserve exact cryptographic wire round trips and do not alter the separate group-validation policy.

## Task 10: Repeat complete Wire plan acceptance

- Rerun the full Task 6 scans, exact fixture categories, both feature matrices, both Clippy/build/help matrices, and semantic production-literal inventory.
- The zero-unclassified-production-literal gate must pass.
- Documentation acceptance (`docs/ARD_WIRE_SYMBOLS.md` and `AGENTS.md` rule) remains mandatory in the later docs/completion plan.

## Task 11: Encrypted-session wire-frame prefix owner

- Move `[BE16 ciphertext length][ciphertext]` extraction behind the session framing owner.
- Both client read paths consume one shared typed frame extractor rather than duplicating raw prefix indexes and drain/skip arithmetic.
- Preserve incomplete-frame buffering, trailing bytes, ciphertext validation, and crypto behavior.

## Task 12: Final targeted source acceptance repeat

- Recheck the encrypted-frame prefix and the complete semantic inventory after Task 11.
- Confirm the full post-change matrix and zero-unclassified production-wire gate before closing the Wire source plan.

## Task 13: Shared RFB banner and refresh-size owners

- Move the complete fixed RFB banner contract and parser to a shared protocol owner consumed by both discovery probing and the authenticated client.
- Remove the discovery path's raw 12-byte buffer and raw prefix match; add probe-focused literal fixtures while preserving fail-closed `Option` behavior.
- Use the existing framebuffer-update-request message-size owner in the HPSS viewer helper return type rather than repeating raw 10.
- Preserve banner negotiation, Apple non-standard minor versions, scanning timeouts, and exact refresh bytes.

## Task 14: Final Wire source acceptance after Task 13

- Repeat the banner/refresh targeted scans, all prior semantic ownership gates, nonzero fixtures, full fresh-target matrix, and 25-file source inventory.
- Close the Wire source plan only after a fresh independent review confirms zero unclassified production wire/layout literals.

## Task 15: Remove leaked RFB field maximum from negotiation

- Remove the client consumer's redundant raw `999` bound; the shared exact three-digit parser already enforces the field maximum.
- Give the supported 3.3/3.7/3.8 negotiation values semantic protocol owners and consume them in the client selection/reply path.
- Preserve exact standard fallback and Apple private-version echo behavior, including the parsed maximum boundary.

## Task 16: Final Wire source acceptance after Task 15

- Repeat the complete Task 14 semantic inventory, all nonzero focused filters, full fresh-target matrix, and 25-file hash gate.
- Require fresh independent approval before declaring the Wire source plan complete.
