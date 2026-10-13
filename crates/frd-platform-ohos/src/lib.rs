//! 鸿蒙私有沙盒 metadata/pin 与 HUKS 安全凭据。没有桌面 Linux 后端。
mod connection_profiles;
mod huks;
mod paths;
mod secure_credentials;
mod server_identity;
mod single_instance;
/// 仅由应用 owner 使用官方 Ability Context.filesDir 建立固定存储能力。
pub fn initialize_context_storage(
    root: &std::path::Path,
) -> Result<(), frd_platform_api::PlatformError> {
    paths::initialize_context_storage(root)
}
pub use connection_profiles::OhosConnectionProfileStore;
pub use secure_credentials::OhosCredentialStore;
pub use server_identity::OhosServerIdentityStore;
pub use single_instance::{OhosSingleInstanceError, OhosSingleInstanceGuard};
#[cfg(test)]
fn private_tempdir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}

/// 单独的开发安全存储验收；从不经过或伪造协议认证成功。
#[cfg(debug_assertions)]
pub fn debug_huks_probe(root: std::path::PathBuf) -> i32 {
    #[cfg(target_env = "ohos")]
    {
        use frd_core::{ProtocolId, SecretBuffer, SessionId};
        use frd_platform_api::{ConnectionProfileKey, SecureCredentialStore};
        let key = ConnectionProfileKey::new(ProtocolId::rdp(), "huks-probe.invalid", 3389, "probe")
            .unwrap();
        let store = OhosCredentialStore::new(root.clone());
        let mut random = [0u8; 12];
        extern "C" {
            fn frd_huks_random(out: *mut u8) -> i32;
        }
        if unsafe { frd_huks_random(random.as_mut_ptr()) } != 0 {
            return -1;
        }
        let text = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        for b in &mut random {
            unsafe { std::ptr::write_volatile(b, 0) }
        }
        let secret = SecretBuffer::from_text(text);
        let s = SessionId::allocate();
        // 仅数字阶段/错误类别进入 debug 状态，不包含 key、路径、密码或长度。
        fn failure(stage: i32, error: frd_platform_api::PlatformError) -> i32 {
            use frd_platform_api::PlatformError::*;
            -(stage * 10
                + match error {
                    Unavailable => 1,
                    StorageFailed => 2,
                    CredentialProviderFailed => 3,
                    ServerIdentityPinMismatch => 4,
                    InvalidProfile => 5,
                    CredentialNotFound => 6,
                    CredentialTooLarge => 7,
                })
        }
        let result = (|| -> Result<i32, i32> {
            store.stage(s, &key, &secret).map_err(|e| failure(1, e))?;
            if store.load(&key).map_err(|e| failure(2, e))?.is_some() {
                return Err(-20);
            }
            store.commit(s, &key).map_err(|e| failure(3, e))?;
            let reloaded = OhosCredentialStore::new(root.clone())
                .load(&key)
                .map_err(|e| failure(4, e))?
                .ok_or(-40)?;
            if reloaded.expose_text() != secret.expose_text() {
                return Err(-50);
            }
            let account = secure_credentials::account(&key);
            let path = root.join(format!("{account}.huks"));
            let mut record = paths::read(&path).map_err(|e| failure(6, e))?.ok_or(-60)?;
            let last = record.last_mut().ok_or(-60)?;
            *last ^= 1;
            paths::write_atomic(&path, &record).map_err(|e| failure(7, e))?;
            match store.load(&key) {
                Err(frd_platform_api::PlatformError::CredentialProviderFailed) => Ok(7),
                Err(error) => Err(failure(8, error)),
                Ok(_) => Err(-80),
            }
        })();
        let cleanup = store.delete(&key);
        let _ = store.discard(s);
        // 保留最初失败，不能让后续同一路径 cleanup 失败掩盖发生阶段。
        match result {
            Err(code) => code,
            Ok(code) => cleanup
                .map(|_| code)
                .unwrap_or_else(|error| failure(9, error)),
        }
    }
    #[cfg(not(target_env = "ohos"))]
    {
        let _ = root;
        -4
    }
}
