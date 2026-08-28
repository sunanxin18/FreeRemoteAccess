use frd_protocol_api::{ProtocolError, ProtocolId};

pub(crate) const RDP_SERVER_IDENTITY_CHANGED: &str = "rdp_server_identity_changed";
pub(crate) const RDP_SERVER_IDENTITY_REJECTED: &str = "rdp_server_identity_rejected";
pub(crate) const RDP_TLS_FAILED: &str = "rdp_tls_failed";

pub(crate) fn rdp_error(code: &'static str) -> ProtocolError {
    ProtocolError::adapter(ProtocolId::rdp(), code)
}
