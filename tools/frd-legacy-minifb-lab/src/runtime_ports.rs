use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use frd_core::{InputEvent, PointerSample, SessionId, SessionInput};
use frd_frame::FrameMailbox;
use frd_media_api::{MediaFrame, MediaPublishError, MediaPublisher};
use frd_protocol_api::{
    MailboxSurfacePublisher, ProtocolError, ProtocolRuntime, RuntimeEventSink, RuntimeWake,
    SessionCommand, SessionEvent,
};

const FRAME_MAILBOX_ENTRIES: usize = 64;
const FRAME_MAILBOX_PIXEL_BYTES: usize = 256 * 1024 * 1024;
const MEDIA_QUEUE_ENTRIES: usize = 16;

struct EventChannel(mpsc::Sender<SessionEvent>);

impl RuntimeEventSink for EventChannel {
    fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError> {
        self.0.send(event).map_err(|_| ProtocolError::Terminal)
    }
}

struct NoopWake;

impl RuntimeWake for NoopWake {
    fn wake(&self) -> Result<(), ProtocolError> {
        Ok(())
    }
}

struct MediaChannel(SyncSender<MediaFrame>);

impl MediaPublisher for MediaChannel {
    fn publish(&self, frame: MediaFrame) -> Result<(), MediaPublishError> {
        self.0.try_send(frame).map_err(|error| match error {
            TrySendError::Full(_) => MediaPublishError::Full,
            TrySendError::Disconnected(_) => MediaPublishError::Closed,
        })
    }
}

pub struct LabRuntimePorts {
    pub runtime: ProtocolRuntime,
    pub commands: mpsc::Sender<SessionCommand>,
    pub events: Receiver<SessionEvent>,
    pub media: Receiver<MediaFrame>,
    pub mailbox: Arc<Mutex<FrameMailbox>>,
}

pub fn create_runtime_ports(session_id: SessionId) -> LabRuntimePorts {
    let mailbox = Arc::new(Mutex::new(FrameMailbox::new(
        FRAME_MAILBOX_ENTRIES,
        FRAME_MAILBOX_PIXEL_BYTES,
    )));
    let (commands, command_receiver) = mpsc::channel();
    let (event_sender, events) = mpsc::channel();
    let (media_sender, media) = mpsc::sync_channel(MEDIA_QUEUE_ENTRIES);
    let runtime = ProtocolRuntime::new(
        session_id,
        command_receiver,
        Box::new(EventChannel(event_sender)),
        Box::new(MailboxSurfacePublisher::new(mailbox.clone())),
        Some(Box::new(MediaChannel(media_sender))),
        Box::new(NoopWake),
    );
    LabRuntimePorts {
        runtime,
        commands,
        events,
        media,
        mailbox,
    }
}

pub fn send_input(
    commands: &mpsc::Sender<SessionCommand>,
    session_id: SessionId,
    generation: u64,
    event: InputEvent,
) -> Result<()> {
    commands
        .send(SessionCommand::Input(SessionInput {
            session_id,
            generation,
            event,
        }))
        .context("Apple adapter input channel 已关闭")
}

pub fn send_pointer_sample(
    commands: &mpsc::Sender<SessionCommand>,
    session_id: SessionId,
    generation: u64,
    sample: PointerSample,
) -> Result<()> {
    send_input(
        commands,
        session_id,
        generation,
        InputEvent::PointerSample(sample),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use frd_core::{InputEvent, PixelPoint, PointerButtons, PointerSample, SessionId, WheelDelta};
    use frd_protocol_api::SessionCommand;

    use super::send_pointer_sample;

    #[test]
    fn one_legacy_pointer_sample_emits_one_atomic_protocol_command() {
        let (commands, received) = mpsc::channel();
        let session_id = SessionId::allocate();
        let sample = PointerSample::new(
            PixelPoint { x: 12, y: 34 },
            PointerButtons {
                primary: true,
                secondary: true,
                ..Default::default()
            },
            WheelDelta {
                horizontal: -1,
                ..Default::default()
            },
        );

        send_pointer_sample(&commands, session_id, 1, sample).unwrap();

        assert!(matches!(
            received.recv().unwrap(),
            SessionCommand::Input(frd_core::SessionInput {
                session_id: id,
                generation: 1,
                event: InputEvent::PointerSample(observed),
            }) if id == session_id && observed == sample
        ));
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
}
