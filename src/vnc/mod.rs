pub mod ard;
#[cfg(any(feature = "media", test))]
pub mod audio_codec;
#[cfg(any(feature = "media", test))]
pub mod audio_input;
#[cfg(feature = "media")]
pub mod audio_io;
pub mod auth;
pub mod client;
pub mod cold_credentials;
pub mod cold_hpss;
pub mod dynamic_resolution;
pub mod hpss;
#[cfg(feature = "media")]
pub mod hpss_session;
pub mod local_username;
pub mod media_negotiation;
pub mod media_protocol;
pub mod media_transport;
pub mod mvs;
pub mod mvs_bitstream;
pub mod mvs_capture_v2;
pub mod mvs_capture_v2_writer;
pub mod mvs_full;
pub mod mvs_stream;
pub mod mvs_wire;
pub mod protocol;
pub mod rsa_srp;
pub mod session;
pub mod srp;
pub mod srtp;

#[cfg(test)]
pub(crate) fn read_private_fixture_text(relative_path: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("本地私有测试 fixture 不可用（{relative_path}）: {error}"))
}

#[cfg(test)]
pub(crate) fn read_private_fixture_bytes(relative_path: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    std::fs::read(path)
        .unwrap_or_else(|error| panic!("本地私有测试 fixture 不可用（{relative_path}）: {error}"))
}
