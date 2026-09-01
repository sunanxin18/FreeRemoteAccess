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
use crate::high_performance::{
    HighPerformanceDiagnostic, HighPerformanceStageObserver, APPLE_HIGH_PERFORMANCE_UNAVAILABLE,
};
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
const HIGH_PERFORMANCE_SESSION_ENCODING_PROFILE: SessionEncodingProfile =
    SessionEncodingProfile::AppleUdpMedia;

fn product_error(protocol_id: frd_core::ProtocolId, code: &'static str) -> ProtocolError {
    ProtocolError::adapter(protocol_id, code)
}

fn select_product_high_performance_security(
    offered: &[u8],
    credentials: &Credentials,
) -> Result<u8, ProtocolError> {
    let mut observer = HighPerformanceStageObserver::disabled();
    select_product_high_performance_security_for_with_observer(
        frd_core::ProtocolId::apple_hpss_mvs(),
        offered,
        credentials,
        &mut observer,
    )
}

fn select_product_high_performance_security_for_with_observer(
    protocol_id: frd_core::ProtocolId,
    offered: &[u8],
    credentials: &Credentials,
    observer: &mut HighPerformanceStageObserver<'_>,
) -> Result<u8, ProtocolError> {
    if credentials.username.is_empty() || credentials.password.expose().is_empty() {
        return Err(product_error(protocol_id, APPLE_CREDENTIALS_REQUIRED));
    }
    let diagnostic = diagnostic_for_product_high_performance_security_offer(offered);
    observer.observe(diagnostic);
    if diagnostic == HighPerformanceDiagnostic::NamedSrpSelected {
        Ok(security::APPLE_SRP)
    } else {
        Err(product_error(
            protocol_id,
            APPLE_HIGH_PERFORMANCE_UNAVAILABLE,
        ))
    }
}

fn diagnostic_for_product_high_performance_security_offer(
    offered: &[u8],
) -> HighPerformanceDiagnostic {
    if offered.contains(&security::APPLE_SRP) {
        HighPerformanceDiagnostic::NamedSrpSelected
    } else {
        HighPerformanceDiagnostic::NamedSrpNotOffered
    }
}

pub struct AppleProtocolFactory;
pub struct AppleHighPerformanceProtocolFactory;

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

    fn into_protocol_error(
        self,
        protocol_id: frd_core::ProtocolId,
        fallback: &'static str,
    ) -> ProtocolError {
        match self {
            Self::Protocol(error) => product_error(protocol_id, error.code()),
            Self::Transport(_) => product_error(protocol_id, fallback),
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
        let protocol_id = frd_core::ProtocolId::apple_hpss_mvs();
        let mut sink = |diagnostic: HighPerformanceDiagnostic| diagnostic.emit();
        let mut observer = HighPerformanceStageObserver::for_protocol(&protocol_id, &mut sink);
        descriptor_with_observer(protocol_id, &mut observer)
    }

    fn create(
        &self,
        request: ConnectRequest,
        runtime: ProtocolRuntime,
    ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
        let protocol_id = frd_core::ProtocolId::apple_hpss_mvs();
        let mut sink = |diagnostic: HighPerformanceDiagnostic| diagnostic.emit();
        let mut observer = HighPerformanceStageObserver::for_protocol(&protocol_id, &mut sink);
        create_product_session_with_observer(
            protocol_id,
            PRODUCT_SESSION_ENCODING_PROFILE,
            request,
            runtime,
            &mut observer,
        )
    }
}

impl ProtocolFactory for AppleHighPerformanceProtocolFactory {
    fn descriptor(&self) -> ProtocolDescriptor {
        let protocol_id = frd_core::ProtocolId::apple_high_performance();
        let mut sink = |diagnostic: HighPerformanceDiagnostic| diagnostic.emit();
        let mut observer = HighPerformanceStageObserver::for_protocol(&protocol_id, &mut sink);
        descriptor_with_observer(protocol_id, &mut observer)
    }

    fn create(
        &self,
        request: ConnectRequest,
        runtime: ProtocolRuntime,
    ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
        let protocol_id = frd_core::ProtocolId::apple_high_performance();
        let mut sink = |diagnostic: HighPerformanceDiagnostic| diagnostic.emit();
        let mut observer = HighPerformanceStageObserver::for_protocol(&protocol_id, &mut sink);
        create_product_session_with_observer(
            protocol_id,
            HIGH_PERFORMANCE_SESSION_ENCODING_PROFILE,
            request,
            runtime,
            &mut observer,
        )
    }
}

fn descriptor_with_observer(
    protocol_id: frd_core::ProtocolId,
    observer: &mut HighPerformanceStageObserver<'_>,
) -> ProtocolDescriptor {
    observer.observe(HighPerformanceDiagnostic::SinkReady);
    ProtocolDescriptor::from(protocol_id)
}

fn create_product_session_with_observer(
    expected_protocol_id: frd_core::ProtocolId,
    encoding_profile: SessionEncodingProfile,
    request: ConnectRequest,
    runtime: ProtocolRuntime,
    observer: &mut HighPerformanceStageObserver<'_>,
) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
    observer.observe(HighPerformanceDiagnostic::FactoryCreate);
    if request.protocol_id != expected_protocol_id {
        return Err(product_error(expected_protocol_id, APPLE_PROTOCOL_MISMATCH));
    }
    let credentials = request
        .credentials
        .as_ref()
        .ok_or_else(|| product_error(expected_protocol_id.clone(), APPLE_CREDENTIALS_REQUIRED))?;
    if credentials.username.is_empty() || credentials.password.expose().is_empty() {
        return Err(product_error(
            expected_protocol_id,
            APPLE_CREDENTIALS_REQUIRED,
        ));
    }
    Ok(Box::new(AppleProtocolSession {
        request,
        runtime,
        encoding_profile,
    }))
}

pub struct AppleProtocolSession {
    request: ConnectRequest,
    runtime: ProtocolRuntime,
    encoding_profile: SessionEncodingProfile,
}

impl ProtocolSession for AppleProtocolSession {
    fn run(mut self: Box<Self>) -> ProtocolExit {
        if let Err(error) = self
            .runtime
            .publish_event(SessionEvent::StageChanged(ConnectionStage::Connecting))
        {
            return ProtocolExit::Failed(error);
        }
        let mut sink = |diagnostic: HighPerformanceDiagnostic| diagnostic.emit();
        let mut observer =
            HighPerformanceStageObserver::for_protocol(&self.request.protocol_id, &mut sink);
        match connect_authenticated_for_profile_with_observer(
            &self.request,
            self.encoding_profile,
            &mut observer,
        ) {
            Ok(established) => {
                // Authentication is complete; do not retain the credential
                // buffer for the long-running HPSS/MVS session.
                self.request.credentials.take();
                observer.observe(HighPerformanceDiagnostic::RuntimeHandoff);
                drop(observer);
                crate::runtime::run_authenticated_session(
                    established,
                    self.runtime,
                    self.request.session_id,
                    self.request.protocol_id.clone(),
                )
            }
            Err(error) => ProtocolExit::Failed(error),
        }
    }
}

#[cfg(test)]
fn connect_authenticated(
    request: &ConnectRequest,
) -> Result<EstablishedAppleSession, ProtocolError> {
    connect_authenticated_for_profile(request, PRODUCT_SESSION_ENCODING_PROFILE)
}

#[cfg(test)]
fn connect_authenticated_for_profile(
    request: &ConnectRequest,
    encoding_profile: SessionEncodingProfile,
) -> Result<EstablishedAppleSession, ProtocolError> {
    let mut sink = |diagnostic: HighPerformanceDiagnostic| diagnostic.emit();
    let mut observer = HighPerformanceStageObserver::for_protocol(&request.protocol_id, &mut sink);
    connect_authenticated_for_profile_with_observer(request, encoding_profile, &mut observer)
}

fn connect_authenticated_for_profile_with_observer(
    request: &ConnectRequest,
    encoding_profile: SessionEncodingProfile,
    observer: &mut HighPerformanceStageObserver<'_>,
) -> Result<EstablishedAppleSession, ProtocolError> {
    let protocol_id = request.protocol_id.clone();
    let stream = match literal_socket_address(request.endpoint.host(), request.endpoint.port()) {
        Some(address) => TcpStream::connect(address),
        None => TcpStream::connect((request.endpoint.host(), request.endpoint.port())),
    }
    .map_err(|_| product_error(protocol_id.clone(), APPLE_CONNECTION_FAILED))?;
    observer.observe(HighPerformanceDiagnostic::TcpConnected);
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut connection = AppleConnection::new(stream);
    let (version, offered) = negotiate(&mut connection, observer)
        .map_err(|_| product_error(protocol_id.clone(), APPLE_NEGOTIATION_FAILED))?;
    let credentials = request
        .credentials
        .as_ref()
        .ok_or_else(|| product_error(protocol_id.clone(), APPLE_CREDENTIALS_REQUIRED))?;
    select_product_high_performance_security_for_with_observer(
        protocol_id.clone(),
        &offered,
        credentials,
        observer,
    )?;
    let password = std::str::from_utf8(credentials.password.expose()).map_err(|_| {
        observer.observe(HighPerformanceDiagnostic::AuthenticationFailed);
        product_error(protocol_id.clone(), APPLE_AUTHENTICATION_FAILED)
    })?;
    let authenticated = authenticate_negotiated_with_observer(
        connection,
        version,
        offered,
        &credentials.username,
        password,
        observer,
    )
    .map_err(|error| {
        observer.observe(HighPerformanceDiagnostic::AuthenticationFailed);
        error.into_protocol_error(protocol_id.clone(), APPLE_AUTHENTICATION_FAILED)
    })?;
    finish_product_authenticated_session_with_observer(
        authenticated,
        encoding_profile,
        protocol_id,
        observer,
    )
}

#[cfg(test)]
fn finish_product_authenticated_session(
    authenticated: AppleAuthenticated,
    profile: SessionEncodingProfile,
    protocol_id: frd_core::ProtocolId,
) -> Result<EstablishedAppleSession, ProtocolError> {
    let mut observer = HighPerformanceStageObserver::disabled();
    finish_product_authenticated_session_with_observer(
        authenticated,
        profile,
        protocol_id,
        &mut observer,
    )
}

fn finish_product_authenticated_session_with_observer(
    authenticated: AppleAuthenticated,
    profile: SessionEncodingProfile,
    protocol_id: frd_core::ProtocolId,
    observer: &mut HighPerformanceStageObserver<'_>,
) -> Result<EstablishedAppleSession, ProtocolError> {
    if authenticated.security_type != security::APPLE_SRP || authenticated.srp_key.is_none() {
        observer.observe(HighPerformanceDiagnostic::EncryptionInvariant);
        return Err(product_error(
            protocol_id,
            APPLE_HIGH_PERFORMANCE_UNAVAILABLE,
        ));
    }
    let established = finish_authenticated_session_with_observer(authenticated, profile, observer)
        .map_err(|error| {
            observer.observe(HighPerformanceDiagnostic::AuthenticationFailed);
            error.into_protocol_error(protocol_id.clone(), APPLE_AUTHENTICATION_FAILED)
        })?;
    if !established.connection.is_encrypted() {
        observer.observe(HighPerformanceDiagnostic::EncryptionInvariant);
        return Err(product_error(
            protocol_id,
            APPLE_HIGH_PERFORMANCE_UNAVAILABLE,
        ));
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
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use frd_frame::SurfaceUpdate;
    use frd_protocol_api::{
        ProtocolError, ProtocolFactory, ProtocolRuntime, RuntimeEventSink, RuntimeWake,
        SessionEvent, SurfacePublisher,
    };

    use crate::session::SessionEncodingProfile;

    struct AcceptEvents;

    impl RuntimeEventSink for AcceptEvents {
        fn publish(&self, _: SessionEvent) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct AcceptFrames;

    impl SurfacePublisher for AcceptFrames {
        fn publish(&self, _: SurfaceUpdate) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct AcceptWake;

    impl RuntimeWake for AcceptWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    fn credentials() -> frd_protocol_api::Credentials {
        let mut password = frd_core::SecretBuffer::new(b"test-password".to_vec());
        frd_protocol_api::Credentials {
            username: "test-user".to_owned(),
            password: password.take(),
        }
    }

    fn request(
        address: std::net::SocketAddr,
        protocol_id: frd_core::ProtocolId,
    ) -> frd_protocol_api::ConnectRequest {
        frd_protocol_api::ConnectRequest {
            session_id: frd_core::SessionId::allocate(),
            endpoint: frd_core::Endpoint::new(address.ip().to_string(), address.port()).unwrap(),
            protocol_id,
            credentials: Some(credentials()),
            saved_server_pin: None,
        }
    }

    fn runtime(session_id: frd_core::SessionId) -> ProtocolRuntime {
        ProtocolRuntime::with_ports(
            session_id,
            Box::new(AcceptEvents),
            Box::new(AcceptFrames),
            Box::new(AcceptWake),
        )
    }

    fn run_preoffer_route(
        profile: SessionEncodingProfile,
        protocol_id: frd_core::ProtocolId,
        server: impl FnOnce(TcpStream) + Send + 'static,
    ) -> (
        Vec<crate::high_performance::HighPerformanceDiagnostic>,
        ProtocolError,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            server(stream);
        });
        let request = request(address, protocol_id.clone());
        let mut stages = Vec::new();
        let mut sink = |stage| stages.push(stage);
        let mut observer =
            super::HighPerformanceStageObserver::for_protocol(&protocol_id, &mut sink);

        let error = match super::connect_authenticated_for_profile_with_observer(
            &request,
            profile,
            &mut observer,
        ) {
            Ok(_) => panic!("pre-offer fixture must terminate before authentication"),
            Err(error) => error,
        };
        drop(observer);
        server.join().unwrap();
        (stages, error)
    }

    #[test]
    fn explicit_high_performance_factory_routes_sink_and_create_markers_only_for_hp() {
        let hp_id = frd_core::ProtocolId::apple_high_performance();
        let standard_id = frd_core::ProtocolId::apple_hpss_mvs();

        let mut hp_stages = Vec::new();
        let mut hp_sink = |stage| hp_stages.push(stage);
        let mut hp_observer =
            super::HighPerformanceStageObserver::for_protocol(&hp_id, &mut hp_sink);
        let descriptor = super::descriptor_with_observer(hp_id.clone(), &mut hp_observer);
        let hp_request = request("127.0.0.1:9".parse().unwrap(), hp_id.clone());
        let session_id = hp_request.session_id;
        let _session = super::create_product_session_with_observer(
            hp_id.clone(),
            SessionEncodingProfile::AppleUdpMedia,
            hp_request,
            runtime(session_id),
            &mut hp_observer,
        )
        .unwrap();
        drop(hp_observer);
        assert_eq!(descriptor.id, hp_id);
        assert_eq!(
            hp_stages,
            [
                crate::high_performance::HighPerformanceDiagnostic::SinkReady,
                crate::high_performance::HighPerformanceDiagnostic::FactoryCreate,
            ]
        );

        let mut standard_stages = Vec::new();
        let mut standard_sink = |stage| standard_stages.push(stage);
        let mut standard_observer =
            super::HighPerformanceStageObserver::for_protocol(&standard_id, &mut standard_sink);
        let descriptor =
            super::descriptor_with_observer(standard_id.clone(), &mut standard_observer);
        let standard_request = request("127.0.0.1:9".parse().unwrap(), standard_id.clone());
        let session_id = standard_request.session_id;
        let _session = super::create_product_session_with_observer(
            standard_id.clone(),
            SessionEncodingProfile::AppleTcpMvs,
            standard_request,
            runtime(session_id),
            &mut standard_observer,
        )
        .unwrap();
        drop(standard_observer);
        assert_eq!(descriptor.id, standard_id);
        assert!(standard_stages.is_empty());
    }

    #[test]
    fn explicit_high_performance_preoffer_markers_stop_at_their_exact_boundaries() {
        let (invalid_banner_stages, error) = run_preoffer_route(
            SessionEncodingProfile::AppleUdpMedia,
            frd_core::ProtocolId::apple_high_performance(),
            |mut stream| stream.write_all(&[0_u8; 12]).unwrap(),
        );
        assert_eq!(error.code(), "apple_negotiation_failed");
        assert_eq!(
            invalid_banner_stages,
            [crate::high_performance::HighPerformanceDiagnostic::TcpConnected]
        );

        let (missing_offer_stages, error) = run_preoffer_route(
            SessionEncodingProfile::AppleUdpMedia,
            frd_core::ProtocolId::apple_high_performance(),
            |mut stream| {
                stream.write_all(b"RFB 003.008\n").unwrap();
                let mut echoed_banner = [0_u8; 12];
                stream.read_exact(&mut echoed_banner).unwrap();
            },
        );
        assert_eq!(error.code(), "apple_negotiation_failed");
        assert_eq!(
            missing_offer_stages,
            [
                crate::high_performance::HighPerformanceDiagnostic::TcpConnected,
                crate::high_performance::HighPerformanceDiagnostic::RfbBannerAccepted,
            ]
        );

        let (complete_offer_stages, error) = run_preoffer_route(
            SessionEncodingProfile::AppleUdpMedia,
            frd_core::ProtocolId::apple_high_performance(),
            |mut stream| {
                stream.write_all(b"RFB 003.008\n").unwrap();
                let mut echoed_banner = [0_u8; 12];
                stream.read_exact(&mut echoed_banner).unwrap();
                stream
                    .write_all(&[1, crate::protocol::security::APPLE_ARD])
                    .unwrap();
            },
        );
        assert_eq!(
            error.code(),
            crate::high_performance::APPLE_HIGH_PERFORMANCE_UNAVAILABLE
        );
        assert_eq!(
            complete_offer_stages,
            [
                crate::high_performance::HighPerformanceDiagnostic::TcpConnected,
                crate::high_performance::HighPerformanceDiagnostic::RfbBannerAccepted,
                crate::high_performance::HighPerformanceDiagnostic::SecurityOfferReceived,
                crate::high_performance::HighPerformanceDiagnostic::NamedSrpNotOffered,
            ]
        );
    }

    #[test]
    fn standard_mvs_handshake_does_not_route_high_performance_preoffer_markers() {
        let (stages, error) = run_preoffer_route(
            SessionEncodingProfile::AppleTcpMvs,
            frd_core::ProtocolId::apple_hpss_mvs(),
            |mut stream| {
                stream.write_all(b"RFB 003.008\n").unwrap();
                let mut echoed_banner = [0_u8; 12];
                stream.read_exact(&mut echoed_banner).unwrap();
                stream
                    .write_all(&[1, crate::protocol::security::APPLE_ARD])
                    .unwrap();
            },
        );

        assert_eq!(
            error.code(),
            crate::high_performance::APPLE_HIGH_PERFORMANCE_UNAVAILABLE
        );
        assert!(stages.is_empty());
    }

    #[test]
    fn explicit_high_performance_authentication_return_emits_terminal_marker() {
        let (stages, error) = run_preoffer_route(
            SessionEncodingProfile::AppleUdpMedia,
            frd_core::ProtocolId::apple_high_performance(),
            |mut stream| {
                stream.write_all(b"RFB 003.008\n").unwrap();
                let mut echoed_banner = [0_u8; 12];
                stream.read_exact(&mut echoed_banner).unwrap();
                stream
                    .write_all(&[1, crate::protocol::security::APPLE_SRP])
                    .unwrap();
                let mut selection_and_step1_prefix = [0_u8; 6];
                stream.read_exact(&mut selection_and_step1_prefix).unwrap();
                assert_eq!(
                    selection_and_step1_prefix[0],
                    crate::protocol::security::APPLE_SRP
                );
                assert_eq!(
                    selection_and_step1_prefix[1],
                    crate::protocol::security::APPLE_SRP
                );
            },
        );

        assert_eq!(error.code(), "apple_authentication_failed");
        assert_eq!(
            stages,
            [
                crate::high_performance::HighPerformanceDiagnostic::TcpConnected,
                crate::high_performance::HighPerformanceDiagnostic::RfbBannerAccepted,
                crate::high_performance::HighPerformanceDiagnostic::SecurityOfferReceived,
                crate::high_performance::HighPerformanceDiagnostic::NamedSrpSelected,
                crate::high_performance::HighPerformanceDiagnostic::SrpStep1Written,
                crate::high_performance::HighPerformanceDiagnostic::AuthenticationFailed,
            ]
        );
    }

    #[test]
    fn explicit_high_performance_invalid_password_text_emits_terminal_marker() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"RFB 003.008\n").unwrap();
            let mut echoed_banner = [0_u8; 12];
            stream.read_exact(&mut echoed_banner).unwrap();
            stream
                .write_all(&[1, crate::protocol::security::APPLE_SRP])
                .unwrap();
        });

        let protocol_id = frd_core::ProtocolId::apple_high_performance();
        let mut request = request(address, protocol_id.clone());
        let mut invalid_password = frd_core::SecretBuffer::new(vec![0xff]);
        request.credentials.as_mut().unwrap().password = invalid_password.take();
        let mut stages = Vec::new();
        let mut sink = |stage| stages.push(stage);
        let mut observer =
            super::HighPerformanceStageObserver::for_protocol(&protocol_id, &mut sink);

        let error = match super::connect_authenticated_for_profile_with_observer(
            &request,
            SessionEncodingProfile::AppleUdpMedia,
            &mut observer,
        ) {
            Ok(_) => panic!("invalid password text must fail authentication"),
            Err(error) => error,
        };
        drop(observer);
        server.join().unwrap();

        assert_eq!(error.code(), "apple_authentication_failed");
        assert_eq!(
            stages,
            [
                crate::high_performance::HighPerformanceDiagnostic::TcpConnected,
                crate::high_performance::HighPerformanceDiagnostic::RfbBannerAccepted,
                crate::high_performance::HighPerformanceDiagnostic::SecurityOfferReceived,
                crate::high_performance::HighPerformanceDiagnostic::NamedSrpSelected,
                crate::high_performance::HighPerformanceDiagnostic::AuthenticationFailed,
            ]
        );
    }

    #[test]
    fn explicit_high_performance_finalization_routes_session_boundaries_in_order() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut client_init = [0u8; 1];
            stream.read_exact(&mut client_init).unwrap();
            assert_eq!(
                client_init,
                [crate::protocol::apple_session::ENCRYPTED_SESSION_CLIENT_INIT]
            );

            let mut server_init = vec![0x00, 0x02, 0x00, 0x02];
            server_init
                .extend_from_slice(&[32, 24, 0, 1, 0, 0xff, 0, 0xff, 0, 0xff, 16, 8, 0, 0, 0, 0]);
            server_init.extend_from_slice(&0u32.to_be_bytes());
            stream.write_all(&server_init).unwrap();

            let mut session_requests = vec![0u8; 66 + 24];
            stream.read_exact(&mut session_requests).unwrap();
        });

        let stream = TcpStream::connect(address).unwrap();
        let authenticated = super::AppleAuthenticated {
            connection: crate::AppleConnection::new(stream),
            security_type: crate::protocol::security::APPLE_SRP,
            srp_key: Some([0xabu8; 64]),
        };
        let protocol_id = frd_core::ProtocolId::apple_high_performance();
        let mut stages = Vec::new();
        let mut sink = |stage| stages.push(stage);
        let mut observer = crate::high_performance::HighPerformanceStageObserver::for_protocol(
            &protocol_id,
            &mut sink,
        );

        let error = match super::finish_product_authenticated_session_with_observer(
            authenticated,
            SessionEncodingProfile::AppleUdpMedia,
            protocol_id,
            &mut observer,
        ) {
            Ok(_) => panic!("withheld EncryptionInfo must fail finalization"),
            Err(error) => error,
        };
        drop(observer);
        server.join().unwrap();

        assert_eq!(error.code(), "apple_authentication_failed");
        assert_eq!(
            stages,
            [
                crate::high_performance::HighPerformanceDiagnostic::ClientInitWritten,
                crate::high_performance::HighPerformanceDiagnostic::ServerInitAccepted,
                crate::high_performance::HighPerformanceDiagnostic::EncryptionRequestWritten,
                crate::high_performance::HighPerformanceDiagnostic::AuthenticationFailed,
            ]
        );
    }

    #[test]
    fn finish_authenticated_session_keeps_handshake_timeout_through_encryption_info() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (peer_ready_tx, peer_ready_rx) = mpsc::channel();
        let (peer_release_tx, peer_release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut client_init = [0u8; 1];
            stream.read_exact(&mut client_init).unwrap();
            assert_eq!(
                client_init,
                [crate::protocol::apple_session::ENCRYPTED_SESSION_CLIENT_INIT]
            );

            let mut server_init = vec![0x00, 0x02, 0x00, 0x02];
            server_init
                .extend_from_slice(&[32, 24, 0, 1, 0, 0xff, 0, 0xff, 0, 0xff, 16, 8, 0, 0, 0, 0]);
            server_init.extend_from_slice(&0u32.to_be_bytes());
            stream.write_all(&server_init).unwrap();

            let mut session_requests = vec![0u8; 66 + 24];
            stream.read_exact(&mut session_requests).unwrap();
            peer_ready_tx.send(()).unwrap();
            peer_release_rx.recv().unwrap();
            stream.shutdown(Shutdown::Both).unwrap();
        });

        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let shutdown = stream.try_clone().unwrap();
        let authenticated = super::AppleAuthenticated {
            connection: crate::AppleConnection::new(stream),
            security_type: crate::protocol::security::APPLE_SRP,
            srp_key: Some([0xabu8; 64]),
        };
        let (result_tx, result_rx) = mpsc::channel();
        let finisher = thread::spawn(move || {
            result_tx
                .send(super::finish_authenticated_session(
                    authenticated,
                    SessionEncodingProfile::AppleUdpMedia,
                ))
                .unwrap();
        });

        peer_ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let result = result_rx.recv_timeout(Duration::from_secs(1));

        shutdown.shutdown(Shutdown::Both).unwrap();
        peer_release_tx.send(()).unwrap();
        server.join().unwrap();
        finisher.join().unwrap();

        let error = match result.expect("withheld EncryptionInfo must not block the finisher") {
            Ok(_) => panic!("withheld EncryptionInfo must fail finalization"),
            Err(error) => error,
        };
        match error {
            super::AppleHandshakeError::Transport(_) => {}
            super::AppleHandshakeError::Protocol(error) => {
                panic!("withheld EncryptionInfo must time out, got {error:?}");
            }
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
        assert_eq!(
            super::diagnostic_for_product_high_performance_security_offer(&[]),
            crate::high_performance::HighPerformanceDiagnostic::NamedSrpNotOffered
        );
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
        assert_eq!(
            super::diagnostic_for_product_high_performance_security_offer(&[
                crate::protocol::security::APPLE_SRP,
            ]),
            crate::high_performance::HighPerformanceDiagnostic::NamedSrpSelected
        );
    }

    #[test]
    fn unencrypted_product_session_is_rejected_after_finalization_without_hpss_bytes() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut finalization_bytes = [0u8; 64];
            let received = stream.read(&mut finalization_bytes).unwrap();
            finalization_bytes[..received].to_vec()
        });
        let authenticated = super::AppleAuthenticated {
            connection: crate::AppleConnection::new(client),
            security_type: crate::protocol::security::APPLE_ARD,
            srp_key: None,
        };

        let error = match super::finish_product_authenticated_session(
            authenticated,
            SessionEncodingProfile::AppleTcpMvs,
            frd_core::ProtocolId::apple_hpss_mvs(),
        ) {
            Ok(_) => panic!("未加密产品会话不得进入 runtime"),
            Err(error) => error,
        };

        assert_eq!(
            error.code(),
            crate::high_performance::APPLE_HIGH_PERFORMANCE_UNAVAILABLE
        );
        assert!(
            server.join().unwrap().is_empty(),
            "不一致的 legacy 产品状态必须在 ClientInit 前拒绝"
        );
    }

    #[test]
    fn standard_and_high_performance_factories_keep_independent_identities_and_profiles() {
        assert_eq!(
            super::PRODUCT_SESSION_ENCODING_PROFILE,
            SessionEncodingProfile::AppleTcpMvs
        );
        assert_eq!(
            super::HIGH_PERFORMANCE_SESSION_ENCODING_PROFILE,
            SessionEncodingProfile::AppleUdpMedia
        );
        assert_eq!(
            super::AppleProtocolFactory.descriptor().id,
            frd_core::ProtocolId::apple_hpss_mvs()
        );
        assert_eq!(
            super::AppleHighPerformanceProtocolFactory.descriptor().id,
            frd_core::ProtocolId::apple_high_performance()
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

fn negotiate(
    connection: &mut AppleConnection,
    observer: &mut HighPerformanceStageObserver<'_>,
) -> Result<((u8, u8), Vec<u8>)> {
    let raw_banner = connection.read_vec(frd_wire_rfb::RFB_BANNER_BYTES)?;
    let banner = decode_banner(&raw_banner)?;
    observer.observe(HighPerformanceDiagnostic::RfbBannerAccepted);
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
    observer.observe(HighPerformanceDiagnostic::SecurityOfferReceived);
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
    connection: AppleConnection,
    version: (u8, u8),
    offered: impl AsRef<[u8]>,
    username: &str,
    password: &str,
) -> Result<AppleAuthenticated, AppleHandshakeError> {
    let mut observer = HighPerformanceStageObserver::disabled();
    authenticate_negotiated_with_observer(
        connection,
        version,
        offered,
        username,
        password,
        &mut observer,
    )
}

fn authenticate_negotiated_with_observer(
    mut connection: AppleConnection,
    version: (u8, u8),
    offered: impl AsRef<[u8]>,
    username: &str,
    password: &str,
    observer: &mut HighPerformanceStageObserver<'_>,
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
            srp::authenticate_with_observer(&mut connection, username, password, observer)
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
    let mut observer = HighPerformanceStageObserver::disabled();
    finish_authenticated_session_with_observer(authenticated, profile, &mut observer)
}

fn finish_authenticated_session_with_observer(
    authenticated: AppleAuthenticated,
    profile: SessionEncodingProfile,
    observer: &mut HighPerformanceStageObserver<'_>,
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
    observer.observe(HighPerformanceDiagnostic::ClientInitWritten);
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
    observer.observe(HighPerformanceDiagnostic::ServerInitAccepted);

    if let Some(srp_key) = srp_key {
        let crypto = session::establish_with_table_with_observer(
            &mut connection,
            &srp_key,
            profile,
            observer,
        )
        .map_err(AppleHandshakeError::transport)?;
        connection
            .set_crypto(crypto)
            .map_err(AppleHandshakeError::transport)?;
    }
    connection.set_read_timeout(None).ok();

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
