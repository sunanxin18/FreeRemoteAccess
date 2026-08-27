mod controller;

pub use controller::{
    ActiveSessionError, ActiveSessionSlot, AppAction, AppController, AppControllerError, AppLaunch,
    IdentityDecisionError, ProductPolicy,
};
use frd_core::SessionId;
pub use frd_protocol_api::PresentationEvent;
use frd_protocol_api::ServerIdentityDecision;
use frd_ui_model::ConnectionSubmission;
pub use frd_ui_model::Page as AppPage;

/// UI 只在此低频语义边界驱动会话；不包含输入或帧数据。
pub enum AppIntent {
    Connect(ConnectionSubmission),
    CancelConnect,
    Disconnect,
    ReturnToConnection,
    ResolveServerIdentity {
        session_id: SessionId,
        challenge_id: u64,
        decision: ServerIdentityDecision,
    },
}

impl From<ConnectionSubmission> for AppIntent {
    fn from(submission: ConnectionSubmission) -> Self {
        Self::Connect(submission)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use frd_core::{CredentialProviderId, PixelSize, SecretBuffer, SessionId, TargetSystem};
    use frd_frame::FrameCompleteness;
    use frd_platform_api::{
        CredentialProvider, PlatformCapabilities, PlatformError, ServerIdentityStore,
    };

    use frd_protocol_api::{
        AudioState, ClipboardPayload, ConnectionStage, Endpoint, ProtocolCatalog, ProtocolError,
        ProtocolId, ServerIdentityChallenge, ServerIdentityDecision, ServerIdentityValidation,
        SessionEvent,
    };
    use frd_session::{CleanupError, CleanupOperations, SessionCoordinator};

    use frd_ui_model::{ConnectionDraft, ConnectionForm, LaunchOptions, ProtocolChoice};

    use super::{
        ActiveSessionSlot, AppAction, AppController, AppControllerError, AppIntent, AppLaunch,
        AppPage, PresentationEvent, ProductPolicy,
    };

    #[test]
    fn controller_transitions_connection_through_remote_session_and_failed() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller = AppController::connection_form(complete_form());
        let intent = controller
            .connection_form_mut()
            .expect("connection form is editable")
            .take_connect_intent(&catalog)
            .expect("complete form emits connect intent");

        let request = match controller
            .handle_intent(intent, &catalog, &store)
            .expect("connect intent is accepted")
            .expect("connect starts one worker")
        {
            AppAction::StartSession(request) => request,
            AppAction::SessionCommand(_) => panic!("connect must start through the single path"),
        };
        let session_id = request.session_id;
        assert!(matches!(controller.page(), AppPage::Connecting { .. }));
        drop(request);

        controller
            .handle_session_event(SessionEvent::StageChanged(ConnectionStage::TransportReady));
        assert!(matches!(
            controller.page(),
            AppPage::AwaitingFirstFrame { .. }
        ));
        controller.handle_session_event(SessionEvent::SurfaceGenerationChanged {
            session_id,
            generation: 1,
            size: PixelSize::new(800, 600).expect("valid size"),
        });
        controller.handle_presentation(PresentationEvent::FramePresented {
            session_id,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::FullBaseline,
        });
        assert!(matches!(controller.page(), AppPage::RemoteSession { .. }));

        controller.handle_session_event(SessionEvent::Error(ProtocolError::Adapter {
            protocol_id: ProtocolId::apple_hpss_mvs(),
            code: "test_connection_failed",
        }));
        assert!(matches!(
            controller.page(),
            AppPage::Failed { code, .. } if code == "test_connection_failed"
        ));

        controller
            .handle_intent(AppIntent::ReturnToConnection, &catalog, &store)
            .expect("failed page returns to the retained form");
        let AppPage::ConnectionForm(form) = controller.page() else {
            panic!("failure returns to the connection form");
        };
        assert_eq!(form.draft.username, "test-user");
        assert!(form.password_is_empty());
    }

    #[test]
    fn connect_resolves_auto_and_loads_the_saved_pin_before_starting_a_worker() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::with_saved_pin([0x33; 32]);
        let mut controller = AppController::connection_form(complete_form());
        let submission = controller
            .connection_form_mut()
            .expect("connection form is editable")
            .take_submission(&catalog)
            .expect("complete form submits");

        let action = controller
            .handle_intent(submission, &catalog, &store)
            .expect("connect intent is accepted")
            .expect("connect starts one worker");
        let AppAction::StartSession(request) = action else {
            panic!("connect starts through the session request path");
        };

        assert_eq!(request.protocol_id, ProtocolId::apple_hpss_mvs());
        assert_eq!(request.saved_server_pin, Some([0x33; 32]));
    }

    #[test]
    fn effective_capabilities_are_the_protocol_platform_and_policy_intersection() {
        let session_id = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(session_id, 1);
        controller.handle_session_event(SessionEvent::CapabilitiesChanged(
            frd_protocol_api::SessionCapabilities {
                dynamic_resolution: true,
                clipboard_read: true,
                clipboard_write: true,
                remote_audio: true,
                text_input: true,
            },
        ));
        controller.set_platform_capabilities(PlatformCapabilities {
            dynamic_resolution: true,
            clipboard_read: true,
            clipboard_write: false,
            remote_audio: true,
            text_input: false,
        });
        controller.set_product_policy(ProductPolicy {
            dynamic_resolution: true,
            clipboard_read: false,
            clipboard_write: true,
            remote_audio: true,
            text_input: true,
        });

        assert_eq!(
            controller.effective_capabilities(),
            frd_protocol_api::SessionCapabilities {
                dynamic_resolution: true,
                clipboard_read: false,
                clipboard_write: false,
                remote_audio: true,
                text_input: false,
            }
        );
    }

    #[test]
    fn controller_rejects_a_second_active_connection() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller = AppController::connection_form(complete_form());
        let first = complete_form()
            .take_connect_intent(&catalog)
            .expect("first intent");
        let second = complete_form()
            .take_connect_intent(&catalog)
            .expect("second intent");

        let first_action = controller
            .handle_intent(first, &catalog, &store)
            .expect("first connection is accepted");
        assert!(matches!(
            controller.handle_intent(second, &catalog, &store),
            Err(AppControllerError::SessionAlreadyActive)
        ));
        drop(first_action);
    }

    #[test]
    fn controller_allows_reconnect_only_after_coordinator_cleanup_completes() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller = AppController::connection_form(complete_form());
        let first = complete_form()
            .take_submission(&catalog)
            .expect("first submission");
        let AppAction::StartSession(first_request) = controller
            .handle_intent(first, &catalog, &store)
            .expect("first connection")
            .expect("session start action")
        else {
            panic!("first connection starts a session");
        };
        let first_session_id = first_request.session_id;
        controller
            .handle_intent(AppIntent::Disconnect, &catalog, &store)
            .expect("disconnect begins");

        let blocked = complete_form()
            .take_submission(&catalog)
            .expect("blocked submission");
        assert!(matches!(
            controller.handle_intent(blocked, &catalog, &store),
            Err(AppControllerError::SessionAlreadyActive)
        ));

        let mut coordinator = SessionCoordinator::new(ProtocolCatalog::new([]));
        let mut cleanup = RecordingCleanup::default();
        let complete = coordinator
            .complete_cleanup(first_session_id, &mut cleanup)
            .expect("all session resources are reclaimed");
        controller
            .finish_session_cleanup(complete)
            .expect("cleanup capability releases the controller slot");

        let reconnect = complete_form()
            .take_submission(&catalog)
            .expect("reconnect submission");
        assert!(matches!(
            controller.handle_intent(reconnect, &catalog, &store),
            Ok(Some(AppAction::StartSession(_)))
        ));
    }

    #[test]
    fn empty_launch_options_leave_the_connection_form_without_an_intent() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let provider = TestCredentialProvider::success();
        let mut launch = AppLaunch::new(LaunchOptions::default(), &provider, &catalog);

        assert!(launch.take_connect_intent().is_none());
        assert!(matches!(
            launch.controller().page(),
            AppPage::ConnectionForm(_)
        ));
    }

    #[test]
    fn partial_connect_launch_stays_on_the_editable_form() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let provider = TestCredentialProvider::success();
        let mut launch = AppLaunch::new(
            LaunchOptions {
                target_system: Some(TargetSystem::MacOs),
                address: Some("host.invalid".to_owned()),
                connect_when_complete: true,
                ..LaunchOptions::default()
            },
            &provider,
            &catalog,
        );

        assert!(launch.take_connect_intent().is_none());
        let AppPage::ConnectionForm(form) = launch.controller().page() else {
            panic!("partial launch must remain editable");
        };
        assert!(form.errors().port.is_some());
        assert!(form.errors().username.is_some());
        assert!(form.errors().password.is_some());
    }

    #[test]
    fn complete_launch_without_connect_only_prefills_the_form() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let provider = TestCredentialProvider::success();
        let mut options = complete_launch_options();
        options.connect_when_complete = false;
        let mut launch = AppLaunch::new(options, &provider, &catalog);

        assert!(launch.take_connect_intent().is_none());
        let AppPage::ConnectionForm(form) = launch.controller().page() else {
            panic!("prefilled launch stays on the form");
        };
        assert_eq!(form.draft.username, "provider-user");
        assert!(!form.password_is_empty());
    }

    #[test]
    fn complete_connect_launch_emits_exactly_one_connect_intent() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let provider = TestCredentialProvider::success();
        let mut launch = AppLaunch::new(complete_launch_options(), &provider, &catalog);

        assert!(launch.take_connect_intent().is_some());
        assert!(launch.take_connect_intent().is_none());
        let AppPage::ConnectionForm(form) = launch.controller().page() else {
            panic!("intent still uses the same form path");
        };
        assert!(form.password_is_empty());
    }

    #[test]
    fn failed_credential_provider_leaves_the_form_and_starts_no_worker() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let provider = TestCredentialProvider::failure();
        let mut launch = AppLaunch::new(complete_launch_options(), &provider, &catalog);

        assert!(launch.take_connect_intent().is_none());
        let AppPage::ConnectionForm(form) = launch.controller().page() else {
            panic!("provider failure must leave the form visible");
        };
        assert_eq!(
            form.errors().username.as_deref(),
            Some("credential_provider_failed")
        );
        assert_eq!(
            form.errors().password.as_deref(),
            Some("credential_provider_failed")
        );
        assert!(form.password_is_empty());
    }

    #[test]
    fn failed_provider_is_visible_even_when_launch_does_not_auto_connect() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let provider = TestCredentialProvider::failure();
        let mut options = complete_launch_options();
        options.connect_when_complete = false;
        let mut launch = AppLaunch::new(options, &provider, &catalog);

        assert!(launch.take_connect_intent().is_none());
        let AppPage::ConnectionForm(form) = launch.controller().page() else {
            panic!("provider failure keeps the editable form visible");
        };
        assert_eq!(
            form.errors().username.as_deref(),
            Some("credential_provider_failed")
        );
        assert_eq!(
            form.errors().password.as_deref(),
            Some("credential_provider_failed")
        );
    }

    fn complete_form() -> ConnectionForm {
        let mut form = ConnectionForm::new(ConnectionDraft {
            target_system: Some(TargetSystem::MacOs),
            address: "host.invalid".to_owned(),
            port: Some(5900),
            protocol: ProtocolChoice::Automatic,
            username: "test-user".to_owned(),
        });
        form.set_password(SecretBuffer::new(b"test-password".to_vec()));
        form
    }

    fn complete_launch_options() -> LaunchOptions {
        LaunchOptions {
            target_system: Some(TargetSystem::MacOs),
            address: Some("host.invalid".to_owned()),
            port: Some(5900),
            protocol: Some(ProtocolId::apple_hpss_mvs()),
            username_provider: Some(CredentialProviderId::environment()),
            password_provider: Some(CredentialProviderId::environment()),
            connect_when_complete: true,
        }
    }

    struct TestCredentialProvider {
        fails: bool,
    }

    impl TestCredentialProvider {
        fn success() -> Self {
            Self { fails: false }
        }

        fn failure() -> Self {
            Self { fails: true }
        }
    }

    impl CredentialProvider for TestCredentialProvider {
        fn load_username(&self, _: &CredentialProviderId) -> Result<String, PlatformError> {
            if self.fails {
                Err(PlatformError::CredentialProviderFailed)
            } else {
                Ok("provider-user".to_owned())
            }
        }

        fn load_password(&self, _: &CredentialProviderId) -> Result<SecretBuffer, PlatformError> {
            if self.fails {
                Err(PlatformError::CredentialProviderFailed)
            } else {
                Ok(SecretBuffer::new(b"provider-password".to_vec()))
            }
        }
    }

    #[test]
    fn active_session_slot_rejects_disconnect_for_another_session() {
        let first = SessionId::allocate();
        let other = SessionId::allocate();
        let mut slot = ActiveSessionSlot::default();

        assert!(slot.begin_connect(first).is_ok());
        assert!(slot.begin_disconnect(other).is_err());
        assert!(slot.mark_active(first).is_ok());
        assert!(slot.begin_disconnect(other).is_err());
    }

    #[test]
    fn active_session_slot_stays_occupied_after_begin_disconnect_without_cleanup_capability() {
        let first = SessionId::allocate();
        let second = SessionId::allocate();
        let mut slot = ActiveSessionSlot::default();

        assert!(slot.begin_connect(first).is_ok());
        assert!(slot.begin_connect(second).is_err());
        assert!(slot.begin_disconnect(first).is_ok());
        assert!(slot.begin_connect(second).is_err());
    }

    #[test]
    fn completed_coordinator_cleanup_releases_slot_after_ordered_resource_reclamation() {
        let first = SessionId::allocate();
        let second = SessionId::allocate();
        let mut slot = ActiveSessionSlot::default();
        let mut coordinator = SessionCoordinator::new(ProtocolCatalog::new([]));
        let mut cleanup = RecordingCleanup::default();

        slot.begin_connect(first)
            .expect("first connect is accepted");
        slot.begin_disconnect(first)
            .expect("first session begins disconnect");
        let completed = coordinator
            .complete_cleanup(first, &mut cleanup)
            .expect("all resources are reclaimed");
        slot.finish_cleanup(completed)
            .expect("matching cleanup completion releases the slot");
        assert!(slot.begin_connect(second).is_ok());
        assert_eq!(
            cleanup.calls,
            vec![
                "cancel",
                "shutdown_writer",
                "join_workers_and_audio",
                "dispose_mailbox"
            ]
        );
    }

    #[test]
    fn cleanup_completion_for_another_session_does_not_release_disconnect_slot() {
        let first = SessionId::allocate();
        let other = SessionId::allocate();
        let second = SessionId::allocate();
        let mut slot = ActiveSessionSlot::default();
        let mut coordinator = SessionCoordinator::new(ProtocolCatalog::new([]));
        let mut cleanup = RecordingCleanup::default();

        slot.begin_connect(first)
            .expect("first connect is accepted");
        slot.begin_disconnect(first)
            .expect("first session begins disconnect");
        let other_completion = coordinator
            .complete_cleanup(other, &mut cleanup)
            .expect("other cleanup completes");

        assert!(slot.finish_cleanup(other_completion).is_err());
        assert!(slot.begin_connect(second).is_err());
    }

    #[test]
    fn failed_coordinator_cleanup_keeps_disconnect_slot_occupied() {
        let first = SessionId::allocate();
        let second = SessionId::allocate();
        let mut slot = ActiveSessionSlot::default();
        let mut coordinator = SessionCoordinator::new(ProtocolCatalog::new([]));
        let mut cleanup = RecordingCleanup::failing_at(CleanupError::ShutdownWriter);

        slot.begin_connect(first)
            .expect("first connect is accepted");
        slot.begin_disconnect(first)
            .expect("first session begins disconnect");
        assert!(matches!(
            coordinator.complete_cleanup(first, &mut cleanup),
            Err(CleanupError::ShutdownWriter)
        ));
        assert!(slot.begin_connect(second).is_err());
        assert_eq!(
            cleanup.calls,
            vec![
                "cancel",
                "shutdown_writer",
                "join_workers_and_audio",
                "dispose_mailbox"
            ]
        );
    }

    #[test]
    fn stale_presentation_event_does_not_enter_remote_session() {
        let current = SessionId::allocate();
        let stale = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(current, 3);

        controller.handle_presentation(PresentationEvent::FramePresented {
            session_id: stale,
            generation: 3,
            revision: 9,
            completeness: FrameCompleteness::FullBaseline,
        });

        assert!(!matches!(controller.page(), AppPage::RemoteSession { .. }));
    }

    #[test]
    fn current_full_baseline_presentation_enters_remote_session() {
        let current = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(current, 3);

        controller.handle_presentation(PresentationEvent::FramePresented {
            session_id: current,
            generation: 3,
            revision: 9,
            completeness: FrameCompleteness::FullBaseline,
        });

        assert!(matches!(controller.page(), AppPage::RemoteSession { .. }));
    }

    #[test]
    fn current_incremental_presentation_does_not_enter_remote_session() {
        let current = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(current, 3);

        controller.handle_presentation(PresentationEvent::FramePresented {
            session_id: current,
            generation: 3,
            revision: 9,
            completeness: FrameCompleteness::Incremental,
        });

        assert!(!matches!(controller.page(), AppPage::RemoteSession { .. }));
    }

    #[test]
    fn presentation_after_terminal_failure_does_not_resurrect_the_session_page() {
        let current = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(current, 3);
        controller.handle_session_event(SessionEvent::Error(ProtocolError::Adapter {
            protocol_id: ProtocolId::apple_hpss_mvs(),
            code: "terminal_test_failure",
        }));

        controller.handle_presentation(PresentationEvent::FramePresented {
            session_id: current,
            generation: 3,
            revision: 9,
            completeness: FrameCompleteness::FullBaseline,
        });

        assert!(matches!(
            controller.page(),
            AppPage::Failed { code, .. } if code == "terminal_test_failure"
        ));
    }

    #[test]
    fn stage_change_after_terminal_failure_does_not_resurrect_connecting() {
        let current = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(current, 3);
        controller.handle_session_event(SessionEvent::Error(ProtocolError::Adapter {
            protocol_id: ProtocolId::apple_hpss_mvs(),
            code: "terminal_test_failure",
        }));

        controller.handle_session_event(SessionEvent::StageChanged(ConnectionStage::Connecting));

        assert!(matches!(
            controller.page(),
            AppPage::Failed { code, .. } if code == "terminal_test_failure"
        ));
    }

    #[test]
    fn controller_keeps_latest_inbound_clipboard_until_taken() {
        let session_id = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(session_id, 1);

        controller.handle_session_event(SessionEvent::Clipboard(ClipboardPayload::new(vec![0x11])));
        controller.handle_session_event(SessionEvent::Clipboard(ClipboardPayload::new(vec![0x22])));

        assert_eq!(
            controller
                .take_inbound_clipboard()
                .expect("latest clipboard is available")
                .as_bytes(),
            &[0x22]
        );
        assert!(controller.take_inbound_clipboard().is_none());
    }

    #[test]
    fn controller_aggregates_latest_audio_state() {
        let session_id = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(session_id, 1);

        controller.handle_session_event(SessionEvent::AudioState(AudioState::Starting));
        controller.handle_session_event(SessionEvent::AudioState(AudioState::Playing));

        assert_eq!(controller.audio_state(), &AudioState::Playing);
    }

    #[test]
    fn current_challenge_decision_is_accepted_but_stale_one_is_rejected() {
        let session_id = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(session_id, 1);
        controller.handle_server_identity_challenge(challenge(session_id, 7, [0x11; 32]));

        assert!(controller
            .resolve_server_identity(session_id, 7, ServerIdentityDecision::TrustOnce)
            .is_ok());
        assert!(controller
            .resolve_server_identity(session_id, 6, ServerIdentityDecision::TrustOnce)
            .is_err());
    }

    #[test]
    fn remember_persists_only_the_currently_challenged_pin() {
        let session_id = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(session_id, 1);
        let store = RecordingStore::default();
        controller.handle_server_identity_challenge(challenge(session_id, 7, [0x11; 32]));

        assert!(controller
            .resolve_server_identity_with_store(
                session_id,
                6,
                ServerIdentityDecision::TrustAndRemember,
                &store,
            )
            .is_err());
        assert!(store.stores.lock().expect("store lock").is_empty());

        controller
            .resolve_server_identity_with_store(
                session_id,
                7,
                ServerIdentityDecision::TrustAndRemember,
                &store,
            )
            .expect("current challenge is persisted");
        assert_eq!(
            *store.stores.lock().expect("store lock"),
            vec![(
                ProtocolId::apple_hpss_mvs(),
                Endpoint::new("mac.example", 5900).expect("valid endpoint"),
                [0x11; 32],
            )]
        );
    }

    #[test]
    fn pin_mismatch_rejects_trust_decisions_without_storing_or_emitting_a_command() {
        let session_id = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(session_id, 1);
        let store = RecordingStore::default();
        controller.handle_server_identity_challenge(challenge_with_validation(
            session_id,
            7,
            [0x22; 32],
            ServerIdentityValidation::PinMismatch,
        ));

        assert!(matches!(
            controller.resolve_server_identity(session_id, 7, ServerIdentityDecision::TrustOnce),
            Err(super::IdentityDecisionError::PinMismatch)
        ));
        assert!(matches!(
            controller.resolve_server_identity_with_store(
                session_id,
                7,
                ServerIdentityDecision::TrustAndRemember,
                &store,
            ),
            Err(super::IdentityDecisionError::PinMismatch)
        ));
        assert!(store.stores.lock().expect("store lock").is_empty());
        assert!(controller
            .resolve_server_identity(session_id, 7, ServerIdentityDecision::Reject)
            .is_ok());
    }

    #[test]
    fn remember_rejects_current_non_unknown_identity_without_storing() {
        let session_id = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(session_id, 1);
        let store = RecordingStore::default();
        controller.handle_server_identity_challenge(challenge_with_validation(
            session_id,
            7,
            [0x11; 32],
            ServerIdentityValidation::PinMatched,
        ));

        assert!(matches!(
            controller.resolve_server_identity_with_store(
                session_id,
                7,
                ServerIdentityDecision::TrustAndRemember,
                &store,
            ),
            Err(super::IdentityDecisionError::TrustAndRememberRequiresUnknown)
        ));
        assert!(store.stores.lock().expect("store lock").is_empty());
    }

    fn challenge(
        session_id: SessionId,
        challenge_id: u64,
        pin: [u8; 32],
    ) -> ServerIdentityChallenge {
        challenge_with_validation(
            session_id,
            challenge_id,
            pin,
            ServerIdentityValidation::Unknown,
        )
    }

    fn challenge_with_validation(
        session_id: SessionId,
        challenge_id: u64,
        pin: [u8; 32],
        validation: ServerIdentityValidation,
    ) -> ServerIdentityChallenge {
        ServerIdentityChallenge {
            session_id,
            challenge_id,
            protocol_id: ProtocolId::apple_hpss_mvs(),
            endpoint: Endpoint::new("mac.example", 5900).expect("valid endpoint"),
            sha256_fingerprint: pin,
            subject: "mac.example".to_owned(),
            issuer: "local test".to_owned(),
            validation,
        }
    }

    #[derive(Default)]
    struct RecordingStore {
        saved_pin: Option<[u8; 32]>,
        stores: Mutex<Vec<(ProtocolId, Endpoint, [u8; 32])>>,
    }

    impl RecordingStore {
        fn with_saved_pin(saved_pin: [u8; 32]) -> Self {
            Self {
                saved_pin: Some(saved_pin),
                stores: Mutex::new(Vec::new()),
            }
        }
    }

    impl ServerIdentityStore for RecordingStore {
        fn load_pin(
            &self,
            _: &ProtocolId,
            _: &Endpoint,
        ) -> Result<Option<[u8; 32]>, PlatformError> {
            Ok(self.saved_pin)
        }

        fn store_pin(
            &self,
            protocol: &ProtocolId,
            endpoint: &Endpoint,
            pin: [u8; 32],
        ) -> Result<(), PlatformError> {
            self.stores
                .lock()
                .expect("store lock")
                .push((protocol.clone(), endpoint.clone(), pin));
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingCleanup {
        calls: Vec<&'static str>,
        failure: Option<CleanupError>,
    }

    impl RecordingCleanup {
        fn failing_at(failure: CleanupError) -> Self {
            Self {
                calls: Vec::new(),
                failure: Some(failure),
            }
        }

        fn result(&self, error: CleanupError) -> Result<(), CleanupError> {
            (self.failure == Some(error))
                .then_some(error)
                .map_or(Ok(()), Err)
        }
    }

    impl CleanupOperations for RecordingCleanup {
        fn cancel(&mut self) -> Result<(), CleanupError> {
            self.calls.push("cancel");
            self.result(CleanupError::Cancel)
        }

        fn shutdown_writer(&mut self) -> Result<(), CleanupError> {
            self.calls.push("shutdown_writer");
            self.result(CleanupError::ShutdownWriter)
        }

        fn join_workers_and_audio(&mut self) -> Result<(), CleanupError> {
            self.calls.push("join_workers_and_audio");
            self.result(CleanupError::JoinWorkersAndAudio)
        }

        fn dispose_mailbox(&mut self) -> Result<(), CleanupError> {
            self.calls.push("dispose_mailbox");
            self.result(CleanupError::DisposeMailbox)
        }
    }
}
