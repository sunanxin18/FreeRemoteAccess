use frd_core::{SecretBuffer, SessionId};
use frd_platform_api::{ConnectionProfileKey, PlatformError, SecureCredentialStore};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const MAX_CREDENTIAL_BYTES: usize = 65_536;
struct PendingCredential {
    key: ConnectionProfileKey,
    password: SecretBuffer,
}
/// 尚未认证的密码只保留在可清零内存中；进程退出不会留下待提交文件或钥匙串条目。
pub struct LinuxCredentialStore {
    pending: Mutex<HashMap<u64, Arc<PendingCredential>>>,
    backend_mutation: Mutex<()>,
    backend: Box<dyn CredentialBackend>,
}
impl LinuxCredentialStore {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            backend_mutation: Mutex::new(()),
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
        let credential = Arc::new(PendingCredential {
            key: key.clone(),
            password: SecretBuffer::new(text.as_bytes().to_vec()),
        });
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| PlatformError::StorageFailed)?;
        pending.insert(session.get(), credential);
        Ok(())
    }
    fn commit(&self, session: SessionId, key: &ConnectionProfileKey) -> Result<(), PlatformError> {
        // 外部写入串行；pending锁只保护条目所有权，不能跨Secret Service调用。
        let _mutation = self
            .backend_mutation
            .lock()
            .map_err(|_| PlatformError::StorageFailed)?;
        let credential = self
            .pending
            .lock()
            .map_err(|_| PlatformError::StorageFailed)?
            .get(&session.get())
            .cloned()
            .ok_or(PlatformError::CredentialNotFound)?;
        if credential.key != *key {
            return Err(PlatformError::InvalidProfile);
        }
        // 失败保留仍存在的原条目供重试，绝不重新插入已discard/purge/替换的条目。
        self.backend.write(&account(key), &credential.password)?;
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| PlatformError::StorageFailed)?;
        if pending
            .get(&session.get())
            .is_some_and(|current| Arc::ptr_eq(current, &credential))
        {
            pending.remove(&session.get());
        }
        Ok(())
    }
    /// 清除pending/retry所有权；不能撤销已经开始且已授权的OS凭据写入。
    fn discard(&self, session: SessionId) -> Result<(), PlatformError> {
        self.pending
            .lock()
            .map_err(|_| PlatformError::StorageFailed)?
            .remove(&session.get());
        Ok(())
    }
    fn delete(&self, key: &ConnectionProfileKey) -> Result<(), PlatformError> {
        let _mutation = self
            .backend_mutation
            .lock()
            .map_err(|_| PlatformError::StorageFailed)?;
        self.backend.delete(&account(key))?;
        // 保持原合同：只有外部删除成功后才清除该key当前的pending条目。
        self.pending
            .lock()
            .map_err(|_| PlatformError::StorageFailed)?
            .retain(|_, credential| credential.key != *key);
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
            backend_mutation: Mutex::new(()),
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
    struct BlockingBackend {
        entered: std::sync::mpsc::Sender<&'static str>,
        release: Mutex<std::sync::mpsc::Receiver<bool>>,
        fake: std::sync::Arc<Fake>,
    }
    impl CredentialBackend for BlockingBackend {
        fn read(&self, account: &str) -> Result<Option<SecretBuffer>, PlatformError> {
            self.fake.read(account)
        }
        fn write(&self, account: &str, value: &SecretBuffer) -> Result<(), PlatformError> {
            self.entered.send("write").unwrap();
            if !self.release.lock().unwrap().recv().unwrap() {
                return Err(PlatformError::Unavailable);
            }
            self.fake.write(account, value)
        }
        fn delete(&self, account: &str) -> Result<(), PlatformError> {
            self.entered.send("delete").unwrap();
            if !self.release.lock().unwrap().recv().unwrap() {
                return Err(PlatformError::Unavailable);
            }
            self.fake.delete(account)
        }
    }
    fn blocked_fixture() -> (
        std::sync::Arc<LinuxCredentialStore>,
        ConnectionProfileKey,
        std::sync::mpsc::Receiver<&'static str>,
        std::sync::mpsc::Sender<bool>,
    ) {
        let (_, fake, key) = fixture();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let store = LinuxCredentialStore {
            pending: Mutex::new(HashMap::new()),
            backend_mutation: Mutex::new(()),
            backend: Box::new(BlockingBackend {
                entered: entered_tx,
                release: Mutex::new(release_rx),
                fake,
            }),
        };
        (std::sync::Arc::new(store), key, entered_rx, release_tx)
    }
    #[test]
    fn pending_memory_operations_complete_while_secret_service_commit_is_blocked() {
        let (store, key, entered, release) = blocked_fixture();
        let session = SessionId::allocate();
        store
            .stage(
                session,
                &key,
                &SecretBuffer::from_text("synthetic-old".into()),
            )
            .unwrap();
        let writer = std::thread::spawn({
            let store = store.clone();
            let key = key.clone();
            move || store.commit(session, &key)
        });
        assert_eq!(
            entered
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "write"
        );
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let edits = std::thread::spawn({
            let store = store.clone();
            let key = key.clone();
            move || {
                store
                    .stage(
                        session,
                        &key,
                        &SecretBuffer::from_text("synthetic-new".into()),
                    )
                    .unwrap();
                store.discard(session).unwrap();
                store.purge_pending().unwrap();
                done_tx.send(()).unwrap();
            }
        });
        let completed = done_rx.recv_timeout(std::time::Duration::from_secs(1));
        release.send(true).unwrap();
        writer.join().unwrap().unwrap();
        edits.join().unwrap();
        assert!(
            completed.is_ok(),
            "stage/discard/purge不得等待外部commit完成"
        );
        assert!(store.pending.lock().unwrap().is_empty());
    }
    #[test]
    fn completed_old_commit_does_not_remove_same_session_replacement() {
        let (store, key, entered, release) = blocked_fixture();
        let session = SessionId::allocate();
        store
            .stage(
                session,
                &key,
                &SecretBuffer::from_text("synthetic-old".into()),
            )
            .unwrap();
        let writer = std::thread::spawn({
            let store = store.clone();
            let key = key.clone();
            move || store.commit(session, &key)
        });
        assert_eq!(
            entered
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "write"
        );
        store
            .stage(
                session,
                &key,
                &SecretBuffer::from_text("synthetic-new".into()),
            )
            .unwrap();
        release.send(true).unwrap();
        writer.join().unwrap().unwrap();
        assert_eq!(
            store
                .pending
                .lock()
                .unwrap()
                .get(&session.get())
                .unwrap()
                .password
                .expose_text(),
            Some("synthetic-new")
        );
        // 已授权且已开始的旧写入仍可完成；替换条目必须保留供下一次commit。
        assert_eq!(
            store.load(&key).unwrap().unwrap().expose_text(),
            Some("synthetic-old")
        );
        release.send(true).unwrap();
        store.commit(session, &key).unwrap();
        assert_eq!(
            store.load(&key).unwrap().unwrap().expose_text(),
            Some("synthetic-new")
        );
        assert!(store.pending.lock().unwrap().is_empty());
    }
    #[test]
    fn failed_commit_never_resurrects_discarded_purged_or_replaced_entries() {
        for operation in 0..3 {
            let (store, key, entered, release) = blocked_fixture();
            let session = SessionId::allocate();
            store
                .stage(
                    session,
                    &key,
                    &SecretBuffer::from_text("synthetic-old".into()),
                )
                .unwrap();
            let writer = std::thread::spawn({
                let store = store.clone();
                let key = key.clone();
                move || store.commit(session, &key)
            });
            assert_eq!(
                entered
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap(),
                "write"
            );
            match operation {
                0 => store.discard(session).unwrap(),
                1 => store.purge_pending().unwrap(),
                _ => store
                    .stage(
                        session,
                        &key,
                        &SecretBuffer::from_text("synthetic-new".into()),
                    )
                    .unwrap(),
            }
            release.send(false).unwrap();
            assert_eq!(writer.join().unwrap(), Err(PlatformError::Unavailable));
            let pending = store.pending.lock().unwrap();
            if operation < 2 {
                assert!(pending.is_empty());
            } else {
                assert_eq!(
                    pending.get(&session.get()).unwrap().password.expose_text(),
                    Some("synthetic-new")
                );
            }
            assert!(store.load(&key).unwrap().is_none());
        }
    }
    #[test]
    fn delete_and_commit_external_mutations_are_serialized_and_delete_failure_preserves_pending() {
        let (store, key, entered, release) = blocked_fixture();
        let session = SessionId::allocate();
        store
            .stage(session, &key, &SecretBuffer::from_text("synthetic".into()))
            .unwrap();
        let writer = std::thread::spawn({
            let store = store.clone();
            let key = key.clone();
            move || store.commit(session, &key)
        });
        assert_eq!(
            entered
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "write"
        );
        assert!(store.backend_mutation.try_lock().is_err());
        let deleter = std::thread::spawn({
            let store = store.clone();
            let key = key.clone();
            move || store.delete(&key)
        });
        release.send(true).unwrap();
        writer.join().unwrap().unwrap();
        assert_eq!(
            entered
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "delete"
        );
        assert!(store.backend_mutation.try_lock().is_err());
        store
            .stage(
                session,
                &key,
                &SecretBuffer::from_text("synthetic-new".into()),
            )
            .unwrap();
        assert!(store.load(&key).unwrap().is_some());
        release.send(false).unwrap();
        assert_eq!(deleter.join().unwrap(), Err(PlatformError::Unavailable));
        assert!(store.pending.lock().unwrap().contains_key(&session.get()));
        // 删除先进入后，排队的commit必须在删除完成后重新查pending，不能复活旧值。
        let deleter = std::thread::spawn({
            let store = store.clone();
            let key = key.clone();
            move || store.delete(&key)
        });
        assert_eq!(
            entered
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "delete"
        );
        let writer = std::thread::spawn({
            let store = store.clone();
            let key = key.clone();
            move || store.commit(session, &key)
        });
        release.send(true).unwrap();
        deleter.join().unwrap().unwrap();
        assert_eq!(
            writer.join().unwrap(),
            Err(PlatformError::CredentialNotFound)
        );
        assert!(entered.try_recv().is_err());
        assert!(store.pending.lock().unwrap().is_empty());
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
