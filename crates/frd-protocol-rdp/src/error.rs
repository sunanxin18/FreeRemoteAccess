use frd_protocol_api::{ProtocolError, ProtocolId};

pub(crate) const RDP_ACTIVATION_FAILED: &str = "rdp_activation_failed";
pub(crate) const RDP_CANCELLED: &str = "rdp_cancelled";
pub(crate) const RDP_DNS_FAILED: &str = "rdp_dns_failed";
pub(crate) const RDP_LICENSE_FAILED: &str = "rdp_license_failed";
pub(crate) const RDP_LOGON_FAILED: &str = "rdp_logon_failed";
pub(crate) const RDP_NLA_FAILED: &str = "rdp_nla_failed";
pub(crate) const RDP_SERVER_IDENTITY_CHANGED: &str = "rdp_server_identity_changed";
pub(crate) const RDP_SERVER_IDENTITY_REJECTED: &str = "rdp_server_identity_rejected";
pub(crate) const RDP_TCP_FAILED: &str = "rdp_tcp_failed";
pub(crate) const RDP_TLS_FAILED: &str = "rdp_tls_failed";

pub(crate) fn rdp_error(code: &'static str) -> ProtocolError {
    ProtocolError::adapter(ProtocolId::rdp(), code)
}
