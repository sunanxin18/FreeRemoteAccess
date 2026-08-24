# Repository Guidelines

## Current Roadmap State (2026-08-23)

- **P1 Dynamic resolution: implemented as a default-off experiment.**
  `hpssview --dynamic-resolution` becomes available only after fail-closed
  session evidence: matching initial `ServerState`, a successfully applied
  current-generation complete MVS full frame, and the local interactive
  controlling role. It debounces viewport changes for 250 ms, makes `Pending`
  visible only after its resized `0x09` write succeeds, keeps one latest target
  in flight, commits only an exact acknowledgement, and times out a missing
  acknowledgement after two seconds without replacing the old surface. A
  confirmed commit atomically replaces the generation-bound `DisplaySurface`,
  resets MVS state, and requests a full frame; rendering and pointer mapping
  always use current window/surface dimensions. Resized `0x09` remains an
  opt-in wire experiment, not live-interoperability proof.
- **P2 MVS incremental frames: conservative reassembly/resync implemented.**
  `MvsRecordAssembler` reassembles the declared total exactly (including the
  captured 32748 + 26572 = 59320 case). Full, partial, and malformed payloads
  are classified strictly; partial data never enters the JPEG path and instead
  causes a rate-limited-but-not-dropped full resync. Decoder tables/reference
  state is generation-scoped. `.mvs` capture uses `FRDMVS01` and preserves the
  rectangle; legacy capture files are rejected explicitly. Exact type-1 partial
  decoding is **BLOCKED** pending a trustworthy fixture or complete decoder
  recovery.
- **P3 Mac-to-PC audio and P4 UDP transport are live-interoperable.**
  The typed Message 1 parser, version-3 `0x1c` configuration, version-2 Message
  2 answer, generation-bound Audio/Video sockets, Apple-compatible SRTP/SRTCP,
  replay handling, AAC-ELD decode, and Windows playback are integrated. Bounded
  target runs authenticated both audio and video, decoded a non-silent 48 kHz
  stereo AAC-ELD access unit, and played it through the selected Windows output.
  Live proof is bounded rather than a claim about arbitrary packet loss or very
  long rollover behavior.
- **P5 PC-to-Mac audio is unsupported for the permitted login model and is
  fail-closed.**  Offline Ghidra recovery of the stock macOS 26.6.2
  `ScreenSharing.framework` establishes that `audioChatSupported` requires an
  IDS session or an Apple-ID invitation address. `setAudioChatMuted:` dispatches
  only to AVConference for an accepted Screen Sharing QR/IDS invitation, legacy
  IDS, or the invitation agent. There is no recovered username/password HPSS
  Audio Chat branch, and ARD 3.10 itself exposes no Audio Chat control path.
  Prior Active mode-4 plus authenticated SRTCP proved only that a generic
  AVConference endpoint received/reported packets; it did not prove product
  ownership, decode, playback, or a generic remote input device.
  `--udp-audio-input` therefore fails before network session selection, the
  Windows microphone remains unopened, and no more native-output tone tests are
  warranted. Mode-4 code and fixtures remain offline reverse-engineering
  evidence only. Reopening P5 requires new Apple binary evidence for a separate
  stock non-IDS password-session branch; Apple-ID and server-side fallbacks are
  prohibited by the client-only boundary below.

## Client-Only Product Boundary

- FreeRemoteDesk is a remote-login **client**. Its product architecture must
  interoperate with the unmodified Apple remote-login services already present
  on the Mac.
- Never require, add, install, deploy, or run a custom companion application,
  daemon, launch agent, virtual-audio driver/plugin, relay, proxy, or other
  supporting program on the Mac server. This prohibition also applies to
  test-only fallback implementations and to documentation or scripts that make
  such a component part of the FreeRemoteDesk solution.
- P5 must therefore use an Apple-native protocol accepted by the stock Mac
  service. If identity, entitlement, or protocol evidence is insufficient,
  keep the feature fail-closed and report the exact blocker; do not substitute
  a server-side helper.
- P5 must authenticate only with the Mac account credentials already used for
  remote login/screen sharing. The Windows client must not request, retain, or
  use Apple ID, iCloud, IDS, APNs, or QuickRelay identity credentials. A native
  Apple path that requires those credentials is out of product scope even if
  its wire protocol can be recovered.
- Built-in macOS commands may be used for authorized observation and bounded
  interoperability tests. Installing or running a third-party instrumentation
  agent on the Mac requires separate explicit authorization and never counts as
  a production implementation path.

## Project Structure & Module Organization
FreeRemoteDesk is a pure-Rust native remote-login client. Windows, macOS, and
Linux desktop delivery is Phase 1; Android and HarmonyOS are Phase 2 platform
adapters over the same core. The desktop application uses one
`winit + egui + wgpu` window.

- `src/main.rs`: native GUI launch plus protocol/reverse-engineering CLI tools.
- `src/core/`, `src/session/`: platform/protocol-neutral contracts and bounded
  session engine.
- `src/protocols/`: RDP, Apple ARD, and standard RFB adapters.
- `src/platform/`, `src/ui/`: native platform boundary and the single Rust GUI.
- `src/arp.rs`: ARP discovery via Windows `SendARP` and LAN host probing.
- `src/vnc/`: protocol stack and security logic. `session.rs`, `srp.rs`, and
  `rsa_srp.rs` cover Apple session/auth experiments; `hpss.rs` covers HPSS
  negotiation/media records/capture; `mvs.rs`, `mvs_stream.rs`,
  `dynamic_resolution.rs`, and `hpss_session.rs` implement the evidence-bounded
  P1/P2 state machines; `media_protocol.rs`, `media_negotiation.rs`,
  `media_transport.rs`, `srtp.rs`, `audio_codec.rs`, and `audio_io.rs` contain
  the verified P3/P4 control, transport, cryptographic, codec, and device paths.
- `src/framebuffer.rs`: bounded CPU framebuffer used by protocol decoders.
- `src/proxy.rs`: local capture proxy for protocol capture.
- `packaging/`: native Windows MSI, macOS app/pkg/dmg, and Linux deb/rpm/AppImage.
- `docs/ARD_PROTOCOL.md`: ARD protocol notes and reverse notes.
- `ard_re/`: local Apple ARD/Screen Sharing reverse-engineering evidence,
  extracted binaries, Ghidra scripts/projects, disassembly, and prior probes.
- `target/`, `ard_capture/`, and generated artifacts are build/runtime outputs;
  keep them local only.

## ARD 3.10 Reverse-Engineering Baseline

- Source image: `RemoteDesktop3.10.dmg`, SHA-256
  `BF1B94FBA16122122E4D10ABFCE77B2C7C4A60B02AA78C176D05CD014BF40CBC`.
  The extracted application is under
  `ard_re/ard310/MacWk.CN/Remote Desktop.app`.
- Local tools: 7-Zip at `C:\Program Files\7-Zip\7z.exe`; Ghidra 12.1.2 at
  `D:\Program Files\ghidra` (headless entrypoint `support\analyzeHeadless.bat`);
  Windows Frida 17.17.0 is installed. A Mac `frida-server` has been installed at
  `/usr/local/bin/frida-server`, but the default remote port was not reachable
  on 2026-08-24. Do not claim live Frida connectivity until the server is
  running temporarily, its version matches the Windows client, and a read-only
  attach probe succeeds; do not configure persistence or automatic startup.
- Ghidra projects: `ard_re/ghidra_proj/ARDClient310` for the ARD application and
  `ard_re/ghidra_proj/ScreenSharingClient` for the extracted Screen Sharing
  client, plus `ard_re/ghidra_proj/ScreenSharingFrameworkCurrent` for the stock
  macOS 26.6.2 arm64e framework extracted read-only from dyld cache (SHA-256
  `C144D9F966397B2D8B0B4A3C36A21D39A3593A41954C1C76E619EC88F8E21AEA`).
  `ard_re/FindStringRefs.java` locates string/selector references and
  `ard_re/DumpAt.java` exports decompiled functions. `PrintProgramMap.java`
  records program identity, address map, and matching function symbols before
  an address from another Mach-O or dyld-cache image is used.
- The installed GhidraMCP extension manifest is malformed. Headless Ghidra works;
  do not claim live MCP connectivity until both bridge import and the local
  `/methods` endpoint have been verified.

### Verified P1/P2 Apple-client evidence

- Dynamic-resolution availability is gated by: AVC media stream, a display
  configuration accepted by `dynamicResolutionModeAvailable` (the UI names the
  two-virtual-display case as unavailable), an active controlling session, and
  a non-paused state. The local controller treats unknown predicates as false;
  its evidence latch is not a claim that every private Apple predicate was
  observed directly.
- ARD toggles `sessionView.dynamicResolutionMode`; zoom-to-fit, actual-size, and
  zoom-in/out behavior changes when that mode is active. The application's
  `windowDidResize:` only adjusts UI geometry, so a received `ServerState` or a
  window notification is not by itself a dynamic-resolution implementation.
- Server-side evidence calls `HandleCodecChanged` and reallocates the
  multi-variant codec when resolution changes. P1 must therefore make display
  geometry, input transforms, framebuffer allocation, and MVS decoder reset one
  atomic generation transition.
- The Apple decoder has separate `DecodeMVSUpdate` and
  `DecodeMVSPartialUpdate` paths in `RFBViewerLib/DecodeMultiVariant.c`.
  Partial updates validate the byte sequence `0x6d 0x76 0x73` (`mvs`) through a
  bit reader and depend on persistent decoder state. Keep transport reassembly,
  MVS bitstream parsing, and framebuffer application as separate layers.
- Evidence establishes MVS/OpenCL/shared-memory behavior and AVC/HEVC symbols;
  it does not establish exact partial-block fields, resized-`0x09` support,
  UDP framing, HDR, framerate semantics, or all quality/refinement semantics.
  Label hypotheses as such and capture a failing fixture before implementing an
  inferred field layout.

## Build, Test, and Development Commands
- `cargo build --locked --release` — build the native GUI client for the host platform.
- `cargo build --locked --no-default-features --features cli` — protocol CLI only.
- `cargo test --locked --all-targets --features gui` — full desktop test matrix.
- `cargo test key_derivation_vector` — run a focused cryptography regression test.
- `cargo fmt --all -- --check` — verify formatting before PR.
- `cargo run -- --help` — print CLI usage.
- Running `.\target\release\freeremotedesk.exe` without arguments opens the GUI.
- Protocol CLI credentials come from the non-echoing
  `FRD_USERNAME`/`FRD_PASSWORD` environment provider, never argv.
- Before changing P1/P2, add focused tests for generation transitions, exact
  MVS record reassembly, full-frame reset, partial update rejection/resync, and
  stale-frame handling. The current expected verification matrix is
  `cargo fmt --all -- --check`, the GUI test matrix, GUI and CLI-only builds,
  and the top-level help output. Treat live target validation as
  a separate evidence step; do not imply it from unit tests.

## Coding Style & Naming Conventions
- Rust 2021, prefer idiomatic Rust style and module boundaries.
- Use 4-space indentation and `snake_case` for functions/variables, `PascalCase` for types, `SCREAMING_SNAKE_CASE` for constants.
- Keep user-facing CLI text and comments in Simplified Chinese.
- Keep protocol changes consistent across modules when touching the pixel contract
  (`protocol.rs`, `client.rs`, `framebuffer.rs`, `core/frame.rs`, and
  `ui/remote_texture.rs`).

## Testing Guidelines
- Primary framework: Rust built-in test harness via `cargo test`.
- Place tests in `#[cfg(test)]` modules near implementation unless a dedicated test utility is needed.
- Use deterministic fixtures and explicit test names that describe behavior (e.g., `key_derivation_vector`, `handshake_rejects_unknown_encoding`).
- Run focused tests before larger edits; at minimum, run a full pass with `cargo test` before proposing protocol or cryptography changes.

## Commit & Pull Request Guidelines
- This directory currently has no `.git` history metadata, so there are no established repository commit conventions in this working tree.
- When committed elsewhere, use clear Conventional Commit-style messages (`feat:`, `fix:`, `refactor:`, `test:`).
- PR descriptions should include:
  - Scope and expected behavior change.
  - Commands executed (`cargo fmt`, `cargo test`, manual CLI verification commands).
  - Platform assumption (Windows, macOS target behavior when relevant).
  - Any security impact around network/auth behavior.

## Security & Configuration Tips
- Never add real credentials to source control; store local test credentials in `CREDENTIALS.local.md` (gitignored).
- Do not hardcode IPs/passwords in code or examples.
- The authorized Mac target and login details are local test secrets. Read them
  only from `CREDENTIALS.local.md` or a non-echoing environment/credential
  provider; never copy them into `AGENTS.md`, source, reverse notes, tool command
  lines, captures, or test output.
- Favor conservative defaults for timeout/retry/probe settings; LAN capture behavior can affect network load.
