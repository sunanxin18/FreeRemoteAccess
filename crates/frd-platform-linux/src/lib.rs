//! Linux 平台服务；协议与其他平台的实现保持独立。
mod connection_profiles;
mod credentials;
mod paths;
mod secure_credentials;
mod server_identity;
mod single_instance;
pub use connection_profiles::LinuxConnectionProfileStore;
pub use credentials::EnvironmentCredentialProvider;
pub use secure_credentials::LinuxCredentialStore;
pub use server_identity::LinuxServerIdentityStore;
pub use single_instance::{LinuxSingleInstanceError, LinuxSingleInstanceGuard};

#[cfg(all(test, unix))]
fn private_tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}
