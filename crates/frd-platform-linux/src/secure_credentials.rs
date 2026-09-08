use frd_core::{SecretBuffer, SessionId};
use frd_platform_api::{ConnectionProfileKey, PlatformError, SecureCredentialStore};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;

const MAX_CREDENTIAL_BYTES: usize = 65_536;
struct PendingCredential {
    key: ConnectionProfileKey,
    password: SecretBuffer,
}
/// 尚未认证的密码只保留在可清零内存中；进程退出不会留下待提交文件或钥匙串条目。
pub struct LinuxCredentialStore {
    pending: Mutex<HashMap<u64, PendingCredential>>,
    backend: Box<dyn CredentialBackend>,
}
impl LinuxCredentialStore {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            backend: Box::new(NativeBackend),
        }
    }
}
impl Default for LinuxCredentialStore {
    fn default() -> Self {
        Self::new()
    }
}
impl SecureCredentialStore for LinuxCredentialStore {
    fn load(&self, key: &ConnectionProfileKey) -> Result<Option<SecretBuffer>, PlatformError> {
        self.backend.read(&account(key))
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
        self.backend.write(&account(key), &credential.password)?;
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
        self.backend.delete(&account(key))?;
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

/// 同步调用由应用现有凭据工作线程执行；不在绘制或输入回调内连接 D-Bus。
trait CredentialBackend: Send + Sync {
    fn read(&self, account: &str) -> Result<Option<SecretBuffer>, PlatformError>;
    fn write(&self, account: &str, value: &SecretBuffer) -> Result<(), PlatformError>;
    fn delete(&self, account: &str) -> Result<(), PlatformError>;
}
struct NativeBackend;
#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use secret_service::{blocking::SecretService, EncryptionType};
    const SERVICE: &str = "org.freeremotedesk.credentials.v1";
    fn attributes(account: &str) -> HashMap<&str, &str> {
        HashMap::from([("application", SERVICE), ("account", account)])
    }
    fn connect() -> Result<SecretService<'static>, PlatformError> {
        SecretService::connect(EncryptionType::Dh).map_err(|_| PlatformError::Unavailable)
    }
    impl CredentialBackend for NativeBackend {
        fn read(&self, account: &str) -> Result<Option<SecretBuffer>, PlatformError> {
            let service = connect()?;
            let items = service
                .search_items(attributes(account))
                .map_err(|_| PlatformError::Unavailable)?;
            if !items.locked.is_empty() {
                return Err(PlatformError::Unavailable);
            }
            if items.unlocked.len() > 1 {
                return Err(PlatformError::StorageFailed);
            }
            let Some(item) = items.unlocked.first() else {
                return Ok(None);
            };
            if item.is_locked().map_err(|_| PlatformError::Unavailable)? {
                return Err(PlatformError::Unavailable);
            }
            let secret =
                SecretBuffer::new(item.get_secret().map_err(|_| PlatformError::Unavailable)?);
            let text = secret
                .expose_text()
                .ok_or(PlatformError::CredentialProviderFailed)?;
            if text.len() > MAX_CREDENTIAL_BYTES {
                return Err(PlatformError::CredentialTooLarge);
            }
            Ok(Some(secret))
        }
        fn write(&self, account: &str, value: &SecretBuffer) -> Result<(), PlatformError> {
            let service = connect()?;
            let items = service
                .search_items(attributes(account))
                .map_err(|_| PlatformError::Unavailable)?;
            if !items.locked.is_empty() {
                return Err(PlatformError::Unavailable);
            }
            if items.unlocked.len() > 1 {
                return Err(PlatformError::StorageFailed);
            }
            let text = value
                .expose_text()
                .ok_or(PlatformError::CredentialProviderFailed)?;
            if let Some(item) = items.unlocked.first() {
                if item.is_locked().map_err(|_| PlatformError::Unavailable)? {
                    return Err(PlatformError::Unavailable);
                }
                return item
                    .set_secret(text.as_bytes(), "text/plain; charset=utf-8")
                    .map_err(|_| PlatformError::StorageFailed);
            }
            let collection = service
                .get_default_collection()
                .map_err(|_| PlatformError::Unavailable)?;
            if collection
                .is_locked()
                .map_err(|_| PlatformError::Unavailable)?
            {
                return Err(PlatformError::Unavailable);
            }
            collection
                .create_item(
                    "FreeRemoteDesk",
                    attributes(account),
                    text.as_bytes(),
                    true,
                    "text/plain; charset=utf-8",
                )
                .map(|_| ())
                .map_err(|_| PlatformError::StorageFailed)
        }
        fn delete(&self, account: &str) -> Result<(), PlatformError> {
            let service = connect()?;
            let items = service
                .search_items(attributes(account))
                .map_err(|_| PlatformError::Unavailable)?;
            if !items.locked.is_empty() {
                return Err(PlatformError::Unavailable);
            }
            for item in items.unlocked {
                if item.is_locked().map_err(|_| PlatformError::Unavailable)? {
                    return Err(PlatformError::Unavailable);
                }
                item.delete().map_err(|_| PlatformError::StorageFailed)?;
            }
            Ok(())
        }
    }
}
#[cfg(not(target_os = "linux"))]
impl CredentialBackend for NativeBackend {
    fn read(&self, _: &str) -> Result<Option<SecretBuffer>, PlatformError> {
        Err(PlatformError::Unavailable)
    }
    fn write(&self, _: &str, _: &SecretBuffer) -> Result<(), PlatformError> {
        Err(PlatformError::Unavailable)
    }
    fn delete(&self, _: &str) -> Result<(), PlatformError> {
        Err(PlatformError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frd_core::ProtocolId;
    #[derive(Default)]
    struct Fake {
        values: Mutex<HashMap<String, SecretBuffer>>,
        unavailable: std::sync::atomic::AtomicBool,
    }
    impl CredentialBackend for std::sync::Arc<Fake> {
        fn read(&self, account: &str) -> Result<Option<SecretBuffer>, PlatformError> {
            if self.unavailable.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(PlatformError::Unavailable);
            }
            Ok(self
                .values
                .lock()
                .unwrap()
                .get(account)
                .map(|v| SecretBuffer::from_text(v.expose_text().unwrap().to_owned())))
        }
        fn write(&self, account: &str, value: &SecretBuffer) -> Result<(), PlatformError> {
            if self.unavailable.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(PlatformError::Unavailable);
            }
            self.values.lock().unwrap().insert(
                account.into(),
                SecretBuffer::from_text(value.expose_text().unwrap().to_owned()),
            );
            Ok(())
        }
        fn delete(&self, account: &str) -> Result<(), PlatformError> {
            if self.unavailable.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(PlatformError::Unavailable);
            }
            self.values.lock().unwrap().remove(account);
            Ok(())
        }
    }
    fn fixture() -> (
        LinuxCredentialStore,
        std::sync::Arc<Fake>,
        ConnectionProfileKey,
    ) {
        let fake = std::sync::Arc::new(Fake::default());
        let store = LinuxCredentialStore {
            pending: Mutex::new(HashMap::new()),
            backend: Box::new(fake.clone()),
        };
        (
            store,
            fake,
            ConnectionProfileKey::new(ProtocolId::rdp(), "fixture.invalid", 3389, "synthetic")
                .unwrap(),
        )
    }
    #[test]
    fn stage_is_memory_only_and_commit_requires_exact_session_and_profile() {
        let (store, fake, key) = fixture();
        let session = SessionId::allocate();
        store
            .stage(session, &key, &SecretBuffer::from_text("fixture".into()))
            .unwrap();
        assert!(store.load(&key).unwrap().is_none());
        assert_eq!(
            store.commit(SessionId::allocate(), &key),
            Err(PlatformError::CredentialNotFound)
        );
        let other =
            ConnectionProfileKey::new(ProtocolId::rdp(), "other.invalid", 3389, "synthetic")
                .unwrap();
        assert_eq!(
            store.commit(session, &other),
            Err(PlatformError::InvalidProfile)
        );
        store.commit(session, &key).unwrap();
        assert_eq!(
            store.load(&key).unwrap().unwrap().expose_text(),
            Some("fixture")
        );
        assert_eq!(
            store.commit(session, &key),
            Err(PlatformError::CredentialNotFound)
        );
        assert!(fake
            .values
            .lock()
            .unwrap()
            .keys()
            .all(|key| key.len() == 64 && !key.contains("synthetic")));
    }
    #[test]
    fn unavailable_backend_preserves_pending_for_retry_and_discard_purge_delete_remove_it() {
        let (store, fake, key) = fixture();
        let session = SessionId::allocate();
        store
            .stage(session, &key, &SecretBuffer::from_text("fixture".into()))
            .unwrap();
        fake.unavailable
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(store.commit(session, &key), Err(PlatformError::Unavailable));
        assert!(matches!(store.load(&key), Err(PlatformError::Unavailable)));
        fake.unavailable
            .store(false, std::sync::atomic::Ordering::Relaxed);
        store.commit(session, &key).unwrap();
        for operation in 0..3 {
            store
                .stage(session, &key, &SecretBuffer::from_text("fixture".into()))
                .unwrap();
            match operation {
                0 => store.discard(session),
                1 => store.purge_pending(),
                _ => store.delete(&key),
            }
            .unwrap();
            assert_eq!(
                store.commit(session, &key),
                Err(PlatformError::CredentialNotFound)
            );
        }
        assert!(store.load(&key).unwrap().is_none());
    }
    #[test]
    fn hash_uses_length_delimited_fields_and_stage_rejects_oversized_secret() {
        let (store, _, key) = fixture();
        let a = ConnectionProfileKey::new(ProtocolId::rdp(), "a", 3389, "bc").unwrap();
        let b = ConnectionProfileKey::new(ProtocolId::rdp(), "ab", 3389, "c").unwrap();
        assert_ne!(account(&a), account(&b));
        assert_eq!(
            store.stage(
                SessionId::allocate(),
                &key,
                &SecretBuffer::new(vec![b'a'; MAX_CREDENTIAL_BYTES + 1])
            ),
            Err(PlatformError::CredentialTooLarge)
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod native_tests {
    use super::*;
    #[test]
    #[ignore = "仅在隔离 D-Bus/临时 keyring 中显式运行合成凭据往返"]
    fn native_secret_service_authenticated_commit_roundtrip() {
        assert_eq!(
            std::env::var("FRD_LINUX_KEYRING_TEST").as_deref(),
            Ok("1"),
            "需要显式隔离 keyring 验证开关"
        );
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let key = ConnectionProfileKey::new(
            frd_core::ProtocolId::rdp(),
            format!("fixture-{}-{nonce}.invalid", std::process::id()),
            3389,
            "synthetic-keyring-test",
        )
        .unwrap();
        struct Cleanup(ConnectionProfileKey);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = LinuxCredentialStore::new().delete(&self.0);
            }
        }
        let _cleanup = Cleanup(key.clone());
        let store = LinuxCredentialStore::new();
        let session = SessionId::allocate();
        assert!(store.load(&key).unwrap().is_none());
        store
            .stage(
                session,
                &key,
                &SecretBuffer::from_text("synthetic-keyring-fixture-v1".into()),
            )
            .unwrap();
        assert!(LinuxCredentialStore::new().load(&key).unwrap().is_none());
        store.commit(session, &key).unwrap();
        let loaded = LinuxCredentialStore::new().load(&key).unwrap().unwrap();
        assert!(
            loaded.expose_text() == Some("synthetic-keyring-fixture-v1"),
            "合成凭据往返不一致"
        );
        store.delete(&key).unwrap();
        assert!(LinuxCredentialStore::new().load(&key).unwrap().is_none());
    }
}
