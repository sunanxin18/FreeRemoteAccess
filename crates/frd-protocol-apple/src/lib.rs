//! Apple Remote Desktop 的认证与加密会话适配器。

pub mod ard;
pub mod audio_codec;
pub mod audio_input;
mod auth;
mod connection;
pub mod dynamic_resolution;
mod factory;
pub mod hevc_access_unit;
pub mod hevc_rtp;
pub mod hevc_sps;
mod high_performance;
pub mod hpss;
pub mod media_negotiation;
pub mod media_protocol;
mod media_runtime;
pub mod media_transport;
pub mod mvs;
pub mod mvs_bitstream;
pub mod mvs_full;
pub mod mvs_stream;
pub mod mvs_wire;
mod network_reader;
pub mod protocol;
pub mod rsa_srp;
mod runtime;
pub mod session;
pub mod srp;
pub mod srtp;
mod surface_publisher;

pub use auth::{
    select_apple_security_type, APPLE_CREDENTIALS_REQUIRED, APPLE_SECURITY_TYPE_UNAVAILABLE,
};
pub use connection::{
    is_cold_deadline_error, is_peer_closed, is_timeout, AppleConnection, AppleWriterHandle,
};
pub use factory::{
    authenticate_negotiated, finish_authenticated_session, AppleAuthenticated, AppleHandshakeError,
    AppleProtocolFactory, AppleProtocolSession, AppleSessionMetadata, EstablishedAppleSession,
};
pub use runtime::run_established_hpss_session;
pub use session::{SessionCrypto, SessionEncodingProfile};

#[cfg(test)]
pub(crate) fn read_private_fixture_text(relative_path: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative_path);
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("本地私有测试 fixture 不可用（{relative_path}）: {error}"))
}

#[cfg(test)]
pub(crate) fn read_private_fixture_bytes(relative_path: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative_path);
    std::fs::read(path)
        .unwrap_or_else(|error| panic!("本地私有测试 fixture 不可用（{relative_path}）: {error}"))
}
