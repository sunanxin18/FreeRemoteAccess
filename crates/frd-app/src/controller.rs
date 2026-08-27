use frd_core::{InputEvent, SessionId, SessionInput};
use frd_frame::FrameCompleteness;
use frd_platform_api::{PlatformCapabilities, PlatformError, ServerIdentityStore};
use frd_protocol_api::{
    PresentationEvent, ServerIdentityChallenge, ServerIdentityDecision, SessionCapabilities,
    SessionCommand, SessionEvent,
};
use frd_ui_model::Page;

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
    Store(PlatformError),
}

pub struct AppController {
    session_id: SessionId,
    generation: u64,
    page: Page,
    protocol_capabilities: SessionCapabilities,
    platform_capabilities: PlatformCapabilities,
    policy: ProductPolicy,
    challenge: Option<ServerIdentityChallenge>,
}

impl AppController {
    pub fn awaiting_first_frame(session_id: SessionId, generation: u64) -> Self {
        assert!(generation != 0, "首帧 generation 必须大于零");
        Self {
            session_id,
            generation,
            page: Page::AwaitingFirstFrame,
            protocol_capabilities: SessionCapabilities::default(),
            platform_capabilities: PlatformCapabilities::default(),
            policy: ProductPolicy::default(),
            challenge: None,
        }
    }

    pub fn page(&self) -> &Page {
        &self.page
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

    pub fn handle_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::CapabilitiesChanged(capabilities) => {
                self.protocol_capabilities = capabilities;
            }
            SessionEvent::SurfaceGenerationChanged {
                session_id,
                generation,
                ..
            } if session_id == self.session_id && generation > self.generation => {
                self.generation = generation;
                self.page = Page::AwaitingFirstFrame;
            }
            SessionEvent::ServerIdentityChallenge(challenge) => {
                self.handle_server_identity_challenge(challenge);
            }
            _ => {}
        }
    }

    pub fn handle_presentation(&mut self, event: PresentationEvent) {
        let PresentationEvent::FramePresented {
            session_id,
            generation,
            completeness,
            ..
        } = event;
        if session_id == self.session_id
            && generation == self.generation
            && completeness == FrameCompleteness::FullBaseline
        {
            self.page = Page::RemoteSession {
                capabilities: self.effective_capabilities(),
            };
        }
    }

    /// AppIntent 不承载热路径输入；仅当前已呈现会话可路由 protocol-neutral 输入。
    pub fn route_input(&self, event: InputEvent) -> Option<SessionCommand> {
        matches!(self.page, Page::RemoteSession { .. }).then_some(SessionCommand::Input(
            SessionInput {
                session_id: self.session_id,
                generation: self.generation,
                event,
            },
        ))
    }

    pub fn handle_server_identity_challenge(&mut self, challenge: ServerIdentityChallenge) {
        if challenge.session_id == self.session_id {
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
        if session_id != self.session_id
            || challenge.session_id != session_id
            || challenge.challenge_id != challenge_id
        {
            return Err(IdentityDecisionError::StaleChallenge);
        }
        if decision == ServerIdentityDecision::TrustAndRemember {
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

#[derive(Default)]
pub struct ActiveSessionSlot {
    state: Option<SlotState>,
}

impl ActiveSessionSlot {
    pub fn begin_connect(&mut self, session_id: SessionId) -> Result<(), ()> {
        if self.state.is_some() {
            return Err(());
        }
        self.state = Some(SlotState::Connecting(session_id));
        Ok(())
    }

    pub fn mark_active(&mut self, session_id: SessionId) -> Result<(), ()> {
        match self.state {
            Some(SlotState::Connecting(current)) if current == session_id => {
                self.state = Some(SlotState::Active(session_id));
                Ok(())
            }
            _ => Err(()),
        }
    }

    pub fn begin_disconnect(&mut self, session_id: SessionId) -> Result<(), ()> {
        match self.state {
            Some(SlotState::Connecting(current) | SlotState::Active(current))
                if current == session_id =>
            {
                self.state = Some(SlotState::Disconnecting(session_id));
                Ok(())
            }
            _ => Err(()),
        }
    }

    pub fn finish_cleanup(&mut self, session_id: SessionId) {
        if matches!(self.state, Some(SlotState::Connecting(current) | SlotState::Active(current) | SlotState::Disconnecting(current)) if current == session_id)
        {
            self.state = None;
        }
    }
}
