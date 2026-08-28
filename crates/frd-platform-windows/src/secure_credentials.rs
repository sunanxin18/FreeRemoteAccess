use frd_core::{SecretBuffer, SessionId};
use frd_platform_api::{ConnectionProfileKey, PlatformError, SecureCredentialStore};
use sha2::{Digest, Sha256};

const COMMITTED_PREFIX: &str = "FreeRemoteDesk/profile/";
const PENDING_PREFIX: &str = "FreeRemoteDesk/pending/";
const PENDING_FILTER: &str = "FreeRemoteDesk/pending/*";

pub struct WindowsCredentialStore;

impl WindowsCredentialStore {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for WindowsCredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecureCredentialStore for WindowsCredentialStore {
    fn load(&self, key: &ConnectionProfileKey) -> Result<Option<SecretBuffer>, PlatformError> {
        read_credential(&committed_target(key))
    }

    fn stage(
        &self,
        session: SessionId,
        key: &ConnectionProfileKey,
        password: &SecretBuffer,
    ) -> Result<(), PlatformError> {
        write_credential(&pending_target(session), key.username(), password)
    }

    fn commit(&self, session: SessionId, key: &ConnectionProfileKey) -> Result<(), PlatformError> {
        let pending = pending_target(session);
        let password = read_credential(&pending)?.ok_or(PlatformError::CredentialNotFound)?;
        write_credential(&committed_target(key), key.username(), &password)?;
        delete_credential(&pending)
    }

    fn discard(&self, session: SessionId) -> Result<(), PlatformError> {
        delete_credential(&pending_target(session))
    }

    fn delete(&self, key: &ConnectionProfileKey) -> Result<(), PlatformError> {
        delete_credential(&committed_target(key))
    }

    fn purge_pending(&self) -> Result<(), PlatformError> {
        for target in enumerate_pending_targets()? {
            delete_credential(&target)?;
        }
        Ok(())
    }
}

fn committed_target(key: &ConnectionProfileKey) -> String {
    let mut hash = Sha256::new();
    hash.update(b"FreeRemoteDesk/profile/v1");
    hash_field(&mut hash, key.protocol().as_str().as_bytes());
    hash_field(&mut hash, key.address().as_bytes());
    hash.update(key.port().to_le_bytes());
    hash_field(&mut hash, key.username().as_bytes());
    let digest = hash.finalize();
    let mut target = String::with_capacity(COMMITTED_PREFIX.len() + digest.len() * 2);
    target.push_str(COMMITTED_PREFIX);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        target.push(HEX[(byte >> 4) as usize] as char);
        target.push(HEX[(byte & 0x0f) as usize] as char);
    }
    target
}

fn hash_field(hash: &mut Sha256, field: &[u8]) {
    hash.update((field.len() as u64).to_le_bytes());
    hash.update(field);
}

fn pending_target(session: SessionId) -> String {
    format!("{PENDING_PREFIX}{}-{}", std::process::id(), session.get())
}

#[cfg(windows)]
fn write_credential(
    target: &str,
    username: &str,
    password: &SecretBuffer,
) -> Result<(), PlatformError> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Security::Credentials::{
        CredWriteW, CREDENTIALW, CRED_MAX_CREDENTIAL_BLOB_SIZE,
        CRED_MAX_GENERIC_TARGET_NAME_LENGTH, CRED_MAX_USERNAME_LENGTH, CRED_PERSIST_LOCAL_MACHINE,
        CRED_TYPE_GENERIC,
    };

    let password = password
        .expose_text()
        .ok_or(PlatformError::StorageFailed)?
        .as_bytes();
    if password.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
        return Err(PlatformError::CredentialTooLarge);
    }
    let credential_blob_size =
        u32::try_from(password.len()).map_err(|_| PlatformError::CredentialTooLarge)?;
    let mut target = wide_input(target, CRED_MAX_GENERIC_TARGET_NAME_LENGTH as usize)?;
    let mut username = wide_input(username, CRED_MAX_USERNAME_LENGTH as usize)?;
    let credential = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        Comment: std::ptr::null_mut(),
        LastWritten: FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        },
        CredentialBlobSize: credential_blob_size,
        CredentialBlob: if password.is_empty() {
            std::ptr::null_mut()
        } else {
            password.as_ptr().cast_mut()
        },
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: std::ptr::null_mut(),
        TargetAlias: std::ptr::null_mut(),
        UserName: username.as_mut_ptr(),
    };
    // SAFETY: all pointers refer to live buffers for the duration of the call. CredWriteW treats
    // CredentialBlob as input and does not mutate it; lengths and nul terminators are validated.
    let written = unsafe { CredWriteW(&credential, 0) };
    if written == 0 {
        Err(PlatformError::StorageFailed)
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn write_credential(_: &str, _: &str, _: &SecretBuffer) -> Result<(), PlatformError> {
    Err(PlatformError::Unavailable)
}

#[cfg(windows)]
fn read_credential(target: &str) -> Result<Option<SecretBuffer>, PlatformError> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND};
    use windows_sys::Win32::Security::Credentials::{
        CredReadW, CREDENTIALW, CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_MAX_GENERIC_TARGET_NAME_LENGTH,
        CRED_TYPE_GENERIC,
    };

    let target = wide_input(target, CRED_MAX_GENERIC_TARGET_NAME_LENGTH as usize)?;
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: target is a live nul-terminated UTF-16 string and credential is a valid out pointer.
    let read = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if read == 0 {
        // SAFETY: GetLastError is read immediately after the failed Win32 call.
        let error = unsafe { GetLastError() };
        return if error == ERROR_NOT_FOUND {
            Ok(None)
        } else {
            Err(PlatformError::StorageFailed)
        };
    }
    if credential.is_null() {
        return Err(PlatformError::StorageFailed);
    }
    let allocation = ReadCredentialAllocation(credential);
    // SAFETY: a successful CredReadW returns one initialized CREDENTIALW allocation owned by
    // allocation until the end of this function.
    let credential = unsafe { &*allocation.0 };
    let blob_len = credential.CredentialBlobSize as usize;
    if blob_len > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
        return Err(PlatformError::CredentialTooLarge);
    }
    if blob_len == 0 {
        return Ok(Some(SecretBuffer::new(Vec::new())));
    }
    if credential.CredentialBlob.is_null() {
        return Err(PlatformError::StorageFailed);
    }
    // SAFETY: the Win32 allocation describes CredentialBlobSize initialized bytes, and the
    // allocation remains live while the bytes are copied into the wiping SecretBuffer owner.
    let secret = SecretBuffer::new(unsafe {
        std::slice::from_raw_parts(credential.CredentialBlob, blob_len).to_vec()
    });
    Ok(Some(secret))
}

#[cfg(not(windows))]
fn read_credential(_: &str) -> Result<Option<SecretBuffer>, PlatformError> {
    Err(PlatformError::Unavailable)
}

#[cfg(windows)]
fn delete_credential(target: &str) -> Result<(), PlatformError> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND};
    use windows_sys::Win32::Security::Credentials::{
        CredDeleteW, CRED_MAX_GENERIC_TARGET_NAME_LENGTH, CRED_TYPE_GENERIC,
    };

    let target = wide_input(target, CRED_MAX_GENERIC_TARGET_NAME_LENGTH as usize)?;
    // SAFETY: target is a live nul-terminated UTF-16 string for the duration of the call.
    let deleted = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
    if deleted != 0 {
        return Ok(());
    }
    // SAFETY: GetLastError is read immediately after the failed Win32 call.
    let error = unsafe { GetLastError() };
    if error == ERROR_NOT_FOUND {
        Ok(())
    } else {
        Err(PlatformError::StorageFailed)
    }
}

#[cfg(not(windows))]
fn delete_credential(_: &str) -> Result<(), PlatformError> {
    Err(PlatformError::Unavailable)
}

#[cfg(windows)]
fn enumerate_pending_targets() -> Result<Vec<String>, PlatformError> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND};
    use windows_sys::Win32::Security::Credentials::{
        CredEnumerateW, CREDENTIALW, CRED_MAX_GENERIC_TARGET_NAME_LENGTH, CRED_TYPE_GENERIC,
    };

    let filter = wide_input(PENDING_FILTER, CRED_MAX_GENERIC_TARGET_NAME_LENGTH as usize)?;
    let mut count = 0;
    let mut credentials: *mut *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: filter is a live nul-terminated UTF-16 string and both output pointers are valid.
    let enumerated = unsafe { CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut credentials) };
    if enumerated == 0 {
        // SAFETY: GetLastError is read immediately after the failed Win32 call.
        let error = unsafe { GetLastError() };
        return if error == ERROR_NOT_FOUND {
            Ok(Vec::new())
        } else {
            Err(PlatformError::StorageFailed)
        };
    }
    if credentials.is_null() {
        return Err(PlatformError::StorageFailed);
    }
    let allocation = EnumeratedCredentialAllocation { credentials, count };
    let mut targets = Vec::new();
    for index in 0..count as usize {
        // SAFETY: CredEnumerateW returned count initialized entries in the pointer array.
        let credential = unsafe { *allocation.credentials.add(index) };
        if credential.is_null() {
            return Err(PlatformError::StorageFailed);
        }
        // SAFETY: each non-null entry points to an initialized CREDENTIALW owned by allocation.
        let credential = unsafe { &*credential };
        if credential.Type != CRED_TYPE_GENERIC {
            continue;
        }
        let target = wide_output(
            credential.TargetName,
            CRED_MAX_GENERIC_TARGET_NAME_LENGTH as usize,
        )?;
        if target.starts_with(PENDING_PREFIX) {
            targets.push(target);
        }
    }
    Ok(targets)
}

#[cfg(not(windows))]
fn enumerate_pending_targets() -> Result<Vec<String>, PlatformError> {
    Err(PlatformError::Unavailable)
}

#[cfg(windows)]
fn wide_input(value: &str, maximum_units: usize) -> Result<Vec<u16>, PlatformError> {
    if value.contains('\0') {
        return Err(PlatformError::InvalidProfile);
    }
    let mut encoded = value.encode_utf16().collect::<Vec<_>>();
    if encoded.len() > maximum_units {
        return Err(PlatformError::InvalidProfile);
    }
    encoded.push(0);
    Ok(encoded)
}

#[cfg(windows)]
fn wide_output(pointer: *const u16, maximum_units: usize) -> Result<String, PlatformError> {
    if pointer.is_null() {
        return Err(PlatformError::StorageFailed);
    }
    let mut length = 0;
    while length <= maximum_units {
        // SAFETY: Win32 promises a nul-terminated target no longer than the documented maximum;
        // length remains inside that API-owned output contract.
        if unsafe { *pointer.add(length) } == 0 {
            // SAFETY: the preceding scan established length initialized UTF-16 units.
            let units = unsafe { std::slice::from_raw_parts(pointer, length) };
            return String::from_utf16(units).map_err(|_| PlatformError::StorageFailed);
        }
        length += 1;
    }
    Err(PlatformError::StorageFailed)
}

#[cfg(windows)]
struct ReadCredentialAllocation(*mut windows_sys::Win32::Security::Credentials::CREDENTIALW);

#[cfg(windows)]
impl Drop for ReadCredentialAllocation {
    fn drop(&mut self) {
        use windows_sys::Win32::Security::Credentials::CredFree;

        // SAFETY: this object uniquely owns the successful CredReadW allocation. Nested fields
        // are part of that allocation and must not be freed separately.
        unsafe {
            wipe_credential_blob(self.0);
            CredFree(self.0.cast());
        }
    }
}

#[cfg(windows)]
struct EnumeratedCredentialAllocation {
    credentials: *mut *mut windows_sys::Win32::Security::Credentials::CREDENTIALW,
    count: u32,
}

#[cfg(windows)]
impl Drop for EnumeratedCredentialAllocation {
    fn drop(&mut self) {
        use windows_sys::Win32::Security::Credentials::CredFree;

        // SAFETY: this object uniquely owns the successful CredEnumerateW pointer-array
        // allocation. Its count entries and nested credentials stay valid until the one CredFree.
        unsafe {
            for index in 0..self.count as usize {
                wipe_credential_blob(*self.credentials.add(index));
            }
            CredFree(self.credentials.cast());
        }
    }
}

#[cfg(windows)]
unsafe fn wipe_credential_blob(
    credential: *mut windows_sys::Win32::Security::Credentials::CREDENTIALW,
) {
    if credential.is_null() {
        return;
    }
    // SAFETY: callers hold the owning CredReadW/CredEnumerateW allocation, whose nested blob is
    // writable and valid for CredentialBlobSize bytes until CredFree.
    let credential = unsafe { &mut *credential };
    if credential.CredentialBlob.is_null() {
        return;
    }
    for index in 0..credential.CredentialBlobSize as usize {
        // SAFETY: index is within the Win32-reported credential blob allocation.
        unsafe { std::ptr::write_volatile(credential.CredentialBlob.add(index), 0) };
    }
}

#[cfg(test)]
mod tests {
    use frd_core::{ProtocolId, SecretBuffer, SessionId};
    use frd_platform_api::{ConnectionProfileKey, PlatformError, SecureCredentialStore};

    use super::{
        committed_target, delete_credential, pending_target, read_credential,
        WindowsCredentialStore,
    };

    fn test_key() -> ConnectionProfileKey {
        ConnectionProfileKey::new(
            ProtocolId::apple_hpss_mvs(),
            "credential-target-test.invalid",
            5900,
            "credential-target-user",
        )
        .expect("test profile key is valid")
    }

    #[test]
    fn credential_target_hides_profile_identity() {
        let key = test_key();
        let first = committed_target(&key);
        let second = committed_target(&key);

        assert!(first.starts_with("FreeRemoteDesk/profile/"));
        assert!(first == second);
        assert!(!first.contains(key.address()));
        assert!(!first.contains(key.username()));
    }

    #[cfg(windows)]
    struct CredentialCleanup {
        pending: String,
        committed: String,
    }

    #[cfg(windows)]
    impl Drop for CredentialCleanup {
        fn drop(&mut self) {
            let _ = delete_credential(&self.pending);
            let _ = delete_credential(&self.committed);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_vault_stages_commits_loads_discards_purges_and_deletes() {
        let session = SessionId::allocate();
        let unique = format!("{}-{}", std::process::id(), session.get());
        let key = ConnectionProfileKey::new(
            ProtocolId::apple_hpss_mvs(),
            format!("vault-test-{unique}.invalid"),
            5900,
            format!("vault-test-user-{unique}"),
        )
        .expect("test profile key is valid");
        let pending = pending_target(session);
        let committed = committed_target(&key);
        let _cleanup = CredentialCleanup {
            pending: pending.clone(),
            committed,
        };
        let store = WindowsCredentialStore::new();
        let password = SecretBuffer::from_text(format!("vault-test-secret-{unique}"));

        store
            .stage(session, &key, &password)
            .expect("pending credential is staged");
        store
            .commit(session, &key)
            .expect("pending credential is committed");
        assert!(
            read_credential(&pending)
                .expect("committed pending credential can be checked")
                .is_none(),
            "commit left the pending credential in the vault"
        );
        let loaded = store
            .load(&key)
            .expect("committed credential is read")
            .expect("committed credential exists");
        assert!(
            loaded
                .expose_text()
                .zip(password.expose_text())
                .is_some_and(|(actual, expected)| actual == expected),
            "loaded credential differs from the staged credential"
        );

        store.delete(&key).expect("committed credential is deleted");
        assert!(
            store
                .load(&key)
                .expect("missing committed credential is handled")
                .is_none(),
            "deleted credential remained in the vault"
        );
        store
            .delete(&key)
            .expect("deleting a missing committed credential is idempotent");

        store
            .stage(session, &key, &password)
            .expect("second pending credential is staged");
        store
            .discard(session)
            .expect("pending credential is discarded");
        assert!(
            read_credential(&pending)
                .expect("discarded pending credential can be checked")
                .is_none(),
            "discarded pending credential remained in the vault"
        );
        assert!(matches!(
            store.commit(session, &key),
            Err(PlatformError::CredentialNotFound)
        ));

        store
            .stage(session, &key, &password)
            .expect("third pending credential is staged");
        store
            .purge_pending()
            .expect("pending credential prefix is purged");
        assert!(
            read_credential(&pending)
                .expect("purged pending credential can be checked")
                .is_none(),
            "purged pending credential remained in the vault"
        );
        store
            .purge_pending()
            .expect("purging an empty pending prefix is idempotent");
    }

    #[cfg(windows)]
    #[test]
    fn windows_vault_rejects_oversized_and_invalid_utf16_inputs_before_writing() {
        let store = WindowsCredentialStore::new();
        let session = SessionId::allocate();
        let key = test_key();
        let oversized = SecretBuffer::new(vec![b'x'; 2561]);
        assert!(matches!(
            store.stage(session, &key, &oversized),
            Err(PlatformError::CredentialTooLarge)
        ));

        let invalid_username = ConnectionProfileKey::new(
            ProtocolId::apple_hpss_mvs(),
            "invalid-utf16-test.invalid",
            5900,
            "invalid\0username",
        )
        .expect("profile API currently accepts embedded nul characters");
        let password = SecretBuffer::from_text("unused-test-secret".to_owned());
        assert!(matches!(
            store.stage(session, &invalid_username, &password),
            Err(PlatformError::InvalidProfile)
        ));
    }
}
