//! 协议注册、会话及运行时端口的协议中立边界。

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use frd_core::{PhysicalViewport, PixelSize, SecretBytes, SessionId, SessionInput};
use frd_frame::{FrameCompleteness, FrameMailbox, PixelFormat, SurfaceUpdate};
use frd_media_api::{MediaFrame, MediaPublishError, MediaPublisher};

pub use frd_core::{Endpoint, ProtocolId, TargetSystem};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    UnsupportedTargetProtocol,
    UnregisteredProtocol,
    FactoryDescriptorMismatch,
    InvalidGeneration,
    SurfaceCapacityExceeded,
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
    Adapter {
        protocol_id: ProtocolId,
        code: &'static str,
    },
}

impl ProtocolError {
    pub fn adapter(protocol_id: ProtocolId, code: &'static str) -> Self {
        Self::Adapter { protocol_id, code }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedTargetProtocol => "unsupported_target_protocol",
            Self::UnregisteredProtocol => "unregistered_protocol",
            Self::FactoryDescriptorMismatch => "factory_descriptor_mismatch",
            Self::InvalidGeneration => "invalid_generation",
            Self::SurfaceCapacityExceeded => "surface_capacity_exceeded",
            Self::EventPortClosed => "event_port_closed",
            Self::FramePortRejected => "frame_port_rejected",
            Self::WakeFailed => "wake_failed",
            Self::MediaPortClosed => "media_port_closed",
            Self::Terminal => "terminal",
            Self::GenerationPublicationReserved => "generation_publication_reserved",
            Self::SurfaceResetReserved => "surface_reset_reserved",
            Self::StaleSession => "stale_session",
            Self::StaleSurface => "stale_surface",
            Self::NeedsFullSnapshot => "needs_full_snapshot",
            Self::Adapter { code, .. } => code,
        }
    }

    pub fn protocol_id(&self) -> Option<&ProtocolId> {
        match self {
            Self::Adapter { protocol_id, .. } => Some(protocol_id),
            _ => None,
        }
    }
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
    pub credential_requirements: CredentialRequirements,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CredentialRequirements {
    pub username: bool,
    pub password: bool,
}

impl CredentialRequirements {
    pub const fn username_password() -> Self {
        Self {
            username: true,
            password: true,
        }
    }
}

impl From<ProtocolId> for ProtocolDescriptor {
    fn from(id: ProtocolId) -> Self {
        Self {
            display_name: id.as_str().to_owned(),
            default_port: default_port_for(&id),
            credential_requirements: CredentialRequirements::username_password(),
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

    pub fn descriptors(&self) -> &[ProtocolDescriptor] {
        &self.descriptors
    }

    pub fn descriptor(&self, protocol_id: &ProtocolId) -> Option<&ProtocolDescriptor> {
        self.descriptors
            .iter()
            .find(|descriptor| &descriptor.id == protocol_id)
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

const MAX_IDENTITY_VALIDATION_CODE_BYTES: usize = 64;
const MAX_IDENTITY_VALIDATION_REASON_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerIdentityValidationFailureError {
    InvalidCode,
    InvalidReason,
}

/// 仅承载 adapter 映射后的稳定代码与用户安全摘要；禁止复制底层原始诊断。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerIdentityValidationFailure {
    code: String,
    reason: String,
}

impl ServerIdentityValidationFailure {
    pub fn new(
        code: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<Self, ServerIdentityValidationFailureError> {
        let code = code.into();
        if code.is_empty()
            || code.len() > MAX_IDENTITY_VALIDATION_CODE_BYTES
            || !code.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
        {
            return Err(ServerIdentityValidationFailureError::InvalidCode);
        }

        let reason = reason.into();
        if reason.is_empty()
            || reason.len() > MAX_IDENTITY_VALIDATION_REASON_BYTES
            || reason.trim() != reason
            || reason.chars().any(char::is_control)
        {
            return Err(ServerIdentityValidationFailureError::InvalidReason);
        }
        Ok(Self { code, reason })
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    fn generic_unknown() -> Self {
        Self {
            code: "identity.system_trust_unavailable".to_owned(),
            reason: "系统未确认该服务器身份".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerIdentityValidationKind {
    Unknown,
    TrustedBySystem,
    PinMatched,
    PinMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerIdentityValidationInvariantError {
    MissingFailure,
    UnexpectedFailure,
}

/// 把验证结论与安全详情绑定；`Unknown` 在类型构造边界必定携带详情。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerIdentityValidation {
    kind: ServerIdentityValidationKind,
    failure: Option<ServerIdentityValidationFailure>,
}

impl ServerIdentityValidation {
    pub fn new(
        kind: ServerIdentityValidationKind,
        failure: Option<ServerIdentityValidationFailure>,
    ) -> Result<Self, ServerIdentityValidationInvariantError> {
        match (kind, failure) {
            (ServerIdentityValidationKind::Unknown, Some(failure)) => Ok(Self {
                kind,
                failure: Some(failure),
            }),
            (ServerIdentityValidationKind::Unknown, None) => {
                Err(ServerIdentityValidationInvariantError::MissingFailure)
            }
            (_, Some(_)) => Err(ServerIdentityValidationInvariantError::UnexpectedFailure),
            (_, None) => Ok(Self {
                kind,
                failure: None,
            }),
        }
    }

    pub fn unknown(failure: ServerIdentityValidationFailure) -> Self {
        Self {
            kind: ServerIdentityValidationKind::Unknown,
            failure: Some(failure),
        }
    }

    pub fn kind(&self) -> ServerIdentityValidationKind {
        self.kind
    }

    pub fn failure(&self) -> Option<&ServerIdentityValidationFailure> {
        self.failure.as_ref()
    }

    pub fn is_unknown(&self) -> bool {
        self.kind == ServerIdentityValidationKind::Unknown
    }

    pub fn is_pin_mismatch(&self) -> bool {
        self.kind == ServerIdentityValidationKind::PinMismatch
    }

    pub fn allows_credential_continuation(&self) -> bool {
        matches!(
            self.kind,
            ServerIdentityValidationKind::TrustedBySystem
                | ServerIdentityValidationKind::PinMatched
        )
    }

    fn pin_matched() -> Self {
        Self {
            kind: ServerIdentityValidationKind::PinMatched,
            failure: None,
        }
    }

    fn pin_mismatch() -> Self {
        Self {
            kind: ServerIdentityValidationKind::PinMismatch,
            failure: None,
        }
    }
}

pub fn evaluate_server_identity(
    saved_pin: Option<[u8; 32]>,
    presented_pin: [u8; 32],
) -> ServerIdentityValidation {
    match saved_pin {
        Some(pin) if pin == presented_pin => ServerIdentityValidation::pin_matched(),
        Some(_) => ServerIdentityValidation::pin_mismatch(),
        None => {
            ServerIdentityValidation::unknown(ServerIdentityValidationFailure::generic_unknown())
        }
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

#[derive(Debug, Eq, PartialEq)]
pub struct ClipboardPayload(Box<[u8]>);

impl ClipboardPayload {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes.into_boxed_slice())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// 与具体音频设备无关的远端音频生命周期状态。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum AudioState {
    #[default]
    Unavailable,
    Starting,
    Playing,
    Stopped,
    Failed,
}

/// framebuffer 画面请求成功写出，到对应完整响应完成 decode、commit 与 surface
/// publication 的协议侧耗时。它不表示 ping、输入到显示或本地 present 延迟。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameResponseTiming {
    pub generation: u64,
    pub sample_ms: u32,
    pub smoothed_ms: u32,
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

#[derive(Debug, Eq, PartialEq)]
pub enum SessionEvent {
    StageChanged(ConnectionStage),
    ServerIdentityChallenge(ServerIdentityChallenge),
    SurfaceGenerationChanged {
        session_id: SessionId,
        generation: u64,
        size: frd_core::PixelSize,
    },
    CapabilitiesChanged(SessionCapabilities),
    FrameResponseTiming(FrameResponseTiming),
    Clipboard(ClipboardPayload),
    AudioState(AudioState),
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
    fn preflight_generation(
        &self,
        _size: PixelSize,
        _format: PixelFormat,
    ) -> Result<(), ProtocolError> {
        Ok(())
    }

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
    fn preflight_generation(
        &self,
        size: PixelSize,
        format: PixelFormat,
    ) -> Result<(), ProtocolError> {
        self.mailbox
            .lock()
            .map_err(|_| ProtocolError::FramePortRejected)?
            .supports_complete_surface(size, format)
            .then_some(())
            .ok_or(ProtocolError::SurfaceCapacityExceeded)
    }

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

/// 一次无副作用的 generation 端口准入结果。
///
/// 字段保持私有，且该值不可复制；只有完成准入的同一个 `ProtocolRuntime`
/// 才能消费它。协议适配器可在分配解码 surface 或发送几何相关请求前取得
/// admission，随后用同一份尺寸、像素格式与 frame port 契约提交 Startup。
#[derive(Debug)]
pub struct GenerationAdmission {
    runtime_identity: Arc<()>,
    previous_generation: Option<u64>,
    session_id: SessionId,
    generation: u64,
    size: PixelSize,
    format: PixelFormat,
}

impl GenerationAdmission {
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn size(&self) -> PixelSize {
        self.size
    }

    pub fn format(&self) -> PixelFormat {
        self.format
    }
}

pub struct ProtocolRuntime {
    runtime_identity: Arc<()>,
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
            runtime_identity: Arc::new(()),
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

    /// 对新的 generation 做无副作用 frame-port 准入。
    ///
    /// 端口拒绝会立即令 runtime 进入终态；成功只返回绑定当前 runtime 与
    /// generation 状态的 opaque admission，不发布 event、Reset 或 wake。
    pub fn admit_generation(
        &mut self,
        session_id: SessionId,
        generation: u64,
        size: frd_core::PixelSize,
        format: PixelFormat,
    ) -> Result<GenerationAdmission, ProtocolError> {
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
        if let Err(error) = self.frames.preflight_generation(size, format) {
            self.poison();
            return Err(error);
        }
        Ok(GenerationAdmission {
            runtime_identity: self.runtime_identity.clone(),
            previous_generation: self.current_generation,
            session_id,
            generation,
            size,
            format,
        })
    }

    /// 消费协议适配器无法绑定到其当前状态的 generation admission。
    ///
    /// 非终态 runtime 必须 fail-closed，且该拒绝不会发布 event、Reset 或 wake。
    pub fn reject_invalid_generation_admission(
        &mut self,
        _admission: GenerationAdmission,
    ) -> Result<(), ProtocolError> {
        if self.terminal {
            return Err(ProtocolError::Terminal);
        }
        self.poison();
        Err(ProtocolError::InvalidGeneration)
    }

    /// 按 event、Reset、单次 wake 的顺序消费一次准入并发布 generation。
    /// 任一端口失败后进入终态；不会伪称回滚已发布的 event/Reset。
    pub fn begin_admitted_generation(
        &mut self,
        admission: GenerationAdmission,
    ) -> Result<(), ProtocolError> {
        if self.terminal {
            return Err(ProtocolError::Terminal);
        }
        if !Arc::ptr_eq(&admission.runtime_identity, &self.runtime_identity)
            || admission.session_id != self.session_id
            || admission.previous_generation != self.current_generation
            || admission.generation == 0
            || self
                .current_generation
                .is_some_and(|current| admission.generation <= current)
        {
            self.poison();
            return Err(ProtocolError::InvalidGeneration);
        }
        if let Err(error) = self.events.publish(SessionEvent::SurfaceGenerationChanged {
            session_id: admission.session_id,
            generation: admission.generation,
            size: admission.size,
        }) {
            self.poison();
            return Err(error);
        }
        if let Err(error) = self.frames.publish(SurfaceUpdate::Reset {
            session_id: admission.session_id,
            generation: admission.generation,
            size: admission.size,
            format: admission.format,
        }) {
            self.poison();
            return Err(error);
        }
        if let Err(error) = self.wake.wake() {
            self.poison();
            return Err(error);
        }
        self.current_generation = Some(admission.generation);
        Ok(())
    }

    /// 准入与发布的兼容入口。需要在昂贵准备之前 fail-closed 的适配器应显式
    /// 使用 `admit_generation` / `begin_admitted_generation` 两阶段契约。
    pub fn begin_generation(
        &mut self,
        session_id: SessionId,
        generation: u64,
        size: frd_core::PixelSize,
        format: PixelFormat,
    ) -> Result<(), ProtocolError> {
        let admission = self.admit_generation(session_id, generation, size, format)?;
        self.begin_admitted_generation(admission)
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

    /// 尝试发布可独立降级的媒体帧。媒体端口背压或关闭不会把桌面会话
    /// 标记为终态；调用方必须显式停止该媒体流，不能在循环中隐藏重试。
    pub fn try_publish_optional_media(&mut self, frame: MediaFrame) -> Result<(), ProtocolError> {
        if self.terminal {
            return Err(ProtocolError::Terminal);
        }
        let Some(media) = &self.media else {
            return Err(ProtocolError::MediaPortClosed);
        };
        media.publish(frame).map_err(|error| match error {
            MediaPublishError::Closed | MediaPublishError::Full => ProtocolError::MediaPortClosed,
        })
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
        RuntimeWake, SessionCommand, SessionEvent, SurfacePublisher, TargetSystem,
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

        assert_eq!(
            validation.kind(),
            super::ServerIdentityValidationKind::PinMismatch
        );
        assert!(!validation.allows_credential_continuation());
    }

    #[test]
    fn identity_validation_failure_accepts_only_bounded_sanitized_code_and_reason() {
        let failure = super::ServerIdentityValidationFailure::new(
            "certificate.hostname_mismatch",
            "证书名称与目标端点不匹配",
        )
        .expect("bounded sanitized failure is accepted");
        assert_eq!(failure.code(), "certificate.hostname_mismatch");
        assert_eq!(failure.reason(), "证书名称与目标端点不匹配");

        assert!(super::ServerIdentityValidationFailure::new("bad code", "安全摘要").is_err());
        assert!(super::ServerIdentityValidationFailure::new(
            "certificate.expired",
            "第一行\n第二行"
        )
        .is_err());
        assert!(super::ServerIdentityValidationFailure::new(
            "certificate.expired",
            "x".repeat(257),
        )
        .is_err());
    }

    #[test]
    fn unknown_identity_validation_without_reason_is_rejected() {
        assert_eq!(
            super::ServerIdentityValidation::new(
                super::ServerIdentityValidationKind::Unknown,
                None,
            ),
            Err(super::ServerIdentityValidationInvariantError::MissingFailure)
        );
    }

    #[test]
    fn generic_unknown_identity_validation_always_exposes_a_bounded_reason() {
        let validation = evaluate_server_identity(None, [0x22; 32]);

        assert_eq!(
            validation.kind(),
            super::ServerIdentityValidationKind::Unknown
        );
        let failure = validation
            .failure()
            .expect("unknown validation always carries a safe reason");
        assert!(!failure.code().is_empty());
        assert!(!failure.reason().is_empty());
        assert!(
            super::ServerIdentityValidationFailure::new(failure.code(), failure.reason(),).is_ok()
        );
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
    fn generation_admission_defers_publication_and_commits_the_exact_contract() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(800, 600).expect("valid size");
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::with_ports(
            session_id,
            Box::new(RecordingEvents(log.clone())),
            Box::new(RecordingFrames(log.clone())),
            Box::new(RecordingWake(log.clone())),
        );

        let admission = runtime
            .admit_generation(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb)
            .expect("preflight accepts generation");

        assert!(log.lock().expect("log lock").is_empty());
        assert_eq!(admission.session_id(), session_id);
        assert_eq!(admission.generation(), 1);
        assert_eq!(admission.size(), size);
        assert_eq!(admission.format(), PixelFormat::Bgrx8UnormSrgb);
        runtime
            .begin_admitted_generation(admission)
            .expect("admitted generation commits");
        assert_eq!(
            *log.lock().expect("log lock"),
            vec!["event", "reset", "wake"]
        );
    }

    #[test]
    fn cross_runtime_generation_admission_poison_target_without_publication() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(800, 600).expect("valid size");
        let first_log = Arc::new(Mutex::new(Vec::new()));
        let second_log = Arc::new(Mutex::new(Vec::new()));
        let mut first = ProtocolRuntime::with_ports(
            session_id,
            Box::new(RecordingEvents(first_log.clone())),
            Box::new(RecordingFrames(first_log.clone())),
            Box::new(RecordingWake(first_log.clone())),
        );
        let mut second = ProtocolRuntime::with_ports(
            session_id,
            Box::new(RecordingEvents(second_log.clone())),
            Box::new(RecordingFrames(second_log.clone())),
            Box::new(RecordingWake(second_log.clone())),
        );

        let foreign = first
            .admit_generation(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb)
            .unwrap();
        assert_eq!(
            second.begin_admitted_generation(foreign),
            Err(ProtocolError::InvalidGeneration)
        );
        assert!(second.requires_shutdown());
        assert!(!first.requires_shutdown());
        assert!(first_log.lock().unwrap().is_empty());
        assert!(second_log.lock().unwrap().is_empty());
        assert_eq!(
            second.begin_generation(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb),
            Err(ProtocolError::Terminal)
        );
        assert!(second_log.lock().unwrap().is_empty());
    }

    #[test]
    fn stale_generation_admission_poison_runtime_without_republication() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(800, 600).expect("valid size");
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::with_ports(
            session_id,
            Box::new(RecordingEvents(log.clone())),
            Box::new(RecordingFrames(log.clone())),
            Box::new(RecordingWake(log.clone())),
        );

        let stale = runtime
            .admit_generation(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb)
            .unwrap();
        runtime
            .begin_generation(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb)
            .unwrap();
        assert_eq!(
            runtime.begin_admitted_generation(stale),
            Err(ProtocolError::InvalidGeneration)
        );
        assert!(runtime.requires_shutdown());
        assert_eq!(*log.lock().unwrap(), vec!["event", "reset", "wake"]);
        assert_eq!(
            runtime.begin_generation(session_id, 2, size, PixelFormat::Bgrx8UnormSrgb),
            Err(ProtocolError::Terminal)
        );
        assert_eq!(*log.lock().unwrap(), vec!["event", "reset", "wake"]);
    }

    #[test]
    fn mailbox_surface_preflight_rejects_oversized_generation_before_publication_and_poison_runtime(
    ) {
        let session_id = SessionId::allocate();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mailbox = Arc::new(Mutex::new(FrameMailbox::new(4, 12)));
        let mut runtime = ProtocolRuntime::with_ports(
            session_id,
            Box::new(RecordingEvents(log.clone())),
            Box::new(MailboxSurfacePublisher::new(mailbox.clone())),
            Box::new(RecordingWake(log.clone())),
        );

        let error = runtime
            .begin_generation(
                session_id,
                1,
                PixelSize::new(2, 2).unwrap(),
                PixelFormat::Bgrx8UnormSrgb,
            )
            .unwrap_err();

        assert_eq!(error, ProtocolError::SurfaceCapacityExceeded);
        assert_eq!(error.code(), "surface_capacity_exceeded");
        assert!(runtime.requires_shutdown());
        assert!(log.lock().expect("log lock").is_empty());
        assert!(mailbox.lock().expect("mailbox lock").is_empty());
        assert_eq!(
            runtime.begin_generation(
                session_id,
                1,
                PixelSize::new(1, 1).unwrap(),
                PixelFormat::Bgrx8UnormSrgb,
            ),
            Err(ProtocolError::Terminal)
        );
    }

    #[test]
    fn mailbox_surface_preflight_accepts_exact_fit_in_event_reset_wake_order() {
        let session_id = SessionId::allocate();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mailbox = Arc::new(Mutex::new(FrameMailbox::new(4, 16)));
        let mut runtime = ProtocolRuntime::with_ports(
            session_id,
            Box::new(RecordingEvents(log.clone())),
            Box::new(MailboxSurfacePublisher::new(mailbox.clone())),
            Box::new(MailboxInspectingWake {
                mailbox: mailbox.clone(),
                log: log.clone(),
            }),
        );

        runtime
            .begin_generation(
                session_id,
                1,
                PixelSize::new(2, 2).unwrap(),
                PixelFormat::Bgrx8UnormSrgb,
            )
            .expect("exact-fit surface is accepted");

        assert_eq!(
            *log.lock().expect("log lock"),
            vec!["event", "reset", "wake"]
        );
        assert!(matches!(
            mailbox.lock().expect("mailbox lock").pop(),
            Some(SurfaceUpdate::Reset {
                session_id: observed_session,
                generation: 1,
                size,
                format: PixelFormat::Bgrx8UnormSrgb,
            }) if observed_session == session_id && size == PixelSize::new(2, 2).unwrap()
        ));
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
    fn adapter_runtime_publishes_clipboard_and_audio_events_with_one_wake_each() {
        let session_id = SessionId::allocate();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::with_ports(
            session_id,
            Box::new(AdapterEvents(log.clone())),
            Box::new(AdapterFrames(log.clone())),
            Box::new(RecordingWake(log.clone())),
        );

        runtime
            .publish_event(SessionEvent::Clipboard(super::ClipboardPayload::new(vec![
                0x11,
            ])))
            .expect("clipboard event is delivered");
        runtime
            .publish_event(SessionEvent::AudioState(super::AudioState::Playing))
            .expect("audio event is delivered");

        assert_eq!(
            *log.lock().expect("log lock"),
            vec!["clipboard", "wake", "audio", "wake"]
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
        runtime
            .begin_generation(
                session_id,
                1,
                PixelSize::new(1, 1).expect("valid size"),
                PixelFormat::Bgrx8UnormSrgb,
            )
            .expect("generation fits the mailbox byte budget");

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
            Box::new(FaultWake(fault_state.clone())),
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
            runtime.publish_event(SessionEvent::Clipboard(super::ClipboardPayload::new(vec![
                0x11,
            ]))),
            Err(ProtocolError::Terminal)
        );
        assert_eq!(
            runtime.publish_event(SessionEvent::AudioState(super::AudioState::Playing)),
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
        assert_eq!(
            fault_state.lock().expect("fault state").attempts,
            vec!["event", "reset", "wake", "event", "reset", "wake"]
        );
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
    fn optional_media_full_or_closed_does_not_poison_runtime_or_retry() {
        for failure in [MediaPublishError::Full, MediaPublishError::Closed] {
            let session_id = SessionId::allocate();
            let (_, receiver) = mpsc::channel();
            let log = Arc::new(Mutex::new(Vec::new()));
            let attempts = Arc::new(Mutex::new(0usize));
            let mut runtime = ProtocolRuntime::new(
                session_id,
                receiver,
                Box::new(RecordingEvents(log.clone())),
                Box::new(RecordingFrames(log.clone())),
                Some(Box::new(OptionalFailingMedia {
                    failure,
                    attempts: attempts.clone(),
                })),
                Box::new(RecordingWake(log)),
            );
            establish_generation(&mut runtime, session_id, 1);

            assert_eq!(
                runtime.try_publish_optional_media(MediaFrame::EncodedVideo {
                    timestamp_us: 1,
                    bytes: vec![0x11].into_boxed_slice(),
                }),
                Err(ProtocolError::MediaPortClosed)
            );
            assert!(!runtime.requires_shutdown());
            assert_eq!(*attempts.lock().expect("optional media attempts"), 1);
            assert!(runtime
                .publish_event(SessionEvent::AudioState(super::AudioState::Failed))
                .is_ok());
        }
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
            validation: evaluate_server_identity(None, [0x11; 32]),
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

    struct MailboxInspectingWake {
        mailbox: Arc<Mutex<FrameMailbox>>,
        log: Arc<Mutex<Vec<&'static str>>>,
    }

    impl RuntimeWake for MailboxInspectingWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            let mailbox = self.mailbox.lock().expect("mailbox lock");
            assert_eq!(mailbox.len(), 1, "Reset must precede wake");
            assert_eq!(mailbox.queued_pixel_bytes(), 0);
            let mut log = self.log.lock().expect("log lock");
            log.push("reset");
            log.push("wake");
            Ok(())
        }
    }

    struct AdapterEvents(Arc<Mutex<Vec<&'static str>>>);

    impl RuntimeEventSink for AdapterEvents {
        fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError> {
            let label = match event {
                SessionEvent::SurfaceGenerationChanged { .. } => "generation",
                SessionEvent::ServerIdentityChallenge(_) => "identity",
                SessionEvent::Clipboard(_) => "clipboard",
                SessionEvent::AudioState(_) => "audio",
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

    struct OptionalFailingMedia {
        failure: MediaPublishError,
        attempts: Arc<Mutex<usize>>,
    }

    impl MediaPublisher for OptionalFailingMedia {
        fn publish(&self, _: MediaFrame) -> Result<(), MediaPublishError> {
            *self.attempts.lock().expect("optional media attempts") += 1;
            Err(self.failure.clone())
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
