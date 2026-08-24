# CLAUDE.md

This file provides project-specific guidance to Claude Code.

## Product boundary

FreeRemoteAccess is a pure-Rust remote-login client. It connects directly to
unmodified native remote services and never requires a companion, daemon,
driver, relay, or proxy on the controlled machine.

Phase 1 delivers native Windows, macOS, and Linux clients. Android and
HarmonyOS phone/PC support are Phase 2 platform adapters over the same Rust
core. The desktop UI is one `winit + egui + wgpu` window; do not reintroduce
Flutter, Dart, a C ABI UI bridge, or `minifb`.

Mac OS uses Apple Screen Sharing/ARD first and standard VNC only as the lowest
priority fallback. Mac login accepts only the machine account username and
password. Never request or persist Apple ID, iCloud, IDS, APNs, or QuickRelay
credentials. PC-to-Mac audio remains fail-closed without evidence for a stock
username/password service path.

All comments and user-facing strings are Simplified Chinese. Identifiers are
English. Local credentials belong only in the gitignored
`CREDENTIALS.local.md` or a non-echoing credential provider.

## Commands

```powershell
cargo fmt --all -- --check
cargo test --locked --all-targets --features gui
cargo build --locked --release
cargo build --locked --no-default-features --features cli
```

Running `target\release\freeremotedesk.exe` without arguments opens the native
GUI. Never pass credentials on argv.

## Architecture

- `src/core/`: normalized connection, frame, viewport, input, and error types.
- `src/protocols/`: RDP, Apple ARD, and standard RFB protocol adapters.
- `src/session/`: bounded protocol worker and session event delivery.
- `src/platform/`: host window/device/lifecycle boundary.
- `src/ui/`: single-window winit/egui/wgpu application and persistent GPU texture.
- `src/vnc/`: proven Apple/RFB authentication, HPSS/MVS, UDP/SRTP, and AAC code.
- `packaging/`: Windows, macOS, and Linux native installer builders.

Client platform and remote protocol are independent dimensions. Do not put
platform window APIs into a protocol adapter or protocol wire behavior into the
renderer. Frame updates are generation-bound: a reset replaces surface state
atomically, dirty rectangles update the persistent GPU texture, and stale
generations are rejected.

## Verification rules

Unit/mock tests, compilation, and package creation are offline gates. They are
not evidence of live RDP/ARD/VNC interoperability. Record live target results
separately, and keep uncertain Apple wire layouts labelled as hypotheses until
a trustworthy capture or recovered decoder proves them.
