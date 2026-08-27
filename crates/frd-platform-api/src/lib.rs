//! 平台服务的窄接口；实现位于各 OS crate，绝不进入协议运行时。

use frd_core::{CredentialProviderId, Endpoint, ProtocolId, SecretBuffer};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlatformError {
    Unavailable,
    StorageFailed,
    CredentialProviderFailed,
    ServerIdentityPinMismatch,
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
