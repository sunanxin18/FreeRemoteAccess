use crossbeam_channel::Receiver;

#[cfg(feature = "cli")]
pub mod apple_ard;
#[cfg(feature = "cli")]
pub mod rfb;

use crate::app::connection::ProtocolKind;
use crate::session::{ProtocolContext, SessionCommand, SessionError, SessionEventSink};

pub trait ProtocolAdapter: Send + 'static {
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
        ProtocolKind::Rdp => Err(SessionError::new("rdp_adapter_not_available")),
        ProtocolKind::Auto => Err(SessionError::new("protocol_selection_incomplete")),
        #[cfg(not(feature = "cli"))]
        ProtocolKind::AppleRfb | ProtocolKind::StandardRfb => {
            Err(SessionError::new("rfb_adapter_not_available"))
        }
    }
}
