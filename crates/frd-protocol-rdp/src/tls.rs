use std::net::TcpStream;
use std::sync::{Arc, Mutex};

use frd_protocol_api::{Endpoint, ProtocolError};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::{ClientConfig, ClientConnection, Resumption};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme, StreamOwned};
use rustls_platform_verifier::{ConfigVerifierExt, Verifier};
use x509_cert::der::Decode as _;
use x509_cert::Certificate;

use crate::error::{rdp_error, RDP_TLS_FAILED};
use crate::server_identity::{
    fingerprint_sha256, AcceptedServerIdentity, ObservedServerIdentity, PlatformValidationFailure,
    SanitizedCertificateNames,
};

type TlsStream = StreamOwned<ClientConnection, TcpStream>;

#[allow(dead_code)] // Task 3 consumes the stream and CredSSP public key.
pub(crate) struct VerifiedTlsTransport {
    stream: TlsStream,
    server_public_key: Vec<u8>,
}

impl VerifiedTlsTransport {
    #[allow(dead_code)] // Task 3 consumes this seam.
    pub(crate) fn into_parts(self) -> (TlsStream, Vec<u8>) {
        (self.stream, self.server_public_key)
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
        let platform_validation = self
            .platform
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            .map(|_| ())
            .map_err(|_| PlatformValidationFailure);
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
    let config = match accepted_identity {
        AcceptedServerIdentity::SystemTrusted { .. } => platform_client_config()?,
        AcceptedServerIdentity::ExactPin { fingerprint } => {
            let provider = configured_crypto_provider();
            let mut config = ClientConfig::builder_with_provider(provider.clone())
                .with_safe_default_protocol_versions()
                .map_err(|_| tls_error())?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(ExactPinVerifier {
                    fingerprint: *fingerprint,
                    provider,
                }))
                .with_no_client_auth();
            config.resumption = Resumption::disabled();
            config.enable_early_data = false;
            config
        }
    };
    let tls = complete_handshake(stream, endpoint, config)?;
    let leaf = tls
        .conn
        .peer_certificates()
        .and_then(|certificates| certificates.first())
        .ok_or_else(tls_error)?;
    let server_public_key = extract_server_public_key(leaf.as_ref())?;
    Ok(VerifiedTlsTransport {
        stream: tls,
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
