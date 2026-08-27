use frd_core::{InputEvent, SessionId, SessionInput};
use frd_frame::FrameCompleteness;
use frd_platform_api::{
    CredentialProvider, PlatformCapabilities, PlatformError, ServerIdentityStore,
};
use frd_protocol_api::{
    AudioState, ClipboardPayload, ConnectRequest, ConnectionStage, Credentials, PresentationEvent,
    ProtocolCatalog, ProtocolError, ProtocolSelection, ServerIdentityChallenge,
    ServerIdentityDecision, SessionCapabilities, SessionCommand, SessionEvent,
};
use frd_session::CleanupComplete;
use frd_ui_model::{
    ConnectionDraft, ConnectionForm, ConnectionSubmission, LaunchOptions, Page, ProtocolChoice,
};

use crate::AppIntent;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProductPolicy {
    pub dynamic_resolution: bool,
    pub clipboard_read: bool,
    pub clipboard_write: bool,
    pub remote_audio: bool,
    pub text_input: bool,
}

impl ProductPolicy {
    fn as_capabilities(self) -> SessionCapabilities {
        SessionCapabilities {
            dynamic_resolution: self.dynamic_resolution,
            clipboard_read: self.clipboard_read,
            clipboard_write: self.clipboard_write,
            remote_audio: self.remote_audio,
            text_input: self.text_input,
        }
    }
}

fn platform_capabilities(capabilities: PlatformCapabilities) -> SessionCapabilities {
    SessionCapabilities {
        dynamic_resolution: capabilities.dynamic_resolution,
        clipboard_read: capabilities.clipboard_read,
        clipboard_write: capabilities.clipboard_write,
        remote_audio: capabilities.remote_audio,
        text_input: capabilities.text_input,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdentityDecisionError {
    NoCurrentChallenge,
    StaleChallenge,
    PinMismatch,
    TrustAndRememberRequiresUnknown,
    Store(PlatformError),
}

pub enum AppAction {
    StartSession(ConnectRequest),
    SessionCommand(SessionCommand),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppControllerError {
    SessionAlreadyActive,
    NoActiveSession,
    InvalidConnection(ProtocolError),
    Platform(PlatformError),
    Identity(IdentityDecisionError),
}

pub struct AppLaunch {
    controller: AppController,
    pending_connect: Option<AppIntent>,
}

impl AppLaunch {
    pub fn new(
        options: LaunchOptions,
        provider: &dyn CredentialProvider,
        catalog: &ProtocolCatalog,
    ) -> Self {
        let mut form = ConnectionForm::new(ConnectionDraft {
            target_system: options.target_system,
            address: options.address.unwrap_or_default(),
            port: options.port,
            protocol: options
                .protocol
                .map(ProtocolChoice::Explicit)
                .unwrap_or(ProtocolChoice::Automatic),
            username: String::new(),
        });

        let username_provider_failed = options.username_provider.as_ref().is_some_and(|id| {
            provider
                .load_username(id)
                .map(|username| form.draft.username = username)
                .is_err()
        });
        let password_provider_failed = options.password_provider.as_ref().is_some_and(|id| {
            provider
                .load_password(id)
                .map(|password| form.set_password(password))
                .is_err()
        });

        let form_valid = !options.connect_when_complete || form.validate(catalog);
        if username_provider_failed {
            form.set_username_error("credential_provider_failed");
        }
        if password_provider_failed {
            form.set_password_error("credential_provider_failed");
        }
        let pending_connect = if options.connect_when_complete {
            (form_valid && !username_provider_failed && !password_provider_failed)
                .then(|| form.take_submission(catalog))
                .flatten()
                .map(AppIntent::Connect)
        } else {
            None
        };

        Self {
            controller: AppController::connection_form(form),
            pending_connect,
        }
    }

    pub fn take_connect_intent(&mut self) -> Option<AppIntent> {
        self.pending_connect.take()
    }

    pub fn controller(&self) -> &AppController {
        &self.controller
    }

    pub fn controller_mut(&mut self) -> &mut AppController {
        &mut self.controller
    }
}

pub struct AppController {
    session_id: Option<SessionId>,
    generation: u64,
    page: Page,
    session_slot: ActiveSessionSlot,
    protocol_capabilities: SessionCapabilities,
    platform_capabilities: PlatformCapabilities,
    policy: ProductPolicy,
    challenge: Option<ServerIdentityChallenge>,
    inbound_clipboard: Option<ClipboardPayload>,
    audio_state: AudioState,
}

impl AppController {
    pub fn connection_form(form: ConnectionForm) -> Self {
        Self {
            session_id: None,
            generation: 0,
            page: Page::ConnectionForm(form),
            session_slot: ActiveSessionSlot::default(),
            protocol_capabilities: SessionCapabilities::default(),
            platform_capabilities: PlatformCapabilities::default(),
            policy: ProductPolicy::default(),
            challenge: None,
            inbound_clipboard: None,
            audio_state: AudioState::Unavailable,
        }
    }

    pub fn awaiting_first_frame(session_id: SessionId, generation: u64) -> Self {
        assert!(generation != 0, "首帧 generation 必须大于零");
        let mut session_slot = ActiveSessionSlot::default();
        session_slot
            .begin_connect(session_id)
            .expect("新控制器的会话槽为空");
        session_slot
            .mark_active(session_id)
            .expect("新控制器的会话进入活动状态");
        Self {
            session_id: Some(session_id),
            generation,
            page: Page::AwaitingFirstFrame {
                draft: ConnectionDraft::default(),
                stage: ConnectionStage::TransportReady,
                diagnostics: None,
            },
            session_slot,
            protocol_capabilities: SessionCapabilities::default(),
            platform_capabilities: PlatformCapabilities::default(),
            policy: ProductPolicy::default(),
            challenge: None,
            inbound_clipboard: None,
            audio_state: AudioState::Unavailable,
        }
    }

    pub fn page(&self) -> &Page {
        &self.page
    }

    pub fn connection_form_mut(&mut self) -> Option<&mut ConnectionForm> {
        match &mut self.page {
            Page::ConnectionForm(form) => Some(form),
            _ => None,
        }
    }

    pub fn current_server_identity_challenge(&self) -> Option<&ServerIdentityChallenge> {
        self.challenge.as_ref()
    }

    pub fn finish_session_cleanup(
        &mut self,
        cleanup: CleanupComplete,
    ) -> Result<(), ActiveSessionError> {
        let session_id = cleanup.session_id();
        self.session_slot.finish_cleanup(cleanup)?;
        if self.session_id == Some(session_id) {
            self.session_id = None;
            self.generation = 0;
            self.challenge = None;
        }
        Ok(())
    }

    pub fn handle_intent<I: Into<AppIntent>>(
        &mut self,
        intent: I,
        catalog: &ProtocolCatalog,
        store: &dyn ServerIdentityStore,
    ) -> Result<Option<AppAction>, AppControllerError> {
        match intent.into() {
            AppIntent::Connect(submission) => {
                self.start_connection(submission, catalog, store).map(Some)
            }
            AppIntent::CancelConnect | AppIntent::Disconnect => {
                let session_id = self.session_id.ok_or(AppControllerError::NoActiveSession)?;
                self.session_slot
                    .begin_disconnect(session_id)
                    .map_err(|_| AppControllerError::NoActiveSession)?;
                Ok(Some(AppAction::SessionCommand(SessionCommand::Disconnect)))
            }
            AppIntent::ReturnToConnection => {
                let draft = self.page.retained_draft();
                self.page = Page::ConnectionForm(ConnectionForm::new(draft));
                Ok(None)
            }
            AppIntent::ResolveServerIdentity {
                session_id,
                challenge_id,
                decision,
            } => self
                .resolve_server_identity_with_store(session_id, challenge_id, decision, store)
                .map(AppAction::SessionCommand)
                .map(Some)
                .map_err(AppControllerError::Identity),
        }
    }

    fn start_connection(
        &mut self,
        mut submission: ConnectionSubmission,
        catalog: &ProtocolCatalog,
        store: &dyn ServerIdentityStore,
    ) -> Result<AppAction, AppControllerError> {
        if self.session_slot.is_occupied() {
            return Err(AppControllerError::SessionAlreadyActive);
        }
        let target =
            submission
                .draft
                .target_system
                .ok_or(AppControllerError::InvalidConnection(
                    ProtocolError::UnsupportedTargetProtocol,
                ))?;
        let protocol_id = catalog
            .select(
                target,
                ProtocolSelection::Explicit(submission.resolved_protocol.clone()),
            )
            .map_err(AppControllerError::InvalidConnection)?;
        let endpoint = frd_core::Endpoint::new(
            submission.draft.address.clone(),
            submission.draft.port.unwrap_or(0),
        )
        .ok_or(AppControllerError::InvalidConnection(
            ProtocolError::UnsupportedTargetProtocol,
        ))?;
        let saved_server_pin = store
            .load_pin(&protocol_id, &endpoint)
            .map_err(AppControllerError::Platform)?;
        let session_id = SessionId::allocate();
        self.session_slot
            .begin_connect(session_id)
            .map_err(|_| AppControllerError::SessionAlreadyActive)?;

        let draft = submission.draft.clone();
        let request = ConnectRequest {
            session_id,
            endpoint,
            protocol_id,
            credentials: Some(Credentials {
                username: submission.draft.username,
                password: submission.password.take(),
            }),
            saved_server_pin,
        };
        self.session_id = Some(session_id);
        self.generation = 0;
        self.challenge = None;
        self.protocol_capabilities = SessionCapabilities::default();
        self.page = Page::Connecting {
            draft,
            stage: ConnectionStage::Connecting,
            diagnostics: None,
        };
        Ok(AppAction::StartSession(request))
    }

    pub fn effective_capabilities(&self) -> SessionCapabilities {
        self.protocol_capabilities
            .intersection(platform_capabilities(self.platform_capabilities))
            .intersection(self.policy.as_capabilities())
    }

    pub fn set_platform_capabilities(&mut self, capabilities: PlatformCapabilities) {
        self.platform_capabilities = capabilities;
    }

    pub fn set_product_policy(&mut self, policy: ProductPolicy) {
        self.policy = policy;
    }

    /// 取走最新的入站剪贴板；数据只在内存中短暂聚合，不持久化。
    pub fn take_inbound_clipboard(&mut self) -> Option<ClipboardPayload> {
        self.inbound_clipboard.take()
    }

    pub fn audio_state(&self) -> &AudioState {
        &self.audio_state
    }

    pub fn handle_session_event(&mut self, event: SessionEvent) {
        if matches!(self.page, Page::ConnectionForm(_) | Page::Failed { .. }) {
            return;
        }
        match event {
            SessionEvent::StageChanged(stage) => {
                let draft = self.page.retained_draft();
                self.page = if stage == ConnectionStage::TransportReady {
                    Page::AwaitingFirstFrame {
                        draft,
                        stage,
                        diagnostics: None,
                    }
                } else {
                    Page::Connecting {
                        draft,
                        stage,
                        diagnostics: None,
                    }
                };
            }
            SessionEvent::CapabilitiesChanged(capabilities) => {
                self.protocol_capabilities = capabilities;
                if matches!(self.page, Page::RemoteSession { .. }) {
                    self.page = Page::RemoteSession {
                        draft: self.page.retained_draft(),
                        capabilities: self.effective_capabilities(),
                    };
                }
            }
            SessionEvent::Clipboard(payload) => {
                self.inbound_clipboard = Some(payload);
            }
            SessionEvent::AudioState(state) => {
                self.audio_state = state;
            }
            SessionEvent::SurfaceGenerationChanged {
                session_id,
                generation,
                ..
            } if Some(session_id) == self.session_id && generation > self.generation => {
                self.generation = generation;
                self.page = Page::AwaitingFirstFrame {
                    draft: self.page.retained_draft(),
                    stage: ConnectionStage::TransportReady,
                    diagnostics: None,
                };
            }
            SessionEvent::ServerIdentityChallenge(challenge) => {
                self.handle_server_identity_challenge(challenge);
            }
            SessionEvent::Error(error) => {
                self.page = Page::Failed {
                    draft: self.page.retained_draft(),
                    code: error.code().to_owned(),
                };
                self.challenge = None;
            }
            SessionEvent::Closed(exit) => {
                let code = match exit {
                    frd_protocol_api::ProtocolExit::Closed => "session_closed",
                    frd_protocol_api::ProtocolExit::Failed(ref error) => error.code(),
                };
                self.page = Page::Failed {
                    draft: self.page.retained_draft(),
                    code: code.to_owned(),
                };
                self.challenge = None;
            }
            _ => {}
        }
    }

    pub fn handle_presentation(&mut self, event: PresentationEvent) {
        if !matches!(self.page, Page::AwaitingFirstFrame { .. }) {
            return;
        }
        let PresentationEvent::FramePresented {
            session_id,
            generation,
            completeness,
            ..
        } = event;
        if Some(session_id) == self.session_id
            && generation == self.generation
            && completeness == FrameCompleteness::FullBaseline
        {
            self.page = Page::RemoteSession {
                draft: self.page.retained_draft(),
                capabilities: self.effective_capabilities(),
            };
        }
    }

    /// AppIntent 不承载热路径输入；仅当前已呈现会话可路由 protocol-neutral 输入。
    pub fn route_input(&self, event: InputEvent) -> Option<SessionCommand> {
        let session_id = self.session_id?;
        matches!(self.page, Page::RemoteSession { .. }).then_some(SessionCommand::Input(
            SessionInput {
                session_id,
                generation: self.generation,
                event,
            },
        ))
    }

    pub fn handle_server_identity_challenge(&mut self, challenge: ServerIdentityChallenge) {
        if Some(challenge.session_id) == self.session_id {
            self.challenge = Some(challenge);
        }
    }

    pub fn resolve_server_identity(
        &mut self,
        session_id: SessionId,
        challenge_id: u64,
        decision: ServerIdentityDecision,
    ) -> Result<SessionCommand, IdentityDecisionError> {
        self.resolve_server_identity_inner(session_id, challenge_id, decision, None)
    }

    pub fn resolve_server_identity_with_store(
        &mut self,
        session_id: SessionId,
        challenge_id: u64,
        decision: ServerIdentityDecision,
        store: &dyn ServerIdentityStore,
    ) -> Result<SessionCommand, IdentityDecisionError> {
        self.resolve_server_identity_inner(session_id, challenge_id, decision, Some(store))
    }

    fn resolve_server_identity_inner(
        &mut self,
        session_id: SessionId,
        challenge_id: u64,
        decision: ServerIdentityDecision,
        store: Option<&dyn ServerIdentityStore>,
    ) -> Result<SessionCommand, IdentityDecisionError> {
        let challenge = self
            .challenge
            .as_ref()
            .ok_or(IdentityDecisionError::NoCurrentChallenge)?;
        if Some(session_id) != self.session_id
            || challenge.session_id != session_id
            || challenge.challenge_id != challenge_id
        {
            return Err(IdentityDecisionError::StaleChallenge);
        }
        if challenge.validation == frd_protocol_api::ServerIdentityValidation::PinMismatch
            && decision != ServerIdentityDecision::Reject
        {
            return Err(IdentityDecisionError::PinMismatch);
        }
        if decision == ServerIdentityDecision::TrustAndRemember {
            if challenge.validation != frd_protocol_api::ServerIdentityValidation::Unknown {
                return Err(IdentityDecisionError::TrustAndRememberRequiresUnknown);
            }
            let store = store.ok_or(IdentityDecisionError::NoCurrentChallenge)?;
            store
                .store_pin(
                    &challenge.protocol_id,
                    &challenge.endpoint,
                    challenge.sha256_fingerprint,
                )
                .map_err(IdentityDecisionError::Store)?;
        }
        self.challenge = None;
        Ok(SessionCommand::ResolveServerIdentity {
            session_id,
            challenge_id,
            decision,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlotState {
    Connecting(SessionId),
    Active(SessionId),
    Disconnecting(SessionId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveSessionError {
    Occupied,
    InvalidTransition,
}

#[derive(Default)]
pub struct ActiveSessionSlot {
    state: Option<SlotState>,
}

impl ActiveSessionSlot {
    pub fn is_occupied(&self) -> bool {
        self.state.is_some()
    }

    pub fn begin_connect(&mut self, session_id: SessionId) -> Result<(), ActiveSessionError> {
        if self.state.is_some() {
            return Err(ActiveSessionError::Occupied);
        }
        self.state = Some(SlotState::Connecting(session_id));
        Ok(())
    }

    pub fn mark_active(&mut self, session_id: SessionId) -> Result<(), ActiveSessionError> {
        match self.state {
            Some(SlotState::Connecting(current)) if current == session_id => {
                self.state = Some(SlotState::Active(session_id));
                Ok(())
            }
            _ => Err(ActiveSessionError::InvalidTransition),
        }
    }

    pub fn begin_disconnect(&mut self, session_id: SessionId) -> Result<(), ActiveSessionError> {
        match self.state {
            Some(SlotState::Connecting(current) | SlotState::Active(current))
                if current == session_id =>
            {
                self.state = Some(SlotState::Disconnecting(session_id));
                Ok(())
            }
            _ => Err(ActiveSessionError::InvalidTransition),
        }
    }

    /// 仅接受 `SessionCoordinator` 在完整资源回收后签发的完成能力。
    pub fn finish_cleanup(&mut self, cleanup: CleanupComplete) -> Result<(), ActiveSessionError> {
        match self.state {
            Some(SlotState::Disconnecting(current)) if current == cleanup.session_id() => {
                self.state = None;
                Ok(())
            }
            _ => Err(ActiveSessionError::InvalidTransition),
        }
    }
}
