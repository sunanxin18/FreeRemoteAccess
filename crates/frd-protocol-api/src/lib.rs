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
    Terminal,
    GenerationPublicationReserved,
    SurfaceResetReserved,
    StaleSession,
    StaleSurface,
    NeedsFullSnapshot,
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
            frd_frame::PushOutcome::Rejected => Err(ProtocolError::FramePortRejected),
            frd_frame::PushOutcome::NeedsFullSnapshot => Err(ProtocolError::NeedsFullSnapshot),
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
    terminal: bool,
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
            terminal: false,
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
    /// 任一端口失败后进入终态；不会伪称回滚已发布的 event/Reset。
    pub fn begin_generation(
        &mut self,
        session_id: SessionId,
        generation: u64,
        size: frd_core::PixelSize,
        format: PixelFormat,
    ) -> Result<(), ProtocolError> {
        if self.terminal {
            return Err(ProtocolError::Terminal);
        }
        if session_id != self.session_id
            || generation == 0
            || self
                .current_generation
                .is_some_and(|current| generation <= current)
        {
            return Err(ProtocolError::InvalidGeneration);
        }
        if let Err(error) = self.events.publish(SessionEvent::SurfaceGenerationChanged {
            session_id,
            generation,
            size,
        }) {
            self.poison();
            return Err(error);
        }
        if let Err(error) = self.frames.publish(SurfaceUpdate::Reset {
            session_id,
            generation,
            size,
            format,
        }) {
            self.poison();
            return Err(error);
        }
        if let Err(error) = self.wake.wake() {
            self.poison();
            return Err(error);
        }
        self.current_generation = Some(generation);
        Ok(())
    }

    /// `true` 时 `ProtocolSession::run` / coordinator 必须关闭并 join 该会话。
    pub fn requires_shutdown(&self) -> bool {
        self.terminal
    }

    /// 发布普通协议事件并只唤醒一次。generation 生命周期事件只能由
    /// `begin_generation` 成对发布，防止其绕过 Reset 配对。
    pub fn publish_event(&mut self, event: SessionEvent) -> Result<(), ProtocolError> {
        if self.terminal {
            return Err(ProtocolError::Terminal);
        }
        match &event {
            SessionEvent::SurfaceGenerationChanged { .. } => {
                return Err(ProtocolError::GenerationPublicationReserved);
            }
            SessionEvent::ServerIdentityChallenge(challenge)
                if challenge.session_id != self.session_id =>
            {
                return Err(ProtocolError::StaleSession);
            }
            _ => {}
        }
        if let Err(error) = self.events.publish(event) {
            self.poison();
            return Err(error);
        }
        if let Err(error) = self.wake.wake() {
            self.poison();
            return Err(error);
        }
        Ok(())
    }

    /// 发布当前 generation 的普通 frame traffic 并只唤醒一次。Reset 始终由
    /// `begin_generation` 独占，避免将新的 geometry 与生命周期事件拆开。
    pub fn publish_surface(&mut self, update: SurfaceUpdate) -> Result<(), ProtocolError> {
        if self.terminal {
            return Err(ProtocolError::Terminal);
        }
        match &update {
            SurfaceUpdate::Reset { .. } => return Err(ProtocolError::SurfaceResetReserved),
            SurfaceUpdate::Damage {
                session_id,
                generation,
                ..
            }
            | SurfaceUpdate::FrameBoundary {
                session_id,
                generation,
                ..
            } if *session_id != self.session_id || self.current_generation != Some(*generation) => {
                return Err(ProtocolError::StaleSurface);
            }
            SurfaceUpdate::Damage { .. } | SurfaceUpdate::FrameBoundary { .. } => {}
        }
        match self.frames.publish(update) {
            Ok(()) => {}
            Err(ProtocolError::NeedsFullSnapshot) => {
                return Err(ProtocolError::NeedsFullSnapshot);
            }
            Err(error) => {
                self.poison();
                return Err(error);
            }
        }
        if let Err(error) = self.wake.wake() {
            self.poison();
            return Err(error);
        }
        Ok(())
    }

    /// writer 端仅可看到本会话、当前 generation 的输入；旧命令直接丢弃。
    pub fn try_next_command(&mut self) -> Option<SessionCommand> {
        loop {
            match self.commands.try_recv() {
                Ok(SessionCommand::Disconnect) => return Some(SessionCommand::Disconnect),
                Ok(_) if self.terminal => {}
                Ok(SessionCommand::Input(input))
                    if input.session_id != self.session_id
                        || self.current_generation != Some(input.generation) => {}
                Ok(SessionCommand::ViewportChanged {
                    session_id,
                    generation,
                    ..
                }) if session_id != self.session_id
                    || self.current_generation != Some(generation) => {}
                Ok(SessionCommand::ResolveServerIdentity { session_id, .. })
                    if session_id != self.session_id => {}
                Ok(command) => return Some(command),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return None,
            }
        }
    }

    pub fn publish_media(&mut self, frame: MediaFrame) -> Result<(), ProtocolError> {
        if self.terminal {
            return Err(ProtocolError::Terminal);
        }
        let Some(media) = &self.media else {
            return Err(ProtocolError::MediaPortClosed);
        };
        let result = media.publish(frame).map_err(|error| match error {
            MediaPublishError::Closed | MediaPublishError::Full => ProtocolError::MediaPortClosed,
        });
        if result.is_err() {
            self.poison();
        }
        result
    }

    fn poison(&mut self) {
        self.terminal = true;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc, Mutex};

    use frd_core::{InputEvent, PhysicalViewport, PixelRect, PixelSize, SessionId, SessionInput};
    use frd_frame::{
        FrameCompleteness, FrameMailbox, PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate,
    };
    use frd_media_api::{MediaFrame, MediaPublishError, MediaPublisher};

    use super::{
        evaluate_server_identity, Endpoint, MailboxSurfacePublisher, ProtocolCatalog,
        ProtocolError, ProtocolId, ProtocolRuntime, ProtocolSelection, RuntimeEventSink,
        RuntimeWake, ServerIdentityValidation, SessionCommand, SessionEvent, SurfacePublisher,
        TargetSystem,
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

    #[test]
    fn adapter_runtime_publishes_identity_and_current_frames_with_one_wake_each() {
        let session_id = SessionId::allocate();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::with_ports(
            session_id,
            Box::new(AdapterEvents(log.clone())),
            Box::new(AdapterFrames(log.clone())),
            Box::new(RecordingWake(log.clone())),
        );
        establish_generation(&mut runtime, session_id, 1);

        runtime
            .publish_event(SessionEvent::ServerIdentityChallenge(identity_challenge(
                session_id,
            )))
            .expect("identity event is delivered");
        runtime
            .publish_surface(damage(session_id, 1, 1))
            .expect("current damage is delivered");
        runtime
            .publish_surface(SurfaceUpdate::FrameBoundary {
                session_id,
                generation: 1,
                revision: 1,
                completeness: FrameCompleteness::Incremental,
            })
            .expect("current frame boundary is delivered");

        assert_eq!(
            *log.lock().expect("log lock"),
            vec![
                "generation",
                "reset",
                "wake",
                "identity",
                "wake",
                "damage",
                "wake",
                "boundary",
                "wake",
            ]
        );
    }

    #[test]
    fn adapter_runtime_rejects_direct_generation_reset_and_stale_tagged_publications() {
        let session_id = SessionId::allocate();
        let stale_session = SessionId::allocate();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::with_ports(
            session_id,
            Box::new(AdapterEvents(log.clone())),
            Box::new(AdapterFrames(log.clone())),
            Box::new(RecordingWake(log.clone())),
        );
        establish_generation(&mut runtime, session_id, 1);

        assert_eq!(
            runtime.publish_event(SessionEvent::SurfaceGenerationChanged {
                session_id,
                generation: 2,
                size: PixelSize::new(800, 600).expect("valid size"),
            }),
            Err(ProtocolError::GenerationPublicationReserved)
        );
        assert_eq!(
            runtime.publish_surface(SurfaceUpdate::Reset {
                session_id,
                generation: 2,
                size: PixelSize::new(800, 600).expect("valid size"),
                format: PixelFormat::Bgrx8UnormSrgb,
            }),
            Err(ProtocolError::SurfaceResetReserved)
        );
        assert_eq!(
            runtime.publish_surface(damage(stale_session, 1, 1)),
            Err(ProtocolError::StaleSurface)
        );
        assert_eq!(
            runtime.publish_event(SessionEvent::ServerIdentityChallenge(identity_challenge(
                stale_session,
            ))),
            Err(ProtocolError::StaleSession)
        );
        assert_eq!(
            *log.lock().expect("log lock"),
            vec!["generation", "reset", "wake"]
        );
    }

    #[test]
    fn mailbox_full_snapshot_signal_is_recoverable_and_does_not_wake_or_poison_runtime() {
        let session_id = SessionId::allocate();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mailbox = Arc::new(Mutex::new(FrameMailbox::new(1, 1024)));
        let mut runtime = ProtocolRuntime::with_ports(
            session_id,
            Box::new(AdapterEvents(log.clone())),
            Box::new(MailboxSurfacePublisher::new(mailbox)),
            Box::new(RecordingWake(log.clone())),
        );
        establish_generation(&mut runtime, session_id, 1);

        assert_eq!(
            runtime.publish_surface(damage(session_id, 1, 1)),
            Err(ProtocolError::NeedsFullSnapshot)
        );
        assert!(!runtime.requires_shutdown());
        assert_eq!(*log.lock().expect("log lock"), vec!["generation", "wake"]);
    }

    #[test]
    fn runtime_drops_wrong_and_old_input_but_delivers_the_current_generation() {
        let session_id = SessionId::allocate();
        let wrong_session = SessionId::allocate();
        let (sender, receiver) = mpsc::channel();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            receiver,
            Box::new(RecordingEvents(log.clone())),
            Box::new(RecordingFrames(log.clone())),
            None,
            Box::new(RecordingWake(log)),
        );
        establish_generation(&mut runtime, session_id, 1);
        sender
            .send(input(wrong_session, 1))
            .expect("runtime receiver remains open");
        sender
            .send(input(session_id, 1))
            .expect("runtime receiver remains open");

        assert!(matches!(
            runtime.try_next_command(),
            Some(SessionCommand::Input(SessionInput { session_id: id, generation: 1, .. })) if id == session_id
        ));

        establish_generation(&mut runtime, session_id, 2);
        sender
            .send(input(session_id, 1))
            .expect("runtime receiver remains open");
        sender
            .send(input(session_id, 2))
            .expect("runtime receiver remains open");

        assert!(matches!(
            runtime.try_next_command(),
            Some(SessionCommand::Input(SessionInput { session_id: id, generation: 2, .. })) if id == session_id
        ));
    }

    #[test]
    fn runtime_drops_stale_viewport_and_wrong_identity_resolution_commands() {
        let session_id = SessionId::allocate();
        let wrong_session = SessionId::allocate();
        let (sender, receiver) = mpsc::channel();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            receiver,
            Box::new(RecordingEvents(log.clone())),
            Box::new(RecordingFrames(log.clone())),
            None,
            Box::new(RecordingWake(log)),
        );
        establish_generation(&mut runtime, session_id, 1);

        sender
            .send(viewport_command(wrong_session, 1))
            .expect("runtime receiver remains open");
        sender
            .send(viewport_command(session_id, 1))
            .expect("runtime receiver remains open");
        assert!(matches!(
            runtime.try_next_command(),
            Some(SessionCommand::ViewportChanged { session_id: id, generation: 1, .. }) if id == session_id
        ));

        sender
            .send(identity_resolution(wrong_session))
            .expect("runtime receiver remains open");
        sender
            .send(identity_resolution(session_id))
            .expect("runtime receiver remains open");
        assert!(matches!(
            runtime.try_next_command(),
            Some(SessionCommand::ResolveServerIdentity { session_id: id, .. }) if id == session_id
        ));

        establish_generation(&mut runtime, session_id, 2);
        sender
            .send(viewport_command(session_id, 1))
            .expect("runtime receiver remains open");
        sender
            .send(viewport_command(session_id, 2))
            .expect("runtime receiver remains open");
        assert!(matches!(
            runtime.try_next_command(),
            Some(SessionCommand::ViewportChanged { session_id: id, generation: 2, .. }) if id == session_id
        ));
    }

    #[test]
    fn poisoned_runtime_rejects_event_frame_and_media_without_calling_media_publisher() {
        let session_id = SessionId::allocate();
        let (_, receiver) = mpsc::channel();
        let fault_state = Arc::new(Mutex::new(FaultState::new(FailureAt::Wake)));
        let media = Arc::new(Mutex::new(0usize));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            receiver,
            Box::new(FaultEvents(fault_state.clone())),
            Box::new(FaultFrames(fault_state.clone())),
            Some(Box::new(RecordingMedia(media.clone()))),
            Box::new(FaultWake(fault_state)),
        );
        establish_generation(&mut runtime, session_id, 1);
        assert!(runtime
            .begin_generation(
                session_id,
                2,
                PixelSize::new(800, 600).expect("valid size"),
                PixelFormat::Bgrx8UnormSrgb,
            )
            .is_err());

        assert_eq!(
            runtime.publish_event(SessionEvent::StageChanged(
                super::ConnectionStage::Connecting
            )),
            Err(ProtocolError::Terminal)
        );
        assert_eq!(
            runtime.publish_surface(damage(session_id, 1, 2)),
            Err(ProtocolError::Terminal)
        );
        assert_eq!(
            runtime.publish_media(MediaFrame::EncodedVideo {
                timestamp_us: 1,
                bytes: vec![0x11].into_boxed_slice(),
            }),
            Err(ProtocolError::Terminal)
        );
        assert_eq!(*media.lock().expect("media calls"), 0);
    }

    #[test]
    fn media_port_failure_poison_runtime_before_returning() {
        let session_id = SessionId::allocate();
        let (_, receiver) = mpsc::channel();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            receiver,
            Box::new(RecordingEvents(log.clone())),
            Box::new(RecordingFrames(log.clone())),
            Some(Box::new(FailingMedia)),
            Box::new(RecordingWake(log)),
        );
        establish_generation(&mut runtime, session_id, 1);

        assert_eq!(
            runtime.publish_media(MediaFrame::EncodedVideo {
                timestamp_us: 1,
                bytes: vec![0x11].into_boxed_slice(),
            }),
            Err(ProtocolError::MediaPortClosed)
        );
        assert!(runtime.requires_shutdown());
    }

    #[test]
    fn publication_port_failure_is_terminal_without_retry_or_old_input_delivery() {
        for failure in [FailureAt::Event, FailureAt::Reset, FailureAt::Wake] {
            let session_id = SessionId::allocate();
            let (sender, receiver) = mpsc::channel();
            let state = Arc::new(Mutex::new(FaultState::new(failure)));
            let mut runtime = ProtocolRuntime::new(
                session_id,
                receiver,
                Box::new(FaultEvents(state.clone())),
                Box::new(FaultFrames(state.clone())),
                None,
                Box::new(FaultWake(state.clone())),
            );

            establish_generation(&mut runtime, session_id, 1);
            assert!(runtime
                .begin_generation(
                    session_id,
                    2,
                    PixelSize::new(800, 600).expect("valid size"),
                    PixelFormat::Bgrx8UnormSrgb,
                )
                .is_err());
            assert!(runtime.requires_shutdown());
            assert_eq!(
                state.lock().expect("fault state").attempts,
                failure.expected_attempts()
            );

            sender
                .send(input(session_id, 1))
                .expect("runtime receiver remains open");
            sender
                .send(SessionCommand::ViewportChanged {
                    session_id,
                    generation: 1,
                    viewport: viewport(),
                })
                .expect("runtime receiver remains open");
            sender
                .send(SessionCommand::ClipboardWrite(
                    super::ClipboardPayload::new(vec![0x11]),
                ))
                .expect("runtime receiver remains open");
            sender
                .send(SessionCommand::ResolveServerIdentity {
                    session_id,
                    challenge_id: 9,
                    decision: super::ServerIdentityDecision::Reject,
                })
                .expect("runtime receiver remains open");
            sender
                .send(SessionCommand::Disconnect)
                .expect("runtime receiver remains open");
            assert!(matches!(
                runtime.try_next_command(),
                Some(SessionCommand::Disconnect)
            ));

            assert_eq!(
                runtime.begin_generation(
                    session_id,
                    2,
                    PixelSize::new(800, 600).expect("valid size"),
                    PixelFormat::Bgrx8UnormSrgb,
                ),
                Err(ProtocolError::Terminal)
            );
            assert_eq!(
                state.lock().expect("fault state").attempts,
                failure.expected_attempts()
            );
        }
    }

    fn establish_generation(runtime: &mut ProtocolRuntime, session_id: SessionId, generation: u64) {
        runtime
            .begin_generation(
                session_id,
                generation,
                PixelSize::new(800, 600).expect("valid size"),
                PixelFormat::Bgrx8UnormSrgb,
            )
            .expect("generation succeeds");
    }

    fn input(session_id: SessionId, generation: u64) -> SessionCommand {
        SessionCommand::Input(SessionInput {
            session_id,
            generation,
            event: InputEvent::ReleaseAll,
        })
    }

    fn viewport_command(session_id: SessionId, generation: u64) -> SessionCommand {
        SessionCommand::ViewportChanged {
            session_id,
            generation,
            viewport: viewport(),
        }
    }

    fn identity_resolution(session_id: SessionId) -> SessionCommand {
        SessionCommand::ResolveServerIdentity {
            session_id,
            challenge_id: 9,
            decision: super::ServerIdentityDecision::Reject,
        }
    }

    fn identity_challenge(session_id: SessionId) -> super::ServerIdentityChallenge {
        super::ServerIdentityChallenge {
            session_id,
            challenge_id: 9,
            protocol_id: ProtocolId::apple_hpss_mvs(),
            endpoint: Endpoint::new("mac.example", 5900).expect("valid endpoint"),
            sha256_fingerprint: [0x11; 32],
            subject: "mac.example".to_owned(),
            issuer: "test issuer".to_owned(),
            validation: ServerIdentityValidation::Unknown,
        }
    }

    fn damage(session_id: SessionId, generation: u64, revision: u64) -> SurfaceUpdate {
        SurfaceUpdate::Damage {
            session_id,
            generation,
            revision,
            patches: vec![PixelPatch {
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                stride_bytes: 4,
                pixels: PixelBuffer::new(vec![0x11; 4]),
            }],
        }
    }

    fn viewport() -> PhysicalViewport {
        let size = PixelSize::new(800, 600).expect("valid size");
        PhysicalViewport::new(
            size,
            PixelRect {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            },
            size,
        )
        .expect("valid viewport")
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

    struct AdapterEvents(Arc<Mutex<Vec<&'static str>>>);

    impl RuntimeEventSink for AdapterEvents {
        fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError> {
            let label = match event {
                SessionEvent::SurfaceGenerationChanged { .. } => "generation",
                SessionEvent::ServerIdentityChallenge(_) => "identity",
                _ => "event",
            };
            self.0.lock().expect("log lock").push(label);
            Ok(())
        }
    }

    struct AdapterFrames(Arc<Mutex<Vec<&'static str>>>);

    impl SurfacePublisher for AdapterFrames {
        fn publish(&self, update: SurfaceUpdate) -> Result<(), ProtocolError> {
            let label = match update {
                SurfaceUpdate::Reset { .. } => "reset",
                SurfaceUpdate::Damage { .. } => "damage",
                SurfaceUpdate::FrameBoundary { .. } => "boundary",
            };
            self.0.lock().expect("log lock").push(label);
            Ok(())
        }
    }

    struct RecordingMedia(Arc<Mutex<usize>>);

    impl MediaPublisher for RecordingMedia {
        fn publish(&self, _: MediaFrame) -> Result<(), MediaPublishError> {
            *self.0.lock().expect("media calls") += 1;
            Ok(())
        }
    }

    struct FailingMedia;

    impl MediaPublisher for FailingMedia {
        fn publish(&self, _: MediaFrame) -> Result<(), MediaPublishError> {
            Err(MediaPublishError::Closed)
        }
    }

    #[derive(Clone, Copy)]
    enum FailureAt {
        Event,
        Reset,
        Wake,
    }

    impl FailureAt {
        fn expected_attempts(self) -> Vec<&'static str> {
            match self {
                Self::Event => vec!["event", "reset", "wake", "event"],
                Self::Reset => vec!["event", "reset", "wake", "event", "reset"],
                Self::Wake => vec!["event", "reset", "wake", "event", "reset", "wake"],
            }
        }
    }

    struct FaultState {
        failure: FailureAt,
        attempts: Vec<&'static str>,
    }

    impl FaultState {
        fn new(failure: FailureAt) -> Self {
            Self {
                failure,
                attempts: Vec::new(),
            }
        }
    }

    struct FaultEvents(Arc<Mutex<FaultState>>);

    impl RuntimeEventSink for FaultEvents {
        fn publish(&self, _: SessionEvent) -> Result<(), ProtocolError> {
            let mut state = self.0.lock().expect("fault state");
            state.attempts.push("event");
            (state.attempts.len() > 3 && matches!(state.failure, FailureAt::Event))
                .then_some(ProtocolError::EventPortClosed)
                .map_or(Ok(()), Err)
        }
    }

    struct FaultFrames(Arc<Mutex<FaultState>>);

    impl SurfacePublisher for FaultFrames {
        fn publish(&self, _: SurfaceUpdate) -> Result<(), ProtocolError> {
            let mut state = self.0.lock().expect("fault state");
            state.attempts.push("reset");
            (state.attempts.len() > 3 && matches!(state.failure, FailureAt::Reset))
                .then_some(ProtocolError::FramePortRejected)
                .map_or(Ok(()), Err)
        }
    }

    struct FaultWake(Arc<Mutex<FaultState>>);

    impl RuntimeWake for FaultWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            let mut state = self.0.lock().expect("fault state");
            state.attempts.push("wake");
            (state.attempts.len() > 3 && matches!(state.failure, FailureAt::Wake))
                .then_some(ProtocolError::WakeFailed)
                .map_or(Ok(()), Err)
        }
    }
}
