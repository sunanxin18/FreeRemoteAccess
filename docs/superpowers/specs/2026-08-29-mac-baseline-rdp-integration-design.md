# Mac-Baseline Windows RDP Integration Design

## Status

- Approved approach: Option A, 2026-08-29.
- Common source baseline: `6a8fe30a61b95672774040022093c9fbe91bf9bc`.
- Verified Mac working tree: `codex/windows-client-rearchitecture` at the
  common baseline plus its current uncommitted files.
- RDP source history: `main` through
  `a57032d739063e6e589fabfbee2f4bb958fb834a`.
- Windows RDP live interoperability remains blocked until an authorized stock
  Windows target separate from the active Codex console is available.

## Goal

Make the verified Windows winit/wgpu Apple HPSS/MVS client the immutable
product baseline, then transplant the independently implemented IronRDP
adapter onto that baseline without changing Apple wire behavior, remote
content geometry, input safety, title-bar behavior, or protocol selection.

The final local `main` must contain both protocol families behind the existing
protocol-neutral contracts. A failure or capability change in one adapter must
not mutate the other adapter's state or introduce a cross-protocol fallback.

## Repository Evidence and Root Cause

`main@a57032d` and `codex/windows-client-rearchitecture@6a8fe30` have the exact
merge base `6a8fe30`. The Mac/title-bar working tree contains 31 modified
tracked files and 36 untracked files after that commit. Git cannot merge or
replay content that was never committed, so the verified Mac increment never
entered the branch graph while the RDP work proceeded as commits from the same
base.

The first RDP-side descendant, `62f7233`, packages fonts and application icons.
Most of those files are already byte-identical to the verified working tree.
It must not be replayed wholesale because the working tree also contains newer
AccessKit, title-bar, DPI, icon, and font integration.

## Selected Integration Strategy

1. Audit the verified working tree for secrets, generated files, and unrelated
   edits, then commit its exact source and asset snapshot as the Mac product
   baseline. Do not selectively replace Apple, renderer, input, title-bar, or
   font files with their current `main` versions.
2. Create `codex/mac-baseline-rdp-integration` from that immutable commit.
3. Treat `62f7233` as already superseded by the baseline. Preserve any missing
   provenance or deterministic asset-export assertions explicitly instead of
   replaying the whole commit.
4. Transplant the RDP design, plan, independent adapter crate, and RDP-internal
   fixes from `a8b9c73` through `a57032d` in dependency order.
5. Resolve shared files semantically. Never use a blanket `ours` or `theirs`
   resolution for manifests, the lockfile, the Windows composition root, the
   desktop shell, or the UI model.
6. Regenerate `Cargo.lock` only after all manifests and source integrations are
   complete.
7. Run the complete offline gate and a fresh bounded Windows-to-Mac live gate.
   Only after those pass may local `main` advance to the integration result.

A direct merge of `main` into the dirty worktree is rejected because it makes
the RDP branch the effective conflict baseline and can silently overwrite the
already validated Mac title-bar, input, and audio behavior.

## Protocol and Platform Boundaries

- `frd-protocol-apple` remains the only owner of Apple authentication, HPSS,
  MVS type-0/type-1, Apple writer ordering, UDP media, SRTP/SRTCP, AAC-ELD, and
  Apple dynamic-resolution experiments.
- `frd-protocol-rdp` remains the only owner of TLS, certificate preflight,
  CredSSP/NLA, RDP activation/reactivation, graphics PDUs, RDP input encoding,
  Display Control, CLIPRDR, and RDPSND.
- Neither adapter may depend on the other. RDP must not depend on
  `frd-wire-rfb`, UI, renderer, winit, wgpu, or a platform crate.
- Concrete adapter types are imported only by
  `apps/freeremotedesk-windows/src/main.rs`.
- `frd-session`, `frd-app`, the frame contract, renderer, compositor, and UI
  model remain protocol-neutral.
- Mac automatic selection resolves only to Apple HPSS/MVS. Windows automatic
  selection resolves only to RDP. A failed connection never falls back to a
  different protocol.
- No new RDP-specific page, title-bar control, profile field, or public feature
  schema is added by this integration.

## Shared-File Integration Rules

### Workspace manifests and lockfile

The root manifest must retain the verified `egui-winit/accesskit` feature and
icon tooling while adding `frd-protocol-rdp` as a workspace member. The Windows
application manifest must retain title-bar Windows APIs and add only the RDP
adapter dependency. `Cargo.lock` is regenerated from these final manifests;
no side's lockfile is accepted wholesale.

### Windows composition root

The verified window icon and `WindowChromeFailed` classification remain. The
composition root registers exactly two concrete factories, Apple and RDP, and
passes the same factory collection to the protocol catalog and
`DesktopApplication`. No concrete factory import is permitted elsewhere.

### Desktop shell

The verified custom title bar, AccessKit integration, native Windows chrome,
DPI transition, content rectangle, pointer mapping, and HID-normalized input
remain authoritative. The RDP-side lazy audio corrections are integrated into
that implementation:

- no-media and video-only sessions do not open a platform audio device;
- the device opens on the first supported PCM frame;
- that first PCM frame is delivered exactly once and is not discarded;
- closing the media publisher terminates the audio worker without affecting
  the graphics session.

These rules are protocol-neutral and must continue to pass Apple audio
regressions.

### UI model and dependency boundary

The session chrome model remains unchanged. Tests register both protocols and
prove that Mac automatic selection remains Apple while Windows automatic
selection becomes RDP. Dependency tests prove that concrete protocol imports
are limited to the Windows composition root and that neutral crates do not
gain concrete adapter dependencies.

## Keyboard Contract Reconciliation

The verified baseline defines `PhysicalKeyCode` as a USB HID Keyboard/Keypad
usage. The Windows shell normalizes winit keys into that space, and the Apple
adapter translates HID usages into its Apple/RFB wire representation.

The current RDP implementation instead interprets the raw number as a Windows
Set-1/E0 scan code. Reusing the HID number directly is incorrect: for example,
HID usage `0x1e` is the `1` key, while Set-1 scan code `0x1e` is `A`.

The integration therefore adds a private, exhaustive-for-supported-keys
USB-HID-to-RDP-Set-1/E0 translation inside `frd-protocol-rdp::input`. The
neutral key contract must not be changed back to Windows scan codes. Focus
loss, capability loss, generation changes, and disconnect continue to use the
existing protocol-neutral `ReleaseAll` contract.

Focused core tests cover letters, digits, Enter, left and right modifiers,
navigation keys, keypad keys, unsupported usages, press/release ordering, and
`ReleaseAll`. Lock-key synchronization remains at its currently documented RDP
scope and is not expanded through a new public input schema in this
integration.

## Error and Capability Isolation

- Each adapter returns its own stable, sanitized `ProtocolExit`; the shell is
  the sole producer of the product-level closed state.
- An RDP certificate, NLA, graphics, optional-channel, or input failure cannot
  modify Apple retry, capability, generation, audio, or decoder state.
- Apple HPSS/MVS failure cannot trigger RDP or standard VNC fallback.
- Audio, clipboard, dynamic-resolution, and text-input title-bar states are
  derived from the active adapter's negotiated protocol-neutral capabilities.
- Unsupported or unnegotiated RDP optional channels remain disabled and do not
  affect graphics or Apple sessions.
- Windows RDP remains `开发中` until a separate authorized stock Windows target
  passes login, first frame, increments, input, and disconnect. Offline tests
  and a release build do not upgrade that state.

## Verification Gates

### Source and dependency gates

- Scan staged baseline and integration diffs for credentials, local target
  addresses, ignored captures, build output, and unexpected binary files.
- Verify that only the Windows composition root imports both concrete adapter
  crates.
- Verify that `frd-protocol-rdp` has no dependency on Apple, RFB wire, UI,
  renderer, shell, or platform crates.
- Verify that no RDP commit changes Apple wire, MVS, Apple crypto, or Apple
  media source files after the baseline commit.

### Automated gates

- `cargo +stable fmt --all -- --check`
- `cargo +stable test --workspace`
- `cargo +stable test --workspace --no-default-features`
- `cargo +stable build --workspace --no-default-features`
- `cargo +stable build -p freeremotedesk-windows --release`
- Focused Apple MVS, input, generation, media, and presentation tests.
- Focused RDP identity, NLA, graphics, HID input, lifecycle, Display Control,
  CLIPRDR, and RDPSND tests.
- Dependency-boundary and automatic protocol-selection tests.
- Title-bar content-rectangle, DPI, pointer-mapping, AccessKit, icon, and lazy
  audio tests.
- Clippy for the Windows application, shell, UI crates, icon tool, Apple
  adapter, and RDP adapter with warnings denied.

Tests use Cargo's normal parallelism. The integration must not add a global
serial-build or low-concurrency policy.

### Bounded Windows-to-Mac live gate

Use credentials only from the ignored local credential provider. Launch one
release process and verify:

1. Mac automatic selection chooses Apple HPSS/MVS and authenticates.
2. A full baseline frame appears with correct color and title-bar geometry.
3. A mouse click causes an observable remote UI change.
4. A keyboard event causes an observable remote UI change.
5. The resulting type-1/MVS changes refresh without reconnecting.
6. Leaving or unfocusing the remote surface stops remote input and releases
   held state.
7. Disconnect returns to the same centered connection form.
8. Closing the application leaves no client process.

### Windows RDP live gate

Do not loop back into the active Codex console. Full RDP interoperability waits
for an independently authorized stock Windows physical target or isolated VM.
Until then, record the exact blocker and keep the README matrix at `开发中`.

## Delivery and Git Rules

- Preserve the immutable Mac baseline commit and the logical RDP commit
  history wherever conflict resolution permits.
- Record semantic conflict resolutions in focused integration commits rather
  than hiding them in one merge result.
- Do not delete or overwrite unrelated untracked files in the main worktree.
- Advance local `main` only after every available gate passes and the remaining
  RDP live blocker is documented.
- Do not push the rewritten local `main` to a remote without separate explicit
  user authorization.
