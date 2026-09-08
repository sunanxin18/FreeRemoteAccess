//! Windows RDP protocol adapter boundary.

mod active_session;
mod audio;
#[allow(dead_code)]
mod avc444;
mod baseline;
#[allow(dead_code)]
mod clearcodec;
mod clipboard;
#[allow(dead_code)]
mod codec_selection;
mod config;
mod connector;
mod display;
mod egfx;
mod error;
mod factory;
mod input;
mod pixel_convert;
mod runtime;
mod server_identity;
mod surface;
mod tls;
mod upstream;
mod writer;
mod yuv_convert;

pub use config::{ParsedUsername, RdpClientPlatformIdentity, RdpConnectionConfig};
pub use egfx::{
    Avc420DecoderProvider, Avc444Decoder, Avc444DecoderProvider, EgfxDecoderProvider,
    ValidatedAvc444Bitmap, ValidatedAvc444Encoding,
};
pub use factory::{
    RdpEgfxDiagnostics, RdpEgfxFailure, RdpGraphicsAdvertisementGate, RdpGraphicsCapabilities,
    RdpGraphicsObserver, RdpProtocolFactory, RdpProtocolSession,
};

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

#[allow(dead_code)]
#[path = "progressive/wire.rs"]
mod progressive_wire;
