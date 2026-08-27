//! 协议注册、会话及运行时端口的协议中立边界。

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use frd_core::{PhysicalViewport, SecretBytes, SessionId, SessionInput};
use frd_frame::{FrameCompleteness, FrameMailbox, PixelFormat, SurfaceUpdate};
use frd_media_api::{MediaFrame, MediaPublishError, MediaPublisher};

pub use frd_core::{Endpoint, ProtocolId, TargetSystem};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    UnsupportedTargetProtocol,
    UnregisteredProtocol,
    FactoryDescriptorMismatch,
    InvalidGeneration,
    EventPortClosed,
    FramePortRejected,
    WakeFailed,
    MediaPortClosed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionCapabilities {
    pub dynamic_resolution: bool,
    pub clipboard_read: bool,
    pub clipboard_write: bool,
    pub remote_audio: bool,
    pub text_input: bool,
}

impl SessionCapabilities {
    pub fn intersection(self, other: Self) -> Self {
        Self {
            dynamic_resolution: self.dynamic_resolution && other.dynamic_resolution,
            clipboard_read: self.clipboard_read && other.clipboard_read,
            clipboard_write: self.clipboard_write && other.clipboard_write,
            remote_audio: self.remote_audio && other.remote_audio,
            text_input: self.text_input && other.text_input,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolDescriptor {
    pub id: ProtocolId,
    pub display_name: String,
    pub default_port: u16,
}

impl From<ProtocolId> for ProtocolDescriptor {
    fn from(id: ProtocolId) -> Self {
        Self {
            display_name: id.as_str().to_owned(),
            default_port: default_port_for(&id),
            id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolSelection {
    Automatic,
    Explicit(ProtocolId),
}

pub struct ProtocolCatalog {
    descriptors: Vec<ProtocolDescriptor>,
}

impl ProtocolCatalog {
    pub fn new(ids: impl IntoIterator<Item = ProtocolId>) -> Self {
        Self {
            descriptors: ids.into_iter().map(ProtocolDescriptor::from).collect(),
        }
    }

    pub fn select(
        &self,
        target: TargetSystem,
        selection: ProtocolSelection,
    ) -> Result<ProtocolId, ProtocolError> {
        let protocol_id = match selection {
            ProtocolSelection::Automatic => {
                default_protocol_for(target).ok_or(ProtocolError::UnsupportedTargetProtocol)?
            }
            ProtocolSelection::Explicit(protocol_id) => protocol_id,
        };
        if !target_permits_protocol(target, &protocol_id) {
            return Err(ProtocolError::UnsupportedTargetProtocol);
        }
        self.descriptors
            .iter()
            .any(|descriptor| descriptor.id == protocol_id)
            .then_some(protocol_id)
            .ok_or(ProtocolError::UnregisteredProtocol)
    }
}

fn default_protocol_for(target: TargetSystem) -> Option<ProtocolId> {
    match target {
        TargetSystem::MacOs => Some(ProtocolId::apple_hpss_mvs()),
        TargetSystem::Windows => Some(ProtocolId::rdp()),
        TargetSystem::Linux => Some(ProtocolId::rfb()),
        TargetSystem::Custom => None,
    }
}

fn target_permits_protocol(target: TargetSystem, protocol_id: &ProtocolId) -> bool {
    match target {
        TargetSystem::MacOs => *protocol_id == ProtocolId::apple_hpss_mvs(),
        TargetSystem::Windows => *protocol_id == ProtocolId::rdp(),
        TargetSystem::Linux => *protocol_id == ProtocolId::rfb(),
        TargetSystem::Custom => true,
    }
}

fn default_port_for(protocol_id: &ProtocolId) -> u16 {
    if *protocol_id == ProtocolId::rdp() {
        3389
    } else {
        5900
    }
}

pub struct Credentials {
    pub username: String,
    pub password: SecretBytes,
}

pub struct ConnectRequest {
    pub session_id: SessionId,
    pub endpoint: Endpoint,
    pub protocol_id: ProtocolId,
    pub credentials: Option<Credentials>,
    /// 仅由 app 在启动 worker 前加载的一条 endpoint/protocol 精确 pin 快照。
    pub saved_server_pin: Option<[u8; 32]>,
}

pub trait ProtocolFactory: Send + Sync {
    fn descriptor(&self) -> ProtocolDescriptor;

    /// 仅构造会话；禁止连接、阻塞或启动线程。
    fn create(
        &self,
        request: ConnectRequest,
        runtime: ProtocolRuntime,
    ) -> Result<Box<dyn ProtocolSession>, ProtocolError>;
}

pub trait ProtocolSession: Send {
    /// coordinator 启动后，所有阻塞协议工作只在此处执行。
    fn run(self: Box<Self>) -> ProtocolExit;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolExit {
    Closed,
    Failed(ProtocolError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionStage {
    Connecting,
    TransportReady,
    AwaitingIdentityDecision,
    Disconnecting,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerIdentityValidation {
    Unknown,
    TrustedBySystem,
    PinMatched,
    PinMismatch,
}

impl ServerIdentityValidation {
    pub fn allows_credential_continuation(&self) -> bool {
        matches!(self, Self::TrustedBySystem | Self::PinMatched)
    }
}

pub fn evaluate_server_identity(
    saved_pin: Option<[u8; 32]>,
    presented_pin: [u8; 32],
) -> ServerIdentityValidation {
    match saved_pin {
        Some(pin) if pin == presented_pin => ServerIdentityValidation::PinMatched,
        Some(_) => ServerIdentityValidation::PinMismatch,
        None => ServerIdentityValidation::Unknown,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerIdentityChallenge {
    pub session_id: SessionId,
    pub challenge_id: u64,
    pub protocol_id: ProtocolId,
    pub endpoint: Endpoint,
    pub sha256_fingerprint: [u8; 32],
    pub subject: String,
    pub issuer: String,
    pub validation: ServerIdentityValidation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerIdentityDecision {
    TrustOnce,
    TrustAndRemember,
    Reject,
}

#[derive(Debug)]
pub struct ClipboardPayload(Box<[u8]>);

impl ClipboardPayload {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes.into_boxed_slice())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

pub enum SessionCommand {
    Input(SessionInput),
    ViewportChanged {
        session_id: SessionId,
        generation: u64,
        viewport: PhysicalViewport,
    },
    ResolveServerIdentity {
        session_id: SessionId,
        challenge_id: u64,
        decision: ServerIdentityDecision,
    },
    ClipboardWrite(ClipboardPayload),
    Disconnect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionEvent {
    StageChanged(ConnectionStage),
    ServerIdentityChallenge(ServerIdentityChallenge),
    SurfaceGenerationChanged {
        session_id: SessionId,
        generation: u64,
        size: frd_core::PixelSize,
    },
    CapabilitiesChanged(SessionCapabilities),
    Closed(ProtocolExit),
    Error(ProtocolError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PresentationEvent {
    FramePresented {
        session_id: SessionId,
        generation: u64,
        revision: u64,
        completeness: FrameCompleteness,
    },
}

pub trait RuntimeEventSink: Send {
    fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError>;
}

pub trait SurfacePublisher: Send {
    fn publish(&self, update: SurfaceUpdate) -> Result<(), ProtocolError>;
}

pub trait RuntimeWake: Send {
    fn wake(&self) -> Result<(), ProtocolError>;
}

pub struct MailboxSurfacePublisher {
    mailbox: Arc<Mutex<FrameMailbox>>,
}

impl MailboxSurfacePublisher {
    pub fn new(mailbox: Arc<Mutex<FrameMailbox>>) -> Self {
        Self { mailbox }
    }
}

impl SurfacePublisher for MailboxSurfacePublisher {
    fn publish(&self, update: SurfaceUpdate) -> Result<(), ProtocolError> {
        match self
            .mailbox
            .lock()
            .map_err(|_| ProtocolError::FramePortRejected)?
            .push(update)
        {
            frd_frame::PushOutcome::Queued => Ok(()),
            frd_frame::PushOutcome::Rejected | frd_frame::PushOutcome::NeedsFullSnapshot => {
                Err(ProtocolError::FramePortRejected)
            }
        }
    }
}

pub struct ProtocolRuntime {
    session_id: SessionId,
    current_generation: Option<u64>,
    commands: Receiver<SessionCommand>,
    events: Box<dyn RuntimeEventSink>,
    frames: Box<dyn SurfacePublisher>,
    media: Option<Box<dyn MediaPublisher>>,
    wake: Box<dyn RuntimeWake>,
}

impl ProtocolRuntime {
    pub fn new(
        session_id: SessionId,
        commands: Receiver<SessionCommand>,
        events: Box<dyn RuntimeEventSink>,
        frames: Box<dyn SurfacePublisher>,
        media: Option<Box<dyn MediaPublisher>>,
        wake: Box<dyn RuntimeWake>,
    ) -> Self {
        Self {
            session_id,
            current_generation: None,
            commands,
            events,
            frames,
            media,
            wake,
        }
    }

    pub fn with_ports(
        session_id: SessionId,
        events: Box<dyn RuntimeEventSink>,
        frames: Box<dyn SurfacePublisher>,
        wake: Box<dyn RuntimeWake>,
    ) -> Self {
        let (_, commands) = mpsc::channel();
        Self::new(session_id, commands, events, frames, None, wake)
    }

    /// 按 event、Reset、单次 wake 的顺序发布新的 generation。
    /// 任一步失败都会返回错误且不会报告成功；随后各层只可安全地销毁此会话。
    pub fn begin_generation(
        &mut self,
        session_id: SessionId,
        generation: u64,
        size: frd_core::PixelSize,
        format: PixelFormat,
    ) -> Result<(), ProtocolError> {
        if session_id != self.session_id
            || generation == 0
            || self
                .current_generation
                .is_some_and(|current| generation <= current)
        {
            return Err(ProtocolError::InvalidGeneration);
        }
        self.events
            .publish(SessionEvent::SurfaceGenerationChanged {
                session_id,
                generation,
                size,
            })?;
        self.frames.publish(SurfaceUpdate::Reset {
            session_id,
            generation,
            size,
            format,
        })?;
        self.wake.wake()?;
        self.current_generation = Some(generation);
        Ok(())
    }

    /// writer 端仅可看到本会话、当前 generation 的输入；旧命令直接丢弃。
    pub fn try_next_command(&mut self) -> Option<SessionCommand> {
        loop {
            match self.commands.try_recv() {
                Ok(SessionCommand::Input(input))
                    if input.session_id != self.session_id
                        || self.current_generation != Some(input.generation) => {}
                Ok(command) => return Some(command),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return None,
            }
        }
    }

    pub fn publish_media(&self, frame: MediaFrame) -> Result<(), ProtocolError> {
        let Some(media) = &self.media else {
            return Err(ProtocolError::MediaPortClosed);
        };
        media.publish(frame).map_err(|error| match error {
            MediaPublishError::Closed | MediaPublishError::Full => ProtocolError::MediaPortClosed,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use frd_core::{PixelSize, SessionId};
    use frd_frame::{PixelFormat, SurfaceUpdate};

    use super::{
        evaluate_server_identity, ProtocolCatalog, ProtocolError, ProtocolId, ProtocolRuntime,
        ProtocolSelection, RuntimeEventSink, RuntimeWake, ServerIdentityValidation, SessionEvent,
        SurfacePublisher, TargetSystem,
    };

    #[test]
    fn automatic_mac_selection_resolves_to_the_only_apple_protocol() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);

        assert_eq!(
            catalog.select(TargetSystem::MacOs, ProtocolSelection::Automatic),
            Ok(ProtocolId::apple_hpss_mvs())
        );
    }

    #[test]
    fn pin_mismatch_fails_closed_before_credential_continuation() {
        let validation = evaluate_server_identity(Some([0x11; 32]), [0x22; 32]);

        assert_eq!(validation, ServerIdentityValidation::PinMismatch);
        assert!(!validation.allows_credential_continuation());
    }

    #[test]
    fn generation_publication_orders_event_reset_and_one_wake() {
        let session_id = SessionId::allocate();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::with_ports(
            session_id,
            Box::new(RecordingEvents(log.clone())),
            Box::new(RecordingFrames(log.clone())),
            Box::new(RecordingWake(log.clone())),
        );

        runtime
            .begin_generation(
                session_id,
                1,
                PixelSize::new(800, 600).expect("valid size"),
                PixelFormat::Bgrx8UnormSrgb,
            )
            .expect("paired publication succeeds");

        assert_eq!(
            *log.lock().expect("log lock"),
            vec!["event", "reset", "wake"]
        );
    }

    struct RecordingEvents(Arc<Mutex<Vec<&'static str>>>);

    impl RuntimeEventSink for RecordingEvents {
        fn publish(&self, _: SessionEvent) -> Result<(), ProtocolError> {
            self.0.lock().expect("log lock").push("event");
            Ok(())
        }
    }

    struct RecordingFrames(Arc<Mutex<Vec<&'static str>>>);

    impl SurfacePublisher for RecordingFrames {
        fn publish(&self, update: SurfaceUpdate) -> Result<(), ProtocolError> {
            assert!(matches!(update, SurfaceUpdate::Reset { .. }));
            self.0.lock().expect("log lock").push("reset");
            Ok(())
        }
    }

    struct RecordingWake(Arc<Mutex<Vec<&'static str>>>);

    impl RuntimeWake for RecordingWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            self.0.lock().expect("log lock").push("wake");
            Ok(())
        }
    }
}
