# FreeRemoteAccess Five-Platform Client Shell Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the approved Flutter connection/session shell, expose the existing Rust RFB/ARD core through a platform-neutral session boundary, add an IronRDP backend, and produce native-host package artifacts for Windows, macOS, Linux, Android, and iOS.

**Architecture:** Flutter 3.41 owns forms and navigation while Rust owns validation, credentials, protocol sessions, and framebuffer generations. A narrow FFI bridge carries commands and state; decoded pixels stay on a native texture path. Native-host GitHub Actions jobs package each supported platform, with iOS intentionally unsigned.

**Tech Stack:** Flutter 3.41.9, Dart 3.11.5, Rust 1.96, flutter_rust_bridge 2.13, IronRDP 0.17 / ironrdp-client 0.1, GitHub Actions

**Spec:** `docs/superpowers/specs/2026-08-24-five-platform-client-shell-design.md`

## Global Constraints

- The product is client-only and must not add a server companion, relay, proxy, daemon, driver, or plugin to a remote host.
- Mac login accepts only the native Mac username and password; Apple ID and IDS credentials are forbidden.
- Official builds pin Flutter 3.41.9; HarmonyOS later pins CPF-Flutter 3.41.10-ohos-1.0.0.
- Flutter code stays within the Flutter 3.41 API surface.
- Passwords are never stored, logged, placed in argv, or serialized into recent connections.
- UI labels and user-facing errors are Simplified Chinese; the approved selector label is `Mac OS`.
- Protocol adapters, not the UI, own transport behavior.

---

### Task 1: Platform-neutral connection contract

**Files:**
- Create: `src/lib.rs`
- Create: `src/app/mod.rs`
- Create: `src/app/connection.rs`
- Modify: `Cargo.toml`
- Modify: `src/main.rs`
- Test: `src/app/connection.rs`

**Interfaces:**
- Consumes: endpoint strings and account fields from all frontends.
- Produces: `validate_connection(ConnectionRequest) -> Result<ValidatedConnection, ConnectionValidationError>` and stable protocol/service enums.

- [ ] **Step 1: Write failing connection validation tests**

```rust
#[test]
fn mac_os_defaults_to_rfb_port_without_exposing_password() {
    let validated = validate_connection(request(ServiceKind::MacOsArd, "mac.local", None)).unwrap();
    assert_eq!(validated.endpoint.port(), 5900);
    assert_eq!(validated.protocol, ProtocolKind::AppleRfb);
    assert!(!format!("{validated:?}").contains("secret-value"));
}

#[test]
fn windows_domain_is_rejected_for_vnc() {
    let mut value = request(ServiceKind::LinuxVnc, "linux.local", Some(5900));
    value.domain = Some("WORKGROUP".into());
    assert_eq!(validate_connection(value).unwrap_err().code(), "domain_not_supported");
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test --locked app::connection --lib`

Expected: compilation fails because `src/lib.rs` and the connection types do not exist.

- [ ] **Step 3: Implement the minimal typed validation boundary**

```rust
pub fn validate_connection(request: ConnectionRequest) -> Result<ValidatedConnection, ConnectionValidationError> {
    let host = request.host.trim();
    if host.is_empty() { return Err(ConnectionValidationError::new("host", "host_required")); }
    if request.username.trim().is_empty() { return Err(ConnectionValidationError::new("username", "username_required")); }
    if request.password.is_empty() { return Err(ConnectionValidationError::new("password", "password_required")); }
    if !matches!(request.service, ServiceKind::WindowsRdp) && request.domain.as_deref().is_some_and(|v| !v.trim().is_empty()) {
        return Err(ConnectionValidationError::new("domain", "domain_not_supported"));
    }
    ValidatedConnection::from_request(request)
}
```

- [ ] **Step 4: Run focused and existing tests**

Run: `cargo test --locked app::connection --lib && cargo test --locked --quiet`

Expected: all connection tests and the existing 491 non-private tests pass.

- [ ] **Step 5: Commit**

```text
feat(core): add cross-platform connection contract
```

### Task 2: Flutter 3.41 application scaffold and connection UI

**Files:**
- Create: `toolchains/flutter.json`
- Create: `app/pubspec.yaml`
- Create: `app/lib/main.dart`
- Create: `app/lib/connection/connection_form.dart`
- Create: `app/lib/connection/connection_model.dart`
- Create: `app/lib/session/session_page.dart`
- Create: generated platform folders under `app/android`, `app/ios`, `app/linux`, `app/macos`, and `app/windows`
- Test: `app/test/connection_form_test.dart`

**Interfaces:**
- Consumes: `ServiceKind`, host, port, username, password, and optional domain.
- Produces: an adaptive `ConnectionForm` and a `ConnectionDraft` with no persistence behavior.

- [ ] **Step 1: Generate the pinned five-platform Flutter project**

Run: `flutter create --platforms=android,ios,linux,macos,windows --org io.freeremote --project-name freeremote_access app`

Expected: Flutter creates only the five named platforms and reports success under Flutter 3.41.9.

- [ ] **Step 2: Replace the generated widget test with failing product tests**

```dart
testWidgets('shows the approved Mac OS service label', (tester) async {
  await tester.pumpWidget(const FreeRemoteAccessApp());
  await tester.tap(find.byKey(const Key('service-selector')));
  await tester.pumpAndSettle();
  expect(find.text('Mac OS'), findsOneWidget);
});

testWidgets('domain is visible only for Windows', (tester) async {
  await tester.pumpWidget(const FreeRemoteAccessApp());
  expect(find.byKey(const Key('domain-field')), findsNothing);
  await tester.tap(find.text('自动识别'));
  await tester.tap(find.text('Windows').last);
  await tester.pumpAndSettle();
  expect(find.byKey(const Key('domain-field')), findsOneWidget);
});
```

- [ ] **Step 3: Run widget tests and verify RED**

Run: `cd app && flutter test test/connection_form_test.dart`

Expected: tests fail because the approved app and selector do not exist.

- [ ] **Step 4: Implement the adaptive connection form**

```dart
enum ServiceKind { automatic, windows, macOs, linuxVnc }

extension ServiceLabel on ServiceKind {
  String get label => switch (this) {
    ServiceKind.automatic => '自动识别',
    ServiceKind.windows => 'Windows',
    ServiceKind.macOs => 'Mac OS',
    ServiceKind.linuxVnc => 'Linux / VNC',
  };
}
```

The form uses keyed fields `service-selector`, `host-field`, `port-field`,
`username-field`, `password-field`, `domain-field`, and `connect-button`.
At widths below 720 logical pixels, the recent-connections rail is placed
after the form; otherwise it is a fixed-width left rail.

- [ ] **Step 5: Run Flutter tests and analyzer**

Run: `cd app && flutter test && flutter analyze`

Expected: all widget tests pass and analyzer exits without errors.

- [ ] **Step 6: Commit**

```text
feat(ui): add adaptive five-platform connection screen
```

### Task 3: Rust bridge and secret-safe validation

**Files:**
- Create: `native/freeremote_bridge/Cargo.toml`
- Create: `native/freeremote_bridge/src/lib.rs`
- Create: `native/freeremote_bridge/src/api.rs`
- Create: `app/lib/bridge/bridge.dart`
- Create: generated bridge bindings under `app/lib/bridge/generated`
- Modify: `app/pubspec.yaml`
- Test: `native/freeremote_bridge/src/api.rs`
- Test: `app/test/connection_submission_test.dart`

**Interfaces:**
- Consumes: `ConnectionDraft` from Task 2.
- Produces: `validateConnection(BridgeConnectionRequest) -> BridgeValidationResult`; passwords are passed only for the call lifetime.

- [ ] **Step 1: Write a failing Rust bridge mapping test**

```rust
#[test]
fn maps_mac_os_to_apple_rfb_without_returning_password() {
    let result = validate_connection_bridge(BridgeConnectionRequest::fixture("mac_os")).unwrap();
    assert_eq!(result.protocol, "apple_rfb");
    assert_eq!(result.port, 5900);
    assert!(!serde_json::to_string(&result).unwrap().contains("secret-value"));
}
```

- [ ] **Step 2: Verify RED, then implement the bridge DTO mapping**

Run: `cargo test --manifest-path native/freeremote_bridge/Cargo.toml`

Expected before implementation: missing bridge symbols. Expected after minimal implementation: test passes.

- [ ] **Step 3: Write and run a failing Flutter submission test**

The test enters all form fields, presses `connect-button`, and asserts the
bridge receives `mac_os`, host, port, and username while recent-connection
storage receives no password field.

Run: `cd app && flutter test test/connection_submission_test.dart`

Expected: FAIL before the controller exists and PASS after it delegates to the
bridge and clears the password controller on terminal failure.

- [ ] **Step 4: Generate bindings and verify both languages**

Run: `flutter_rust_bridge_codegen generate && cargo test --workspace && cd app && flutter test && flutter analyze`

Expected: generated bindings are current and every command exits zero.

- [ ] **Step 5: Commit**

```text
feat(bridge): connect Flutter form to Rust validation
```

### Task 4: Normalized RFB/ARD session events

**Files:**
- Create: `src/app/session.rs`
- Create: `src/app/rfb_session.rs`
- Modify: `src/vnc/client.rs`
- Modify: `src/framebuffer.rs`
- Test: `src/app/rfb_session.rs`

**Interfaces:**
- Consumes: `ValidatedConnection` with `ProtocolKind::AppleRfb` or `ProtocolKind::StandardRfb`.
- Produces: `SessionEvent::{State, Frame, Resize, Clipboard, Bell, Terminated}` and `SessionCommand::{Pointer, Key, Resize, Clipboard, Disconnect}`.

- [ ] **Step 1: Write a failing generation test**

```rust
#[test]
fn resize_invalidates_frames_from_the_old_generation() {
    let mut state = SessionFramebuffer::new(1, 800, 600).unwrap();
    state.begin_generation(2, 1024, 768).unwrap();
    assert_eq!(state.apply_frame(frame_for(1)), FrameDisposition::Stale);
    assert_eq!(state.apply_frame(frame_for(2)), FrameDisposition::Applied);
}
```

- [ ] **Step 2: Verify RED and implement the normalized state machine**

Run: `cargo test --locked app::rfb_session --lib`

Expected: missing session state before implementation; all focused tests pass after the minimal generation gate is added.

- [ ] **Step 3: Adapt existing RFB events without changing wire behavior**

Map existing `ServerEvent::FramebufferUpdate`, clipboard, bell, and close
events to normalized session events. Keep Apple HPSS/MVS generation handling
inside the existing evidence-bounded state machines.

- [ ] **Step 4: Run the full Rust matrix**

Run: `cargo test --locked --quiet && cargo test --locked --no-default-features --quiet`

Expected: no failures in either configuration.

- [ ] **Step 5: Commit**

```text
refactor(rfb): expose normalized client session events
```

### Task 5: IronRDP client adapter

**Files:**
- Create: `src/app/rdp_session.rs`
- Modify: `Cargo.toml`
- Modify: `src/app/session.rs`
- Test: `src/app/rdp_session.rs`

**Interfaces:**
- Consumes: `ValidatedConnection` with `ProtocolKind::Rdp`.
- Produces: the same `SessionEvent` and consumes the same `SessionCommand` used by RFB.

- [ ] **Step 1: Write failing configuration tests**

```rust
#[test]
fn rdp_config_requires_nla_and_preserves_domain() {
    let config = build_rdp_config(validated_windows_request()).unwrap();
    assert_eq!(config.destination().port(), 3389);
    assert_eq!(config.properties().domain(), Some("WORKGROUP"));
    assert!(config.properties().enable_credssp_support());
}
```

- [ ] **Step 2: Verify RED and add the minimal IronRDP dependency set**

Use `ironrdp-client = { version = "0.1.0", default-features = false, features = ["rustls"] }`, plus Tokio only for the adapter runtime. Do not enable gateway, pipe proxy, COM plugin, device redirection, or server crates.

Run: `cargo test --locked app::rdp_session --lib`

Expected: configuration test passes after mapping destination, username,
password, domain, client identity, 32-bit color, server pointer, TLS disabled,
and CredSSP enabled.

- [ ] **Step 3: Map IronRDP output and input events**

Map `RdpOutputEvent::Image` to a generation-bound frame, pointer events to
normalized cursor events, connector failures to stable error codes, and
termination to one terminal event. Map resize and close commands through the
client input sender.

- [ ] **Step 4: Verify the adapter and full Rust suite**

Run: `cargo test --locked app::rdp_session --lib && cargo test --locked --quiet`

Expected: adapter tests and all existing tests pass.

- [ ] **Step 5: Commit**

```text
feat(rdp): add native Windows server adapter
```

### Task 6: Native texture session surface

**Files:**
- Create: `app/lib/session/remote_surface.dart`
- Create: per-platform texture registration code under the five generated platform folders
- Modify: `native/freeremote_bridge/src/api.rs`
- Modify: `app/lib/session/session_page.dart`
- Test: `app/test/session_page_test.dart`

**Interfaces:**
- Consumes: normalized frame generation, dimensions, and native texture handle.
- Produces: aspect-fit rendering plus pointer coordinate mapping into current generation dimensions.

- [ ] **Step 1: Write failing session widget tests**

Test that only a connected state creates a `Texture`, disconnect removes it,
and a 1920x1080 remote surface letterboxes correctly inside a 1000x1000 host.

Run: `cd app && flutter test test/session_page_test.dart`

Expected: FAIL before `RemoteSurface` exists.

- [ ] **Step 2: Implement the surface and platform texture lifecycle**

Each plugin instance owns its texture registration. It unregisters only after
the matching session generation terminates. Pixel uploads occur on the native
render path; Dart receives only texture ID, width, height, and generation.

- [ ] **Step 3: Verify UI and Rust lifecycle tests**

Run: `cd app && flutter test && flutter analyze && cd .. && cargo test --locked --quiet`

Expected: all commands exit zero.

- [ ] **Step 4: Commit**

```text
feat(render): add generation-bound remote texture surface
```

### Task 7: Five-platform packaging workflows

**Files:**
- Create: `.github/workflows/build-packages.yml`
- Create: `packaging/windows/Product.wxs`
- Create: `packaging/linux/build-deb.sh`
- Create: `packaging/macos/build-dmg.sh`
- Create: `packaging/check-artifact.ps1`
- Modify: `app/android/app/build.gradle.kts`
- Modify: `README.md`

**Interfaces:**
- Consumes: tagged source, pinned Flutter JSON, and native Rust bridge.
- Produces: MSI, DMG, DEB, APK, and unsigned iOS application archive artifacts with SHA-256 manifests.

- [ ] **Step 1: Write package contract checks before workflows**

`packaging/check-artifact.ps1` accepts `-Platform`, `-Path`, and `-ExpectedName`;
it fails on a missing/empty artifact and writes `<artifact>.sha256` after success.
Pester-independent invocation tests exercise a zero-byte fixture and a valid
fixture in the runner temporary directory.

- [ ] **Step 2: Add native-host workflow jobs**

Pin Flutter from `toolchains/flutter.json`. Use `windows-2025` for Flutter
Windows plus WiX MSI, `macos-15` for Flutter macOS/iOS plus DMG and unsigned iOS
archive, and `ubuntu-24.04` for Linux DEB and Android APK. Each job runs Flutter
tests and Rust tests before packaging and uploads exactly named artifacts.

- [ ] **Step 3: Run local Windows release and MSI build**

Run: `cd app && flutter build windows --release`, then build
`FreeRemoteAccess-0.1.0-windows-x64.msi` and run
`packaging/check-artifact.ps1` against it.

Expected: application launches to the connection form and the MSI is non-empty
with a SHA-256 sidecar.

- [ ] **Step 4: Push the feature branch and verify every CI job**

Run: `git push -u origin feat/five-platform-client`, then inspect all five
native-host job conclusions and artifact names through GitHub CLI.

Expected: Windows, macOS, Linux, Android, and unsigned iOS build jobs succeed.

- [ ] **Step 5: Commit**

```text
build: package FreeRemoteAccess for five platforms
```

### Task 8: Final interoperability and release evidence

**Files:**
- Create: `docs/validation/five-platform-build-matrix.md`
- Modify: `README.md`

**Interfaces:**
- Consumes: package artifacts and authorized native test servers.
- Produces: a truthful matrix separating build proof from live protocol proof.

- [ ] **Step 1: Run fresh source gates**

Run: `cargo fmt -- --check`, both Cargo test/build configurations, `flutter
analyze`, `flutter test`, and the package contract checks.

- [ ] **Step 2: Run bounded live Windows and Mac OS sessions**

For each authorized server, authenticate with its native account, render a
non-empty frame, verify pointer mapping, disconnect cleanly, and record only
non-secret timestamps, protocol, dimensions, and result.

- [ ] **Step 3: Record platform package evidence**

Record workflow run URL, runner image, Flutter revision, Rust version, artifact
name, size, SHA-256, and whether signing was applied. Label unsigned iOS output
as non-installable without external provisioning.

- [ ] **Step 4: Commit**

```text
docs: record five-platform build and interoperability evidence
```

