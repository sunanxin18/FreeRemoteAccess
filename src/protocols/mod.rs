use crossbeam_channel::Receiver;

use crate::session::{ProtocolContext, SessionCommand, SessionError, SessionEventSink};

pub trait ProtocolAdapter: Send + 'static {
    fn run(
        self: Box<Self>,
        context: ProtocolContext,
        commands: Receiver<SessionCommand>,
        events: SessionEventSink,
    ) -> Result<(), SessionError>;
}
