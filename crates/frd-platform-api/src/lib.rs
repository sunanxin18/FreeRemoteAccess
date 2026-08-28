//! 平台服务的窄接口；实现位于各 OS crate，绝不进入协议运行时。

use frd_core::{CredentialProviderId, Endpoint, ProtocolId, SecretBuffer, SessionId, TargetSystem};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlatformError {
    Unavailable,
    StorageFailed,
    CredentialProviderFailed,
    ServerIdentityPinMismatch,
    InvalidProfile,
    CredentialNotFound,
    CredentialTooLarge,
}

pub trait CredentialProvider {
    fn load_username(&self, provider: &CredentialProviderId) -> Result<String, PlatformError>;

    fn load_password(&self, provider: &CredentialProviderId)
        -> Result<SecretBuffer, PlatformError>;
}

pub trait ServerIdentityStore: Send + Sync {
    fn load_pin(
        &self,
        protocol: &ProtocolId,
        endpoint: &Endpoint,
    ) -> Result<Option<[u8; 32]>, PlatformError>;

    fn store_pin(
        &self,
        protocol: &ProtocolId,
        endpoint: &Endpoint,
        pin: [u8; 32],
    ) -> Result<(), PlatformError>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlatformCapabilities {
    pub dynamic_resolution: bool,
    pub clipboard_read: bool,
    pub clipboard_write: bool,
    pub remote_audio: bool,
    pub text_input: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ConnectionProfileKey {
    protocol: ProtocolId,
    address: String,
    port: u16,
    username: String,
}

impl ConnectionProfileKey {
    pub fn new(
        protocol: ProtocolId,
        address: impl Into<String>,
        port: u16,
        username: impl Into<String>,
    ) -> Option<Self> {
        let address = address.into();
        let username = username.into();
        (!address.trim().is_empty() && port != 0 && !username.trim().is_empty()).then_some(Self {
            protocol,
            address,
            port,
            username,
        })
    }

    pub fn protocol(&self) -> &ProtocolId {
        &self.protocol
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn username(&self) -> &str {
        &self.username
    }
}

/// Public profile metadata deliberately excludes credentials.
///
/// ```compile_fail
/// # use frd_platform_api::SavedConnectionProfile;
/// # fn read_password(profile: &SavedConnectionProfile) {
/// let _password = &profile.password;
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedConnectionProfile {
    pub key: ConnectionProfileKey,
    pub target_system: TargetSystem,
    pub last_success_order: u64,
}

impl SavedConnectionProfile {
    pub fn sort_most_recent(profiles: &mut [Self]) {
        profiles.sort_by_key(|profile| std::cmp::Reverse(profile.last_success_order));
    }
}

pub trait ConnectionProfileStore: Send + Sync {
    fn list(&self) -> Result<Vec<SavedConnectionProfile>, PlatformError>;
    fn upsert(&self, profile: &SavedConnectionProfile) -> Result<(), PlatformError>;
    fn delete(&self, key: &ConnectionProfileKey) -> Result<(), PlatformError>;
}

pub trait SecureCredentialStore: Send + Sync {
    fn load(&self, key: &ConnectionProfileKey) -> Result<Option<SecretBuffer>, PlatformError>;
    fn stage(
        &self,
        session: SessionId,
        key: &ConnectionProfileKey,
        password: &SecretBuffer,
    ) -> Result<(), PlatformError>;
    fn commit(&self, session: SessionId, key: &ConnectionProfileKey) -> Result<(), PlatformError>;
    fn discard(&self, session: SessionId) -> Result<(), PlatformError>;
    fn delete(&self, key: &ConnectionProfileKey) -> Result<(), PlatformError>;
    fn purge_pending(&self) -> Result<(), PlatformError>;
}

#[cfg(test)]
mod tests {
    use super::{ConnectionProfileKey, SavedConnectionProfile};
    use frd_core::{ProtocolId, TargetSystem};

    fn test_profile(order: u64) -> SavedConnectionProfile {
        SavedConnectionProfile {
            key: ConnectionProfileKey::new(ProtocolId::apple_hpss_mvs(), "sun", 5900, "alice")
                .expect("test profile key should be valid"),
            target_system: TargetSystem::MacOs,
            last_success_order: order,
        }
    }

    #[test]
    fn profile_key_rejects_empty_identity_fields() {
        assert!(
            ConnectionProfileKey::new(ProtocolId::new("apple-hpss").unwrap(), "", 5900, "sun",)
                .is_none()
        );
    }

    #[test]
    fn saved_profile_orders_newest_success_first() {
        let older = test_profile(1);
        let newer = test_profile(2);
        let mut profiles = vec![older, newer.clone()];
        SavedConnectionProfile::sort_most_recent(&mut profiles);
        assert_eq!(profiles[0], newer);
    }
}
