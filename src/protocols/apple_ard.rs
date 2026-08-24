use std::net::ToSocketAddrs;
use std::time::Duration;

use crossbeam_channel::Receiver;
use secrecy::ExposeSecret;

use crate::app::connection::ProtocolKind;
use crate::protocols::ProtocolAdapter;
use crate::session::{
    ProtocolContext, SessionCommand, SessionError, SessionEvent, SessionEventSink,
};
use crate::vnc::client::{SecurityPolicy, VncClient};
use crate::vnc::media_negotiation::AudioMediaFlow;
use crate::vnc::session::SessionEncodingProfile;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
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
        let connection = context.into_connection();
        if connection.protocol != ProtocolKind::AppleRfb {
            return Err(SessionError::new("apple_protocol_mismatch"));
        }
        let address = (connection.endpoint.host(), connection.endpoint.port())
            .to_socket_addrs()
            .map_err(|_| SessionError::new("endpoint_resolution_failed"))?
            .next()
            .ok_or_else(|| SessionError::new("endpoint_resolution_empty"))?;
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

        #[cfg(feature = "viewer")]
        {
            crate::vnc::hpss_viewer::run_protocol_session(
                client.conn,
                DISPLAY_NAME,
                client.width,
                client.height,
                true,
                AudioMediaFlow::MacToPc,
                commands,
                events,
            )
            .map_err(|_| SessionError::new("apple_hpss_session_failed"))
        }
        #[cfg(not(feature = "viewer"))]
        {
            let _ = (client, commands, events);
            Err(SessionError::new("apple_hpss_feature_unavailable"))
        }
    }
}
