use crossbeam_channel::Receiver;

use crate::protocols::ProtocolAdapter;
use crate::session::{ProtocolContext, SessionCommand, SessionError, SessionEventSink};

use super::rfb::RfbAdapter;

pub struct AppleArdAdapter;

impl AppleArdAdapter {
    pub const fn new() -> Self {
        Self
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
        Box::new(RfbAdapter::apple_native()).run(context, commands, events)
    }
}
