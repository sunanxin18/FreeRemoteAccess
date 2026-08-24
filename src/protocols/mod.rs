use crossbeam_channel::Receiver;
#[cfg(feature = "cli")]
use std::net::{SocketAddr, ToSocketAddrs};

#[cfg(feature = "cli")]
pub mod apple_ard;
pub mod auto;
#[cfg(feature = "rdp")]
pub mod rdp;
#[cfg(feature = "cli")]
pub mod rfb;

use crate::app::connection::ProtocolKind;
#[cfg(feature = "cli")]
use crate::app::connection::ValidatedConnection;
use crate::session::{ProtocolContext, SessionCommand, SessionError, SessionEventSink};

pub trait ProtocolAdapter: Send + 'static {
    /// `run` 返回前必须停止并丢弃全部 `SessionEventSink` 克隆，避免会话完成后继续生产事件。
    fn run(
        self: Box<Self>,
        context: ProtocolContext,
        commands: Receiver<SessionCommand>,
        events: SessionEventSink,
    ) -> Result<(), SessionError>;
}

#[cfg(feature = "cli")]
pub(crate) fn resolve_connection_endpoint(
    connection: &ValidatedConnection,
) -> Result<SocketAddr, SessionError> {
    if let Some(address) = connection.endpoint.pinned_addr() {
        return Ok(address);
    }
    (connection.endpoint.host(), connection.endpoint.port())
        .to_socket_addrs()
        .map_err(|_| SessionError::new("endpoint_resolution_failed"))?
        .next()
        .ok_or_else(|| SessionError::new("endpoint_resolution_empty"))
}

pub fn adapter_for(protocol: ProtocolKind) -> Result<Box<dyn ProtocolAdapter>, SessionError> {
    match protocol {
        #[cfg(feature = "cli")]
        ProtocolKind::AppleRfb => Ok(Box::new(apple_ard::AppleArdAdapter::new())),
        #[cfg(feature = "cli")]
        ProtocolKind::StandardRfb => Ok(Box::new(rfb::RfbAdapter::standard())),
        #[cfg(feature = "rdp")]
        ProtocolKind::Rdp => Ok(Box::new(rdp::RdpAdapter::new())),
        #[cfg(not(feature = "rdp"))]
        ProtocolKind::Rdp => Err(SessionError::new("rdp_adapter_not_available")),
        ProtocolKind::Auto => Ok(Box::new(auto::AutoAdapter::new())),
        #[cfg(not(feature = "cli"))]
        ProtocolKind::AppleRfb | ProtocolKind::StandardRfb => {
            Err(SessionError::new("rfb_adapter_not_available"))
        }
    }
}
