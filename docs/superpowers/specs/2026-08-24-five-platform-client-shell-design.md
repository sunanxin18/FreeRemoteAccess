# FreeRemoteAccess 五平台客户端与鸿蒙兼容基线设计

**Status:** Approved with the server label corrected to `Mac OS`
**Date:** 2026-08-24
**Scope:** Windows, macOS, Linux, Android, iOS first; HarmonyOS after those five

## Product boundary

FreeRemoteAccess is only a remote-login client. It connects directly to the
unmodified native service selected by the user:

- `Windows` uses RDP, with TCP 3389 as the default.
- `Mac OS` uses Apple's Screen Sharing / ARD-compatible RFB service, with TCP
  5900 as the default.
- `Linux / VNC` uses standard RFB, with TCP 5900 as the default.
- `Auto` derives the initial protocol from an explicit port and otherwise runs
  bounded standard handshakes. It never starts a relay or a server companion.

No Apple ID, iCloud, IDS, APNs, or QuickRelay credentials are accepted. The
password-authenticated Mac account path remains the only ARD product path.

## Toolchain baseline

The application source targets the common API surface of Flutter 3.41:

- Official five-platform builds pin upstream Flutter `3.41.9` and Dart
  `3.11.5`.
- The later HarmonyOS build pins CPF-Flutter
  `3.41.10-ohos-1.0.0` from the `oh-3.41.9-release` line.
- `pubspec.yaml` must not raise its Flutter minimum above `3.41.0`, and code
  must not depend on APIs introduced after upstream 3.41.
- Toolchain revisions and archive hashes are recorded in
  `toolchains/flutter.json`; CI rejects an unpinned Flutter channel.

Flutter owns application navigation, forms, accessibility, adaptive layout,
and low-frequency session controls. Rust owns authentication, protocol state,
network I/O, framebuffer state, capability negotiation, and input translation.
Decoded frames do not cross a serialized Dart bridge. Each platform renderer
consumes a native texture or shared pixel surface. The bridge carries only
commands, session state, errors, geometry, and texture handles.

## Connection screen

Desktop widths show a recent-connections rail and one connection card. Mobile
widths collapse recent connections below the card. The first release stores
recent endpoint metadata but never stores a password.

The connection card contains:

1. Service selector: `Auto`, `Windows`, `Mac OS`, `Linux / VNC`.
2. Server field accepting a host name, IPv4 address, or bracketed IPv6 address.
3. Port field, defaulted by the selected service and editable by the user.
4. Username field.
5. Password field with obscured text and no persistence.
6. Domain field visible only for `Windows`.
7. A single primary `Connect` action.

Validation is shared with Rust. Empty hosts, zero ports, missing usernames,
missing passwords, and a domain supplied to a non-RDP service fail before any
socket is opened. Error messages are Simplified Chinese and never echo the
password.

## Session screen

The remote texture fills the available content area using aspect-fit by
default. A compact overlay exposes disconnect, full screen, scaling, keyboard,
clipboard, audio state, and network state. Desktop builds capture raw keyboard
and pointer input. Mobile builds expose touchpad and direct-touch modes plus an
explicit software-keyboard action.

The view model has these externally observable phases:

- `idle`
- `validating`
- `connecting`
- `connected`
- `disconnecting`
- `failed`

Only `connected` owns a texture handle. Generation changes invalidate stale
frames, input transforms, and pending resize acknowledgements atomically.

## Rust public boundary

`src/app/connection.rs` provides the platform-neutral value types:

```rust
pub enum ServiceKind { Auto, WindowsRdp, MacOsArd, LinuxVnc }

pub struct ConnectionRequest {
    pub service: ServiceKind,
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    pub password: String,
    pub domain: Option<String>,
}

pub struct ValidatedConnection {
    pub protocol: ProtocolKind,
    pub endpoint: Endpoint,
    pub username: String,
    pub password: secrecy::SecretString,
    pub domain: Option<String>,
}

pub fn validate_connection(request: ConnectionRequest)
    -> Result<ValidatedConnection, ConnectionValidationError>;
```

The public error type identifies a field and a stable error code. UI text is
mapped from that code in Dart, so Rust does not leak secrets or bind the core to
one presentation language.

## Protocol adapters

`RfbSession` wraps the existing RFB/ARD implementation and emits normalized
frame, cursor, clipboard, bell, resize, and termination events. `RdpSession`
wraps `ironrdp-client` and maps `RdpOutputEvent::Image` into the same generation
bound framebuffer contract. The GUI selects an adapter; adapters own all
transport decisions required by the native server protocol.

RDP starts with TLS plus CredSSP/NLA, software-decoded images, server pointer
events, display-control resize, and no gateway or custom DVC proxy. RFB starts
with the existing Raw/CopyRect path and the verified Apple authentication path.

## Packaging outputs

CI builds on the native host for each platform:

- Windows x64: application directory plus MSI.
- macOS arm64 and x64 where supported: application bundle plus DMG.
- Linux x64: application bundle plus `.deb`.
- Android arm64: release APK.
- iOS arm64: unsigned release application archive because Apple signing
  credentials are outside this project's credential boundary.

The iOS archive is a verified build artifact, not an installable App Store IPA.
A device-installable IPA requires an external signing owner to apply a
certificate and provisioning profile after the build; FreeRemoteAccess and its
CI do not request or retain those credentials.

## Acceptance gates

- Dart widget tests verify service labels, conditional domain visibility,
  responsive layout, validation errors, and password non-persistence.
- Rust tests verify default ports, endpoint parsing, protocol selection, secret
  redaction, and unsupported combinations.
- Official five-platform CI runs the build on the native host and uploads the
  named package artifact.
- Windows runs a local package smoke test. macOS, Linux, Android, and iOS run
  native-host compile/package checks in CI.
- A protocol is not described as interoperable until a bounded connection to
  the corresponding stock server has authenticated and rendered a non-empty
  frame.
