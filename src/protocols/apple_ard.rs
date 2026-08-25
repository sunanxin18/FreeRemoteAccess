use std::time::Duration;

use crossbeam_channel::Receiver;
use secrecy::ExposeSecret;

use crate::app::connection::ProtocolKind;
use crate::protocols::{resolve_connection_endpoint, ProtocolAdapter};
use crate::session::{
    ProtocolContext, SessionCommand, SessionError, SessionEvent, SessionEventSink,
};
use crate::vnc::client::{SecurityPolicy, VncClient};
#[cfg(feature = "media")]
use crate::vnc::media_negotiation::AudioMediaFlow;
use crate::vnc::session::SessionEncodingProfile;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(feature = "media")]
const DISPLAY_NAME: &str = "FreeRemoteAccess 虚拟显示器";

pub struct AppleArdAdapter;

impl AppleArdAdapter {
    pub const fn new() -> Self {
        Self
    }

    pub const fn session_profile() -> SessionEncodingProfile {
        SessionEncodingProfile::AppleUdpMedia
    }
}

impl Default for AppleArdAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolAdapter for AppleArdAdapter {
    fn run(
        self: Box<Self>,
        context: ProtocolContext,
        commands: Receiver<SessionCommand>,
        events: SessionEventSink,
    ) -> Result<(), SessionError> {
        events.emit(SessionEvent::Connecting)?;
        let (connection, platform_services) = context.into_parts();
        if connection.protocol != ProtocolKind::AppleRfb {
            return Err(SessionError::new("apple_protocol_mismatch"));
        }
        let address = resolve_connection_endpoint(&connection)?;
        let client = VncClient::connect_timeout_with_policy(
            &address,
            CONNECT_TIMEOUT,
            Some(&connection.username),
            Some(connection.password.expose_secret()),
            Self::session_profile(),
            SecurityPolicy::AppleNativeOnly,
        )
        .map_err(|_| SessionError::new("apple_native_connect_failed"))?;
        if !client.conn.is_encrypted() {
            return Err(SessionError::new("apple_native_encryption_required"));
        }

        #[cfg(feature = "media")]
        {
            crate::vnc::hpss_session::run_protocol_session(
                client.conn,
                DISPLAY_NAME,
                client.width,
                client.height,
                true,
                AudioMediaFlow::MacToPc,
                platform_services,
                commands,
                events,
            )
            .map_err(|_| SessionError::new("apple_hpss_session_failed"))
        }
        #[cfg(not(feature = "media"))]
        {
            let _ = (client, platform_services, commands, events);
            Err(SessionError::new("apple_hpss_feature_unavailable"))
        }
    }
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;

    use super::*;
    use crate::app::connection::{validate_connection, ConnectionRequest, ServiceKind};

    #[test]
    fn auto_apple_connection_uses_the_pinned_probe_address() {
        let connection = validate_connection(ConnectionRequest {
            service: ServiceKind::Auto,
            host: "mac.example".to_owned(),
            port: None,
            username: "local-user".to_owned(),
            password: SecretString::from("secret".to_owned()),
            domain: None,
        })
        .unwrap()
        .select_auto_protocol(ProtocolKind::AppleRfb, "192.0.2.41:5900".parse().unwrap())
        .unwrap();

        assert_eq!(
            resolve_connection_endpoint(&connection).unwrap(),
            "192.0.2.41:5900".parse().unwrap()
        );
    }

    #[test]
    fn apple_protocol_delegates_audio_without_owning_platform_device_apis() {
        let adapter_source = include_str!("apple_ard.rs");
        let session_source = include_str!("../vnc/hpss_session.rs");

        for source in [adapter_source, session_source] {
            assert!(!source.contains(concat!("cpal", "::")));
            assert!(!source.contains(concat!("default_", "output_device")));
            assert!(!source.contains(concat!("AudioPlayback", "::open_default")));
        }
        assert!(adapter_source.contains("context.into_parts()"));
        assert!(adapter_source.contains("platform_services,"));
        assert!(session_source.contains("AudioOutputSpec::normalized()"));
    }
}
