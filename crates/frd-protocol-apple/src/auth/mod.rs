use frd_protocol_api::{Credentials, ProtocolError};

use crate::protocol::security;

pub const APPLE_SECURITY_TYPE_UNAVAILABLE: &str = "apple_security_type_unavailable";
pub const APPLE_CREDENTIALS_REQUIRED: &str = "apple_credentials_required";

fn apple_error(code: &'static str) -> ProtocolError {
    ProtocolError::adapter(frd_core::ProtocolId::apple_hpss_mvs(), code)
}

pub fn select_apple_security_type(
    offered: &[u8],
    credentials: &Credentials,
) -> Result<u8, ProtocolError> {
    if credentials.username.is_empty() || credentials.password.expose().is_empty() {
        return Err(apple_error(APPLE_CREDENTIALS_REQUIRED));
    }
    [
        security::APPLE_SRP,
        security::APPLE_RSA_SRP,
        security::APPLE_ARD,
    ]
    .into_iter()
    .find(|security_type| offered.contains(security_type))
    .ok_or_else(|| apple_error(APPLE_SECURITY_TYPE_UNAVAILABLE))
}
