//! Windows RDP protocol adapter boundary.

mod active_session;
mod baseline;
mod config;
mod connector;
mod error;
mod factory;
mod input;
mod runtime;
mod server_identity;
mod surface;
mod tls;
mod upstream;
mod writer;

pub use config::{ParsedUsername, RdpConnectionConfig};
pub use factory::{RdpProtocolFactory, RdpProtocolSession};

/// Compile-time seam consumed by Task 3's connector without exposing RDP TLS
/// types outside this private adapter crate.
type IdentityVerificationSeam = (
    fn(
        std::net::TcpStream,
        &frd_protocol_api::Endpoint,
    ) -> Result<server_identity::ObservedServerIdentity, frd_protocol_api::ProtocolError>,
    fn(
        frd_protocol_api::Endpoint,
        Option<[u8; 32]>,
        server_identity::ObservedServerIdentity,
        frd_core::SessionId,
        &mut frd_protocol_api::ProtocolRuntime,
    )
        -> Result<Option<server_identity::AcceptedServerIdentity>, frd_protocol_api::ProtocolError>,
    fn(
        std::net::TcpStream,
        &frd_protocol_api::Endpoint,
        &server_identity::AcceptedServerIdentity,
    ) -> Result<tls::VerifiedTlsTransport, frd_protocol_api::ProtocolError>,
);

#[allow(dead_code)]
const IDENTITY_VERIFICATION_SEAM: IdentityVerificationSeam = (
    tls::credential_free_preflight,
    server_identity::resolve_server_identity,
    tls::establish_verified_tls,
);
