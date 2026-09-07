use frd_core::{SecretBuffer, SessionId};
use frd_platform_api::{ConnectionProfileKey, PlatformError, SecureCredentialStore};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;

#[cfg(target_os = "macos")]
const SERVICE: &str = "org.freeremotedesk.credentials.v1";
const MAX_CREDENTIAL_BYTES: usize = 65_536;
struct PendingCredential {
    key: ConnectionProfileKey,
    password: SecretBuffer,
}
/// 尚未认证的密码只保留在可清零内存中；进程退出不会留下待提交文件或钥匙串条目。
pub struct MacCredentialStore {
    pending: Mutex<HashMap<u64, PendingCredential>>,
}
impl MacCredentialStore {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }
}
impl Default for MacCredentialStore {
    fn default() -> Self {
        Self::new()
    }
}
impl SecureCredentialStore for MacCredentialStore {
    fn load(&self, key: &ConnectionProfileKey) -> Result<Option<SecretBuffer>, PlatformError> {
        read_password(&account(key))
    }
    fn stage(
        &self,
        session: SessionId,
        key: &ConnectionProfileKey,
        password: &SecretBuffer,
    ) -> Result<(), PlatformError> {
        let text = password
            .expose_text()
            .ok_or(PlatformError::CredentialProviderFailed)?;
        if text.len() > MAX_CREDENTIAL_BYTES {
            return Err(PlatformError::CredentialTooLarge);
        }
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| PlatformError::StorageFailed)?;
        pending.insert(
            session.get(),
            PendingCredential {
                key: key.clone(),
                password: SecretBuffer::new(text.as_bytes().to_vec()),
            },
        );
        Ok(())
    }
    fn commit(&self, session: SessionId, key: &ConnectionProfileKey) -> Result<(), PlatformError> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| PlatformError::StorageFailed)?;
        let credential = pending
            .get(&session.get())
            .ok_or(PlatformError::CredentialNotFound)?;
        if credential.key != *key {
            return Err(PlatformError::InvalidProfile);
        }
        write_password(&account(key), &credential.password)?;
        pending.remove(&session.get());
        Ok(())
    }
    fn discard(&self, session: SessionId) -> Result<(), PlatformError> {
        self.pending
            .lock()
            .map_err(|_| PlatformError::StorageFailed)?
            .remove(&session.get());
        Ok(())
    }
    fn delete(&self, key: &ConnectionProfileKey) -> Result<(), PlatformError> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| PlatformError::StorageFailed)?;
        delete_password(&account(key))?;
        pending.retain(|_, credential| credential.key != *key);
        Ok(())
    }
    fn purge_pending(&self) -> Result<(), PlatformError> {
        self.pending
            .lock()
            .map_err(|_| PlatformError::StorageFailed)?
            .clear();
        Ok(())
    }
}
fn account(key: &ConnectionProfileKey) -> String {
    let mut hash = Sha256::new();
    for field in [key.protocol().as_str(), key.address(), key.username()] {
        hash.update((field.len() as u64).to_le_bytes());
        hash.update(field.as_bytes());
    }
    hash.update(key.port().to_le_bytes());
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
#[cfg(target_os = "macos")]
fn read_password(account: &str) -> Result<Option<SecretBuffer>, PlatformError> {
    // Profile selection runs on the UI thread. Avoid allowing a locked or
    // re-signed Keychain item to open an interactive authorization prompt and
    // leave the connection form unresponsive; the caller can report the
    // credential as unavailable and ask for the password again.
    use security_framework::item::{ItemClass, ItemSearchOptions, SearchResult};

    let mut options = ItemSearchOptions::new();
    options
        .class(ItemClass::generic_password())
        .service(SERVICE)
        .account(account)
        .load_data(true)
        .skip_authenticated_items(true);
    match options.search() {
        Ok(mut results) => match results.pop() {
            None => Ok(None),
            Some(SearchResult::Data(bytes)) => Ok(Some(SecretBuffer::new(bytes))),
            Some(_) => Err(PlatformError::StorageFailed),
        },
        Err(error) if error.code() == -25300 => Ok(None),
        Err(_) => Err(PlatformError::StorageFailed),
    }
}
#[cfg(target_os = "macos")]
fn write_password(account: &str, password: &SecretBuffer) -> Result<(), PlatformError> {
    let text = password
        .expose_text()
        .ok_or(PlatformError::CredentialProviderFailed)?;
    security_framework::passwords::set_generic_password(SERVICE, account, text.as_bytes())
        .map_err(|_| PlatformError::StorageFailed)
}
#[cfg(target_os = "macos")]
fn delete_password(account: &str) -> Result<(), PlatformError> {
    match security_framework::passwords::delete_generic_password(SERVICE, account) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == -25300 => Ok(()),
        Err(_) => Err(PlatformError::StorageFailed),
    }
}
#[cfg(not(target_os = "macos"))]
fn read_password(_: &str) -> Result<Option<SecretBuffer>, PlatformError> {
    Err(PlatformError::Unavailable)
}
#[cfg(not(target_os = "macos"))]
fn write_password(_: &str, _: &SecretBuffer) -> Result<(), PlatformError> {
    Err(PlatformError::Unavailable)
}
#[cfg(not(target_os = "macos"))]
fn delete_password(_: &str) -> Result<(), PlatformError> {
    Err(PlatformError::Unavailable)
}
#[cfg(test)]
mod tests {
    use super::*;
    use frd_core::ProtocolId;
    fn key() -> ConnectionProfileKey {
        ConnectionProfileKey::new(ProtocolId::rdp(), "fixture.invalid", 3389, "synthetic").unwrap()
    }
    #[test]
    fn absent_pending_cannot_commit() {
        assert_eq!(
            MacCredentialStore::new().commit(SessionId::allocate(), &key()),
            Err(PlatformError::CredentialNotFound)
        );
    }
    #[test]
    fn discard_and_purge_remove_pending_credentials_before_commit() {
        let store = MacCredentialStore::new();
        let session = SessionId::allocate();
        store
            .stage(
                session,
                &key(),
                &SecretBuffer::from_text("synthetic-fixture".into()),
            )
            .unwrap();
        store.discard(session).unwrap();
        assert_eq!(
            store.commit(session, &key()),
            Err(PlatformError::CredentialNotFound)
        );
        store
            .stage(
                session,
                &key(),
                &SecretBuffer::from_text("synthetic-fixture".into()),
            )
            .unwrap();
        store.purge_pending().unwrap();
        assert_eq!(
            store.commit(session, &key()),
            Err(PlatformError::CredentialNotFound)
        );
    }
    #[test]
    fn pending_is_bound_to_exact_profile() {
        let store = MacCredentialStore::new();
        let session = SessionId::allocate();
        store
            .stage(
                session,
                &key(),
                &SecretBuffer::from_text("synthetic-fixture".into()),
            )
            .unwrap();
        let other =
            ConnectionProfileKey::new(ProtocolId::rdp(), "other.invalid", 3389, "synthetic")
                .unwrap();
        assert_eq!(
            store.commit(session, &other),
            Err(PlatformError::InvalidProfile)
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "仅显式运行：使用唯一合成凭据验证系统钥匙串并清理"]
    fn native_keychain_commits_only_authenticated_pending_and_reloads_from_new_store() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let key = ConnectionProfileKey::new(
            ProtocolId::rdp(),
            format!("fixture-{nonce}.invalid"),
            3389,
            "synthetic-keychain-test",
        )
        .unwrap();
        struct Cleanup(ConnectionProfileKey);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = MacCredentialStore::new().delete(&self.0);
            }
        }
        let _cleanup = Cleanup(key.clone());
        let store = MacCredentialStore::new();
        assert!(store.load(&key).unwrap().is_none());
        let session = SessionId::allocate();
        store
            .stage(
                session,
                &key,
                &SecretBuffer::from_text("合成测试凭据-1".into()),
            )
            .unwrap();
        assert!(MacCredentialStore::new().load(&key).unwrap().is_none());
        store.commit(session, &key).unwrap();
        assert_eq!(
            MacCredentialStore::new()
                .load(&key)
                .unwrap()
                .unwrap()
                .expose_text(),
            Some("合成测试凭据-1")
        );
        assert_eq!(
            store.commit(session, &key),
            Err(PlatformError::CredentialNotFound)
        );
        store
            .stage(
                session,
                &key,
                &SecretBuffer::from_text("合成测试凭据-2".into()),
            )
            .unwrap();
        store.commit(session, &key).unwrap();
        assert_eq!(
            MacCredentialStore::new()
                .load(&key)
                .unwrap()
                .unwrap()
                .expose_text(),
            Some("合成测试凭据-2")
        );
        store.delete(&key).unwrap();
        assert!(MacCredentialStore::new().load(&key).unwrap().is_none());
    }
}
