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
    pub(crate) username: ParsedUsername,
}

pub(crate) struct ConnectorCredentials {
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) domain: Option<String>,
}

impl RdpConnectionConfig {
    pub(crate) fn take_connector_credentials(
        &mut self,
    ) -> Result<ConnectorCredentials, ProtocolError> {
        let credentials = self
            .request
            .credentials
            .take()
            .ok_or_else(|| rdp_error(RDP_CREDENTIALS_REQUIRED))?;
        if credentials.password.expose().is_empty() {
            return Err(rdp_error(RDP_CREDENTIALS_REQUIRED));
        }
        let password = std::str::from_utf8(credentials.password.expose())
            .map(str::to_owned)
            .map_err(|_| rdp_error(crate::error::RDP_NLA_FAILED))?;
        let username = self
            .username
            .upn()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| self.username.account().to_owned());

        Ok(ConnectorCredentials {
            username,
            password,
            domain: self.username.domain().map(ToOwned::to_owned),
        })
    }
}

impl TryFrom<ConnectRequest> for RdpConnectionConfig {
    type Error = ProtocolError;

    fn try_from(request: ConnectRequest) -> Result<Self, Self::Error> {
        if request.protocol_id != ProtocolId::rdp() {
            return Err(rdp_error(RDP_PROTOCOL_MISMATCH));
        }
        let credentials = request
            .credentials
            .as_ref()
            .ok_or_else(|| rdp_error(RDP_CREDENTIALS_REQUIRED))?;
        let username = ParsedUsername::parse(&credentials.username)
            .map_err(|()| rdp_error(RDP_INVALID_USERNAME))?;
        Ok(Self { request, username })
    }
}

#[cfg(test)]
mod tests {
    use frd_core::{SecretBuffer, SessionId};
    use frd_protocol_api::{ConnectRequest, Credentials, Endpoint, ProtocolId};

    use super::RdpConnectionConfig;

    #[test]
    fn connector_secret_validation_is_deferred_until_worker_extraction() {
        let mut config = RdpConnectionConfig::try_from(ConnectRequest {
            session_id: SessionId::allocate(),
            endpoint: Endpoint::new("rdp.example", 3389).expect("valid endpoint"),
            protocol_id: ProtocolId::rdp(),
            credentials: Some(Credentials {
                username: "alice".to_owned(),
                password: SecretBuffer::new(Vec::new()).take(),
            }),
            saved_server_pin: None,
        })
        .expect("factory construction must not inspect the secret bytes");

        match config.take_connector_credentials() {
            Err(error) => assert_eq!(error.code(), "rdp_credentials_required"),
            Ok(_) => panic!("the worker must reject an empty secret"),
        }
    }
}
