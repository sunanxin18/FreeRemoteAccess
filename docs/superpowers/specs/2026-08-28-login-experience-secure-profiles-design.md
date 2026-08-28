# Login Experience and Secure Profiles Design

**Date:** 2026-08-28

**Status:** Approved

**Scope:** Windows-first product login UI with cross-platform storage and shell boundaries

## Objective

Replace the utilitarian connection form with a responsive centered login card,
support multiple remembered connections, store passwords only in the operating
system credential vault, submit from the focused password field on Enter, and
standardize FreeRemoteDesk-owned interface icons on Google Material Symbols
Rounded.

This work changes the product shell and application orchestration only. It must
not alter Apple HPSS, MVS, media, transport, renderer-core, or hot input paths.

## Approved Visual Direction

Use one centered card with a maximum logical width of 460. The shell background
uses the active platform light or dark surface. The card uses a restrained
surface, a 16-point corner radius, minimal elevation, and 24 to 32 points of
internal spacing. The UI must use the platform font chain already owned by the
desktop shell.

The content order is:

1. Product icon, `FreeRemoteDesk`, and `安全连接到远程设备`.
2. Recent-connection selector, defaulting to the last successful profile.
3. Target-system and protocol selectors.
4. Address and port fields.
5. Username and secure password fields.
6. `在此设备上保存登录信息` checkbox with a short secure-storage explanation.
7. Full-width primary `连接` button.

Controls remain a single column at narrow widths. At desktop widths only the
target/protocol pair and address/port pair may share a row. Unsupported target
or protocol combinations are visibly disabled and labelled `即将支持`; they do
not enter a connection flow that is known to fail.

Text-field labels remain visible after entry. Placeholders are hints, not the
only description. Field validation is displayed next to the affected field.
Terminal connection errors remain persistent until the user edits or retries;
they are not represented only by a toast.

## Interaction Contract

- The full-width Connect button is the single primary action.
- Enter submits when the password field owns focus, the IME is not composing,
  the form validates, and no connection is already starting or active.
- Button click and Enter emit the same `AppIntent::Connect` path. The shell must
  deduplicate a single key press and never launch a second session.
- Selecting a recent profile fills target, protocol, address, port, and username
  and loads its password from the platform credential vault. A vault read error
  leaves the password empty and presents a field-local safe error.
- Password visibility is off by default and resets to hidden whenever another
  profile is selected or the page is reconstructed.
- Icon-only controls show a concise Simplified-Chinese tooltip after hover or
  keyboard focus. Tooltips describe controls; short save/delete outcomes may
  use a toast, while errors remain inline or persistent.
- All icon-only targets are at least 44 by 44 logical points on desktop. Future
  Android and HarmonyOS touch shells use at least 48 by 48 density-independent
  pixels.

## Remembered Profile Semantics

A profile contains only:

- opaque profile identifier;
- target system;
- resolved protocol identifier;
- address and port;
- username;
- last-success ordering value.

The profile record never contains a password, token, credential-provider output,
or a diagnostic copied from a protocol adapter.

When `在此设备上保存登录信息` is checked:

1. Form validation runs before any write.
2. The password is written to a session-scoped pending credential entry in the
   OS vault, then the owned `SecretBuffer` moves into the existing connect path.
3. `ConnectionStage::TransportReady` commits the pending credential to the
   profile credential key and atomically publishes the non-secret profile as the
   most recent successful connection.
4. Explicit authentication failure, cancellation, or launch rollback removes
   the pending entry without replacing a previously committed credential.
5. Startup removes stale pending entries left by an interrupted process.

If secure staging fails, connection does not silently continue as though
remembering succeeded. The form remains available with a safe storage error and
the user can retry or clear the checkbox. If final profile publication fails
after authentication, the session remains connected, pending material is
removed, and a non-secret persistent warning explains that credentials were not
saved.

When an existing remembered profile is submitted with the checkbox cleared,
successful authentication removes both its profile metadata and committed
credential. Cancellation or transport failure does not erase an existing saved
profile.

## Layering

### `frd-ui-egui`

Owns rendering, responsive layout, focus handling, password visibility, icon
buttons, tooltips, and creation of protocol-neutral UI intents. It never opens a
credential vault or configuration path.

### `frd-ui-model`

Owns `SavedConnectionProfile`, the recent-profile list, selected profile,
`remember_on_this_device`, form errors, and transient presentation state. Secret
data remains in the existing non-Clone, non-Debug `SecretBuffer`.

### `frd-app`

Coordinates profile selection, secure staging, connection launch, commit on
`TransportReady`, rollback, deletion after successful unremembered login, and
safe status reporting. Protocol adapters remain unaware of saved profiles.

### `frd-platform-api`

Defines narrow object-safe interfaces for non-secret profile metadata and secure
credentials. The interface uses opaque profile and pending-credential IDs; it
does not expose Windows handles or storage paths.

### Platform implementations

- Windows first: Windows Credential Manager generic credentials and a versioned
  metadata record under the current user's local application-data directory.
- macOS: Keychain adapter in the macOS platform crate.
- Linux: Secret Service adapter in the Linux platform crate.
- Android: Keystore-backed encrypted credential adapter in the Android host.
- HarmonyOS NEXT: HUKS-backed adapter in the ArkUI host.

Only Windows is implemented in this phase. Other platform names define the API
boundary, not a claim that their adapters already exist.

## Material Symbols and Application Icon Rules

FreeRemoteDesk-owned function glyphs use a pinned, locally vendored subset of
Google Material Symbols Rounded. Initial login glyphs are `desktop_windows`,
`dns`, `person`, `lock`, `visibility`, `visibility_off`, `expand_more`, `login`,
`shield_lock`, `delete`, and `check_circle`. Default axes are optical size 24,
weight 400, grade 0, and fill 0; fill 1 may distinguish an active state. The
subset, Apache-2.0 license, source URL, and generation command are committed.

The common product mark remains consistent, but packaging is platform-native:
Apple receives unmasked square source layers; Android receives foreground,
background, and monochrome adaptive layers; Windows receives multi-resolution
ICO resources. Platform shells, not protocol or renderer crates, own these
assets.

## Accessibility and Platform Adaptation

- Preserve Windows, macOS, and Linux native window controls and system font
  conventions.
- Adapt spacing, density, focus rings, and colors through shell-level tokens;
  do not fork the login workflow per operating system.
- Every control has a visible label or accessible name. Meaning never relies on
  color alone.
- Keyboard tab order follows visual order. Enter on the password field connects;
  Space activates the remember checkbox and focused icon buttons.
- Password contents, credential IDs, vault target names, and secure-store errors
  are excluded from logs and user diagnostics.

## Acceptance Criteria

- Windows opens the redesigned centered login card in light and dark system
  themes without clipping at supported window scales.
- Multiple successful profiles can be selected and ordered by recency.
- Remembered passwords survive restart and are retrieved from Windows Credential
  Manager; no password bytes appear in the metadata file.
- Failed authentication never replaces an existing committed password.
- Clearing remember and completing authentication removes the saved profile and
  password.
- Enter in the focused password field emits one connection attempt; IME
  composition and repeated key events do not duplicate it.
- Login icon buttons use only the vendored Material Symbols Rounded subset and
  expose Chinese tooltips plus accessible labels.
- Existing Apple protocol, renderer, title-bar, and remote-input focused tests
  remain unchanged and passing.

## Official Design References

- Material Symbols: <https://developers.google.com/fonts/docs/material_symbols>
- Material 3 text fields: <https://m3.material.io/components/text-fields/overview>
- Material 3 tooltips: <https://m3.material.io/components/tooltips/overview>
- Android accessibility targets: <https://developer.android.com/guide/topics/ui/accessibility/views/apps-views>
- Android adaptive icons: <https://developer.android.com/develop/ui/compose/system/icon_design_adaptive>
- Apple text fields: <https://developer.apple.com/design/human-interface-guidelines/text-fields>
- Apple buttons: <https://developer.apple.com/design/human-interface-guidelines/buttons>
- Apple app icons: <https://developer.apple.com/design/human-interface-guidelines/app-icons>
