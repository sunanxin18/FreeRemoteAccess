use std::io::{Read, Write};
use std::mem;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use frd_core::{DisplayConstraints, DisplayIntent, DisplayPlanner, PixelSize, SessionId};
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
use ironrdp::pdu::rdp::capability_sets::client_codecs_capabilities;
use ironrdp::pdu::rdp::client_info::{PerformanceFlags, TimezoneInfo};
use ironrdp_blocking::{Framed, ShouldUpgrade};

use crate::audio::{new_rdpsnd, RdpAudioAdapter};
use crate::clipboard::new_cliprdr;
use crate::config::{RdpClientPlatformIdentity, RdpConnectionConfig};
use crate::display::DisplayControlCapabilityState;
use crate::egfx::{EgfxAdapter, EgfxDecoderProvider, EgfxSurfacePublisher};
use crate::error::{
    rdp_error, RDP_ACTIVATION_FAILED, RDP_DNS_FAILED, RDP_LICENSE_FAILED, RDP_LOGON_FAILED,
    RDP_NLA_FAILED, RDP_TCP_FAILED, RDP_TLS_FAILED,
};
use crate::factory::{RdpEgfxDiagnostics, RdpGraphicsAdvertisementGate, RdpGraphicsCapabilities};
use crate::runtime::{
    wait_for_blocking, wait_for_network_future, CancellationCheckedIo, StageCancellation,
};
use crate::tls::{credential_free_preflight, establish_verified_tls, VerifiedTlsTransport};
use crate::upstream::client_platform_type;

const DEFAULT_DESKTOP_SIZE: DesktopSize = DesktopSize {
    width: 1280,
    height: 720,
};

const RDP_INITIAL_DISPLAY_CONSTRAINTS: DisplayConstraints = DisplayConstraints {
    max_width: Some(u16::MAX as u32),
    max_height: Some(u16::MAX as u32),
    max_area: None,
    max_texture_dimension: None,
    frame_budget_bytes: Some(64 * 1024 * 1024),
    bytes_per_pixel: 4,
};

/// Non-secret graphics capability state for one RDP session.
///
/// The legacy baseline is deliberately the only advertised graphics path until
/// an EGFX adapter and a matching decoder have been registered.  The fields are
/// kept separate so a later server confirmation cannot be mistaken for a client
/// decoder capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RdpGraphicsCapability {
    pub(crate) legacy_bitmap: bool,
    pub(crate) remotefx: bool,
    pub(crate) egfx_advertised: bool,
    pub(crate) egfx_diagnostics: RdpEgfxDiagnostics,
    pub(crate) egfx_confirmed: bool,
    pub(crate) egfx_frame_confirmed: bool,
    pub(crate) avc420: bool,
    pub(crate) avc444: bool,
}

impl RdpGraphicsCapability {
    pub(crate) const fn snapshot(self) -> RdpGraphicsCapabilities {
        RdpGraphicsCapabilities {
            legacy_bitmap: self.legacy_bitmap,
            remotefx: self.remotefx,
            egfx_advertised: self.egfx_advertised,
            egfx_diagnostics: self.egfx_diagnostics,
            egfx_confirmed: self.egfx_confirmed,
            egfx_frame_confirmed: self.egfx_frame_confirmed,
            avc420: self.avc420,
            avc444: self.avc444,
        }
    }
}

impl Default for RdpGraphicsCapability {
    fn default() -> Self {
        baseline_graphics_capabilities()
    }
}

/// Returns the current legacy-only graphics capability boundary.
pub(crate) const fn baseline_graphics_capabilities() -> RdpGraphicsCapability {
    RdpGraphicsCapability {
        legacy_bitmap: true,
        remotefx: true,
        egfx_advertised: false,
        egfx_diagnostics: RdpEgfxDiagnostics {
            avc420_decoded_pictures_total: 0,
            avc444_decoded_updates_total: 0,
            clearcodec_decoded_bitmaps_total: 0,
            progressive_decoded_updates_total: 0,
            progressive_failure_detail: None,
            frames_queued_total: 0,
            frames_runtime_accepted_total: 0,
            start_calls: 0,
            capability_messages_queued: 0,
            typed_confirmation_ever: false,
            confirmed_version: None,
            confirmed_flags: None,
            unhandled_codec_count: 0,
            last_unhandled_codec: None,
            failure_count: 0,
            first_failure: None,
        },
        egfx_confirmed: false,
        egfx_frame_confirmed: false,
        avc420: false,
        avc444: false,
    }
}

fn planned_desktop_size(intent: DisplayIntent) -> DesktopSize {
    let Some(size) = DisplayPlanner::plan(intent, RDP_INITIAL_DISPLAY_CONSTRAINTS)
        .ok()
        .and_then(|plan| plan.remote_size)
    else {
        return DEFAULT_DESKTOP_SIZE;
    };
    let (Ok(width), Ok(height)) = (u16::try_from(size.width), u16::try_from(size.height)) else {
        return DEFAULT_DESKTOP_SIZE;
    };
    DesktopSize { width, height }
}

pub(crate) struct ActivatedRdpSession {
    #[allow(dead_code)] // Task 4 consumes the negotiated connector result.
    pub(crate) connection: ConnectionResult,
    pub(crate) transport: VerifiedTlsTransport,
    pub(crate) audio: RdpAudioAdapter,
    pub(crate) display_capabilities: DisplayControlCapabilityState,
    pub(crate) graphics_capability: RdpGraphicsCapability,
}

struct NegotiatedTcp {
    connector: ClientConnector,
    transport: TcpStream,
    upgrade: ShouldUpgrade,
    audio: RdpAudioAdapter,
    display_capabilities: DisplayControlCapabilityState,
    graphics_capability: RdpGraphicsCapability,
}

pub(crate) async fn connect_and_activate(
    config: &mut RdpConnectionConfig,
    runtime: &mut ProtocolRuntime,
    egfx_decoder_provider: Option<Arc<dyn EgfxDecoderProvider>>,
    graphics_advertisement_gate: RdpGraphicsAdvertisementGate,
) -> Result<ActivatedRdpSession, ProtocolError> {
    runtime.publish_event(SessionEvent::StageChanged(ConnectionStage::Connecting))?;

    let endpoint = config.request.endpoint.clone();
    let addresses = resolve_addresses(&endpoint, runtime).await?;
    let desktop_size = planned_desktop_size(config.request.display_intent);

    let NegotiatedTcp {
        connector: _,
        transport: preflight_transport,
        upgrade: _,
        audio: _,
        display_capabilities: _,
        graphics_capability: _,
    } = negotiate_enhanced_security(
        &addresses,
        config.client_platform,
        desktop_size,
        config.request.session_id,
        egfx_decoder_provider.clone(),
        graphics_advertisement_gate,
        runtime,
    )
    .await?;
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

    let mut negotiated = negotiate_enhanced_security(
        &addresses,
        config.client_platform,
        desktop_size,
        config.request.session_id,
        egfx_decoder_provider,
        graphics_advertisement_gate,
        runtime,
    )
    .await?;
    let audio = negotiated.audio.clone();
    let display_capabilities = negotiated.display_capabilities.clone();
    let graphics_capability = negotiated.graphics_capability;
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
        display_capabilities,
        graphics_capability,
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
    client_platform: RdpClientPlatformIdentity,
    desktop_size: DesktopSize,
    session_id: SessionId,
    egfx_decoder_provider: Option<Arc<dyn EgfxDecoderProvider>>,
    graphics_advertisement_gate: RdpGraphicsAdvertisementGate,
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
        let egfx_decoder_provider = egfx_decoder_provider.clone();
        let negotiated = wait_for_blocking(runtime, shutdown, RDP_TLS_FAILED, move |_| {
            let mut framed = Framed::new(stream);
            let (mut connector, audio, display_capabilities, graphics_capability) =
                baseline_connector(
                    Credentials::SmartCard {
                        pin: String::new(),
                        config: None,
                    },
                    None,
                    desktop_size,
                    client_addr,
                    client_platform,
                    session_id,
                    egfx_decoder_provider.as_ref(),
                    graphics_advertisement_gate,
                );
            let upgrade = ironrdp_blocking::connect_begin(&mut framed, &mut connector)?;
            Ok::<_, ConnectorError>(NegotiatedTcp {
                connector,
                transport: framed.into_inner_no_leftover(),
                upgrade,
                audio,
                display_capabilities,
                graphics_capability,
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
    client_platform: RdpClientPlatformIdentity,
    session_id: SessionId,
    egfx_decoder_provider: Option<&Arc<dyn EgfxDecoderProvider>>,
    graphics_advertisement_gate: RdpGraphicsAdvertisementGate,
) -> (
    ClientConnector,
    RdpAudioAdapter,
    DisplayControlCapabilityState,
    RdpGraphicsCapability,
) {
    let audio = RdpAudioAdapter::new();
    let display_capabilities = DisplayControlCapabilityState::default();
    let observed_display_capabilities = display_capabilities.clone();
    let egfx = egfx_decoder_provider
        .filter(|_| {
            matches!(
                graphics_advertisement_gate,
                RdpGraphicsAdvertisementGate::LiveInteroperable
            )
        })
        .and_then(|provider| {
            let coded_size = PixelSize::new(
                u32::from(desktop_size.width),
                u32::from(desktop_size.height),
            )?;
            let decoder = provider.create_decoder(session_id, coded_size)?;
            let publisher =
                EgfxSurfacePublisher::try_new(session_id, 1)?.with_expected_coded_size(coded_size);
            let publisher = match provider.create_avc444_decoder(session_id, coded_size) {
                Some(decoder) => publisher.with_avc444_decoder(decoder),
                None => publisher,
            };
            Some(EgfxAdapter::with_surface_publisher(
                Some(decoder),
                publisher,
            ))
        });
    let egfx_advertised = egfx.is_some();
    let graphics_capability = if egfx_advertised {
        RdpGraphicsCapability {
            legacy_bitmap: true,
            remotefx: true,
            egfx_advertised: true,
            egfx_diagnostics: RdpEgfxDiagnostics::default(),
            egfx_confirmed: false,
            egfx_frame_confirmed: false,
            // The provider proves that the client can advertise AVC420.  The
            // negotiated codec stays false until the server's
            // CapabilitiesConfirm is observed by the active-session loop.
            avc420: false,
            avc444: false,
        }
    } else {
        baseline_graphics_capabilities()
    };
    let mut dynamic_channels =
        DrdynvcClient::new().with_dynamic_channel(DisplayControlClient::new(move |capabilities| {
            observed_display_capabilities.record(&capabilities);
            Ok(Vec::new())
        }));
    if let Some(egfx) = egfx {
        dynamic_channels = dynamic_channels.with_dynamic_channel(egfx);
    }
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
            platform: client_platform_type(client_platform),
            enable_server_pointer: true,
            request_data: None,
            autologon: false,
            enable_audio_playback: true,
            compression_type: None,
            pointer_software_rendering: false,
            multitransport_flags: None,
            support_dynvc_gfx_protocol: egfx_advertised,
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
    (connector, audio, display_capabilities, graphics_capability)
}

#[cfg(test)]
fn test_dynamic_channels_with_egfx(
    display_capabilities: DisplayControlCapabilityState,
) -> DrdynvcClient {
    let observed_display_capabilities = display_capabilities;
    DrdynvcClient::new()
        .with_dynamic_channel(DisplayControlClient::new(move |capabilities| {
            observed_display_capabilities.record(&capabilities);
            Ok(Vec::new())
        }))
        .with_dynamic_channel(EgfxAdapter::new(
            None,
            Box::new(crate::egfx::NoopEgfxHandler),
        ))
}

#[cfg(test)]
mod tests {
    use ironrdp::connector::Sequence;
    use ironrdp::connector::{ClientConnector, Credentials, DesktopSize};
    use ironrdp::core::{decode, encode_vec, WriteBuf};
    use ironrdp::dvc::pdu::{CreateRequestPdu, DrdynvcServerPdu};
    use ironrdp::dvc::DrdynvcClient;
    use ironrdp::pdu::gcc::ClientEarlyCapabilityFlags;
    use ironrdp::pdu::nego::{self, ResponseFlags, SecurityProtocol};
    use ironrdp::pdu::rdp::capability_sets::{
        CodecId, CodecProperty, MajorPlatformType, RemoteFxContainer, CODEC_ID_REMOTEFX,
    };
    use ironrdp::pdu::x224::{X224Data, X224};
    use ironrdp::svc::SvcProcessor;

    use super::{
        baseline_connector, baseline_graphics_capabilities, planned_desktop_size,
        test_dynamic_channels_with_egfx, RdpGraphicsCapability,
    };
    use crate::config::RdpClientPlatformIdentity;
    use crate::display::DisplayControlCapabilityState;
    use crate::egfx::EgfxDecoderProvider;
    use crate::factory::RdpGraphicsAdvertisementGate;
    use crate::upstream::client_platform_type;
    use frd_core::{DisplayIntent, PixelSize, SessionId};
    use ironrdp_egfx::decode::{DecodedFrame, DecoderError, DecoderResult, H264Decoder};

    #[test]
    fn initial_desktop_size_uses_native_physical_pixels_above_2560() {
        let geometry =
            frd_core::DisplayGeometry::from_physical(PixelSize::new(3840, 2160).unwrap(), 1000)
                .unwrap();
        assert_eq!(
            planned_desktop_size(DisplayIntent::native_display(geometry)),
            DesktopSize {
                width: 3840,
                height: 2160,
            }
        );
    }

    #[test]
    fn initial_desktop_size_accepts_five_k_when_surface_budget_allows_it() {
        assert_eq!(
            planned_desktop_size(DisplayIntent::fixed(PixelSize::new(5120, 2880).unwrap())),
            DesktopSize {
                width: 5120,
                height: 2880,
            }
        );
    }

    #[test]
    fn server_managed_keeps_legacy_safe_fallback() {
        assert_eq!(
            planned_desktop_size(DisplayIntent::default()),
            DesktopSize {
                width: 1280,
                height: 720,
            }
        );
    }

    #[test]
    fn baseline_rdp_capabilities_do_not_advertise_h264() {
        let capabilities = baseline_graphics_capabilities();
        assert_eq!(RdpGraphicsCapability::default(), capabilities);
        assert!(capabilities.legacy_bitmap);
        assert!(capabilities.remotefx);
        assert!(!capabilities.egfx_advertised);
        assert!(!capabilities.egfx_confirmed);
        assert!(!capabilities.avc420);
        assert!(!capabilities.avc444);
    }

    #[test]
    fn egfx_registration_is_separate_from_display_control() {
        let mut channels =
            test_dynamic_channels_with_egfx(DisplayControlCapabilityState::default());

        for (channel_id, channel_name) in [
            (7, "Microsoft::Windows::RDS::DisplayControl"),
            (8, "Microsoft::Windows::RDS::Graphics"),
        ] {
            let request = DrdynvcServerPdu::Create(CreateRequestPdu::new(
                channel_id,
                channel_name.to_owned(),
            ));
            let payload = encode_vec(&request).expect("encode DVC create request");
            channels
                .process(&payload)
                .expect("registered DVC channel should accept create request");
            assert!(channels.get_dvc_by_channel_id(channel_id).is_some());
        }
    }

    #[test]
    fn injected_decoder_provider_registers_avc420_without_avc444() {
        let provider: std::sync::Arc<dyn EgfxDecoderProvider> =
            std::sync::Arc::new(StubEgfxDecoderProvider);
        let session_id = SessionId::allocate();
        let (mut connector, _audio, _display, graphics) = baseline_connector(
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
            RdpClientPlatformIdentity::Windows,
            session_id,
            Some(&provider),
            RdpGraphicsAdvertisementGate::LiveInteroperable,
        );

        assert!(graphics.egfx_advertised);
        assert!(!graphics.avc420);
        assert!(!graphics.egfx_confirmed);
        assert!(!graphics.avc444);
        assert!(
            connector.config.support_dynvc_gfx_protocol,
            "an attached EGFX decoder must opt into the early graphics-channel flag"
        );

        let channels = connector
            .get_static_channel_processor_mut::<DrdynvcClient>()
            .expect("dynamic virtual channel processor");
        let request = DrdynvcServerPdu::Create(CreateRequestPdu::new(
            9,
            "Microsoft::Windows::RDS::Graphics".to_owned(),
        ));
        let payload = encode_vec(&request).expect("encode DVC create request");
        channels
            .process(&payload)
            .expect("registered EGFX channel should accept create request");
        assert!(channels.get_dvc_by_channel_id(9).is_some());
    }

    #[test]
    fn decoder_provider_without_live_gate_keeps_legacy_wire_advertisement() {
        let provider: std::sync::Arc<dyn EgfxDecoderProvider> =
            std::sync::Arc::new(StubEgfxDecoderProvider);
        let session_id = SessionId::allocate();
        let (connector, _audio, _display, graphics) = baseline_connector(
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
            RdpClientPlatformIdentity::Windows,
            session_id,
            Some(&provider),
            RdpGraphicsAdvertisementGate::LegacyOnly,
        );

        assert_eq!(graphics, baseline_graphics_capabilities());
        assert!(!connector.config.support_dynvc_gfx_protocol);
    }

    #[test]
    fn egfx_provider_emits_the_graphics_channel_capability_on_the_wire() {
        let provider: std::sync::Arc<dyn EgfxDecoderProvider> =
            std::sync::Arc::new(StubEgfxDecoderProvider);
        let session_id = SessionId::allocate();
        let (connector, _audio, _display, _graphics) = baseline_connector(
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
            RdpClientPlatformIdentity::Windows,
            session_id,
            Some(&provider),
            RdpGraphicsAdvertisementGate::LiveInteroperable,
        );

        let flags = emitted_early_capability_flags(connector);

        assert!(flags.contains(ClientEarlyCapabilityFlags::SUPPORT_DYN_VC_GFX_PROTOCOL));
    }

    #[test]
    fn legacy_connector_omits_the_graphics_channel_capability_on_the_wire() {
        let session_id = SessionId::allocate();
        let (connector, _audio, _display, _graphics) = baseline_connector(
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
            RdpClientPlatformIdentity::Windows,
            session_id,
            None,
            RdpGraphicsAdvertisementGate::LegacyOnly,
        );

        let flags = emitted_early_capability_flags(connector);
        assert!(!flags.contains(ClientEarlyCapabilityFlags::SUPPORT_DYN_VC_GFX_PROTOCOL));
    }

    fn emitted_early_capability_flags(
        mut connector: ClientConnector,
    ) -> ClientEarlyCapabilityFlags {
        let mut output = WriteBuf::new();
        connector
            .step(&[], &mut output)
            .expect("emit connection request");
        let confirm = encode_vec(&X224(nego::ConnectionConfirm::Response {
            flags: ResponseFlags::empty(),
            protocol: SecurityProtocol::HYBRID | SecurityProtocol::HYBRID_EX,
        }))
        .expect("encode connection confirm");
        connector
            .step(&confirm, &mut WriteBuf::new())
            .expect("accept connection confirm");
        connector.mark_security_upgrade_as_done();
        connector.mark_credssp_as_done();

        let mut initial_output = WriteBuf::new();
        connector
            .step(&[], &mut initial_output)
            .expect("emit GCC connect initial");
        let x224 = decode::<X224<X224Data<'_>>>(initial_output.filled())
            .expect("decode emitted X.224 data");
        let initial = decode::<ironrdp::pdu::mcs::ConnectInitial>(x224.0.data.as_ref())
            .expect("decode emitted MCS connect initial");
        initial
            .conference_create_request
            .gcc_blocks()
            .core
            .optional_data
            .early_capability_flags
            .expect("client emits core early capability flags")
    }

    struct StubEgfxDecoderProvider;

    impl EgfxDecoderProvider for StubEgfxDecoderProvider {
        fn create_decoder(
            &self,
            _session_id: SessionId,
            _coded_size: PixelSize,
        ) -> Option<Box<dyn H264Decoder>> {
            Some(Box::new(StubEgfxDecoder))
        }
    }

    struct StubEgfxDecoder;

    impl H264Decoder for StubEgfxDecoder {
        fn decode(&mut self, _data: &[u8]) -> DecoderResult<DecodedFrame> {
            Err(DecoderError::msg("test decoder is intentionally inert"))
        }

        fn reset(&mut self) {}
    }

    #[test]
    fn approved_identities_map_to_exact_ironrdp_major_platform_types() {
        let cases = [
            (
                RdpClientPlatformIdentity::Windows,
                MajorPlatformType::WINDOWS,
            ),
            (
                RdpClientPlatformIdentity::Macintosh,
                MajorPlatformType::MACINTOSH,
            ),
            (RdpClientPlatformIdentity::Ios, MajorPlatformType::IOS),
            (RdpClientPlatformIdentity::Unix, MajorPlatformType::UNIX),
            (
                RdpClientPlatformIdentity::Android,
                MajorPlatformType::ANDROID,
            ),
        ];

        for (identity, expected) in cases {
            assert_eq!(client_platform_type(identity), expected);
        }
    }

    #[test]
    fn connector_requires_credssp_and_refuses_tls_only_downgrade() {
        let (connector, _audio, _display_capabilities, _graphics_capability) = baseline_connector(
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
            RdpClientPlatformIdentity::Windows,
            SessionId::allocate(),
            None,
            RdpGraphicsAdvertisementGate::LegacyOnly,
        );

        assert!(!connector.config.enable_tls);
        assert!(connector.config.enable_credssp);
        assert!(!connector.config.support_dynvc_gfx_protocol);
    }

    #[test]
    fn connector_offers_only_approved_optional_channels() {
        let (mut connector, _audio, _display_capabilities, _graphics_capability) =
            baseline_connector(
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
                RdpClientPlatformIdentity::Windows,
                SessionId::allocate(),
                None,
                RdpGraphicsAdvertisementGate::LegacyOnly,
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
