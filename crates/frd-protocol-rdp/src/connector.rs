use std::io::{Read, Write};
use std::mem;
use std::net::{SocketAddr, TcpStream};

use frd_protocol_api::{ConnectionStage, ProtocolError, ProtocolRuntime, SessionEvent};
use ironrdp::connector::credssp::CredsspSequence;
use ironrdp::connector::sspi::generator::GeneratorState;
use ironrdp::connector::{
    self, BitmapConfig, ClientConnector, ClientConnectorState, ConnectionResult, ConnectorError,
    ConnectorErrorKind, Credentials, DesktopSize, ServerName,
};
use ironrdp::displaycontrol::client::DisplayControlClient;
use ironrdp::dvc::DrdynvcClient;
use ironrdp::pdu::gcc::KeyboardType;
use ironrdp::pdu::rdp::capability_sets::{client_codecs_capabilities, MajorPlatformType};
use ironrdp::pdu::rdp::client_info::{PerformanceFlags, TimezoneInfo};
use ironrdp_blocking::{Framed, ShouldUpgrade};

use crate::audio::{new_rdpsnd, RdpAudioAdapter};
use crate::clipboard::new_cliprdr;
use crate::config::RdpConnectionConfig;
use crate::error::{
    rdp_error, RDP_ACTIVATION_FAILED, RDP_DNS_FAILED, RDP_LICENSE_FAILED, RDP_LOGON_FAILED,
    RDP_NLA_FAILED, RDP_TCP_FAILED, RDP_TLS_FAILED,
};
use crate::runtime::{
    wait_for_blocking, wait_for_network_future, CancellationCheckedIo, StageCancellation,
};
use crate::tls::{credential_free_preflight, establish_verified_tls, VerifiedTlsTransport};

const DEFAULT_DESKTOP_SIZE: DesktopSize = DesktopSize {
    width: 1280,
    height: 720,
};

pub(crate) struct ActivatedRdpSession {
    #[allow(dead_code)] // Task 4 consumes the negotiated connector result.
    pub(crate) connection: ConnectionResult,
    pub(crate) transport: VerifiedTlsTransport,
    pub(crate) audio: RdpAudioAdapter,
}

struct NegotiatedTcp {
    connector: ClientConnector,
    transport: TcpStream,
    upgrade: ShouldUpgrade,
    audio: RdpAudioAdapter,
}

pub(crate) async fn connect_and_activate(
    config: &mut RdpConnectionConfig,
    runtime: &mut ProtocolRuntime,
) -> Result<ActivatedRdpSession, ProtocolError> {
    runtime.publish_event(SessionEvent::StageChanged(ConnectionStage::Connecting))?;

    let endpoint = config.request.endpoint.clone();
    let addresses = resolve_addresses(&endpoint, runtime).await?;

    let NegotiatedTcp {
        connector: _,
        transport: preflight_transport,
        upgrade: _,
        audio: _,
    } = negotiate_enhanced_security(&addresses, runtime).await?;
    let preflight_shutdown = preflight_transport
        .try_clone()
        .map_err(|_| rdp_error(RDP_TCP_FAILED))?;
    let preflight_endpoint = endpoint.clone();
    let observed = wait_for_blocking(runtime, preflight_shutdown, RDP_TLS_FAILED, move |_| {
        credential_free_preflight(preflight_transport, &preflight_endpoint)
    })
    .await??;

    let accepted_identity = crate::server_identity::resolve_server_identity(
        endpoint.clone(),
        config.request.saved_server_pin,
        observed,
        config.request.session_id,
        runtime,
    )?
    .ok_or_else(|| rdp_error(crate::error::RDP_CANCELLED))?;

    let mut negotiated = negotiate_enhanced_security(&addresses, runtime).await?;
    let audio = negotiated.audio.clone();
    let verified_shutdown = negotiated
        .transport
        .try_clone()
        .map_err(|_| rdp_error(RDP_TCP_FAILED))?;
    let verified_endpoint = endpoint.clone();
    let verified = wait_for_blocking(runtime, verified_shutdown, RDP_TLS_FAILED, move |_| {
        establish_verified_tls(negotiated.transport, &verified_endpoint, &accepted_identity)
    })
    .await??;

    let parsed = config.take_connector_credentials()?;
    negotiated.connector.config.credentials = Credentials::UsernamePassword {
        username: parsed.username,
        password: parsed.password,
    };
    negotiated.connector.config.domain = parsed.domain;
    let _upgraded =
        ironrdp_blocking::mark_as_upgraded(negotiated.upgrade, &mut negotiated.connector);

    let (mut framed, server_public_key) = verified.into_parts();
    let credssp_shutdown = framed
        .get_inner()
        .0
        .sock
        .try_clone()
        .map_err(|_| rdp_error(RDP_TCP_FAILED))?;
    let server_name = ServerName::new(endpoint.host().to_owned());
    let credssp_key = server_public_key.clone();
    let (next_connector, next_framed, credssp_result) = wait_for_blocking(
        runtime,
        credssp_shutdown,
        RDP_NLA_FAILED,
        move |cancellation| {
            let mut connector = negotiated.connector;
            let (stream, leftover) = framed.into_inner();
            let mut guarded_framed = Framed::new_with_leftover(
                CancellationCheckedIo::new(stream, cancellation.clone()),
                leftover,
            );
            let result = perform_credssp(
                &mut connector,
                &mut guarded_framed,
                server_name,
                credssp_key,
                &cancellation,
            );
            let (stream, leftover) = guarded_framed.into_inner();
            let framed = Framed::new_with_leftover(stream.into_inner(), leftover);
            (connector, framed, result)
        },
    )
    .await?;
    let mut connector = next_connector;
    framed = next_framed;
    credssp_result.map_err(map_credssp_error)?;

    let connection = loop {
        if matches!(connector.state, ClientConnectorState::Connected { .. }) {
            match mem::take(&mut connector.state) {
                ClientConnectorState::Connected { result } => break result,
                _ => unreachable!("connected state was matched immediately before taking it"),
            }
        }

        let failure_code = connector_state_failure_code(&connector.state);
        let shutdown = framed
            .get_inner()
            .0
            .sock
            .try_clone()
            .map_err(|_| rdp_error(RDP_TCP_FAILED))?;
        let (next_connector, next_framed, step_result) =
            wait_for_blocking(runtime, shutdown, failure_code, move |cancellation| {
                let mut connector = connector;
                let (stream, leftover) = framed.into_inner();
                let mut guarded_framed = Framed::new_with_leftover(
                    CancellationCheckedIo::new(stream, cancellation.clone()),
                    leftover,
                );
                let mut scratch = Default::default();
                let result = ensure_stage_running(&cancellation).and_then(|()| {
                    ironrdp_blocking::single_sequence_step(
                        &mut guarded_framed,
                        &mut connector,
                        &mut scratch,
                    )
                });
                let result = result.and_then(|()| ensure_stage_running(&cancellation));
                let (stream, leftover) = guarded_framed.into_inner();
                let framed = Framed::new_with_leftover(stream.into_inner(), leftover);
                (connector, framed, result)
            })
            .await?;
        connector = next_connector;
        framed = next_framed;
        step_result.map_err(|_| rdp_error(failure_code))?;
    };

    let transport = VerifiedTlsTransport::from_parts(framed, server_public_key);
    runtime.publish_event(SessionEvent::StageChanged(ConnectionStage::TransportReady))?;

    Ok(ActivatedRdpSession {
        connection,
        transport,
        audio,
    })
}

async fn resolve_addresses(
    endpoint: &frd_protocol_api::Endpoint,
    runtime: &mut ProtocolRuntime,
) -> Result<Vec<SocketAddr>, ProtocolError> {
    let addresses = wait_for_network_future(
        runtime,
        tokio::net::lookup_host((endpoint.host().to_owned(), endpoint.port())),
        RDP_DNS_FAILED,
    )
    .await?
    .collect::<Vec<_>>();
    if addresses.is_empty() {
        Err(rdp_error(RDP_DNS_FAILED))
    } else {
        Ok(addresses)
    }
}

async fn negotiate_enhanced_security(
    addresses: &[SocketAddr],
    runtime: &mut ProtocolRuntime,
) -> Result<NegotiatedTcp, ProtocolError> {
    let mut last_error = rdp_error(RDP_TCP_FAILED);
    for address in addresses.iter().copied() {
        let stream = match wait_for_network_future(
            runtime,
            tokio::net::TcpStream::connect(address),
            RDP_TCP_FAILED,
        )
        .await
        {
            Ok(stream) => stream,
            Err(error) if error.code() == crate::error::RDP_CANCELLED => return Err(error),
            Err(error) => {
                last_error = error;
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let stream = stream.into_std().map_err(|_| rdp_error(RDP_TCP_FAILED))?;
        stream
            .set_nonblocking(false)
            .map_err(|_| rdp_error(RDP_TCP_FAILED))?;
        let client_addr = stream.local_addr().map_err(|_| rdp_error(RDP_TCP_FAILED))?;
        let shutdown = stream.try_clone().map_err(|_| rdp_error(RDP_TCP_FAILED))?;
        let negotiated = wait_for_blocking(runtime, shutdown, RDP_TLS_FAILED, move |_| {
            let mut framed = Framed::new(stream);
            let (mut connector, audio) = baseline_connector(
                Credentials::SmartCard {
                    pin: String::new(),
                    config: None,
                },
                None,
                DEFAULT_DESKTOP_SIZE,
                client_addr,
            );
            let upgrade = ironrdp_blocking::connect_begin(&mut framed, &mut connector)?;
            Ok::<_, ConnectorError>(NegotiatedTcp {
                connector,
                transport: framed.into_inner_no_leftover(),
                upgrade,
                audio,
            })
        })
        .await?;
        return negotiated.map_err(|_| rdp_error(RDP_TLS_FAILED));
    }
    Err(last_error)
}

fn perform_credssp<S>(
    connector: &mut ClientConnector,
    framed: &mut Framed<S>,
    server_name: ServerName,
    server_public_key: Vec<u8>,
    cancellation: &StageCancellation,
) -> Result<(), ConnectorError>
where
    S: Read + Write,
{
    ensure_stage_running(cancellation)?;
    let selected_protocol = match connector.state {
        ClientConnectorState::Credssp { selected_protocol } => selected_protocol,
        _ => {
            return Err(ConnectorError::new(
                "CredSSP state",
                ConnectorErrorKind::General,
            ));
        }
    };
    let (mut sequence, mut request) = CredsspSequence::init(
        connector.config.credentials.clone(),
        connector.config.domain.as_deref(),
        selected_protocol,
        server_name,
        server_public_key,
        None,
    )?;
    loop {
        ensure_stage_running(cancellation)?;
        let client_state = {
            let mut generator = sequence.process_ts_request(request);
            match generator.start() {
                GeneratorState::Completed(result) => result.map_err(|error| {
                    ConnectorError::new("CredSSP", ConnectorErrorKind::Credssp(error))
                })?,
                GeneratorState::Suspended(_) => {
                    return Err(ConnectorError::new(
                        "unexpected CredSSP network request",
                        ConnectorErrorKind::General,
                    ));
                }
            }
        };

        let mut scratch = Default::default();
        let written = sequence.handle_process_result(client_state, &mut scratch)?;
        if let Some(length) = written.size() {
            ensure_stage_running(cancellation)?;
            framed.write_all(&scratch[..length]).map_err(|error| {
                ConnectorError::new("CredSSP write", ConnectorErrorKind::Custom).with_source(error)
            })?;
        }

        let Some(hint) = sequence.next_pdu_hint() else {
            break;
        };
        ensure_stage_running(cancellation)?;
        let response = framed.read_by_hint(hint).map_err(|error| {
            ConnectorError::new("CredSSP read", ConnectorErrorKind::Custom).with_source(error)
        })?;
        ensure_stage_running(cancellation)?;
        if let Some(next_request) = sequence.decode_server_message(&response)? {
            request = next_request;
        } else {
            break;
        }
    }

    ensure_stage_running(cancellation)?;
    connector.mark_credssp_as_done();
    Ok(())
}

fn ensure_stage_running(cancellation: &StageCancellation) -> Result<(), ConnectorError> {
    if cancellation.is_cancelled() {
        Err(ConnectorError::new(
            "cancelled RDP stage",
            ConnectorErrorKind::General,
        ))
    } else {
        Ok(())
    }
}

fn map_credssp_error(error: ConnectorError) -> ProtocolError {
    match error.kind() {
        ConnectorErrorKind::AccessDenied => rdp_error(RDP_LOGON_FAILED),
        ConnectorErrorKind::Credssp(error)
            if matches!(
                error.error_type,
                ironrdp::connector::sspi::ErrorKind::LogonDenied
                    | ironrdp::connector::sspi::ErrorKind::UnknownCredentials
                    | ironrdp::connector::sspi::ErrorKind::NoCredentials
                    | ironrdp::connector::sspi::ErrorKind::IncompleteCredentials
            ) =>
        {
            rdp_error(RDP_LOGON_FAILED)
        }
        _ => rdp_error(RDP_NLA_FAILED),
    }
}

fn connector_state_failure_code(state: &ClientConnectorState) -> &'static str {
    match state {
        ClientConnectorState::LicensingExchange { .. } => RDP_LICENSE_FAILED,
        ClientConnectorState::SecureSettingsExchange { .. } => RDP_LOGON_FAILED,
        _ => RDP_ACTIVATION_FAILED,
    }
}

fn baseline_connector(
    credentials: Credentials,
    domain: Option<String>,
    desktop_size: DesktopSize,
    client_addr: SocketAddr,
) -> (ClientConnector, RdpAudioAdapter) {
    let audio = RdpAudioAdapter::new();
    let dynamic_channels = DrdynvcClient::new()
        .with_dynamic_channel(DisplayControlClient::new(|_capabilities| Ok(Vec::new())));
    let connector = ClientConnector::new(
        connector::Config {
            credentials,
            domain,
            enable_tls: false,
            enable_credssp: true,
            keyboard_type: KeyboardType::IbmEnhanced,
            keyboard_subtype: 0,
            keyboard_layout: 0,
            keyboard_functional_keys_count: 12,
            ime_file_name: String::new(),
            dig_product_id: String::new(),
            desktop_size,
            bitmap: Some(BitmapConfig {
                lossy_compression: false,
                color_depth: 32,
                codecs: client_codecs_capabilities(&[])
                    .expect("empty pinned codec configuration is infallible"),
            }),
            client_build: 0,
            client_name: "FreeRemoteDesk".to_owned(),
            client_dir: String::new(),
            platform: client_platform(),
            enable_server_pointer: true,
            request_data: None,
            autologon: false,
            enable_audio_playback: true,
            compression_type: None,
            pointer_software_rendering: false,
            multitransport_flags: None,
            performance_flags: PerformanceFlags::default(),
            desktop_scale_factor: 0,
            hardware_id: None,
            license_cache: None,
            timezone_info: TimezoneInfo::default(),
            alternate_shell: String::new(),
            work_dir: String::new(),
        },
        client_addr,
    )
    .with_static_channel(dynamic_channels)
    .with_static_channel(new_cliprdr())
    .with_static_channel(new_rdpsnd(&audio));
    (connector, audio)
}

#[cfg(windows)]
fn client_platform() -> MajorPlatformType {
    MajorPlatformType::WINDOWS
}

#[cfg(target_os = "macos")]
fn client_platform() -> MajorPlatformType {
    MajorPlatformType::MACINTOSH
}

#[cfg(target_os = "ios")]
fn client_platform() -> MajorPlatformType {
    MajorPlatformType::IOS
}

#[cfg(target_os = "linux")]
fn client_platform() -> MajorPlatformType {
    MajorPlatformType::UNIX
}

#[cfg(target_os = "android")]
fn client_platform() -> MajorPlatformType {
    MajorPlatformType::ANDROID
}

#[cfg(any(
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "openbsd",
    target_os = "netbsd"
))]
fn client_platform() -> MajorPlatformType {
    MajorPlatformType::UNIX
}

#[cfg(test)]
mod tests {
    use ironrdp::connector::{Credentials, DesktopSize};
    use ironrdp::dvc::DrdynvcClient;
    use ironrdp::pdu::rdp::capability_sets::{
        CodecId, CodecProperty, RemoteFxContainer, CODEC_ID_REMOTEFX,
    };

    use super::baseline_connector;

    #[test]
    fn connector_requires_credssp_and_refuses_tls_only_downgrade() {
        let (connector, _audio) = baseline_connector(
            Credentials::UsernamePassword {
                username: "alice".to_owned(),
                password: String::new(),
            },
            None,
            DesktopSize {
                width: 1280,
                height: 720,
            },
            "127.0.0.1:49152".parse().expect("valid client address"),
        );

        assert!(!connector.config.enable_tls);
        assert!(connector.config.enable_credssp);
    }

    #[test]
    fn connector_offers_only_approved_optional_channels() {
        let (mut connector, _audio) = baseline_connector(
            Credentials::UsernamePassword {
                username: "alice".to_owned(),
                password: String::new(),
            },
            None,
            DesktopSize {
                width: 1280,
                height: 720,
            },
            "127.0.0.1:49152".parse().expect("valid client address"),
        );
        let bitmap = connector
            .config
            .bitmap
            .as_ref()
            .expect("baseline requests an explicit bitmap depth");

        assert_eq!(connector.config.desktop_size.width, 1280);
        assert_eq!(connector.config.desktop_size.height, 720);
        assert_eq!(bitmap.color_depth, 32);
        assert_eq!(bitmap.codecs.0.len(), 1);
        assert_eq!(
            CodecId::from_u8(bitmap.codecs.0[0].id),
            Some(CODEC_ID_REMOTEFX)
        );
        assert!(matches!(
            bitmap.codecs.0[0].property,
            CodecProperty::RemoteFx(RemoteFxContainer::ClientContainer(_))
        ));
        assert!(connector.config.enable_server_pointer);
        assert!(!connector.config.pointer_software_rendering);
        assert!(connector.config.enable_audio_playback);
        assert!(connector.config.multitransport_flags.is_none());
        assert!(connector.config.license_cache.is_none());
        assert_eq!(connector.static_channels.iter().count(), 3);
        assert!(connector
            .get_static_channel_processor::<DrdynvcClient>()
            .is_some());
        assert!(connector
            .get_static_channel_processor::<ironrdp::cliprdr::CliprdrClient>()
            .is_some());
        assert!(connector
            .get_static_channel_processor::<ironrdp::rdpsnd::client::Rdpsnd>()
            .is_some());
    }
}
