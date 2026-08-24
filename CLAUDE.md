# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

FreeRemoteDesk is a Windows-only Rust CLI that discovers hosts on the LAN via ARP and connects to macOS Screen Sharing (the built-in VNC server on TCP 5900). No Npcap, no admin rights, no VNC libraries — ARP uses the Windows kernel `SendARP` API, and the RFB protocol (RFC 6143) is implemented by hand.

- **Comments and all user-facing strings (CLI output, error messages) are in Simplified Chinese.** Identifiers are English. Match this convention.
- `CREDENTIALS.local.md` holds local debug credentials (real Mac mini target + VNC password). It is gitignored — never commit, quote, or copy its contents into other files.
- Not yet a git repository, but `.gitignore` is prepared (`/target`, `*.png`, `CREDENTIALS.local.md`).

## Commands

```powershell
cargo build --release          # build (target\release\freeremotedesk.exe)
cargo test                     # unit tests + end-to-end mock-server integration test
cargo test key_derivation_vector   # run a single test by name
cargo build --no-default-features  # headless build without minifb/viewer
```

Running against a real Mac (see `CREDENTIALS.local.md` for the local target):

```powershell
.\target\release\freeremotedesk.exe scan                  # ARP sweep + 5900 probe
.\target\release\freeremotedesk.exe info <ip> [-p <pwd>]  # handshake/auth info only
.\target\release\freeremotedesk.exe shot <ip> -p <pwd> -o out.png
.\target\release\freeremotedesk.exe view <ip> -p <pwd> [--scale 0.5]   # Ctrl+Q quits
.\target\release\freeremotedesk.exe esess <ip> -u <user> -p <pwd> [--seconds 8]  # encrypted-session check
```

## Architecture

Four subcommands engage progressively deeper: `scan` (ARP + port probe) → `info` (handshake only) → `shot` (auth + one frame) → `view` (full interactive session).

### Pixel-format invariant (the key cross-file contract)

`PixelFormat::OURS` in `src/vnc/protocol.rs` declares 32bpp / little-endian / true-colour `R<<16 | G<<8 | B`. Because of this, Raw rectangle bytes are reinterpreted directly as `u32` `0x00RRGGBB` in `client.rs`, stored as-is in `Framebuffer` (`src/framebuffer.rs`), and pushed to the minifb window buffer without any per-pixel conversion. **Changing this format requires coordinated changes in protocol.rs, client.rs (`read_server_message`), framebuffer.rs, and viewer.rs (PNG export in `framebuffer.rs::to_rgba` assumes it too).**

All RFB integers are big-endian on the wire *except* pixel data, which follows the negotiated client format (little-endian in our case).

### ARP discovery (`src/arp.rs`)

- `SendARP` from `windows-sys` (iphlpapi) sends real ARP requests without drivers/admin rights. Its IP argument must be passed in network byte order: numerically `u32::from_le_bytes(ip.octets())`.
- `local_ipv4()` uses the UDP-`connect()`-then-`local_addr()` trick (no packet sent) to auto-detect the local /24.
- `SendARP` blocks to kernel timeout on offline hosts, so `sweep()` fans out threads (default 128). VNC detection is a separate concurrent TCP connect + 12-byte `RFB ` banner read (`probe_vnc_banner`).

### RFB client (`src/vnc/`)

- `RfbConn` (in `client.rs`): read-buffered TCP (16 KB internal buffer; large reads bypass into the caller's buffer to avoid double copies), writes pass straight through.
- Connection is split into two stages so `info` can work without a password:
  1. `negotiate()` → `Negotiated` (version handshake + security-type list; handles RFB 3.3/3.7/3.8 differences and macOS's non-standard banner version numbers)
  2. `authenticate()` → `VncClient` (security choice + DES challenge-response + ClientInit/ServerInit)
- Security types supported: 1 (None), 2 (VNC Auth DES), 30 (ARD auth, `src/vnc/ard.rs` — DH + AES-128-ECB credential blob), 33 (RSA-SRP, `src/vnc/rsa_srp.rs`), and **36 (SRP-6a, `src/vnc/srp.rs` — the preferred path; returns the SRP session key K)**. Type 36 is followed by the **encrypted session layer** (`src/vnc/session.rs`): `key16 = SHA256(K)[0..16]`, then the server sends a plaintext 52B EncryptionInfo `[16B hdr][BE32 counter][ECB(key16,new_key)][ECB(key16,new_iv)]`, after which *all* traffic both ways is EncryptOneMessage frames: `[BE16 len][CBC ct]`, plaintext `[BE16 orig_len][data][zero pad][20B SHA1(BE32(counter)‖body)]`, CBC IV chained from `new_iv`, independent per-direction counters starting at 0. Set `FRD_ENC=0` to force a plaintext session. The macOS banner version (`RFB 003.889`) must be echoed verbatim — the Apple session layer is version-gated.
- VNC DES auth (`auth.rs`): password truncated/zero-padded to 8 bytes (only the first 8 characters ever matter), each key byte **bit-reversed** (VNC uses LSB-first keys vs FIPS-46 MSB-first), then DES-ECB over the 16-byte challenge. Correctness is pinned by the RFC 6143 test vectors in `auth.rs`.
- `read_server_message()` returns `ServerEvent`s; Raw(0) and CopyRect(1) encodings are decoded. In encrypted sessions the Apple 0x14 heartbeat (8B) is echoed back and skipped. The encoding table rides in the cmd=1 SetEncryption message (Raw-only table → standard RFB Raw rects; sending SetEncodings separately inside the session is rejected by the server).

### Viewer threading model (`src/viewer.rs`)

The socket is used full-duplex across threads: the **reader thread owns the original `TcpStream`** (blocking `read_server_message` loop, applies rects to `Arc<Mutex<Framebuffer>>`, sends incremental update requests), while the **main thread writes through a `try_clone()`d stream** (`Arc<Mutex<TcpStream>>`) for key/mouse events from minifb. Keyboard events are computed by diffing the key set against the previous frame; mouse events only on position/button-mask change. `keysym.rs` maps minifb keys to X11 keysyms.

### Feature gating

The `viewer` feature (default) enables minifb + the `view` subcommand; `src/viewer.rs` and `src/keysym.rs` are `#[cfg(feature = "viewer")]`. `scan`/`info`/`shot` work headless with `--no-default-features`.

## Testing

`cargo test` covers three layers without needing a real Mac:
1. DES key-derivation vector (password `"COW"` → known key) and ECB round-trip (`auth.rs`)
2. End-to-end integration test (`client.rs::end_to_end_with_mock_server`): a thread runs a mock RFB server emulating macOS behaviour ([30, 2, 35] security list), and the client goes through handshake → DES auth → ServerInit → SetPixelFormat/SetEncodings → Raw + CopyRect updates → PNG export, asserting exact pixel results.
3. PNG output validity (header/IHDR checks)

When changing protocol or framebuffer code, keep the mock server test in sync — it is the main regression net.
