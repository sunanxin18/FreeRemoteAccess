use frd_core::{InputEvent, SessionId, SessionInput};
use frd_frame::FrameCompleteness;
use frd_platform_api::{
    ConnectionProfileKey, ConnectionProfileStore, CredentialProvider, PlatformCapabilities,
    PlatformError, SavedConnectionProfile, SecureCredentialStore, ServerIdentityStore,
};
use frd_protocol_api::{
    AudioState, ClipboardPayload, ConnectRequest, ConnectionStage, Credentials, PresentationEvent,
    ProtocolCatalog, ProtocolError, ServerIdentityChallenge, ServerIdentityDecision,
    SessionCapabilities, SessionCommand, SessionEvent,
};
use frd_session::{
    reserve_session_start, CleanupComplete, SessionStartAbort, SessionStartFailure,
    SessionStartOwner, SessionStartPermit,
};
use frd_ui_model::{
    ConnectionDraft, ConnectionForm, ConnectionSubmission, LaunchOptions, Page, ProtocolChoice,
};

use crate::AppIntent;

const PROFILE_PERSISTENCE_WARNING: &str = "登录信息未能安全保存；本次连接仍可继续，请稍后重试。";

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
    StartSession(ConnectRequest, SessionStartPermit),
    SessionCommand(SessionCommand),
}

#[derive(Clone, Copy)]
pub struct AppPlatformStores<'a> {
    pub server_identities: &'a dyn ServerIdentityStore,
    pub profiles: &'a dyn ConnectionProfileStore,
    pub credentials: &'a dyn SecureCredentialStore,
}

struct PendingProfileTransaction {
    session_id: SessionId,
    action: PendingProfileAction,
}

enum PendingProfileAction {
    Remember(SavedConnectionProfile),
    Delete(ConnectionProfileKey),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppControllerError {
    SessionAlreadyActive,
    NoActiveSession,
    InvalidSubmission(&'static str),
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

    pub fn new_with_stores(
        options: LaunchOptions,
        provider: &dyn CredentialProvider,
        catalog: &ProtocolCatalog,
        stores: AppPlatformStores<'_>,
    ) -> Self {
        let mut launch = Self::new(options, provider, catalog);
        if let Some(form) = launch.controller.connection_form_mut() {
            AppController::load_profiles_into_form(form, stores.profiles);
        }
        launch
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
    pending_profile: Option<PendingProfileTransaction>,
    profile_persistence_failed: bool,
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
            pending_profile: None,
            profile_persistence_failed: false,
        }
    }

    pub fn connection_form_with_stores(
        mut form: ConnectionForm,
        stores: AppPlatformStores<'_>,
    ) -> Self {
        Self::load_profiles_into_form(&mut form, stores.profiles);
        Self::connection_form(form)
    }

    #[cfg(test)]
    pub fn awaiting_first_frame(session_id: SessionId, generation: u64) -> Self {
        Self::reserve_awaiting_first_frame(session_id, generation).0
    }

    #[cfg(test)]
    pub(crate) fn awaiting_first_frame_with_start(
        session_id: SessionId,
        generation: u64,
    ) -> (Self, SessionStartPermit) {
        Self::reserve_awaiting_first_frame(session_id, generation)
    }

    #[cfg(test)]
    fn reserve_awaiting_first_frame(
        session_id: SessionId,
        generation: u64,
    ) -> (Self, SessionStartPermit) {
        assert!(generation != 0, "首帧 generation 必须大于零");
        let mut session_slot = ActiveSessionSlot::default();
        let permit = session_slot
            .begin_connect(session_id)
            .expect("新控制器的会话槽为空");
        session_slot
            .mark_active(session_id)
            .expect("新控制器的会话进入活动状态");
        (
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
                pending_profile: None,
                profile_persistence_failed: false,
            },
            permit,
        )
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

    fn load_profiles_into_form(form: &mut ConnectionForm, profiles: &dyn ConnectionProfileStore) {
        match profiles.list() {
            Ok(mut saved) => {
                SavedConnectionProfile::sort_most_recent(&mut saved);
                form.set_profiles(saved);
            }
            Err(_) => form.set_profile_storage_error("profile_storage_failed"),
        }
    }

    pub fn current_server_identity_challenge(&self) -> Option<&ServerIdentityChallenge> {
        self.challenge.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn finish_session_cleanup(
        &mut self,
        cleanup: CleanupComplete,
    ) -> Result<(), ActiveSessionError> {
        self.finish_session_cleanup_internal(cleanup, None)
    }

    pub fn finish_session_cleanup_with_stores(
        &mut self,
        cleanup: CleanupComplete,
        stores: AppPlatformStores<'_>,
    ) -> Result<(), ActiveSessionError> {
        self.finish_session_cleanup_internal(cleanup, Some(stores))
    }

    fn finish_session_cleanup_internal(
        &mut self,
        cleanup: CleanupComplete,
        stores: Option<AppPlatformStores<'_>>,
    ) -> Result<(), ActiveSessionError> {
        let session_id = cleanup.session_id();
        self.session_slot.finish_cleanup(&cleanup)?;
        if self.session_id == Some(session_id) {
            let draft = self.page.retained_draft();
            let return_to_connection = matches!(self.page, Page::Disconnecting { .. });
            self.session_id = None;
            self.reset_session_bound_state();
            if return_to_connection {
                let mut form = ConnectionForm::new(draft);
                if let Some(stores) = stores {
                    Self::load_profiles_into_form(&mut form, stores.profiles);
                }
                self.page = Page::ConnectionForm(form);
            }
        }
        Ok(())
    }

    /// 消费 launcher 已回滚终态；只释放仍处于 Connecting 的同一 reservation。
    #[cfg(test)]
    pub(crate) fn consume_launch_rollback(
        &mut self,
        failure: &SessionStartFailure,
    ) -> Result<(), ActiveSessionError> {
        self.consume_launch_rollback_internal(failure, None)
    }

    pub fn consume_launch_rollback_with_stores(
        &mut self,
        failure: &SessionStartFailure,
        stores: AppPlatformStores<'_>,
    ) -> Result<(), ActiveSessionError> {
        self.consume_launch_rollback_internal(failure, Some(stores))
    }

    fn consume_launch_rollback_internal(
        &mut self,
        failure: &SessionStartFailure,
        stores: Option<AppPlatformStores<'_>>,
    ) -> Result<(), ActiveSessionError> {
        if !matches!(self.page, Page::Connecting { .. }) {
            return Err(ActiveSessionError::InvalidTransition);
        }
        let session_id = failure.abort().session_id();
        if self.session_id != Some(session_id) {
            return Err(ActiveSessionError::InvalidTransition);
        }
        self.session_slot.abort_connect(failure.abort())?;
        if let Some(stores) = stores {
            self.discard_pending_profile(session_id, stores.credentials);
        }
        let draft = self.page.retained_draft();
        self.session_id = None;
        self.reset_session_bound_state();
        self.page = Page::Failed {
            draft,
            code: failure.error().code().to_owned(),
        };
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn handle_intent<I: Into<AppIntent>>(
        &mut self,
        intent: I,
        catalog: &ProtocolCatalog,
        store: &dyn ServerIdentityStore,
    ) -> Result<Option<AppAction>, AppControllerError> {
        self.handle_intent_internal(intent.into(), catalog, store, None)
    }

    pub fn handle_intent_with_stores<I: Into<AppIntent>>(
        &mut self,
        intent: I,
        catalog: &ProtocolCatalog,
        stores: AppPlatformStores<'_>,
    ) -> Result<Option<AppAction>, AppControllerError> {
        self.handle_intent_internal(
            intent.into(),
            catalog,
            stores.server_identities,
            Some(stores),
        )
    }

    fn handle_intent_internal(
        &mut self,
        intent: AppIntent,
        catalog: &ProtocolCatalog,
        store: &dyn ServerIdentityStore,
        stores: Option<AppPlatformStores<'_>>,
    ) -> Result<Option<AppAction>, AppControllerError> {
        match intent {
            AppIntent::Connect(submission) => self
                .start_connection(submission, catalog, store, stores)
                .map(Some),
            AppIntent::SelectSavedProfile(key) => {
                if let Some(stores) = stores {
                    self.select_saved_profile(key, stores.credentials);
                } else if let Some(form) = self.connection_form_mut() {
                    form.set_profile_storage_error("profile_storage_unavailable");
                }
                Ok(None)
            }
            AppIntent::CancelConnect | AppIntent::Disconnect => {
                let session_id = self.session_id.ok_or(AppControllerError::NoActiveSession)?;
                let transition = self
                    .session_slot
                    .begin_disconnect(session_id)
                    .map_err(|_| AppControllerError::NoActiveSession)?;
                if transition == DisconnectTransition::AlreadyInProgress {
                    return Ok(None);
                }
                if let Some(stores) = stores {
                    self.discard_pending_profile(session_id, stores.credentials);
                }
                let draft = self.page.retained_draft();
                self.reset_session_bound_state();
                self.page = Page::Disconnecting { draft };
                Ok(Some(AppAction::SessionCommand(SessionCommand::Disconnect)))
            }
            AppIntent::ReturnToConnection => {
                let draft = self.page.retained_draft();
                let mut form = ConnectionForm::new(draft);
                if let Some(stores) = stores {
                    Self::load_profiles_into_form(&mut form, stores.profiles);
                }
                self.page = Page::ConnectionForm(form);
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
        stores: Option<AppPlatformStores<'_>>,
    ) -> Result<AppAction, AppControllerError> {
        if self.session_slot.is_occupied() {
            return Err(AppControllerError::SessionAlreadyActive);
        }
        let Some(target) = submission.draft.target_system else {
            return self.reject_submission(submission, "target_system_required");
        };
        if submission.draft.address.trim().is_empty() {
            return self.reject_submission(submission, "address_required");
        }
        if submission.draft.port.is_none() || submission.draft.port == Some(0) {
            return self.reject_submission(submission, "port_required");
        }
        if catalog.descriptor(&submission.resolved_protocol).is_none() {
            return self.reject_submission(submission, "unregistered_protocol");
        }
        let protocol_id = match catalog.select(target, submission.draft.protocol.clone().into()) {
            Ok(protocol_id) => protocol_id,
            Err(error) => return self.reject_submission(submission, error.code()),
        };
        if protocol_id != submission.resolved_protocol {
            return self.reject_submission(submission, "protocol_resolution_mismatch");
        }
        let requirements = catalog
            .descriptor(&protocol_id)
            .expect("selected protocol must have a descriptor")
            .credential_requirements;
        if requirements.username && submission.draft.username.trim().is_empty() {
            return self.reject_submission(submission, "username_required");
        }
        if requirements.password && submission.password.is_empty() {
            return self.reject_submission(submission, "password_required");
        }
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
        let pending_profile = if submission.remember_on_this_device {
            let Some(stores) = stores else {
                return self.reject_submission(submission, "profile_storage_unavailable");
            };
            let Some(key) = ConnectionProfileKey::new(
                protocol_id.clone(),
                submission.draft.address.clone(),
                submission.draft.port.unwrap_or(0),
                submission.draft.username.clone(),
            ) else {
                return self.reject_submission(submission, "invalid_profile");
            };
            if stores
                .credentials
                .stage(session_id, &key, &submission.password)
                .is_err()
            {
                return self.reject_submission(submission, "credential_storage_failed");
            }
            Some(PendingProfileTransaction {
                session_id,
                action: PendingProfileAction::Remember(SavedConnectionProfile {
                    key,
                    target_system: target,
                    last_success_order: 0,
                }),
            })
        } else if let Some(key) = submission.selected_profile.clone() {
            if stores.is_none() {
                return self.reject_submission(submission, "profile_storage_unavailable");
            }
            Some(PendingProfileTransaction {
                session_id,
                action: PendingProfileAction::Delete(key),
            })
        } else {
            None
        };
        let permit = match self.session_slot.begin_connect(session_id) {
            Ok(permit) => permit,
            Err(_) => {
                if let (Some(stores), Some(_)) = (stores, pending_profile.as_ref()) {
                    let _ = stores.credentials.discard(session_id);
                }
                return Err(AppControllerError::SessionAlreadyActive);
            }
        };

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
        self.pending_profile = pending_profile;
        self.reset_session_bound_state();
        self.page = Page::Connecting {
            draft,
            stage: ConnectionStage::Connecting,
            diagnostics: None,
        };
        Ok(AppAction::StartSession(request, permit))
    }

    fn select_saved_profile(
        &mut self,
        key: ConnectionProfileKey,
        credentials: &dyn SecureCredentialStore,
    ) {
        let Some(form) = self.connection_form_mut() else {
            return;
        };
        let Some(profile) = form
            .profiles
            .iter()
            .find(|profile| profile.key == key)
            .cloned()
        else {
            form.set_profile_storage_error("saved_profile_not_found");
            return;
        };
        form.select_profile_metadata(&profile);
        match credentials.load(&key) {
            Ok(Some(password)) => form.set_loaded_password(password),
            Ok(None) => form.set_password_error("saved_credential_unavailable"),
            Err(_) => form.set_password_error("credential_storage_failed"),
        }
    }

    fn reject_submission(
        &mut self,
        submission: ConnectionSubmission,
        code: &'static str,
    ) -> Result<AppAction, AppControllerError> {
        let profiles = match &mut self.page {
            Page::ConnectionForm(form) => std::mem::take(&mut form.profiles),
            _ => Vec::new(),
        };
        let remember_on_this_device = submission.remember_on_this_device;
        let selected_profile = submission.selected_profile.clone();
        let mut form = ConnectionForm::new(submission.draft);
        form.set_profiles(profiles);
        form.remember_on_this_device = remember_on_this_device;
        form.selected_profile = selected_profile;
        form.set_password(submission.password);
        form.set_validation_error(code);
        self.page = Page::ConnectionForm(form);
        Err(AppControllerError::InvalidSubmission(code))
    }

    pub fn effective_capabilities(&self) -> SessionCapabilities {
        self.protocol_capabilities
            .intersection(platform_capabilities(self.platform_capabilities))
            .intersection(self.policy.as_capabilities())
    }

    pub fn set_platform_capabilities(&mut self, capabilities: PlatformCapabilities) {
        self.platform_capabilities = capabilities;
        self.refresh_presented_capabilities();
    }

    pub fn set_product_policy(&mut self, policy: ProductPolicy) {
        self.policy = policy;
        self.refresh_presented_capabilities();
    }

    fn refresh_presented_capabilities(&mut self) {
        let effective = self.effective_capabilities();
        if let Page::RemoteSession { capabilities, .. } = &mut self.page {
            *capabilities = effective;
        }
    }

    fn reset_session_bound_state(&mut self) {
        self.generation = 0;
        self.challenge = None;
        self.protocol_capabilities = SessionCapabilities::default();
        self.inbound_clipboard = None;
        self.audio_state = AudioState::Unavailable;
        self.profile_persistence_failed = false;
    }

    /// 取走最新的入站剪贴板；数据只在内存中短暂聚合，不持久化。
    pub fn take_inbound_clipboard(&mut self) -> Option<ClipboardPayload> {
        self.inbound_clipboard.take()
    }

    pub fn audio_state(&self) -> &AudioState {
        &self.audio_state
    }

    pub fn profile_persistence_warning(&self) -> Option<&'static str> {
        self.profile_persistence_failed
            .then_some(PROFILE_PERSISTENCE_WARNING)
    }

    #[cfg(test)]
    pub(crate) fn handle_session_event(&mut self, event: SessionEvent) {
        self.handle_session_event_internal(event, None);
    }

    pub fn handle_session_event_with_stores(
        &mut self,
        session_id: SessionId,
        event: SessionEvent,
        stores: AppPlatformStores<'_>,
    ) {
        if self.session_id != Some(session_id) {
            return;
        }
        self.handle_session_event_internal(event, Some(stores));
    }

    fn handle_session_event_internal(
        &mut self,
        event: SessionEvent,
        stores: Option<AppPlatformStores<'_>>,
    ) {
        if let SessionEvent::Error(error) = &event {
            self.handle_terminal_failure(error.code(), stores);
            return;
        }
        if let SessionEvent::Closed(exit) = &event {
            match exit {
                frd_protocol_api::ProtocolExit::Closed => self.handle_normal_close(stores),
                frd_protocol_api::ProtocolExit::Failed(error) => {
                    self.handle_terminal_failure(error.code(), stores)
                }
            }
            return;
        }
        if matches!(
            self.page,
            Page::ConnectionForm(_) | Page::Disconnecting { .. } | Page::Failed { .. }
        ) {
            return;
        }
        match event {
            SessionEvent::StageChanged(stage) => {
                if stage == ConnectionStage::Disconnecting {
                    self.handle_disconnect_stage(stores);
                    return;
                }
                if matches!(self.page, Page::RemoteSession { .. }) {
                    return;
                }
                let persistence_failed = if stage == ConnectionStage::TransportReady {
                    self.session_id.and_then(|session_id| {
                        stores.and_then(|stores| self.finish_pending_profile(session_id, stores))
                    })
                } else {
                    None
                };
                self.profile_persistence_failed |= persistence_failed.unwrap_or(false);
                if stage == ConnectionStage::TransportReady {
                    if let Some(session_id) = self.session_id {
                        let _ = self.session_slot.mark_active(session_id);
                    }
                }
                let draft = self.page.retained_draft();
                self.page = if stage == ConnectionStage::TransportReady {
                    Page::AwaitingFirstFrame {
                        draft,
                        stage,
                        diagnostics: self.profile_persistence_warning().map(str::to_owned),
                    }
                } else {
                    Page::Connecting {
                        draft,
                        stage,
                        diagnostics: self.profile_persistence_warning().map(str::to_owned),
                    }
                };
            }
            SessionEvent::CapabilitiesChanged(capabilities) => {
                self.protocol_capabilities = capabilities;
                self.refresh_presented_capabilities();
            }
            SessionEvent::Clipboard(payload) => {
                self.inbound_clipboard = Some(payload);
            }
            SessionEvent::AudioState(state) => {
                if state == AudioState::Failed {
                    self.platform_capabilities.remote_audio = false;
                    self.refresh_presented_capabilities();
                }
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
                    diagnostics: self.profile_persistence_warning().map(str::to_owned),
                };
            }
            SessionEvent::ServerIdentityChallenge(challenge) => {
                self.handle_server_identity_challenge(challenge);
            }
            SessionEvent::Error(_) | SessionEvent::Closed(_) => {
                unreachable!("terminal handled above")
            }
            _ => {}
        }
    }

    fn handle_terminal_failure(&mut self, code: &str, stores: Option<AppPlatformStores<'_>>) {
        let Some(session_id) = self.session_id else {
            return;
        };
        if self.session_slot.begin_terminal(session_id).is_err() {
            return;
        }
        if let Some(stores) = stores {
            self.discard_pending_profile(session_id, stores.credentials);
        }
        let draft = self.page.retained_draft();
        let already_failed = matches!(self.page, Page::Failed { .. });
        self.reset_session_bound_state();
        if !already_failed {
            self.page = Page::Failed {
                draft,
                code: code.to_owned(),
            };
        }
    }

    fn finish_pending_profile(
        &mut self,
        session_id: SessionId,
        stores: AppPlatformStores<'_>,
    ) -> Option<bool> {
        let Some(pending) = self.pending_profile.take() else {
            return None;
        };
        if pending.session_id != session_id {
            self.pending_profile = Some(pending);
            return None;
        }
        match pending.action {
            PendingProfileAction::Remember(mut profile) => {
                let next_order = match stores.profiles.list().ok().and_then(|profiles| {
                    profiles
                        .iter()
                        .map(|profile| profile.last_success_order)
                        .max()
                        .unwrap_or(0)
                        .checked_add(1)
                }) {
                    Some(order) => order,
                    None => {
                        let _ = stores.credentials.discard(session_id);
                        return Some(true);
                    }
                };
                profile.last_success_order = next_order;
                let previous_credential = match stores.credentials.load(&profile.key) {
                    Ok(previous) => previous,
                    Err(_) => {
                        let _ = stores.credentials.discard(session_id);
                        return Some(true);
                    }
                };
                if stores.credentials.commit(session_id, &profile.key).is_err() {
                    let _ = stores.credentials.discard(session_id);
                    return Some(true);
                }
                if stores.profiles.upsert(&profile).is_err() {
                    // 该补偿只覆盖本进程内的部分失败；进程崩溃恢复需要独立事务日志。
                    Self::compensate_credential_commit(
                        session_id,
                        &profile.key,
                        previous_credential,
                        stores.credentials,
                    );
                    return Some(true);
                }
                Some(false)
            }
            PendingProfileAction::Delete(key) => {
                if stores.credentials.delete(&key).is_err() {
                    return Some(true);
                }
                Some(stores.profiles.delete(&key).is_err())
            }
        }
    }

    fn compensate_credential_commit(
        session_id: SessionId,
        key: &ConnectionProfileKey,
        previous_credential: Option<frd_core::SecretBuffer>,
        credentials: &dyn SecureCredentialStore,
    ) {
        let restored = previous_credential.is_some_and(|previous| {
            credentials.stage(session_id, key, &previous).is_ok()
                && credentials.commit(session_id, key).is_ok()
        });
        if !restored {
            let _ = credentials.delete(key);
        }
        let _ = credentials.discard(session_id);
    }

    fn discard_pending_profile(
        &mut self,
        session_id: SessionId,
        credentials: &dyn SecureCredentialStore,
    ) {
        let Some(pending) = self.pending_profile.take() else {
            return;
        };
        if pending.session_id != session_id {
            self.pending_profile = Some(pending);
            return;
        }
        let _ = credentials.discard(session_id);
    }

    fn handle_normal_close(&mut self, stores: Option<AppPlatformStores<'_>>) {
        let Some(session_id) = self.session_id else {
            return;
        };
        if self.session_slot.begin_terminal(session_id).is_err() {
            return;
        }
        if let Some(stores) = stores {
            self.discard_pending_profile(session_id, stores.credentials);
        }
        if matches!(self.page, Page::Failed { .. }) {
            return;
        }
        let draft = self.page.retained_draft();
        self.reset_session_bound_state();
        self.page = Page::Disconnecting { draft };
    }

    fn handle_disconnect_stage(&mut self, stores: Option<AppPlatformStores<'_>>) {
        let Some(session_id) = self.session_id else {
            return;
        };
        if self.session_slot.begin_disconnect(session_id).is_err() {
            return;
        }
        if let Some(stores) = stores {
            self.discard_pending_profile(session_id, stores.credentials);
        }
        let draft = self.page.retained_draft();
        self.reset_session_bound_state();
        self.page = Page::Disconnecting { draft };
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
                diagnostics: self.profile_persistence_warning().map(str::to_owned),
            };
        }
    }

    /// AppIntent 不承载热路径输入；仅当前已呈现会话可路由 protocol-neutral 输入。
    pub fn route_input(&self, event: InputEvent) -> Option<SessionCommand> {
        let session_id = self.session_id?;
        if matches!(
            event,
            InputEvent::PhysicalKey { .. } | InputEvent::Text { .. }
        ) && !self.effective_capabilities().text_input
        {
            return None;
        }
        matches!(self.page, Page::RemoteSession { .. }).then_some(SessionCommand::Input(
            SessionInput {
                session_id,
                generation: self.generation,
                event,
            },
        ))
    }

    pub fn handle_server_identity_challenge(&mut self, challenge: ServerIdentityChallenge) {
        if Some(challenge.session_id) == self.session_id
            && !matches!(
                self.page,
                Page::ConnectionForm(_) | Page::Disconnecting { .. } | Page::Failed { .. }
            )
        {
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
        if challenge.validation.is_pin_mismatch() && decision != ServerIdentityDecision::Reject {
            return Err(IdentityDecisionError::PinMismatch);
        }
        if decision == ServerIdentityDecision::TrustAndRemember {
            if !challenge.validation.is_unknown() {
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
enum SlotPhase {
    Connecting,
    Active,
    Disconnecting,
    CleanupPending,
}

struct SlotState {
    session_id: SessionId,
    owner: SessionStartOwner,
    phase: SlotPhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisconnectTransition {
    Started,
    AlreadyInProgress,
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

    pub fn begin_connect(
        &mut self,
        session_id: SessionId,
    ) -> Result<SessionStartPermit, ActiveSessionError> {
        if self.state.is_some() {
            return Err(ActiveSessionError::Occupied);
        }
        let (owner, permit) = reserve_session_start(session_id);
        self.state = Some(SlotState {
            session_id,
            owner,
            phase: SlotPhase::Connecting,
        });
        Ok(permit)
    }

    pub fn mark_active(&mut self, session_id: SessionId) -> Result<(), ActiveSessionError> {
        match self.state.as_mut() {
            Some(state)
                if state.session_id == session_id && state.phase == SlotPhase::Connecting =>
            {
                state.phase = SlotPhase::Active;
                Ok(())
            }
            _ => Err(ActiveSessionError::InvalidTransition),
        }
    }

    /// 仅接受同一 Connecting reservation 的一次性 launcher rollback abort。
    pub fn abort_connect(&mut self, abort: &SessionStartAbort) -> Result<(), ActiveSessionError> {
        match self.state.as_ref() {
            Some(state)
                if state.session_id == abort.session_id()
                    && state.phase == SlotPhase::Connecting
                    && abort.consume_for(&state.owner) =>
            {
                self.state = None;
                Ok(())
            }
            _ => Err(ActiveSessionError::InvalidTransition),
        }
    }

    pub fn begin_disconnect(
        &mut self,
        session_id: SessionId,
    ) -> Result<DisconnectTransition, ActiveSessionError> {
        match self.state.as_mut() {
            Some(state)
                if state.session_id == session_id
                    && matches!(state.phase, SlotPhase::Connecting | SlotPhase::Active) =>
            {
                state.phase = SlotPhase::Disconnecting;
                Ok(DisconnectTransition::Started)
            }
            Some(state)
                if state.session_id == session_id
                    && matches!(
                        state.phase,
                        SlotPhase::Disconnecting | SlotPhase::CleanupPending
                    ) =>
            {
                Ok(DisconnectTransition::AlreadyInProgress)
            }
            _ => Err(ActiveSessionError::InvalidTransition),
        }
    }

    fn begin_terminal(&mut self, session_id: SessionId) -> Result<(), ActiveSessionError> {
        match self.state.as_mut() {
            Some(state)
                if state.session_id == session_id
                    && matches!(
                        state.phase,
                        SlotPhase::Connecting | SlotPhase::Active | SlotPhase::Disconnecting
                    ) =>
            {
                state.phase = SlotPhase::CleanupPending;
                Ok(())
            }
            Some(state)
                if state.session_id == session_id && state.phase == SlotPhase::CleanupPending =>
            {
                Ok(())
            }
            _ => Err(ActiveSessionError::InvalidTransition),
        }
    }

    /// 仅接受 `SessionCoordinator` 在完整资源回收后签发的完成能力。
    pub fn finish_cleanup(&mut self, cleanup: &CleanupComplete) -> Result<(), ActiveSessionError> {
        match self.state.as_ref() {
            Some(state)
                if state.session_id == cleanup.session_id()
                    && matches!(
                        state.phase,
                        SlotPhase::Disconnecting | SlotPhase::CleanupPending
                    )
                    && cleanup.matches_owner(&state.owner) =>
            {
                self.state = None;
                Ok(())
            }
            _ => Err(ActiveSessionError::InvalidTransition),
        }
    }
}
