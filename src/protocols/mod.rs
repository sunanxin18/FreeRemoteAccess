use crossbeam_channel::Receiver;

#[cfg(feature = "cli")]
pub mod apple_ard;
pub mod auto;
#[cfg(feature = "rdp")]
pub mod rdp;
#[cfg(feature = "cli")]
pub mod rfb;

use crate::app::connection::ProtocolKind;
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
