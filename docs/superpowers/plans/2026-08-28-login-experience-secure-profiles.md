# Login Experience and Secure Profiles Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the approved centered Windows login card, multiple recent connections, Windows Credential Manager password persistence, Enter-to-connect, and Material Symbols Rounded login controls without changing protocol or renderer behavior.

**Architecture:** `frd-ui-egui` renders protocol-neutral presentation state, `frd-ui-model` owns the editable form and saved-profile summaries, `frd-app` owns the remember/commit/rollback state machine, and two `frd-platform-api` traits separate non-secret profile metadata from secure credentials. `frd-platform-windows` implements those traits with a versioned local metadata file and Windows Credential Manager; the Windows app only composes the implementations.

**Tech Stack:** Rust 2021, egui/winit/wgpu, `windows-sys` Win32 Credentials API, serde/serde_json for private Windows metadata records, Google Material Symbols Rounded subset.

**Spec:** `docs/superpowers/specs/2026-08-28-login-experience-secure-profiles-design.md`

## Global Constraints

- Windows is the only platform implementation in this phase; every public storage interface remains platform-neutral.
- Passwords enter only `SecretBuffer` and the OS secure credential store; never ordinary metadata, logs, CLI arguments, diagnostics, snapshots, or `Debug` output.
- A pending credential becomes committed only at `ConnectionStage::TransportReady`; explicit failure, cancellation, and launch rollback discard it.
- Button click and Enter in the focused password field use the same `AppIntent::Connect` path and emit at most one start action.
- FreeRemoteDesk-owned functional glyphs use only the pinned Google Material Symbols Rounded subset.
- Do not change Apple HPSS, MVS, media, wire, renderer-core, or remote-input behavior.
- Add only focused storage, orchestration, and UI behavior tests; do not expand protocol test coverage.
- Stage and commit only exact task files because the worktree contains unrelated user changes.

---

### Task 1: Platform-Neutral Saved-Profile Contracts

**Files:**
- Modify: `crates/frd-platform-api/src/lib.rs`

**Interfaces:**
- Produces: `ConnectionProfileKey`, `SavedConnectionProfile`, `ConnectionProfileStore`, and `SecureCredentialStore`.
- Consumes: `TargetSystem`, `ProtocolId`, `SecretBuffer`, and `SessionId` from `frd-core`.

- [ ] **Step 1: Write failing contract tests**

Add tests that construct one key/profile, reject empty key fields, and prove the public profile does not expose a password field:

```rust
#[test]
fn profile_key_rejects_empty_identity_fields() {
    assert!(ConnectionProfileKey::new(
        ProtocolId::new("apple-hpss").unwrap(),
        "",
        5900,
        "sun",
    )
    .is_none());
}

#[test]
fn saved_profile_orders_newest_success_first() {
    let older = test_profile(1);
    let newer = test_profile(2);
    let mut profiles = vec![older, newer.clone()];
    SavedConnectionProfile::sort_most_recent(&mut profiles);
    assert_eq!(profiles[0], newer);
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test -p frd-platform-api profile_key_rejects_empty_identity_fields -- --exact`

Expected: compilation fails because `ConnectionProfileKey` does not exist.

- [ ] **Step 3: Add the minimal public contracts**

Define these signatures without platform paths or Win32 types:

```rust
pub struct ConnectionProfileKey {
    protocol: ProtocolId,
    address: String,
    port: u16,
    username: String,
}

impl ConnectionProfileKey {
    pub fn new(protocol: ProtocolId, address: impl Into<String>, port: u16,
               username: impl Into<String>) -> Option<Self>;
    pub fn protocol(&self) -> &ProtocolId;
    pub fn address(&self) -> &str;
    pub fn port(&self) -> u16;
    pub fn username(&self) -> &str;
}

pub struct SavedConnectionProfile {
    pub key: ConnectionProfileKey,
    pub target_system: TargetSystem,
    pub last_success_order: u64,
}

pub trait ConnectionProfileStore: Send + Sync {
    fn list(&self) -> Result<Vec<SavedConnectionProfile>, PlatformError>;
    fn upsert(&self, profile: &SavedConnectionProfile) -> Result<(), PlatformError>;
    fn delete(&self, key: &ConnectionProfileKey) -> Result<(), PlatformError>;
}

pub trait SecureCredentialStore: Send + Sync {
    fn load(&self, key: &ConnectionProfileKey) -> Result<Option<SecretBuffer>, PlatformError>;
    fn stage(&self, session: SessionId, key: &ConnectionProfileKey,
             password: &SecretBuffer) -> Result<(), PlatformError>;
    fn commit(&self, session: SessionId, key: &ConnectionProfileKey) -> Result<(), PlatformError>;
    fn discard(&self, session: SessionId) -> Result<(), PlatformError>;
    fn delete(&self, key: &ConnectionProfileKey) -> Result<(), PlatformError>;
    fn purge_pending(&self) -> Result<(), PlatformError>;
}
```

Add stable error variants `InvalidProfile`, `CredentialNotFound`, and `CredentialTooLarge`; none carries secret data or raw platform text.

- [ ] **Step 4: Run platform API tests and verify GREEN**

Run: `cargo test -p frd-platform-api`

Expected: all tests pass without warnings.

- [ ] **Step 5: Commit the contract**

```powershell
git add -- crates/frd-platform-api/src/lib.rs
git commit -m "feat: define secure connection profile stores"
```

### Task 2: Windows Profile Metadata Store

**Files:**
- Create: `crates/frd-platform-windows/src/connection_profiles.rs`
- Modify: `crates/frd-platform-windows/src/lib.rs`
- Modify: `crates/frd-platform-windows/Cargo.toml`

**Interfaces:**
- Consumes: `ConnectionProfileStore`, `ConnectionProfileKey`, and `SavedConnectionProfile` from Task 1.
- Produces: `WindowsConnectionProfileStore::current_user_default()` and `WindowsConnectionProfileStore::at_path(PathBuf)`.

- [ ] **Step 1: Write failing round-trip and secret-exclusion tests**

Use `tempfile::tempdir()` and assert newest-first ordering, replacement by exact key, deletion, and absence of the test password in the file bytes:

```rust
#[test]
fn metadata_round_trip_contains_no_password_material() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("connections-v1.json");
    let store = WindowsConnectionProfileStore::at_path(path.clone());
    store.upsert(&test_profile()).unwrap();
    let bytes = std::fs::read(path).unwrap();
    assert!(!bytes.windows(b"test-password".len())
        .any(|window| window == b"test-password"));
    assert_eq!(store.list().unwrap(), vec![test_profile()]);
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test -p frd-platform-windows metadata_round_trip_contains_no_password_material -- --exact`

Expected: compilation fails because `WindowsConnectionProfileStore` does not exist.

- [ ] **Step 3: Implement versioned atomic metadata persistence**

Add private serde wire records with `version: 1`, map target systems to `macos`, `windows`, `linux`, or `custom`, reject unknown versions and malformed keys, and atomically replace `connections-v1.json` through a PID-suffixed temporary file. `current_user_default()` resolves `%LOCALAPPDATA%\FreeRemoteDesk\connections-v1.json`. Sort reads by descending `last_success_order`; do not add a password member to any wire type.

Add private dependencies:

```toml
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

- [ ] **Step 4: Run Windows platform tests and verify GREEN**

Run: `cargo test -p frd-platform-windows connection_profiles`

Expected: round-trip, replacement, malformed-record, and delete tests pass.

- [ ] **Step 5: Commit the metadata store**

```powershell
git add -- crates/frd-platform-windows/Cargo.toml crates/frd-platform-windows/src/connection_profiles.rs crates/frd-platform-windows/src/lib.rs Cargo.lock
git commit -m "feat: persist non-secret Windows connection profiles"
```

### Task 3: Windows Credential Manager Store

**Files:**
- Create: `crates/frd-platform-windows/src/secure_credentials.rs`
- Modify: `crates/frd-platform-windows/src/lib.rs`
- Modify: `crates/frd-platform-windows/Cargo.toml`

**Interfaces:**
- Consumes: `SecureCredentialStore` from Task 1.
- Produces: `WindowsCredentialStore` using generic credential targets under `FreeRemoteDesk/profile/` and `FreeRemoteDesk/pending/`.

- [ ] **Step 1: Write failing deterministic-target and bounded live-vault tests**

Test that the same profile key produces the same SHA-256 target without embedding address or username. Under `cfg(windows)`, use a process-unique pending ID, stage/read/commit/delete a `SecretBuffer`, and install a drop guard that deletes both targets even after assertion failure.

```rust
#[test]
fn credential_target_hides_profile_identity() {
    let key = test_key();
    let target = committed_target(&key);
    assert!(target.starts_with("FreeRemoteDesk/profile/"));
    assert!(!target.contains(key.address()));
    assert!(!target.contains(key.username()));
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test -p frd-platform-windows credential_target_hides_profile_identity -- --exact`

Expected: compilation fails because `committed_target` and `WindowsCredentialStore` do not exist.

- [ ] **Step 3: Implement Win32 generic credential operations**

Enable `windows-sys` feature `Win32_Security_Credentials`. Wrap `CredWriteW`, `CredReadW`, `CredDeleteW`, `CredEnumerateW`, and `CredFree`; use `CRED_TYPE_GENERIC`, `CRED_PERSIST_LOCAL_MACHINE`, UTF-16 target/user strings, and the password's UTF-8 bytes as the credential blob. Copy a read blob directly into `SecretBuffer`, call `CredFree` on every successful read/enumeration, and map `ERROR_NOT_FOUND` to `Ok(None)` only for load/delete. Reject blobs larger than the Win32 generic credential limit.

`stage` writes `FreeRemoteDesk/pending/<session-id>`; `commit` reads the pending secret, writes the SHA-256 committed target, and deletes pending only after the final write succeeds. `purge_pending` enumerates exactly `FreeRemoteDesk/pending/*` and deletes only matching generic credentials.

- [ ] **Step 4: Run the secure-store tests and verify GREEN**

Run: `cargo test -p frd-platform-windows secure_credentials -- --test-threads=1`

Expected: all bounded vault operations pass and their cleanup guard leaves no process-unique credential.

- [ ] **Step 5: Commit the secure store**

```powershell
git add -- crates/frd-platform-windows/Cargo.toml crates/frd-platform-windows/src/secure_credentials.rs crates/frd-platform-windows/src/lib.rs Cargo.lock
git commit -m "feat: store remote passwords in Windows Credential Manager"
```

### Task 4: Application Remember/Commit/Rollback State Machine

**Files:**
- Modify: `crates/frd-ui-model/src/lib.rs`
- Modify: `crates/frd-app/src/lib.rs`
- Modify: `crates/frd-app/src/controller.rs`
- Modify: `crates/frd-app/Cargo.toml`

**Interfaces:**
- Consumes: both storage traits from Tasks 1–3.
- Produces: `AppPlatformStores<'a>`, `AppIntent::SelectSavedProfile`, and pending profile state bound to one `SessionId`.

- [ ] **Step 1: Write failing UI-model tests**

Add tests proving that selecting a profile replaces only non-secret draft fields, resets password visibility, and that `ConnectionSubmission` carries `remember_on_this_device` plus the selected profile key without deriving `Clone` or `Debug`.

- [ ] **Step 2: Verify the UI-model RED state**

Run: `cargo test -p frd-ui-model selecting_saved_profile_replaces_connection_draft -- --exact`

Expected: compilation fails because saved-profile state does not exist.

- [ ] **Step 3: Implement minimal form state**

Add `profiles: Vec<SavedConnectionProfile>`, `selected_profile: Option<ConnectionProfileKey>`, `remember_on_this_device: bool`, and `password_visible: bool` to `ConnectionForm`. Add methods `set_profiles`, `select_profile_metadata`, `set_loaded_password`, and `set_profile_storage_error`; keep the form itself non-Clone and non-Debug.

- [ ] **Step 4: Verify UI-model GREEN**

Run: `cargo test -p frd-ui-model`

Expected: all form validation and new profile-selection tests pass.

- [ ] **Step 5: Write failing application transaction tests**

Use small in-memory trait implementations and cover exactly these transitions:

```rust
#[test]
fn remembered_password_commits_only_after_transport_ready() {
    let fixture = RememberFixture::with_saved_password("old-password");
    let session = fixture.submit_remembered("new-password");
    assert_eq!(fixture.committed_password(), Some("old-password"));
    assert!(fixture.pending_exists(session));
    fixture.publish_stage(session, ConnectionStage::TransportReady);
    assert_eq!(fixture.committed_password(), Some("new-password"));
    assert!(!fixture.pending_exists(session));
}

#[test]
fn authentication_failure_discards_pending_without_overwriting_committed() {
    let fixture = RememberFixture::with_saved_password("old-password");
    let session = fixture.submit_remembered("wrong-password");
    fixture.publish_failure(session, "apple_hpss_session_failed");
    assert_eq!(fixture.committed_password(), Some("old-password"));
    assert!(!fixture.pending_exists(session));
}

#[test]
fn successful_unremembered_login_deletes_selected_profile() {
    let fixture = RememberFixture::with_saved_password("old-password");
    let session = fixture.submit_without_remembering("old-password");
    assert!(fixture.profile_exists());
    fixture.publish_stage(session, ConnectionStage::TransportReady);
    assert!(!fixture.profile_exists());
    assert_eq!(fixture.committed_password(), None);
}

#[test]
fn selecting_profile_loads_only_its_credential() {
    let fixture = RememberFixture::with_two_profiles();
    fixture.select_profile(1);
    assert_eq!(fixture.credential_loads(), vec![fixture.profile_key(1)]);
    assert_eq!(fixture.form_password(), "selected-password");
}
```

`RememberFixture` is a test-only helper containing the real controller plus
in-memory implementations of both Task 1 storage traits; it must not be added to
production code.

- [ ] **Step 6: Verify the transaction RED state**

Run: `cargo test -p frd-app remembered_password_commits_only_after_transport_ready -- --exact`

Expected: compilation fails because the controller has no pending profile transaction.

- [ ] **Step 7: Implement the application transaction**

Add:

```rust
pub struct AppPlatformStores<'a> {
    pub server_identities: &'a dyn ServerIdentityStore,
    pub profiles: &'a dyn ConnectionProfileStore,
    pub credentials: &'a dyn SecureCredentialStore,
}
```

Load/sort profiles when constructing the connection page. Handle profile selection by loading exactly one password. During valid remembered submission, allocate the session, stage the password before moving it into `ConnectRequest`, and record a non-secret pending operation. On `TransportReady`, commit the credential then upsert metadata with a monotonically increasing `last_success_order`. On terminal failure, cancellation, and launch rollback, discard pending. For an unchecked selected profile, defer delete until `TransportReady`. Return stable safe errors to the form; never format keys, usernames, or platform error internals.

- [ ] **Step 8: Verify application GREEN**

Run: `cargo test -p frd-app`

Expected: all existing session lifecycle tests and four new persistence tests pass.

- [ ] **Step 9: Commit the state machine**

```powershell
git add -- crates/frd-ui-model/src/lib.rs crates/frd-app/Cargo.toml crates/frd-app/src/lib.rs crates/frd-app/src/controller.rs Cargo.lock
git commit -m "feat: coordinate remembered connection transactions"
```

### Task 5: Centered Login Card, Enter Submission, and Tooltips

**Files:**
- Modify: `crates/frd-ui-egui/src/connection.rs`
- Modify: `crates/frd-ui-egui/src/lib.rs`
- Create: `crates/frd-ui-egui/src/login_icons.rs`
- Modify: `crates/frd-shell-desktop/src/application.rs`
- Modify: `crates/frd-shell-desktop/src/ui_fonts.rs`

**Interfaces:**
- Consumes: the enhanced `ConnectionForm` and existing Material Symbols font family.
- Produces: responsive login-card rendering and one `AppIntent` per frame.

- [ ] **Step 1: Write failing pure interaction tests**

Extract and test a small decision function:

```rust
assert_eq!(
    connect_trigger(ConnectTriggerInput {
        button_clicked: false,
        password_has_focus: true,
        enter_pressed: true,
        ime_composing: false,
        connection_busy: false,
    }),
    ConnectTrigger::Submit,
);
```

Add separate assertions that IME composition, auto-repeat within the same frame,
and `connection_busy` return `ConnectTrigger::None`.

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test -p frd-ui-egui password_enter_submits_once -- --exact`

Expected: compilation fails because `connect_trigger` does not exist.

- [ ] **Step 3: Implement interaction and login icon semantics**

Define exact name/codepoint/tooltip/accessibility mappings for `desktop_windows`, `dns`, `person`, `lock`, `visibility`, `visibility_off`, `expand_more`, `login`, `shield_lock`, `delete`, and `check_circle`. Reuse the isolated Material Symbols font family, and make every icon-only response use `on_hover_text` plus accessible widget text.

Use one submit boolean per frame and call `form.take_submission(catalog)` once after all controls render. Check Enter only on the password response and ignore IME composition and repeated submission while busy.

- [ ] **Step 4: Render the approved responsive card**

Center a maximum-width 460-point frame in the available login area; use 16-point rounding, 24-point internal margins, visible labels, 48-point primary button height, and platform light/dark visuals. At widths below 520 points stack all fields; at wider widths place target/protocol and address/port in two rows. Show unsupported catalog entries disabled with `即将支持`. Keep validation adjacent to each field and use `系统安全凭据库保护密码` as supporting copy.

- [ ] **Step 5: Run UI tests and verify GREEN**

Run: `cargo test -p frd-ui-egui`

Expected: interaction, semantic icon, and existing session chrome tests pass.

- [ ] **Step 6: Commit the login UI**

```powershell
git add -- crates/frd-ui-egui/src/connection.rs crates/frd-ui-egui/src/lib.rs crates/frd-ui-egui/src/login_icons.rs crates/frd-shell-desktop/src/application.rs crates/frd-shell-desktop/src/ui_fonts.rs
git commit -m "feat: redesign the secure desktop login card"
```

### Task 6: Expand and Verify the Material Symbols Rounded Subset

**Files:**
- Modify: `assets/ui-icons/material-symbols-rounded-24-400.ttf`
- Modify: `assets/ui-icons/README.md`
- Create: `tools/update-material-symbols-rounded.ps1`

**Interfaces:**
- Consumes: pinned upstream commit `84ccef280841abfac506afc4ad4a2782f6d0a1d0` and FontTools 4.63.0.
- Produces: one deterministic offline font containing title-bar and login glyphs.

- [ ] **Step 1: Write the deterministic subset script**

Pin the upstream URL and SHA-256 already recorded in `assets/ui-icons/README.md`. Instantiate optical size 24, weight 400, fill 0, grade 0, and subset exactly the existing title-bar names plus the eleven login names from Task 5. The script verifies the upstream hash before processing and prints the resulting subset hash.

- [ ] **Step 2: Run the script and update provenance**

Run: `powershell -ExecutionPolicy Bypass -File tools/update-material-symbols-rounded.ps1`

Expected: the TTF is replaced, the command exits zero, and the reported hash is copied into `assets/ui-icons/README.md` with the complete sorted glyph-name list.

- [ ] **Step 3: Run semantic font tests**

Run: `cargo test -p frd-ui-egui login_icons`

Expected: every declared login codepoint resolves in the vendored font and no test uses an Emoji or fallback glyph.

- [ ] **Step 4: Commit the pinned asset**

```powershell
git add -- assets/ui-icons/material-symbols-rounded-24-400.ttf assets/ui-icons/README.md tools/update-material-symbols-rounded.ps1
git commit -m "chore: extend the pinned rounded symbol subset"
```

### Task 7: Windows Composition and Product Verification

**Files:**
- Modify: `apps/freeremotedesk-windows/src/main.rs`
- Modify: `apps/freeremotedesk-windows/Cargo.toml`
- Modify: `crates/frd-shell-desktop/src/application.rs`
- Create: `docs/validation/windows-secure-login.md`

**Interfaces:**
- Consumes: Windows stores, app transaction coordinator, and login UI from Tasks 1–6.
- Produces: the runnable Windows product with startup pending-credential cleanup.

- [ ] **Step 1: Write a failing composition boundary test**

Extend `apps/freeremotedesk-windows/tests/dependency_boundary.rs` to require `frd-platform-windows` to remain the only crate importing `Win32_Security_Credentials`, and assert that protocol/render crates do not depend on profile or secure-store implementation modules.

- [ ] **Step 2: Run the boundary test and verify RED**

Run: `cargo test -p freeremotedesk-windows --test dependency_boundary`

Expected: the new composition assertion fails until the Windows stores are wired.

- [ ] **Step 3: Wire Windows platform stores**

Construct `WindowsConnectionProfileStore` and `WindowsCredentialStore` in `main.rs`, call `purge_pending()` before creating `AppLaunch`, and pass both stores with the existing `DpapiServerIdentityStore` through the shell/app service bundle. Map initialization failure to a stable fatal operation and reason without a path, username, target, or raw Win32 code.

- [ ] **Step 4: Run automated verification**

Run in this order:

```powershell
cargo fmt -- --check
cargo test -p frd-platform-api
cargo test -p frd-platform-windows -- --test-threads=1
cargo test -p frd-ui-model
cargo test -p frd-app
cargo test -p frd-ui-egui
cargo test -p freeremotedesk-windows
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p freeremotedesk-windows
```

Expected: every command exits zero with no warnings.

- [ ] **Step 5: Perform bounded local Windows validation**

Launch the release executable without CLI credentials. Verify light/dark theme rendering, recent-profile selection, password visibility tooltip, Enter submission, exactly one connection launch, and that the metadata file contains no entered password bytes. After a successful authorized connection, verify the generic credential exists through Windows Credential Manager UI; after clearing remember and successfully reconnecting, verify both the profile and credential are absent. Do not print or capture the password.

Record command results, binary SHA-256, and each observed UI/storage outcome in `docs/validation/windows-secure-login.md`. Label Mac interoperability as not run if the target is unavailable; do not infer it from local tests.

- [ ] **Step 6: Commit integration and evidence**

```powershell
git add -- apps/freeremotedesk-windows/Cargo.toml apps/freeremotedesk-windows/src/main.rs apps/freeremotedesk-windows/tests/dependency_boundary.rs crates/frd-shell-desktop/src/application.rs docs/validation/windows-secure-login.md Cargo.lock
git commit -m "feat: integrate secure remembered connections on Windows"
```

## Self-Review Record

- Spec coverage: visual hierarchy, multiple profiles, secure staging/commit,
  Enter deduplication, tooltip semantics, official icons, platform boundaries,
  and Windows composition each map to one or more tasks above.
- Placeholder scan: every task names concrete behavior, files, commands, and
  expected outcomes without deferred implementation markers.
- Type consistency: every later task uses the Task 1 store names and the Task 4
  `AppPlatformStores` bundle; profile identity is consistently
  `ConnectionProfileKey` and pending identity is consistently `SessionId`.
