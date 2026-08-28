use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use frd_protocol_api::{Endpoint, ProtocolError};
use ironrdp_blocking::Framed;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::{ClientConfig, ClientConnection, Resumption};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme, StreamOwned};
use rustls_platform_verifier::{ConfigVerifierExt, Verifier};
use x509_cert::der::Decode as _;
use x509_cert::ext::pkix::ExtendedKeyUsage;
use x509_cert::Certificate;

use crate::error::{rdp_error, RDP_SERVER_IDENTITY_CHANGED, RDP_TLS_FAILED};
use crate::server_identity::{
    fingerprint_sha256, AcceptedServerIdentity, ObservedServerIdentity, PlatformValidationFailure,
    SanitizedCertificateNames,
};

pub(crate) type TlsStream = StreamOwned<ClientConnection, TcpStream>;

#[allow(dead_code)] // Task 3 consumes the stream and CredSSP public key.
pub(crate) struct VerifiedTlsTransport {
    stream: Framed<TlsStream>,
    server_public_key: Vec<u8>,
}

impl VerifiedTlsTransport {
    #[allow(dead_code)] // Task 3 consumes this seam.
    pub(crate) fn into_parts(self) -> (Framed<TlsStream>, Vec<u8>) {
        (self.stream, self.server_public_key)
    }

    pub(crate) fn from_parts(stream: Framed<TlsStream>, server_public_key: Vec<u8>) -> Self {
        Self {
            stream,
            server_public_key,
        }
    }

    #[allow(dead_code)] // Task 5 replaces the provisional active-stage shutdown path.
    pub(crate) fn shutdown(&self) {
        let _ = self
            .stream
            .get_inner()
            .0
            .sock
            .shutdown(std::net::Shutdown::Both);
    }
}

#[derive(Debug)]
struct PreflightVerifier {
    platform: Verifier,
    provider: Arc<CryptoProvider>,
    observation: Arc<Mutex<Option<ObservedServerIdentity>>>,
}

impl ServerCertVerifier for PreflightVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let platform_validation = match self.platform.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(_) => Ok(()),
            Err(error) => {
                let failure = PlatformValidationFailure::from_rustls(error);
                if failure.is_unknown_issuer() {
                    match validate_untrusted_leaf_requirements(end_entity, server_name, now) {
                        Ok(()) => Err(failure),
                        Err(error) => Err(PlatformValidationFailure::from_rustls(error)),
                    }
                } else {
                    Err(failure)
                }
            }
        };
        let observed = ObservedServerIdentity {
            fingerprint: fingerprint_sha256(end_entity.as_ref()),
            platform_validation,
            names: certificate_names(end_entity.as_ref()),
        };
        *self.observation.lock().map_err(|_| {
            rustls::Error::General("TLS preflight capture unavailable".to_owned())
        })? = Some(observed);

        // This verifier exists only on a consumed preflight stream. The completed
        // transport is dropped and can never carry RDP or credential bytes.
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug)]
struct ExactPinVerifier {
    fingerprint: [u8; 32],
    provider: Arc<CryptoProvider>,
    mismatch: Arc<AtomicBool>,
}

impl ServerCertVerifier for ExactPinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if fingerprint_sha256(end_entity.as_ref()) != self.fingerprint {
            self.mismatch.store(true, Ordering::Release);
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub(crate) fn credential_free_preflight(
    stream: TcpStream,
    endpoint: &Endpoint,
) -> Result<ObservedServerIdentity, ProtocolError> {
    let observation = Arc::new(Mutex::new(None));
    let mut config = platform_client_config()?;
    let provider = config.crypto_provider().clone();
    let platform = Verifier::new(provider.clone()).map_err(|_| tls_error())?;
    config
        .dangerous()
        .set_certificate_verifier(Arc::new(PreflightVerifier {
            platform,
            provider,
            observation: observation.clone(),
        }));

    let preflight = complete_handshake(stream, endpoint, config)?;
    drop(preflight);
    let observed = observation
        .lock()
        .map_err(|_| tls_error())?
        .take()
        .ok_or_else(tls_error)?;
    Ok(observed)
}

pub(crate) fn establish_verified_tls(
    stream: TcpStream,
    endpoint: &Endpoint,
    accepted_identity: &AcceptedServerIdentity,
) -> Result<VerifiedTlsTransport, ProtocolError> {
    let (config, exact_pin_mismatch) = match accepted_identity {
        AcceptedServerIdentity::SystemTrusted { .. } => (platform_client_config()?, None),
        AcceptedServerIdentity::ExactPin { fingerprint } => {
            let provider = configured_crypto_provider();
            let mismatch = Arc::new(AtomicBool::new(false));
            let mut config = ClientConfig::builder_with_provider(provider.clone())
                .with_safe_default_protocol_versions()
                .map_err(|_| tls_error())?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(ExactPinVerifier {
                    fingerprint: *fingerprint,
                    provider,
                    mismatch: mismatch.clone(),
                }))
                .with_no_client_auth();
            config.resumption = Resumption::disabled();
            config.enable_early_data = false;
            (config, Some(mismatch))
        }
    };
    let tls = match complete_handshake(stream, endpoint, config) {
        Ok(tls) => tls,
        Err(_)
            if exact_pin_mismatch
                .as_ref()
                .is_some_and(|mismatch| mismatch.load(Ordering::Acquire)) =>
        {
            return Err(rdp_error(RDP_SERVER_IDENTITY_CHANGED));
        }
        Err(error) => return Err(error),
    };
    let leaf = tls
        .conn
        .peer_certificates()
        .and_then(|certificates| certificates.first())
        .ok_or_else(tls_error)?;
    let server_public_key = extract_server_public_key(leaf.as_ref())?;
    Ok(VerifiedTlsTransport {
        stream: Framed::new(tls),
        server_public_key,
    })
}

fn platform_client_config() -> Result<ClientConfig, ProtocolError> {
    let mut config = ClientConfig::with_platform_verifier().map_err(|_| tls_error())?;
    config.resumption = Resumption::disabled();
    config.enable_early_data = false;
    Ok(config)
}

fn configured_crypto_provider() -> Arc<CryptoProvider> {
    CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
}

fn complete_handshake(
    stream: TcpStream,
    endpoint: &Endpoint,
    config: ClientConfig,
) -> Result<TlsStream, ProtocolError> {
    let server_name = ServerName::try_from(endpoint.host().to_owned()).map_err(|_| tls_error())?;
    let connection =
        ClientConnection::new(Arc::new(config), server_name).map_err(|_| tls_error())?;
    let mut tls = StreamOwned::new(connection, stream);
    tls.conn
        .complete_io(&mut tls.sock)
        .map_err(|_| tls_error())?;
    Ok(tls)
}

fn certificate_names(leaf_der: &[u8]) -> SanitizedCertificateNames {
    match Certificate::from_der(leaf_der) {
        Ok(certificate) => SanitizedCertificateNames::new(
            &certificate.tbs_certificate.subject.to_string(),
            &certificate.tbs_certificate.issuer.to_string(),
        ),
        Err(_) => SanitizedCertificateNames::new("", ""),
    }
}

fn validate_untrusted_leaf_requirements(
    end_entity: &CertificateDer<'_>,
    server_name: &ServerName<'_>,
    now: UnixTime,
) -> Result<(), rustls::Error> {
    let parsed = rustls::server::ParsedCertificate::try_from(end_entity)?;
    rustls::client::verify_server_name(&parsed, server_name)?;

    let certificate = Certificate::from_der(end_entity.as_ref())
        .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
    let now = now.as_secs();
    if now
        < certificate
            .tbs_certificate
            .validity
            .not_before
            .to_unix_duration()
            .as_secs()
    {
        return Err(rustls::Error::InvalidCertificate(
            rustls::CertificateError::NotValidYet,
        ));
    }
    if now
        > certificate
            .tbs_certificate
            .validity
            .not_after
            .to_unix_duration()
            .as_secs()
    {
        return Err(rustls::Error::InvalidCertificate(
            rustls::CertificateError::Expired,
        ));
    }

    let extended_key_usage = certificate
        .tbs_certificate
        .get::<ExtendedKeyUsage>()
        .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
    if extended_key_usage.is_some_and(|(_, usages)| {
        !usages.0.iter().any(|usage| {
            matches!(
                usage.to_string().as_str(),
                "1.3.6.1.5.5.7.3.1" | "2.5.29.37.0"
            )
        })
    }) {
        return Err(rustls::Error::InvalidCertificate(
            rustls::CertificateError::InvalidPurpose,
        ));
    }

    Ok(())
}

fn extract_server_public_key(leaf_der: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    Certificate::from_der(leaf_der)
        .map_err(|_| tls_error())?
        .tbs_certificate
        .subject_public_key_info
        .subject_public_key
        .as_bytes()
        .map(ToOwned::to_owned)
        .ok_or_else(tls_error)
}

fn tls_error() -> ProtocolError {
    rdp_error(RDP_TLS_FAILED)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rcgen::{date_time_ymd, CertificateParams, ExtendedKeyUsagePurpose, KeyPair};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{CertificateError, Error};

    use super::validate_untrusted_leaf_requirements;

    const TEST_NOW: UnixTime = UnixTime::since_unix_epoch(Duration::from_secs(1_767_225_600));

    #[test]
    fn certificate_wrong_host_fixture_is_not_overridable() {
        let certificate = certificate_fixture(
            "other.test",
            (2020, 1, 1),
            (2030, 1, 1),
            vec![ExtendedKeyUsagePurpose::ServerAuth],
        );

        assert!(matches!(
            validate_untrusted_leaf_requirements(
                &certificate,
                &ServerName::try_from("rdp.test").expect("valid server name"),
                TEST_NOW,
            ),
            Err(Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. }
            ))
        ));
    }

    #[test]
    fn certificate_expired_fixture_is_not_overridable() {
        let certificate = certificate_fixture(
            "rdp.test",
            (2020, 1, 1),
            (2021, 1, 1),
            vec![ExtendedKeyUsagePurpose::ServerAuth],
        );

        assert!(matches!(
            validate_untrusted_leaf_requirements(
                &certificate,
                &ServerName::try_from("rdp.test").expect("valid server name"),
                TEST_NOW,
            ),
            Err(Error::InvalidCertificate(CertificateError::Expired))
        ));
    }

    #[test]
    fn certificate_not_yet_valid_fixture_is_not_overridable() {
        let certificate = certificate_fixture(
            "rdp.test",
            (2030, 1, 1),
            (2040, 1, 1),
            vec![ExtendedKeyUsagePurpose::ServerAuth],
        );

        assert!(matches!(
            validate_untrusted_leaf_requirements(
                &certificate,
                &ServerName::try_from("rdp.test").expect("valid server name"),
                TEST_NOW,
            ),
            Err(Error::InvalidCertificate(CertificateError::NotValidYet))
        ));
    }

    #[test]
    fn certificate_invalid_eku_fixture_is_not_overridable() {
        let certificate = certificate_fixture(
            "rdp.test",
            (2020, 1, 1),
            (2030, 1, 1),
            vec![ExtendedKeyUsagePurpose::ClientAuth],
        );

        assert!(matches!(
            validate_untrusted_leaf_requirements(
                &certificate,
                &ServerName::try_from("rdp.test").expect("valid server name"),
                TEST_NOW,
            ),
            Err(Error::InvalidCertificate(CertificateError::InvalidPurpose))
        ));
    }

    fn certificate_fixture(
        dns_name: &str,
        not_before: (i32, u8, u8),
        not_after: (i32, u8, u8),
        extended_key_usages: Vec<ExtendedKeyUsagePurpose>,
    ) -> CertificateDer<'static> {
        let mut params = CertificateParams::new(vec![dns_name.to_owned()])
            .expect("valid certificate parameters");
        params.not_before = date_time_ymd(not_before.0, not_before.1, not_before.2);
        params.not_after = date_time_ymd(not_after.0, not_after.1, not_after.2);
        params.extended_key_usages = extended_key_usages;
        let key = KeyPair::generate().expect("generate fixture key");
        CertificateDer::from(
            params
                .self_signed(&key)
                .expect("generate fixture certificate")
                .der()
                .to_vec(),
        )
    }
}
