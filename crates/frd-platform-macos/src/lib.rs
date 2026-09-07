//! macOS 平台服务。
#[cfg(test)]
mod tests {
    use super::MacServerIdentityStore;
    use frd_core::{Endpoint, ProtocolId};
    use frd_platform_api::{PlatformError, ServerIdentityStore};
    #[test]
    fn first_pin_persists_and_changed_pin_never_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = MacServerIdentityStore::at_path(dir.path());
        let endpoint = Endpoint::new("fixture.invalid", 3389).unwrap();
        let protocol = ProtocolId::rdp();
        store.store_pin(&protocol, &endpoint, [1; 32]).unwrap();
        assert_eq!(store.load_pin(&protocol, &endpoint).unwrap(), Some([1; 32]));
        assert_eq!(
            store.store_pin(&protocol, &endpoint, [2; 32]),
            Err(PlatformError::ServerIdentityPinMismatch)
        );
        assert_eq!(store.load_pin(&protocol, &endpoint).unwrap(), Some([1; 32]));
    }
}

mod audio;
mod connection_profiles;
mod credentials;
mod paths;
mod secure_credentials;
mod server_identity;
mod single_instance;
pub use audio::MacAudioOutput;
pub use connection_profiles::MacConnectionProfileStore;
pub use credentials::EnvironmentCredentialProvider;
pub use secure_credentials::MacCredentialStore;
pub use server_identity::MacServerIdentityStore;
pub use single_instance::{MacSingleInstanceError, MacSingleInstanceGuard};
