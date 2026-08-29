use std::mem::size_of;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::Duration;

use anyhow::{Context, Result};
use frd_protocol_api::{
    ConnectRequest, ConnectionStage, Credentials, ProtocolDescriptor, ProtocolError, ProtocolExit,
    ProtocolFactory, ProtocolRuntime, ProtocolSession, SessionEvent,
};
use frd_wire_rfb::{
    decode_banner, decode_security_types, decode_security_types_header, decode_server_init,
    decode_server_init_header, encode_banner, SERVER_INIT_HEADER_BYTES,
};

use crate::auth::{select_apple_security_type_parts, APPLE_CREDENTIALS_REQUIRED};
use crate::connection::AppleConnection;
use crate::high_performance::APPLE_HIGH_PERFORMANCE_UNAVAILABLE;
use crate::protocol::{self, security};
use crate::session::{self, SessionEncodingProfile};
use crate::{ard, rsa_srp, srp};

const APPLE_CONNECTION_FAILED: &str = "apple_connection_failed";
const APPLE_NEGOTIATION_FAILED: &str = "apple_negotiation_failed";
const APPLE_AUTHENTICATION_FAILED: &str = "apple_authentication_failed";
const APPLE_PROTOCOL_MISMATCH: &str = "apple_protocol_mismatch";
const SECURITY_FAILURE_REASON_MAX_BYTES: usize = 4096;
const PRODUCT_SESSION_ENCODING_PROFILE: SessionEncodingProfile =
    SessionEncodingProfile::AppleTcpMvs;

fn apple_error(code: &'static str) -> ProtocolError {
    ProtocolError::adapter(frd_core::ProtocolId::apple_hpss_mvs(), code)
}

fn select_product_high_performance_security(
    offered: &[u8],
    credentials: &Credentials,
) -> Result<u8, ProtocolError> {
    if credentials.username.is_empty() || credentials.password.expose().is_empty() {
        return Err(apple_error(APPLE_CREDENTIALS_REQUIRED));
    }
    offered
        .contains(&security::APPLE_SRP)
        .then_some(security::APPLE_SRP)
        .ok_or_else(|| apple_error(APPLE_HIGH_PERFORMANCE_UNAVAILABLE))
}

pub struct AppleProtocolFactory;

#[derive(Debug)]
pub enum AppleHandshakeError {
    Protocol(ProtocolError),
    Transport(anyhow::Error),
}

impl AppleHandshakeError {
    fn protocol(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }

    fn transport(error: impl Into<anyhow::Error>) -> Self {
        Self::Transport(error.into())
    }

    pub fn code(&self) -> Option<&'static str> {
        match self {
            Self::Protocol(error) => Some(error.code()),
            Self::Transport(_) => None,
        }
    }

    pub fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Protocol(error) => anyhow::anyhow!(error.code()),
            Self::Transport(error) => error,
        }
    }

    fn into_protocol_error(self, fallback: &'static str) -> ProtocolError {
        match self {
            Self::Protocol(error) => error,
            Self::Transport(_) => apple_error(fallback),
        }
    }
}

pub struct AppleAuthenticated {
    connection: AppleConnection,
    security_type: u8,
    srp_key: Option<[u8; 64]>,
}

pub struct AppleSessionMetadata {
    pub security_type: u8,
    pub size: frd_core::PixelSize,
    pub pixel_format: frd_wire_rfb::PixelFormat,
    pub name: String,
    pub encoding_profile: SessionEncodingProfile,
}

pub struct EstablishedAppleSession {
    pub connection: AppleConnection,
    pub metadata: AppleSessionMetadata,
}

impl AppleProtocolFactory {
    pub fn select_security_type(
        &self,
        offered: &[u8],
        credentials: &Credentials,
    ) -> Result<u8, ProtocolError> {
        select_product_high_performance_security(offered, credentials)
    }
}

impl ProtocolFactory for AppleProtocolFactory {
    fn descriptor(&self) -> ProtocolDescriptor {
        ProtocolDescriptor::from(frd_core::ProtocolId::apple_hpss_mvs())
    }

    fn create(
        &self,
        request: ConnectRequest,
        runtime: ProtocolRuntime,
    ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
        if request.protocol_id != frd_core::ProtocolId::apple_hpss_mvs() {
            return Err(apple_error(APPLE_PROTOCOL_MISMATCH));
        }
        let credentials = request
            .credentials
            .as_ref()
            .ok_or_else(|| apple_error(APPLE_CREDENTIALS_REQUIRED))?;
        if credentials.username.is_empty() || credentials.password.expose().is_empty() {
            return Err(apple_error(APPLE_CREDENTIALS_REQUIRED));
        }
        Ok(Box::new(AppleProtocolSession { request, runtime }))
    }
}

pub struct AppleProtocolSession {
    request: ConnectRequest,
    runtime: ProtocolRuntime,
}

impl ProtocolSession for AppleProtocolSession {
    fn run(mut self: Box<Self>) -> ProtocolExit {
        if let Err(error) = self
            .runtime
            .publish_event(SessionEvent::StageChanged(ConnectionStage::Connecting))
        {
            return ProtocolExit::Failed(error);
        }
        match connect_authenticated(&self.request) {
            Ok(established) => {
                // Authentication is complete; do not retain the credential
                // buffer for the long-running HPSS/MVS session.
                self.request.credentials.take();
                crate::runtime::run_authenticated_session(
                    established,
                    self.runtime,
                    self.request.session_id,
                )
            }
            Err(error) => ProtocolExit::Failed(error),
        }
    }
}

fn connect_authenticated(
    request: &ConnectRequest,
) -> Result<EstablishedAppleSession, ProtocolError> {
    let stream = match literal_socket_address(request.endpoint.host(), request.endpoint.port()) {
        Some(address) => TcpStream::connect(address),
        None => TcpStream::connect((request.endpoint.host(), request.endpoint.port())),
    }
    .map_err(|_| apple_error(APPLE_CONNECTION_FAILED))?;
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut connection = AppleConnection::new(stream);
    let (version, offered) =
        negotiate(&mut connection).map_err(|_| apple_error(APPLE_NEGOTIATION_FAILED))?;
    let credentials = request
        .credentials
        .as_ref()
        .ok_or_else(|| apple_error(APPLE_CREDENTIALS_REQUIRED))?;
    select_product_high_performance_security(&offered, credentials)?;
    let password = std::str::from_utf8(credentials.password.expose())
        .map_err(|_| apple_error(APPLE_AUTHENTICATION_FAILED))?;
    let authenticated = authenticate_negotiated(
        connection,
        version,
        offered,
        &credentials.username,
        password,
    )
    .map_err(|error| error.into_protocol_error(APPLE_AUTHENTICATION_FAILED))?;
    finish_product_authenticated_session(authenticated, PRODUCT_SESSION_ENCODING_PROFILE)
}

fn finish_product_authenticated_session(
    authenticated: AppleAuthenticated,
    profile: SessionEncodingProfile,
) -> Result<EstablishedAppleSession, ProtocolError> {
    let established = finish_authenticated_session(authenticated, profile)
        .map_err(|error| error.into_protocol_error(APPLE_AUTHENTICATION_FAILED))?;
    if !established.connection.is_encrypted() {
        return Err(apple_error(APPLE_HIGH_PERFORMANCE_UNAVAILABLE));
    }
    Ok(established)
}

fn literal_socket_address(host: &str, port: u16) -> Option<SocketAddr> {
    host.parse::<IpAddr>()
        .ok()
        .map(|address| SocketAddr::new(address, port))
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod product_profile_tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use crate::session::SessionEncodingProfile;

    fn credentials() -> frd_protocol_api::Credentials {
        let mut password = frd_core::SecretBuffer::new(b"test-password".to_vec());
        frd_protocol_api::Credentials {
            username: "test-user".to_owned(),
            password: password.take(),
        }
    }

    #[test]
    fn product_high_performance_security_rejects_every_offer_without_named_srp() {
        let factory = super::AppleProtocolFactory;
        for offered in [
            vec![crate::protocol::security::APPLE_ARD],
            vec![crate::protocol::security::APPLE_RSA_SRP],
            vec![crate::protocol::security::APPLE_ARD_39],
            vec![
                crate::protocol::security::APPLE_ARD,
                crate::protocol::security::APPLE_RSA_SRP,
                crate::protocol::security::APPLE_ARD_39,
            ],
        ] {
            let error = factory
                .select_security_type(&offered, &credentials())
                .unwrap_err();
            assert_eq!(
                error.code(),
                crate::high_performance::APPLE_HIGH_PERFORMANCE_UNAVAILABLE
            );
        }
    }

    #[test]
    fn product_high_performance_security_selects_only_named_srp() {
        let selected = super::AppleProtocolFactory
            .select_security_type(
                &[
                    crate::protocol::security::APPLE_ARD,
                    crate::protocol::security::APPLE_SRP,
                    crate::protocol::security::APPLE_RSA_SRP,
                ],
                &credentials(),
            )
            .unwrap();

        assert_eq!(selected, crate::protocol::security::APPLE_SRP);
    }

    #[test]
    fn unencrypted_product_session_is_rejected_after_finalization_without_hpss_bytes() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut client_init = [0u8; 1];
            stream.read_exact(&mut client_init).unwrap();
            assert_eq!(
                client_init,
                [crate::protocol::apple_session::SHARED_CLIENT_INIT]
            );
            let mut server_init = [0u8; frd_wire_rfb::SERVER_INIT_HEADER_BYTES];
            server_init[..2].copy_from_slice(&8u16.to_be_bytes());
            server_init[2..4].copy_from_slice(&8u16.to_be_bytes());
            server_init[4..20].copy_from_slice(&frd_wire_rfb::PixelFormat::OURS.to_bytes());
            stream.write_all(&server_init).unwrap();
            let mut application_bytes = Vec::new();
            stream.read_to_end(&mut application_bytes).unwrap();
            application_bytes
        });
        let authenticated = super::AppleAuthenticated {
            connection: crate::AppleConnection::new(client),
            security_type: crate::protocol::security::APPLE_ARD,
            srp_key: None,
        };

        let error = match super::finish_product_authenticated_session(
            authenticated,
            SessionEncodingProfile::AppleTcpMvs,
        ) {
            Ok(_) => panic!("未加密产品会话不得进入 runtime"),
            Err(error) => error,
        };

        assert_eq!(
            error.code(),
            crate::high_performance::APPLE_HIGH_PERFORMANCE_UNAVAILABLE
        );
        assert!(server.join().unwrap().is_empty());
    }

    #[test]
    fn product_desktop_uses_the_verified_tcp_mvs_profile() {
        assert_eq!(
            super::PRODUCT_SESSION_ENCODING_PROFILE,
            SessionEncodingProfile::AppleTcpMvs
        );
    }

    #[test]
    fn literal_ip_endpoint_bypasses_windows_name_resolution() {
        assert_eq!(
            super::literal_socket_address("192.0.2.44", 5900),
            Some("192.0.2.44:5900".parse().unwrap())
        );
        assert_eq!(super::literal_socket_address("mac.example", 5900), None);
    }

    #[test]
    fn invalid_server_banner_reports_negotiation_failure_not_connect_failure() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&[0_u8; 12]).unwrap();
        });
        let session_id = frd_core::SessionId::allocate();
        let mut password = frd_core::SecretBuffer::new(b"test-password".to_vec());
        let request = frd_protocol_api::ConnectRequest {
            session_id,
            endpoint: frd_core::Endpoint::new(address.ip().to_string(), address.port()).unwrap(),
            protocol_id: frd_core::ProtocolId::apple_hpss_mvs(),
            credentials: Some(frd_protocol_api::Credentials {
                username: "test-user".to_owned(),
                password: password.take(),
            }),
            saved_server_pin: None,
        };

        let error = match super::connect_authenticated(&request) {
            Ok(_) => panic!("invalid banner must fail negotiation"),
            Err(error) => error,
        };
        server.join().unwrap();

        assert_eq!(error.code(), "apple_negotiation_failed");
    }
}

fn negotiate(connection: &mut AppleConnection) -> Result<((u8, u8), Vec<u8>)> {
    let raw_banner = connection.read_vec(frd_wire_rfb::RFB_BANNER_BYTES)?;
    let banner = decode_banner(&raw_banner)?;
    let parsed = (
        banner.major.min(u16::from(u8::MAX)) as u8,
        banner.minor.min(u16::from(u8::MAX)) as u8,
    );
    let version = if banner.minor >= 8 {
        connection.write_all(&banner.wire)?;
        (3, 8)
    } else {
        let selected = match parsed.1 {
            3 | 7 => parsed,
            _ => (3, 8),
        };
        connection.write_all(&encode_banner(selected.0.into(), selected.1.into())?)?;
        selected
    };
    let security_types = match version.1 {
        3 => {
            let types = decode_security_types(3, &connection.read_vec(size_of::<u32>())?)?;
            if types.is_empty() {
                read_reason(connection)?;
                anyhow::bail!("Apple 服务端拒绝连接");
            }
            types
        }
        7 => {
            let prefix = connection.read_vec(size_of::<u16>())?;
            let (_, count) = decode_security_types_header(7, &prefix)?;
            if count == 0 {
                read_reason(connection)?;
                anyhow::bail!("Apple 服务端拒绝连接");
            }
            let mut bytes = prefix;
            bytes.extend_from_slice(&connection.read_vec(count)?);
            decode_security_types(7, &bytes)?
        }
        _ => {
            let prefix = connection.read_vec(size_of::<u8>())?;
            let (_, count) = decode_security_types_header(8, &prefix)?;
            if count == 0 {
                read_reason(connection)?;
                anyhow::bail!("Apple 服务端拒绝连接");
            }
            let mut bytes = prefix;
            bytes.extend_from_slice(&connection.read_vec(count)?);
            decode_security_types(8, &bytes)?
        }
    };
    Ok((version, security_types))
}

fn read_reason(connection: &mut AppleConnection) -> Result<()> {
    let length = usize::try_from(connection.read_u32()?).context("安全失败原因长度无效")?;
    if length > SECURITY_FAILURE_REASON_MAX_BYTES {
        anyhow::bail!("安全失败原因超出资源预算");
    }
    let _redacted = connection.read_vec(length)?;
    Ok(())
}

pub fn authenticate_negotiated(
    mut connection: AppleConnection,
    version: (u8, u8),
    offered: impl AsRef<[u8]>,
    username: &str,
    password: &str,
) -> Result<AppleAuthenticated, AppleHandshakeError> {
    let security_type =
        select_apple_security_type_parts(offered.as_ref(), username, password.as_bytes())
            .map_err(AppleHandshakeError::protocol)?;
    if version.1 != 3 && security_type != security::APPLE_RSA_SRP {
        connection
            .write_all(&[security_type])
            .map_err(AppleHandshakeError::transport)?;
    }
    let srp_key = match security_type {
        security::APPLE_ARD => {
            ard::authenticate(&mut connection, username, password)
                .map_err(AppleHandshakeError::transport)?;
            None
        }
        security::APPLE_RSA_SRP => {
            rsa_srp::authenticate(&mut connection, username, password)
                .map_err(AppleHandshakeError::transport)?;
            None
        }
        security::APPLE_SRP => Some(
            srp::authenticate(&mut connection, username, password)
                .map_err(AppleHandshakeError::transport)?,
        ),
        _ => unreachable!("严格 Apple selector 只返回已实现类型"),
    };

    Ok(AppleAuthenticated {
        connection,
        security_type,
        srp_key,
    })
}

pub fn finish_authenticated_session(
    authenticated: AppleAuthenticated,
    profile: SessionEncodingProfile,
) -> Result<EstablishedAppleSession, AppleHandshakeError> {
    let AppleAuthenticated {
        mut connection,
        security_type,
        srp_key,
    } = authenticated;

    let encrypted = srp_key.is_some();
    connection
        .write_all(&[if encrypted {
            protocol::apple_session::ENCRYPTED_SESSION_CLIENT_INIT
        } else {
            protocol::apple_session::SHARED_CLIENT_INIT
        }])
        .map_err(AppleHandshakeError::transport)?;
    let header = connection
        .read_vec(SERVER_INIT_HEADER_BYTES)
        .map_err(AppleHandshakeError::transport)?;
    let parsed = decode_server_init_header(&header).map_err(AppleHandshakeError::transport)?;
    let mut server_init = header;
    server_init.extend_from_slice(
        &connection
            .read_vec(parsed.name_length)
            .map_err(AppleHandshakeError::transport)?,
    );
    let server_init = decode_server_init(&server_init).map_err(AppleHandshakeError::transport)?;
    connection.set_read_timeout(None).ok();

    if let Some(srp_key) = srp_key {
        let crypto = session::establish_with_table(&mut connection, &srp_key, profile)
            .map_err(AppleHandshakeError::transport)?;
        connection
            .set_crypto(crypto)
            .map_err(AppleHandshakeError::transport)?;
    }

    Ok(EstablishedAppleSession {
        connection,
        metadata: AppleSessionMetadata {
            security_type,
            size: server_init.size,
            pixel_format: server_init.pixel_format,
            name: server_init.name,
            encoding_profile: profile,
        },
    })
}
