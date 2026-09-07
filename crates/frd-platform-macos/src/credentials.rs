use frd_core::{CredentialProviderId, SecretBuffer};
use frd_platform_api::{CredentialProvider, PlatformError};

pub struct EnvironmentCredentialProvider;

impl CredentialProvider for EnvironmentCredentialProvider {
    fn load_username(&self, provider: &CredentialProviderId) -> Result<String, PlatformError> {
        ensure_environment_provider(provider)?;
        std::env::var("FRD_USERNAME").map_err(|_| PlatformError::CredentialProviderFailed)
    }

    fn load_password(
        &self,
        provider: &CredentialProviderId,
    ) -> Result<SecretBuffer, PlatformError> {
        ensure_environment_provider(provider)?;
        std::env::var("FRD_PASSWORD")
            .map(SecretBuffer::from_text)
            .map_err(|_| PlatformError::CredentialProviderFailed)
    }
}

fn ensure_environment_provider(provider: &CredentialProviderId) -> Result<(), PlatformError> {
    (provider == &CredentialProviderId::environment())
        .then_some(())
        .ok_or(PlatformError::CredentialProviderFailed)
}

#[cfg(test)]
mod tests {
    use frd_core::CredentialProviderId;
    use frd_platform_api::{CredentialProvider, PlatformError};

    use super::EnvironmentCredentialProvider;

    #[test]
    fn environment_provider_rejects_non_environment_provider_ids_without_reading_values() {
        let provider = EnvironmentCredentialProvider;
        let unsupported = CredentialProviderId::new("unsupported").expect("valid provider id");

        assert_eq!(
            provider.load_username(&unsupported),
            Err(PlatformError::CredentialProviderFailed)
        );
        assert!(matches!(
            provider.load_password(&unsupported),
            Err(PlatformError::CredentialProviderFailed)
        ));
    }
}
