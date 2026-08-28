use std::thread;
use std::time::Duration;

use frd_core::SessionId;
use frd_protocol_api::{
    evaluate_server_identity, Endpoint, ProtocolError, ProtocolId, ProtocolRuntime,
    ServerIdentityChallenge, ServerIdentityDecision, SessionCommand, SessionEvent,
};
use sha2::{Digest, Sha256};
use unicode_general_category::{get_general_category, GeneralCategory};

use crate::error::{rdp_error, RDP_SERVER_IDENTITY_CHANGED, RDP_SERVER_IDENTITY_REJECTED};

const MAX_CERTIFICATE_NAME_BYTES: usize = 256;
const UNKNOWN_CERTIFICATE_NAME: &str = "未知";
const IDENTITY_COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PlatformValidationFailure {
    error: rustls::Error,
}

impl PlatformValidationFailure {
    pub(crate) fn from_rustls(error: rustls::Error) -> Self {
        Self { error }
    }

    pub(crate) fn is_unknown_issuer(&self) -> bool {
        matches!(
            self.error,
            rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer)
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SanitizedCertificateNames {
    subject: String,
    issuer: String,
}

impl SanitizedCertificateNames {
    pub(crate) fn new(subject: &str, issuer: &str) -> Self {
        Self {
            subject: sanitize_certificate_name(subject),
            issuer: sanitize_certificate_name(issuer),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum IdentityDisposition {
    SystemTrusted {
        fingerprint: [u8; 32],
    },
    PinMatched {
        fingerprint: [u8; 32],
    },
    Challenge {
        fingerprint: [u8; 32],
        subject: String,
        issuer: String,
    },
    PinMismatch,
    Reject,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AcceptedServerIdentity {
    SystemTrusted { fingerprint: [u8; 32] },
    ExactPin { fingerprint: [u8; 32] },
}

#[derive(Debug)]
pub(crate) struct ObservedServerIdentity {
    pub(crate) fingerprint: [u8; 32],
    pub(crate) platform_validation: Result<(), PlatformValidationFailure>,
    pub(crate) names: SanitizedCertificateNames,
}

pub(crate) fn fingerprint_sha256(leaf_der: &[u8]) -> [u8; 32] {
    Sha256::digest(leaf_der).into()
}

fn classify_identity(
    saved_pin: Option<[u8; 32]>,
    fingerprint: [u8; 32],
    platform_validation: Result<(), PlatformValidationFailure>,
    names: SanitizedCertificateNames,
) -> IdentityDisposition {
    if saved_pin.is_some_and(|saved| saved != fingerprint) {
        return IdentityDisposition::PinMismatch;
    }
    if platform_validation
        .as_ref()
        .is_err_and(|failure| !failure.is_unknown_issuer())
    {
        return IdentityDisposition::Reject;
    }
    match saved_pin {
        Some(saved) if saved == fingerprint => IdentityDisposition::PinMatched { fingerprint },
        Some(_) => unreachable!("mismatching saved pins returned above"),
        None if platform_validation.is_ok() => IdentityDisposition::SystemTrusted { fingerprint },
        None => IdentityDisposition::Challenge {
            fingerprint,
            subject: names.subject,
            issuer: names.issuer,
        },
    }
}

fn identity_challenge(
    session_id: SessionId,
    challenge_id: u64,
    endpoint: Endpoint,
    fingerprint: [u8; 32],
    subject: String,
    issuer: String,
) -> ServerIdentityChallenge {
    ServerIdentityChallenge {
        session_id,
        challenge_id,
        protocol_id: ProtocolId::rdp(),
        endpoint,
        sha256_fingerprint: fingerprint,
        subject,
        issuer,
        validation: evaluate_server_identity(None, fingerprint),
    }
}

fn wait_for_identity_decision(
    session_id: SessionId,
    challenge_id: u64,
    fingerprint: [u8; 32],
    mut next_command: impl FnMut() -> Option<SessionCommand>,
) -> Result<Option<AcceptedServerIdentity>, ProtocolError> {
    loop {
        match next_command() {
            Some(SessionCommand::Disconnect) => return Ok(None),
            Some(SessionCommand::ResolveServerIdentity {
                session_id: command_session_id,
                challenge_id: command_challenge_id,
                decision,
            }) if command_session_id == session_id && command_challenge_id == challenge_id => {
                return match decision {
                    ServerIdentityDecision::TrustOnce
                    | ServerIdentityDecision::TrustAndRemember => {
                        Ok(Some(AcceptedServerIdentity::ExactPin { fingerprint }))
                    }
                    ServerIdentityDecision::Reject => Err(rdp_error(RDP_SERVER_IDENTITY_REJECTED)),
                };
            }
            Some(_) => {}
            None => thread::sleep(IDENTITY_COMMAND_POLL_INTERVAL),
        }
    }
}

pub(crate) fn resolve_server_identity(
    endpoint: Endpoint,
    saved_pin: Option<[u8; 32]>,
    observed: ObservedServerIdentity,
    session_id: SessionId,
    runtime: &mut ProtocolRuntime,
) -> Result<Option<AcceptedServerIdentity>, ProtocolError> {
    match classify_identity(
        saved_pin,
        observed.fingerprint,
        observed.platform_validation,
        observed.names,
    ) {
        IdentityDisposition::SystemTrusted { fingerprint } => {
            Ok(Some(AcceptedServerIdentity::SystemTrusted { fingerprint }))
        }
        IdentityDisposition::PinMatched { fingerprint } => {
            Ok(Some(AcceptedServerIdentity::ExactPin { fingerprint }))
        }
        IdentityDisposition::PinMismatch => Err(rdp_error(RDP_SERVER_IDENTITY_CHANGED)),
        IdentityDisposition::Reject => Err(rdp_error(crate::error::RDP_TLS_FAILED)),
        IdentityDisposition::Challenge {
            fingerprint,
            subject,
            issuer,
        } => {
            let challenge_id = session_id.get();
            runtime.publish_event(SessionEvent::ServerIdentityChallenge(identity_challenge(
                session_id,
                challenge_id,
                endpoint,
                fingerprint,
                subject,
                issuer,
            )))?;
            wait_for_identity_decision(session_id, challenge_id, fingerprint, || {
                runtime.try_next_command()
            })
        }
    }
}

fn sanitize_certificate_name(value: &str) -> String {
    let mut sanitized = String::new();
    let mut pending_space = false;

    for character in value.chars() {
        if get_general_category(character) == GeneralCategory::Format {
            continue;
        }
        if character.is_control() || character.is_whitespace() {
            pending_space = !sanitized.is_empty();
            continue;
        }
        let separator_bytes = usize::from(pending_space);
        if sanitized.len() + separator_bytes + character.len_utf8() > MAX_CERTIFICATE_NAME_BYTES {
            break;
        }
        if pending_space {
            sanitized.push(' ');
            pending_space = false;
        }
        sanitized.push(character);
    }

    if sanitized.is_empty() {
        UNKNOWN_CERTIFICATE_NAME.to_owned()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use frd_core::SessionId;
    use frd_protocol_api::{
        Endpoint, ProtocolId, ServerIdentityDecision, ServerIdentityValidationKind, SessionCommand,
    };
    use rustls::CertificateError;

    use super::{
        classify_identity, fingerprint_sha256, identity_challenge, wait_for_identity_decision,
        AcceptedServerIdentity, IdentityDisposition, PlatformValidationFailure,
        SanitizedCertificateNames,
    };
    use crate::tls::{credential_free_preflight, establish_verified_tls};

    const FINGERPRINT: [u8; 32] = [0x31; 32];

    #[test]
    fn server_identity_fingerprint_hashes_the_complete_leaf_der() {
        assert_eq!(
            fingerprint_sha256(b"abc"),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }

    #[test]
    fn server_identity_system_trust_continues_without_a_challenge() {
        assert_eq!(
            classify_identity(None, FINGERPRINT, Ok(()), names()),
            IdentityDisposition::SystemTrusted {
                fingerprint: FINGERPRINT
            }
        );
    }

    #[test]
    fn server_identity_exact_saved_pin_wins_before_platform_failure() {
        assert_eq!(
            classify_identity(
                Some(FINGERPRINT),
                FINGERPRINT,
                Err(platform_failure(CertificateError::UnknownIssuer)),
                names(),
            ),
            IdentityDisposition::PinMatched {
                fingerprint: FINGERPRINT
            }
        );
    }

    #[test]
    fn server_identity_unknown_self_signed_certificate_requires_a_challenge() {
        assert_eq!(
            classify_identity(
                None,
                FINGERPRINT,
                Err(platform_failure(CertificateError::UnknownIssuer)),
                names(),
            ),
            IdentityDisposition::Challenge {
                fingerprint: FINGERPRINT,
                subject: "CN=rdp.test".to_owned(),
                issuer: "CN=rdp.test".to_owned(),
            }
        );
    }

    #[test]
    fn server_identity_non_issuer_failures_reject_even_an_exact_saved_pin() {
        for error in [
            CertificateError::NotValidForName,
            CertificateError::Expired,
            CertificateError::NotValidYet,
            CertificateError::InvalidPurpose,
            CertificateError::BadEncoding,
        ] {
            assert_eq!(
                classify_identity(
                    Some(FINGERPRINT),
                    FINGERPRINT,
                    Err(platform_failure(error)),
                    names(),
                ),
                IdentityDisposition::Reject
            );
        }
    }

    #[test]
    fn server_identity_saved_pin_mismatch_fails_even_when_system_trusted() {
        assert_eq!(
            classify_identity(Some([0x32; 32]), FINGERPRINT, Ok(()), names()),
            IdentityDisposition::PinMismatch
        );
    }

    #[test]
    fn server_identity_challenge_uses_the_protocol_neutral_contract() {
        let session_id = SessionId::allocate();
        let endpoint = Endpoint::new("rdp.test", 3389).expect("valid endpoint");
        let challenge = identity_challenge(
            session_id,
            17,
            endpoint.clone(),
            FINGERPRINT,
            "CN=rdp.test".to_owned(),
            "CN=issuer.test".to_owned(),
        );

        assert_eq!(challenge.session_id, session_id);
        assert_eq!(challenge.challenge_id, 17);
        assert_eq!(challenge.protocol_id, ProtocolId::rdp());
        assert_eq!(challenge.endpoint, endpoint);
        assert_eq!(challenge.sha256_fingerprint, FINGERPRINT);
        assert_eq!(challenge.subject, "CN=rdp.test");
        assert_eq!(challenge.issuer, "CN=issuer.test");
        assert_eq!(
            challenge.validation.kind(),
            ServerIdentityValidationKind::Unknown
        );
    }

    #[test]
    fn server_identity_reject_returns_a_stable_adapter_error() {
        let session_id = SessionId::allocate();
        let mut commands =
            VecDeque::from([resolution(session_id, 17, ServerIdentityDecision::Reject)]);

        let error =
            wait_for_identity_decision(session_id, 17, FINGERPRINT, || commands.pop_front())
                .expect_err("Reject must fail closed");

        assert_eq!(error.protocol_id(), Some(&ProtocolId::rdp()));
        assert_eq!(error.code(), "rdp_server_identity_rejected");
    }

    #[test]
    fn server_identity_stale_session_and_challenge_decisions_are_ignored() {
        let session_id = SessionId::allocate();
        let stale_session_id = SessionId::allocate();
        let mut commands = VecDeque::from([
            resolution(stale_session_id, 17, ServerIdentityDecision::TrustOnce),
            resolution(session_id, 16, ServerIdentityDecision::TrustOnce),
            resolution(session_id, 17, ServerIdentityDecision::TrustAndRemember),
        ]);

        assert_eq!(
            wait_for_identity_decision(session_id, 17, FINGERPRINT, || commands.pop_front()),
            Ok(Some(AcceptedServerIdentity::ExactPin {
                fingerprint: FINGERPRINT
            }))
        );
        assert!(commands.is_empty());
    }

    #[test]
    fn server_identity_disconnect_stops_waiting_without_approving_the_leaf() {
        let session_id = SessionId::allocate();
        let mut commands = VecDeque::from([SessionCommand::Disconnect]);

        assert_eq!(
            wait_for_identity_decision(session_id, 17, FINGERPRINT, || commands.pop_front()),
            Ok(None)
        );
    }

    #[test]
    fn server_identity_certificate_names_are_bounded_and_sanitized() {
        let untrusted = format!("  CN={}\r\nOU=Remote\u{0007} Desktop  ", "远".repeat(200));
        let names = SanitizedCertificateNames::new(&untrusted, "\n\t");

        assert!(!names.subject.is_empty());
        assert!(names.subject.len() <= 256);
        assert_eq!(names.subject.trim(), names.subject);
        assert!(!names.subject.chars().any(char::is_control));
        assert_eq!(names.issuer, "未知");
    }

    #[test]
    fn server_identity_certificate_names_strip_bidi_and_invisible_format_characters() {
        let names = SanitizedCertificateNames::new(
            "CN=\u{202e}evil\u{2066}\u{200b}peer\u{feff}",
            "CN=issuer.test",
        );

        assert_eq!(names.subject, "CN=evilpeer");
    }

    #[test]
    fn server_identity_certificate_names_format_only_fall_back_to_unknown() {
        let names = SanitizedCertificateNames::new("\u{202e}\u{2066}\u{200b}\u{feff}", "\u{200b}");

        assert_eq!(names.subject, "未知");
        assert_eq!(names.issuer, "未知");
    }

    #[test]
    fn server_identity_certificate_names_do_not_commit_a_boundary_separator() {
        let untrusted = format!("{} 远", "a".repeat(255));
        let names = SanitizedCertificateNames::new(&untrusted, "CN=issuer.test");

        assert_eq!(names.subject, "a".repeat(255));
        assert!(!names.subject.ends_with(' '));
    }

    #[test]
    fn server_identity_tls_preflight_writes_no_application_data_and_exact_pin_reconnects() {
        let (address, server, certificate_der) = spawn_tls_server(2, false);
        let endpoint = Endpoint::new("localhost", address.port()).expect("valid endpoint");

        let observed = credential_free_preflight(
            TcpStream::connect(address).expect("connect preflight stream"),
            &endpoint,
        )
        .expect("credential-free preflight completes");
        assert_eq!(observed.fingerprint, fingerprint_sha256(&certificate_der));
        assert!(observed.platform_validation.is_err());
        assert!(!observed.names.subject.is_empty());
        assert!(observed.names.subject.len() <= 256);
        assert!(!observed.names.subject.chars().any(char::is_control));
        assert!(!observed.names.issuer.is_empty());

        let transport = establish_verified_tls(
            TcpStream::connect(address).expect("connect verified stream"),
            &endpoint,
            &AcceptedServerIdentity::ExactPin {
                fingerprint: observed.fingerprint,
            },
        )
        .expect("approved exact pin establishes TLS");
        drop(transport);

        assert_eq!(
            server.join().expect("TLS test server exits"),
            vec![0, 0],
            "both TLS handshakes must carry zero application bytes"
        );
    }

    #[test]
    fn server_identity_exact_pin_leaf_change_during_verified_reconnect_is_identity_changed() {
        let (address, server, certificate_der) = spawn_tls_server(1, true);
        let endpoint = Endpoint::new("localhost", address.port()).expect("valid endpoint");
        let mut expected_fingerprint = fingerprint_sha256(&certificate_der);
        expected_fingerprint[0] ^= 0xff;

        let error = match establish_verified_tls(
            TcpStream::connect(address).expect("connect verified stream"),
            &endpoint,
            &AcceptedServerIdentity::ExactPin {
                fingerprint: expected_fingerprint,
            },
        ) {
            Ok(_) => panic!("a changed reconnect leaf must fail closed"),
            Err(error) => error,
        };

        assert_eq!(error.code(), "rdp_server_identity_changed");
        assert_eq!(server.join().expect("TLS test server exits"), vec![0]);
    }

    fn names() -> SanitizedCertificateNames {
        SanitizedCertificateNames::new("CN=rdp.test", "CN=rdp.test")
    }

    fn platform_failure(error: CertificateError) -> PlatformValidationFailure {
        PlatformValidationFailure::from_rustls(rustls::Error::InvalidCertificate(error))
    }

    fn resolution(
        session_id: SessionId,
        challenge_id: u64,
        decision: ServerIdentityDecision,
    ) -> SessionCommand {
        SessionCommand::ResolveServerIdentity {
            session_id,
            challenge_id,
            decision,
        }
    }

    fn spawn_tls_server(
        connections: usize,
        allow_rejected_handshake: bool,
    ) -> (
        std::net::SocketAddr,
        thread::JoinHandle<Vec<usize>>,
        Vec<u8>,
    ) {
        use rcgen::{generate_simple_self_signed, CertifiedKey};
        use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};

        let CertifiedKey { cert, key_pair } =
            generate_simple_self_signed(vec!["localhost".to_owned()])
                .expect("generate runtime-only test certificate");
        let certificate_der = cert.der().to_vec();
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(certificate_der.clone())],
                PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into(),
            )
            .expect("valid server TLS config");
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind TLS test server");
        let address = listener.local_addr().expect("TLS test server address");
        let server = thread::spawn(move || {
            let config = Arc::new(server_config);
            let mut application_bytes = Vec::with_capacity(connections);
            for _ in 0..connections {
                let (mut tcp, _) = listener.accept().expect("accept TLS test client");
                tcp.set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("set TLS test read timeout");
                let mut connection =
                    rustls::ServerConnection::new(config.clone()).expect("server connection");
                if let Err(error) = connection.complete_io(&mut tcp) {
                    if allow_rejected_handshake {
                        application_bytes.push(0);
                        continue;
                    }
                    panic!("complete server TLS handshake: {error}");
                }
                let mut tls = rustls::StreamOwned::new(connection, tcp);
                let mut application = [0_u8; 1];
                let bytes = match tls.read(&mut application) {
                    Ok(bytes) => bytes,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                                | std::io::ErrorKind::UnexpectedEof
                        ) =>
                    {
                        0
                    }
                    Err(error) => panic!("read TLS application data: {error}"),
                };
                application_bytes.push(bytes);
            }
            application_bytes
        });

        (address, server, certificate_der)
    }
}
