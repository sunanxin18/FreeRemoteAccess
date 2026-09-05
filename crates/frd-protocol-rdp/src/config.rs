use frd_protocol_api::{ConnectRequest, ProtocolError, ProtocolId};

const RDP_CREDENTIALS_REQUIRED: &str = "rdp_credentials_required";
const RDP_INVALID_USERNAME: &str = "rdp_invalid_username";
const RDP_PROTOCOL_MISMATCH: &str = "rdp_protocol_mismatch";

fn rdp_error(code: &'static str) -> ProtocolError {
    ProtocolError::adapter(ProtocolId::rdp(), code)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedUsername {
    account: String,
    domain: Option<String>,
    upn: Option<String>,
}

impl ParsedUsername {
    // The public parser deliberately exposes no parsing detail: every rejected
    // form maps to the stable adapter-facing `rdp_invalid_username` code.
    // Replacing `()` would change this exported API only to satisfy Clippy.
    #[allow(clippy::result_unit_err)]
    pub fn parse(value: &str) -> Result<Self, ()> {
        if value.is_empty() || value.trim() != value {
            return Err(());
        }

        if let Some((domain, account)) = value.split_once('\\') {
            if domain.is_empty() || account.is_empty() || account.contains('\\') {
                return Err(());
            }
            return Ok(Self {
                account: account.to_owned(),
                domain: Some(domain.to_owned()),
                upn: None,
            });
        }

        if let Some((account, domain)) = value.split_once('@') {
            if account.is_empty() || domain.is_empty() || domain.contains('@') {
                return Err(());
            }
            return Ok(Self {
                account: account.to_owned(),
                domain: None,
                upn: Some(value.to_owned()),
            });
        }

        Ok(Self {
            account: value.to_owned(),
            domain: None,
            upn: None,
        })
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    pub fn domain(&self) -> Option<&str> {
        self.domain.as_deref()
    }

    pub fn upn(&self) -> Option<&str> {
        self.upn.as_deref()
    }
}

pub struct RdpConnectionConfig {
    pub(crate) request: ConnectRequest,
    pub(crate) client_platform: RdpClientPlatformIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RdpClientPlatformIdentity {
    Windows,
    Macintosh,
    Ios,
    Unix,
    Android,
}

pub(crate) struct ConnectorCredentials {
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) domain: Option<String>,
}

impl RdpConnectionConfig {
    pub fn try_new(
        request: ConnectRequest,
        client_platform: RdpClientPlatformIdentity,
    ) -> Result<Self, ProtocolError> {
        if request.protocol_id != ProtocolId::rdp() {
            return Err(rdp_error(RDP_PROTOCOL_MISMATCH));
        }
        if request.credentials.is_none() {
            return Err(rdp_error(RDP_CREDENTIALS_REQUIRED));
        }
        Ok(Self {
            request,
            client_platform,
        })
    }

    pub(crate) fn take_connector_credentials(
        &mut self,
    ) -> Result<ConnectorCredentials, ProtocolError> {
        let credentials = self
            .request
            .credentials
            .take()
            .ok_or_else(|| rdp_error(RDP_CREDENTIALS_REQUIRED))?;
        let username = ParsedUsername::parse(&credentials.username)
            .map_err(|()| rdp_error(RDP_INVALID_USERNAME))?;
        if credentials.password.expose().is_empty() {
            return Err(rdp_error(RDP_CREDENTIALS_REQUIRED));
        }
        let password = std::str::from_utf8(credentials.password.expose())
            .map(str::to_owned)
            .map_err(|_| rdp_error(crate::error::RDP_NLA_FAILED))?;
        let connector_username = username
            .upn()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| username.account().to_owned());

        Ok(ConnectorCredentials {
            username: connector_username,
            password,
            domain: username.domain().map(ToOwned::to_owned),
        })
    }
}

#[cfg(test)]
mod tests {
    use frd_core::{SecretBuffer, SessionId};
    use frd_protocol_api::{ConnectRequest, Credentials, Endpoint, ProtocolId};

    use super::{RdpClientPlatformIdentity, RdpConnectionConfig};

    #[test]
    fn client_platform_identities_are_explicit_protocol_values() {
        let approved = [
            RdpClientPlatformIdentity::Windows,
            RdpClientPlatformIdentity::Macintosh,
            RdpClientPlatformIdentity::Ios,
            RdpClientPlatformIdentity::Unix,
            RdpClientPlatformIdentity::Android,
        ];

        assert_eq!(approved.len(), 5);
    }

    #[test]
    fn factory_construction_defers_invalid_username_until_worker_extraction() {
        let mut config = RdpConnectionConfig::try_new(
            ConnectRequest {
                session_id: SessionId::allocate(),
                endpoint: Endpoint::new("rdp.example", 3389).expect("valid endpoint"),
                protocol_id: ProtocolId::rdp(),
                credentials: Some(Credentials {
                    username: " alice".to_owned(),
                    password: SecretBuffer::new(vec![0x01]).take(),
                }),
                saved_server_pin: None,
            },
            RdpClientPlatformIdentity::Windows,
        )
        .expect("factory construction must not read or parse the username");

        match config.take_connector_credentials() {
            Err(error) => assert_eq!(error.code(), "rdp_invalid_username"),
            Ok(_) => panic!("worker extraction must reject the invalid username"),
        }
    }

    #[test]
    fn connector_secret_validation_is_deferred_until_worker_extraction() {
        let mut config = RdpConnectionConfig::try_new(
            ConnectRequest {
                session_id: SessionId::allocate(),
                endpoint: Endpoint::new("rdp.example", 3389).expect("valid endpoint"),
                protocol_id: ProtocolId::rdp(),
                credentials: Some(Credentials {
                    username: "alice".to_owned(),
                    password: SecretBuffer::new(Vec::new()).take(),
                }),
                saved_server_pin: None,
            },
            RdpClientPlatformIdentity::Windows,
        )
        .expect("factory construction must not inspect the secret bytes");

        match config.take_connector_credentials() {
            Err(error) => assert_eq!(error.code(), "rdp_credentials_required"),
            Ok(_) => panic!("the worker must reject an empty secret"),
        }
    }
}
