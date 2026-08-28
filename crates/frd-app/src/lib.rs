mod controller;

pub use controller::{
    ActiveSessionError, ActiveSessionSlot, AppAction, AppController, AppControllerError, AppLaunch,
    AppPlatformStores, DisconnectTransition, IdentityDecisionError, ProductPolicy,
};
use frd_core::SessionId;
use frd_platform_api::ConnectionProfileKey;
pub use frd_protocol_api::PresentationEvent;
use frd_protocol_api::ServerIdentityDecision;
use frd_ui_model::ConnectionSubmission;
pub use frd_ui_model::Page as AppPage;

/// UI 只在此低频语义边界驱动会话；不包含输入或帧数据。
pub enum AppIntent {
    Connect(ConnectionSubmission),
    SelectSavedProfile(ConnectionProfileKey),
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

    use frd_core::{
        CredentialProviderId, InputEvent, KeyState, Modifiers, PhysicalKeyCode, PixelSize,
        SecretBuffer, SessionId, TargetSystem,
    };
    use frd_frame::FrameCompleteness;
    use frd_platform_api::{
        ConnectionProfileKey, ConnectionProfileStore, CredentialProvider, PlatformCapabilities,
        PlatformError, SavedConnectionProfile, SecureCredentialStore, ServerIdentityStore,
    };

    use frd_protocol_api::{
        evaluate_server_identity, AudioState, ClipboardPayload, ConnectRequest, ConnectionStage,
        Endpoint, ProtocolCatalog, ProtocolError, ProtocolId, ServerIdentityChallenge,
        ServerIdentityDecision, ServerIdentityValidation, SessionCommand, SessionEvent,
    };
    use frd_session::{
        CleanupError, CleanupOperations, SessionCleanupHandle, SessionCoordinator,
        SessionStartOutcome, SessionStartPermit,
    };

    use frd_ui_model::{ConnectionDraft, ConnectionForm, LaunchOptions, ProtocolChoice};

    use super::{
        ActiveSessionSlot, AppAction, AppController, AppControllerError, AppIntent, AppLaunch,
        AppPage, AppPlatformStores, PresentationEvent, ProductPolicy,
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
            AppAction::StartSession(request, _permit) => request,
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
        let AppAction::StartSession(request, _permit) = action else {
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
    fn presented_session_drops_physical_keys_and_text_when_text_input_is_not_negotiated() {
        let session_id = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(session_id, 1);
        controller.set_platform_capabilities(PlatformCapabilities {
            dynamic_resolution: true,
            clipboard_read: false,
            clipboard_write: false,
            remote_audio: true,
            text_input: true,
        });
        controller.set_product_policy(ProductPolicy {
            dynamic_resolution: true,
            clipboard_read: false,
            clipboard_write: false,
            remote_audio: true,
            text_input: true,
        });
        controller.handle_session_event(SessionEvent::CapabilitiesChanged(
            frd_protocol_api::SessionCapabilities {
                dynamic_resolution: true,
                clipboard_read: false,
                clipboard_write: false,
                remote_audio: true,
                text_input: false,
            },
        ));
        controller.handle_presentation(PresentationEvent::FramePresented {
            session_id,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::FullBaseline,
        });

        assert!(controller
            .route_input(InputEvent::PhysicalKey {
                code: PhysicalKeyCode(30),
                state: KeyState::Pressed,
                modifiers: Modifiers::default(),
            })
            .is_none());
        assert!(controller
            .route_input(InputEvent::Text {
                utf8: "x".to_owned(),
            })
            .is_none());
        assert!(controller.route_input(InputEvent::ReleaseAll).is_some());
    }

    #[test]
    fn failed_audio_output_downgrades_the_presented_platform_capability() {
        let session_id = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(session_id, 1);
        controller.set_platform_capabilities(PlatformCapabilities {
            dynamic_resolution: true,
            clipboard_read: false,
            clipboard_write: false,
            remote_audio: true,
            text_input: false,
        });
        controller.set_product_policy(ProductPolicy {
            dynamic_resolution: true,
            clipboard_read: false,
            clipboard_write: false,
            remote_audio: true,
            text_input: false,
        });
        controller.handle_session_event(SessionEvent::CapabilitiesChanged(
            frd_protocol_api::SessionCapabilities {
                dynamic_resolution: true,
                clipboard_read: false,
                clipboard_write: false,
                remote_audio: true,
                text_input: false,
            },
        ));
        controller.handle_presentation(PresentationEvent::FramePresented {
            session_id,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::FullBaseline,
        });
        assert!(controller.effective_capabilities().remote_audio);

        controller.handle_session_event(SessionEvent::AudioState(AudioState::Failed));

        assert_eq!(controller.audio_state(), &AudioState::Failed);
        assert!(!controller.effective_capabilities().remote_audio);
        let AppPage::RemoteSession { capabilities, .. } = controller.page() else {
            panic!("audio degradation must keep the desktop session visible");
        };
        assert!(!capabilities.remote_audio);
    }

    #[test]
    fn presented_capabilities_refresh_when_platform_and_policy_change() {
        let session_id = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(session_id, 1);
        let all = frd_protocol_api::SessionCapabilities {
            dynamic_resolution: true,
            clipboard_read: true,
            clipboard_write: true,
            remote_audio: true,
            text_input: true,
        };
        controller.set_platform_capabilities(PlatformCapabilities {
            dynamic_resolution: true,
            clipboard_read: true,
            clipboard_write: true,
            remote_audio: true,
            text_input: true,
        });
        controller.set_product_policy(ProductPolicy {
            dynamic_resolution: true,
            clipboard_read: true,
            clipboard_write: true,
            remote_audio: true,
            text_input: true,
        });
        controller.handle_session_event(SessionEvent::CapabilitiesChanged(all));
        controller.handle_presentation(PresentationEvent::FramePresented {
            session_id,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::FullBaseline,
        });

        controller.set_platform_capabilities(PlatformCapabilities {
            dynamic_resolution: true,
            clipboard_read: true,
            clipboard_write: true,
            remote_audio: false,
            text_input: true,
        });
        let AppPage::RemoteSession { capabilities, .. } = controller.page() else {
            panic!("session stays presented");
        };
        assert!(!capabilities.remote_audio);

        controller.set_product_policy(ProductPolicy {
            dynamic_resolution: true,
            clipboard_read: false,
            clipboard_write: true,
            remote_audio: true,
            text_input: true,
        });
        let AppPage::RemoteSession { capabilities, .. } = controller.page() else {
            panic!("session stays presented");
        };
        assert!(!capabilities.clipboard_read);
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
    fn malformed_direct_submission_stays_editable_and_starts_no_worker() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller =
            AppController::connection_form(ConnectionForm::new(ConnectionDraft::default()));
        let malformed = frd_ui_model::ConnectionSubmission {
            draft: ConnectionDraft {
                target_system: Some(TargetSystem::MacOs),
                address: "host.invalid".to_owned(),
                port: Some(5900),
                protocol: ProtocolChoice::Automatic,
                username: String::new(),
            },
            resolved_protocol: ProtocolId::apple_hpss_mvs(),
            password: SecretBuffer::new(b"retained-password".to_vec()),
            remember_on_this_device: false,
            selected_profile: None,
        };

        assert_eq!(
            controller.handle_intent(malformed, &catalog, &store).err(),
            Some(AppControllerError::InvalidSubmission("username_required"))
        );
        let AppPage::ConnectionForm(form) = controller.page() else {
            panic!("invalid direct submission stays editable");
        };
        assert_eq!(form.errors().username.as_deref(), Some("username_required"));
        assert!(!form.password_is_empty());

        let valid = complete_form()
            .take_submission(&catalog)
            .expect("valid submission");
        assert!(matches!(
            controller.handle_intent(valid, &catalog, &store),
            Ok(Some(AppAction::StartSession(_, _)))
        ));
    }

    #[test]
    fn direct_submission_without_protocol_required_password_starts_no_worker() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller =
            AppController::connection_form(ConnectionForm::new(ConnectionDraft::default()));
        let malformed = frd_ui_model::ConnectionSubmission {
            draft: ConnectionDraft {
                target_system: Some(TargetSystem::MacOs),
                address: "host.invalid".to_owned(),
                port: Some(5900),
                protocol: ProtocolChoice::Automatic,
                username: "test-user".to_owned(),
            },
            resolved_protocol: ProtocolId::apple_hpss_mvs(),
            password: SecretBuffer::new(Vec::new()),
            remember_on_this_device: false,
            selected_profile: None,
        };

        assert_eq!(
            controller.handle_intent(malformed, &catalog, &store).err(),
            Some(AppControllerError::InvalidSubmission("password_required"))
        );
        let AppPage::ConnectionForm(form) = controller.page() else {
            panic!("invalid direct submission stays editable");
        };
        assert_eq!(form.errors().password.as_deref(), Some("password_required"));
    }

    #[test]
    fn inconsistent_explicit_and_resolved_protocol_stays_editable_without_starting() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs(), ProtocolId::rdp()]);
        let store = RecordingStore::default();
        let mut controller =
            AppController::connection_form(ConnectionForm::new(ConnectionDraft::default()));
        let submission = frd_ui_model::ConnectionSubmission {
            draft: ConnectionDraft {
                target_system: Some(TargetSystem::Custom),
                address: "host.invalid".to_owned(),
                port: Some(5900),
                protocol: ProtocolChoice::Explicit(ProtocolId::rdp()),
                username: "test-user".to_owned(),
            },
            resolved_protocol: ProtocolId::apple_hpss_mvs(),
            password: SecretBuffer::new(b"test-password".to_vec()),
            remember_on_this_device: false,
            selected_profile: None,
        };

        assert_eq!(
            controller.handle_intent(submission, &catalog, &store).err(),
            Some(AppControllerError::InvalidSubmission(
                "protocol_resolution_mismatch"
            ))
        );
        let AppPage::ConnectionForm(form) = controller.page() else {
            panic!("inconsistent protocol stays editable");
        };
        assert_eq!(
            form.errors().protocol.as_deref(),
            Some("protocol_resolution_mismatch")
        );
        assert_eq!(store.load_count(), 0);
    }

    #[test]
    fn unregistered_resolved_protocol_stays_editable_without_starting() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller =
            AppController::connection_form(ConnectionForm::new(ConnectionDraft::default()));
        let submission = frd_ui_model::ConnectionSubmission {
            draft: ConnectionDraft {
                target_system: Some(TargetSystem::MacOs),
                address: "host.invalid".to_owned(),
                port: Some(5900),
                protocol: ProtocolChoice::Automatic,
                username: "test-user".to_owned(),
            },
            resolved_protocol: ProtocolId::new("unregistered-test").expect("valid protocol id"),
            password: SecretBuffer::new(b"test-password".to_vec()),
            remember_on_this_device: false,
            selected_profile: None,
        };

        assert_eq!(
            controller.handle_intent(submission, &catalog, &store).err(),
            Some(AppControllerError::InvalidSubmission(
                "unregistered_protocol"
            ))
        );
        let AppPage::ConnectionForm(form) = controller.page() else {
            panic!("unregistered protocol stays editable");
        };
        assert_eq!(
            form.errors().protocol.as_deref(),
            Some("unregistered_protocol")
        );
        assert_eq!(store.load_count(), 0);
    }

    #[test]
    fn controller_allows_reconnect_only_after_coordinator_cleanup_completes() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller = AppController::connection_form(complete_form());
        let first = complete_form()
            .take_submission(&catalog)
            .expect("first submission");
        let AppAction::StartSession(first_request, first_permit) = controller
            .handle_intent(first, &catalog, &store)
            .expect("first connection")
            .expect("session start action")
        else {
            panic!("first connection starts a session");
        };
        let mut lifecycle = StartedTestSession::from_request(
            first_permit,
            first_request,
            RecordingCleanup::default(),
        );
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

        let complete = lifecycle
            .complete()
            .expect("started session resources are reclaimed");
        controller
            .finish_session_cleanup(complete)
            .expect("cleanup capability releases the controller slot");

        let reconnect = complete_form()
            .take_submission(&catalog)
            .expect("reconnect submission");
        assert!(matches!(
            controller.handle_intent(reconnect, &catalog, &store),
            Ok(Some(AppAction::StartSession(_, _)))
        ));
    }

    #[test]
    fn spontaneous_error_requires_matching_cleanup_before_reconnect() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller = AppController::connection_form(complete_form());
        let AppAction::StartSession(request, permit) = controller
            .handle_intent(
                complete_form()
                    .take_submission(&catalog)
                    .expect("valid submission"),
                &catalog,
                &store,
            )
            .expect("connection starts")
            .expect("start action")
        else {
            panic!("connection starts a worker");
        };
        let session_id = request.session_id;
        let mut lifecycle =
            StartedTestSession::from_request(permit, request, RecordingCleanup::default());
        controller.set_platform_capabilities(PlatformCapabilities {
            dynamic_resolution: true,
            clipboard_read: true,
            clipboard_write: true,
            remote_audio: true,
            text_input: true,
        });
        controller.set_product_policy(ProductPolicy {
            dynamic_resolution: true,
            clipboard_read: true,
            clipboard_write: true,
            remote_audio: true,
            text_input: true,
        });
        controller.handle_session_event(SessionEvent::CapabilitiesChanged(
            frd_protocol_api::SessionCapabilities {
                dynamic_resolution: true,
                clipboard_read: true,
                clipboard_write: true,
                remote_audio: true,
                text_input: true,
            },
        ));
        controller.handle_session_event(SessionEvent::Clipboard(ClipboardPayload::new(vec![0x22])));
        controller.handle_session_event(SessionEvent::AudioState(AudioState::Playing));
        controller.handle_server_identity_challenge(challenge(session_id, 7, [0x11; 32]));

        controller.handle_session_event(SessionEvent::Error(ProtocolError::Adapter {
            protocol_id: ProtocolId::apple_hpss_mvs(),
            code: "spontaneous_failure",
        }));
        assert!(controller.take_inbound_clipboard().is_none());
        assert_eq!(controller.audio_state(), &AudioState::Unavailable);
        assert_eq!(
            controller.effective_capabilities(),
            frd_protocol_api::SessionCapabilities::default()
        );
        assert!(controller.current_server_identity_challenge().is_none());
        let wrong_session_id = SessionId::allocate();
        let mut wrong_slot = ActiveSessionSlot::default();
        let wrong_permit = wrong_slot
            .begin_connect(wrong_session_id)
            .expect("other start reserves a distinct owner");
        let mut wrong_lifecycle = StartedTestSession::from_permit(
            wrong_permit,
            wrong_session_id,
            RecordingCleanup::default(),
        );
        let wrong_completion = wrong_lifecycle
            .complete()
            .expect("other started session cleanup completes");
        assert!(controller.finish_session_cleanup(wrong_completion).is_err());
        let retry = complete_form()
            .take_submission(&catalog)
            .expect("retry submission");
        assert_eq!(
            controller.handle_intent(retry, &catalog, &store).err(),
            Some(AppControllerError::SessionAlreadyActive)
        );

        controller
            .finish_session_cleanup(
                lifecycle
                    .complete()
                    .expect("current started session cleanup completes"),
            )
            .expect("matching cleanup releases the failed session");
        let retry = complete_form()
            .take_submission(&catalog)
            .expect("retry submission");
        assert!(matches!(
            controller.handle_intent(retry, &catalog, &store),
            Ok(Some(AppAction::StartSession(_, _)))
        ));
    }

    #[test]
    fn spontaneous_closed_requires_matching_cleanup_before_reconnect() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller = AppController::connection_form(complete_form());
        let AppAction::StartSession(request, permit) = controller
            .handle_intent(
                complete_form()
                    .take_submission(&catalog)
                    .expect("valid submission"),
                &catalog,
                &store,
            )
            .expect("connection starts")
            .expect("start action")
        else {
            panic!("connection starts a worker");
        };
        let mut lifecycle =
            StartedTestSession::from_request(permit, request, RecordingCleanup::default());

        controller
            .handle_session_event(SessionEvent::Closed(frd_protocol_api::ProtocolExit::Closed));
        let retry = complete_form()
            .take_submission(&catalog)
            .expect("retry submission");
        assert!(matches!(
            controller.handle_intent(retry, &catalog, &store),
            Err(AppControllerError::SessionAlreadyActive)
        ));

        controller
            .finish_session_cleanup(
                lifecycle
                    .complete()
                    .expect("closed started session cleanup completes"),
            )
            .expect("matching cleanup releases the closed session");
        let retry = complete_form()
            .take_submission(&catalog)
            .expect("retry submission");
        assert!(matches!(
            controller.handle_intent(retry, &catalog, &store),
            Ok(Some(AppAction::StartSession(_, _)))
        ));
    }

    #[test]
    fn error_then_closed_is_idempotent_and_preserves_first_terminal_code() {
        let session_id = SessionId::allocate();
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let (mut controller, permit) =
            AppController::awaiting_first_frame_with_start(session_id, 1);
        let mut lifecycle =
            StartedTestSession::from_permit(permit, session_id, RecordingCleanup::default());

        controller.handle_session_event(SessionEvent::Error(ProtocolError::Adapter {
            protocol_id: ProtocolId::apple_hpss_mvs(),
            code: "first_terminal_error",
        }));
        controller.handle_session_event(SessionEvent::Closed(
            frd_protocol_api::ProtocolExit::Failed(ProtocolError::Adapter {
                protocol_id: ProtocolId::apple_hpss_mvs(),
                code: "later_terminal_error",
            }),
        ));
        controller
            .handle_session_event(SessionEvent::Closed(frd_protocol_api::ProtocolExit::Closed));
        assert!(matches!(
            controller.page(),
            AppPage::Failed { code, .. } if code == "first_terminal_error"
        ));
        let retry = complete_form()
            .take_submission(&catalog)
            .expect("retry submission");
        assert!(matches!(
            controller.handle_intent(retry, &catalog, &store),
            Err(AppControllerError::SessionAlreadyActive)
        ));

        controller
            .finish_session_cleanup(
                lifecycle
                    .complete()
                    .expect("terminal started session cleanup completes"),
            )
            .expect("one cleanup capability releases the session once");
        controller
            .handle_session_event(SessionEvent::Closed(frd_protocol_api::ProtocolExit::Closed));
        assert!(matches!(
            controller.page(),
            AppPage::Failed { code, .. } if code == "first_terminal_error"
        ));
        let retry = complete_form()
            .take_submission(&catalog)
            .expect("retry submission");
        assert!(matches!(
            controller.handle_intent(retry, &catalog, &store),
            Ok(Some(AppAction::StartSession(_, _)))
        ));
    }

    /// 纯协议中立测试 harness；只验证 Task 10 ownership，不代表 Apple worker 已接线。
    struct StartedTestSession {
        coordinator: SessionCoordinator,
        handle: SessionCleanupHandle,
    }

    impl StartedTestSession {
        fn from_permit(
            permit: SessionStartPermit,
            session_id: SessionId,
            cleanup: RecordingCleanup,
        ) -> Self {
            Self::from_request(permit, test_connect_request(session_id), cleanup)
        }

        fn from_request(
            permit: SessionStartPermit,
            request: ConnectRequest,
            cleanup: RecordingCleanup,
        ) -> Self {
            let mut coordinator =
                SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
            let handle = match coordinator.start(permit, TargetSystem::MacOs, request, move |_| {
                Ok(Box::new(cleanup) as Box<dyn CleanupOperations>)
            }) {
                SessionStartOutcome::Started(handle) => handle,
                SessionStartOutcome::LaunchRolledBack(failure) => {
                    panic!("test coordinator launch rolled back: {:?}", failure.error())
                }
            };
            Self {
                coordinator,
                handle,
            }
        }

        fn complete(&mut self) -> Result<frd_session::CleanupComplete, CleanupError> {
            self.coordinator.complete_cleanup(&self.handle)
        }
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
    fn remembered_password_commits_only_after_transport_ready() {
        let fixture = RememberFixture::with_saved_password("old-password");
        let session = fixture.submit_remembered("new-password");
        assert_eq!(
            fixture.committed_password(),
            Some("old-password".to_owned())
        );
        assert!(fixture.pending_exists(session));
        fixture.publish_stage(session, ConnectionStage::TransportReady);
        assert_eq!(
            fixture.committed_password(),
            Some("new-password".to_owned())
        );
        assert!(!fixture.pending_exists(session));
    }

    #[test]
    fn new_profile_metadata_failure_removes_the_newly_committed_credential() {
        let fixture = RememberFixture::without_saved_profiles();
        fixture.profiles.fail_next_upsert();

        let session = fixture.submit_new_remembered("new-password");
        fixture.publish_stage(session, ConnectionStage::TransportReady);

        assert_eq!(fixture.committed_password(), None);
        assert!(!fixture.profile_exists());
        assert_eq!(
            fixture.profile_persistence_warning(),
            Some("登录信息未能安全保存；本次连接仍可继续，请稍后重试。")
        );
    }

    #[test]
    fn overwritten_profile_metadata_failure_restores_the_previous_credential() {
        let fixture = RememberFixture::with_saved_password("old-password");
        fixture.profiles.fail_next_upsert();

        let session = fixture.submit_remembered("new-password");
        fixture.publish_stage(session, ConnectionStage::TransportReady);

        assert_eq!(
            fixture.committed_password(),
            Some("old-password".to_owned())
        );
        assert!(fixture.profile_exists());
    }

    #[test]
    fn unremember_keeps_profile_metadata_when_credential_delete_fails() {
        let fixture = RememberFixture::with_saved_password("old-password");
        fixture.credentials.fail_next_delete();

        let session = fixture.submit_without_remembering("old-password");
        fixture.publish_stage(session, ConnectionStage::TransportReady);

        assert!(fixture.profile_exists());
        assert_eq!(
            fixture.committed_password(),
            Some("old-password".to_owned())
        );
    }

    #[test]
    fn profile_persistence_warning_survives_generation_and_presentation_until_disconnect() {
        let fixture = RememberFixture::with_saved_password("old-password");
        fixture.profiles.fail_next_upsert();
        let session = fixture.submit_remembered("new-password");
        fixture.publish_stage(session, ConnectionStage::TransportReady);

        fixture.publish_event(
            session,
            SessionEvent::SurfaceGenerationChanged {
                session_id: session,
                generation: 1,
                size: PixelSize::new(800, 600).expect("valid surface"),
            },
        );
        assert!(fixture.profile_persistence_warning().is_some());
        fixture
            .controller
            .lock()
            .expect("controller lock")
            .handle_presentation(PresentationEvent::FramePresented {
                session_id: session,
                generation: 1,
                revision: 1,
                completeness: FrameCompleteness::FullBaseline,
            });
        assert!(matches!(
            fixture.controller.lock().expect("controller lock").page(),
            AppPage::RemoteSession { diagnostics: Some(message), .. }
                if message == "登录信息未能安全保存；本次连接仍可继续，请稍后重试。"
        ));

        fixture
            .controller
            .lock()
            .expect("controller lock")
            .handle_intent_with_stores(AppIntent::Disconnect, &fixture.catalog, fixture.stores())
            .expect("disconnect starts");
        assert_eq!(fixture.profile_persistence_warning(), None);
    }

    #[test]
    fn successful_save_is_reloaded_as_the_most_recent_profile_after_cleanup() {
        let fixture = RememberFixture::with_two_profiles();
        let AppAction::StartSession(request, permit) =
            fixture.submit_remembered_action("updated-password")
        else {
            panic!("remembered submission starts a session");
        };
        let session = request.session_id;
        let mut lifecycle =
            StartedTestSession::from_request(permit, request, RecordingCleanup::default());
        fixture.publish_stage(session, ConnectionStage::TransportReady);
        fixture.cancel_connect();

        fixture
            .controller
            .lock()
            .expect("controller lock")
            .finish_session_cleanup_with_stores(
                lifecycle.complete().expect("cleanup completes"),
                fixture.stores(),
            )
            .expect("cleanup reloads the connection form");

        let controller = fixture.controller.lock().expect("controller lock");
        let AppPage::ConnectionForm(form) = controller.page() else {
            panic!("cleanup returns to the connection form");
        };
        assert_eq!(form.profiles[0].key, fixture.profile_key(0));
    }

    #[test]
    fn failed_session_return_reloads_and_sorts_profiles_from_the_store() {
        let fixture = RememberFixture::with_two_profiles();
        let session = fixture.submit_remembered("wrong-password");
        fixture.publish_failure(session, "authentication_failed");
        fixture
            .profiles
            .upsert(&SavedConnectionProfile {
                key: fixture.profile_key(1),
                target_system: TargetSystem::MacOs,
                last_success_order: 99,
            })
            .expect("external profile update succeeds");

        fixture
            .controller
            .lock()
            .expect("controller lock")
            .handle_intent_with_stores(
                AppIntent::ReturnToConnection,
                &fixture.catalog,
                fixture.stores(),
            )
            .expect("failed page returns through stores");

        let controller = fixture.controller.lock().expect("controller lock");
        let AppPage::ConnectionForm(form) = controller.page() else {
            panic!("failure returns to the connection form");
        };
        assert_eq!(form.profiles[0].key, fixture.profile_key(1));
    }

    #[test]
    fn authentication_failure_discards_pending_without_overwriting_committed() {
        let fixture = RememberFixture::with_saved_password("old-password");
        let session = fixture.submit_remembered("wrong-password");
        fixture.publish_failure(session, "apple_hpss_session_failed");
        assert_eq!(
            fixture.committed_password(),
            Some("old-password".to_owned())
        );
        assert!(!fixture.pending_exists(session));
    }

    #[test]
    fn successful_unremembered_login_deletes_selected_profile() {
        let fixture = RememberFixture::with_saved_password("old-password");
        let session = fixture.submit_without_remembering("old-password");
        assert!(fixture.profile_exists());
        fixture.publish_stage(session, ConnectionStage::TransportReady);
        assert!(!fixture.profile_exists());
        assert_eq!(fixture.committed_password(), None);
    }

    #[test]
    fn selecting_profile_loads_only_its_credential() {
        let fixture = RememberFixture::with_two_profiles();
        fixture.select_profile(1);
        assert_eq!(fixture.credential_loads(), vec![fixture.profile_key(1)]);
        assert_eq!(fixture.form_password(), "selected-password");
    }

    #[test]
    fn explicit_cancel_discards_pending_without_overwriting_committed() {
        let fixture = RememberFixture::with_saved_password("old-password");
        let session = fixture.submit_remembered("new-password");

        fixture.cancel_connect();

        assert_eq!(
            fixture.committed_password(),
            Some("old-password".to_owned())
        );
        assert!(!fixture.pending_exists(session));
    }

    #[test]
    fn disconnect_stage_discards_pending_without_overwriting_committed() {
        let fixture = RememberFixture::with_saved_password("old-password");
        let session = fixture.submit_remembered("new-password");

        fixture.publish_stage(session, ConnectionStage::Disconnecting);

        assert_eq!(
            fixture.committed_password(),
            Some("old-password".to_owned())
        );
        assert!(!fixture.pending_exists(session));
    }

    #[test]
    fn normal_close_discards_pending_without_overwriting_committed() {
        let fixture = RememberFixture::with_saved_password("old-password");
        let session = fixture.submit_remembered("new-password");

        fixture.publish_event(
            session,
            SessionEvent::Closed(frd_protocol_api::ProtocolExit::Closed),
        );

        assert_eq!(
            fixture.committed_password(),
            Some("old-password".to_owned())
        );
        assert!(!fixture.pending_exists(session));
    }

    #[test]
    fn failed_close_discards_pending_without_overwriting_committed() {
        let fixture = RememberFixture::with_saved_password("old-password");
        let session = fixture.submit_remembered("wrong-password");

        fixture.publish_event(
            session,
            SessionEvent::Closed(frd_protocol_api::ProtocolExit::Failed(
                ProtocolError::adapter(ProtocolId::apple_hpss_mvs(), "apple_hpss_session_failed"),
            )),
        );

        assert_eq!(
            fixture.committed_password(),
            Some("old-password".to_owned())
        );
        assert!(!fixture.pending_exists(session));
    }

    #[test]
    fn launch_rollback_discards_pending_without_overwriting_committed() {
        let fixture = RememberFixture::with_saved_password("old-password");
        let AppAction::StartSession(request, permit) =
            fixture.submit_remembered_action("new-password")
        else {
            panic!("remembered submission starts a session");
        };
        let session = request.session_id;
        let mut coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let failure = match coordinator.start(permit, TargetSystem::MacOs, request, |_| {
            Err(ProtocolError::Terminal)
        }) {
            SessionStartOutcome::Started(_) => panic!("fixture launch must roll back"),
            SessionStartOutcome::LaunchRolledBack(failure) => failure,
        };

        fixture
            .controller
            .lock()
            .expect("controller lock")
            .consume_launch_rollback_with_stores(&failure, fixture.stores())
            .expect("matching rollback is consumed");

        assert_eq!(
            fixture.committed_password(),
            Some("old-password".to_owned())
        );
        assert!(!fixture.pending_exists(session));
    }

    #[test]
    fn foreign_transport_ready_does_not_commit_current_pending_transaction() {
        let fixture = RememberFixture::with_saved_password("old-password");
        let current = fixture.submit_remembered("new-password");
        let foreign = SessionId::allocate();

        fixture.publish_stage(foreign, ConnectionStage::TransportReady);

        assert_eq!(
            fixture.committed_password(),
            Some("old-password".to_owned())
        );
        assert!(fixture.pending_exists(current));
    }

    struct RememberFixture {
        catalog: ProtocolCatalog,
        controller: Mutex<AppController>,
        identities: RecordingStore,
        profiles: MemoryProfileStore,
        credentials: MemoryCredentialStore,
        keys: Vec<ConnectionProfileKey>,
    }

    impl RememberFixture {
        fn without_saved_profiles() -> Self {
            let mut fixture = Self::new(&[]);
            fixture.keys.push(
                ConnectionProfileKey::new(
                    ProtocolId::apple_hpss_mvs(),
                    "new.invalid",
                    5900,
                    "new-user",
                )
                .expect("new fixture profile key is valid"),
            );
            fixture
        }

        fn with_saved_password(password: &str) -> Self {
            Self::new(&[("remembered.invalid", "remembered-user", password, 1)])
        }

        fn with_two_profiles() -> Self {
            Self::new(&[
                ("other.invalid", "other-user", "other-password", 1),
                ("selected.invalid", "selected-user", "selected-password", 2),
            ])
        }

        fn new(entries: &[(&str, &str, &str, u64)]) -> Self {
            let identities = RecordingStore::default();
            let mut saved = Vec::new();
            let mut committed = Vec::new();
            let mut keys = Vec::new();
            for (address, username, password, order) in entries {
                let key = ConnectionProfileKey::new(
                    ProtocolId::apple_hpss_mvs(),
                    *address,
                    5900,
                    *username,
                )
                .expect("fixture profile key is valid");
                saved.push(SavedConnectionProfile {
                    key: key.clone(),
                    target_system: TargetSystem::MacOs,
                    last_success_order: *order,
                });
                committed.push((key.clone(), (*password).to_owned()));
                keys.push(key);
            }
            let profiles = MemoryProfileStore::new(saved);
            let credentials = MemoryCredentialStore::new(committed);
            let stores = AppPlatformStores {
                server_identities: &identities,
                profiles: &profiles,
                credentials: &credentials,
            };
            let controller = AppController::connection_form_with_stores(
                ConnectionForm::new(ConnectionDraft::default()),
                stores,
            );
            Self {
                catalog: ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]),
                controller: Mutex::new(controller),
                identities,
                profiles,
                credentials,
                keys,
            }
        }

        fn stores(&self) -> AppPlatformStores<'_> {
            AppPlatformStores {
                server_identities: &self.identities,
                profiles: &self.profiles,
                credentials: &self.credentials,
            }
        }

        fn select_profile(&self, index: usize) {
            self.controller
                .lock()
                .expect("controller lock")
                .handle_intent_with_stores(
                    AppIntent::SelectSavedProfile(self.profile_key(index)),
                    &self.catalog,
                    self.stores(),
                )
                .expect("saved profile selection succeeds");
        }

        fn submit_remembered(&self, password: &str) -> SessionId {
            let AppAction::StartSession(request, _) = self.submit_remembered_action(password)
            else {
                panic!("remembered submission starts a session");
            };
            request.session_id
        }

        fn submit_remembered_action(&self, password: &str) -> AppAction {
            self.select_profile(0);
            self.submit_action(password, true)
        }

        fn submit_new_remembered(&self, password: &str) -> SessionId {
            {
                let mut controller = self.controller.lock().expect("controller lock");
                let form = controller
                    .connection_form_mut()
                    .expect("fixture is on connection form");
                form.draft.target_system = Some(TargetSystem::MacOs);
                form.draft.address = "new.invalid".to_owned();
                form.draft.port = Some(5900);
                form.draft.protocol = ProtocolChoice::Automatic;
                form.draft.username = "new-user".to_owned();
            }
            self.submit(password, true)
        }

        fn submit_without_remembering(&self, password: &str) -> SessionId {
            self.select_profile(0);
            self.submit(password, false)
        }

        fn submit(&self, password: &str, remember_on_this_device: bool) -> SessionId {
            let AppAction::StartSession(request, _) =
                self.submit_action(password, remember_on_this_device)
            else {
                panic!("fixture submission must start a session");
            };
            request.session_id
        }

        fn submit_action(&self, password: &str, remember_on_this_device: bool) -> AppAction {
            let mut controller = self.controller.lock().expect("controller lock");
            let form = controller
                .connection_form_mut()
                .expect("fixture is on connection form");
            form.set_password(SecretBuffer::new(password.as_bytes().to_vec()));
            form.remember_on_this_device = remember_on_this_device;
            let submission = form
                .take_submission(&self.catalog)
                .expect("fixture form is complete");
            let action = controller
                .handle_intent_with_stores(submission, &self.catalog, self.stores())
                .expect("fixture submission succeeds")
                .expect("fixture submission launches a session");
            action
        }

        fn cancel_connect(&self) {
            self.controller
                .lock()
                .expect("controller lock")
                .handle_intent_with_stores(AppIntent::CancelConnect, &self.catalog, self.stores())
                .expect("cancel is accepted");
        }

        fn publish_stage(&self, session: SessionId, stage: ConnectionStage) {
            self.publish_event(session, SessionEvent::StageChanged(stage));
        }

        fn publish_event(&self, session: SessionId, event: SessionEvent) {
            self.controller
                .lock()
                .expect("controller lock")
                .handle_session_event_with_stores(session, event, self.stores());
        }

        fn publish_failure(&self, session: SessionId, code: &'static str) {
            assert!(self.pending_exists(session));
            self.controller
                .lock()
                .expect("controller lock")
                .handle_session_event_with_stores(
                    session,
                    SessionEvent::Error(ProtocolError::adapter(ProtocolId::apple_hpss_mvs(), code)),
                    self.stores(),
                );
        }

        fn committed_password(&self) -> Option<String> {
            self.credentials.committed(&self.keys[0])
        }

        fn pending_exists(&self, session: SessionId) -> bool {
            self.credentials.pending_exists(session)
        }

        fn profile_exists(&self) -> bool {
            self.profiles.contains(&self.keys[0])
        }

        fn credential_loads(&self) -> Vec<ConnectionProfileKey> {
            self.credentials.loads()
        }

        fn profile_key(&self, index: usize) -> ConnectionProfileKey {
            self.keys[index].clone()
        }

        fn form_password(&self) -> String {
            let mut controller = self.controller.lock().expect("controller lock");
            let Some(form) = controller.connection_form_mut() else {
                panic!("fixture must remain on the connection form");
            };
            form.password_mut()
                .expose_text()
                .expect("fixture password is valid UTF-8")
                .to_owned()
        }

        fn profile_persistence_warning(&self) -> Option<&'static str> {
            self.controller
                .lock()
                .expect("controller lock")
                .profile_persistence_warning()
        }
    }

    struct MemoryProfileStore {
        profiles: Mutex<Vec<SavedConnectionProfile>>,
        fail_upsert: Mutex<bool>,
    }

    impl MemoryProfileStore {
        fn new(profiles: Vec<SavedConnectionProfile>) -> Self {
            Self {
                profiles: Mutex::new(profiles),
                fail_upsert: Mutex::new(false),
            }
        }

        fn fail_next_upsert(&self) {
            *self.fail_upsert.lock().expect("failure lock") = true;
        }

        fn contains(&self, key: &ConnectionProfileKey) -> bool {
            self.profiles
                .lock()
                .expect("profile lock")
                .iter()
                .any(|profile| &profile.key == key)
        }
    }

    impl ConnectionProfileStore for MemoryProfileStore {
        fn list(&self) -> Result<Vec<SavedConnectionProfile>, PlatformError> {
            Ok(self.profiles.lock().expect("profile lock").clone())
        }

        fn upsert(&self, profile: &SavedConnectionProfile) -> Result<(), PlatformError> {
            if std::mem::take(&mut *self.fail_upsert.lock().expect("failure lock")) {
                return Err(PlatformError::Unavailable);
            }
            let mut profiles = self.profiles.lock().expect("profile lock");
            if let Some(existing) = profiles
                .iter_mut()
                .find(|existing| existing.key == profile.key)
            {
                *existing = profile.clone();
            } else {
                profiles.push(profile.clone());
            }
            Ok(())
        }

        fn delete(&self, key: &ConnectionProfileKey) -> Result<(), PlatformError> {
            self.profiles
                .lock()
                .expect("profile lock")
                .retain(|profile| &profile.key != key);
            Ok(())
        }
    }

    struct MemoryCredentialStore {
        committed: Mutex<Vec<(ConnectionProfileKey, String)>>,
        pending: Mutex<Vec<(SessionId, ConnectionProfileKey, String)>>,
        loads: Mutex<Vec<ConnectionProfileKey>>,
        fail_delete: Mutex<bool>,
    }

    impl MemoryCredentialStore {
        fn new(committed: Vec<(ConnectionProfileKey, String)>) -> Self {
            Self {
                committed: Mutex::new(committed),
                pending: Mutex::new(Vec::new()),
                loads: Mutex::new(Vec::new()),
                fail_delete: Mutex::new(false),
            }
        }

        fn fail_next_delete(&self) {
            *self.fail_delete.lock().expect("failure lock") = true;
        }

        fn committed(&self, key: &ConnectionProfileKey) -> Option<String> {
            self.committed
                .lock()
                .expect("credential lock")
                .iter()
                .find(|(stored_key, _)| stored_key == key)
                .map(|(_, password)| password.clone())
        }

        fn pending_exists(&self, session: SessionId) -> bool {
            self.pending
                .lock()
                .expect("pending lock")
                .iter()
                .any(|(stored_session, _, _)| *stored_session == session)
        }

        fn loads(&self) -> Vec<ConnectionProfileKey> {
            self.loads.lock().expect("load lock").clone()
        }
    }

    impl SecureCredentialStore for MemoryCredentialStore {
        fn load(&self, key: &ConnectionProfileKey) -> Result<Option<SecretBuffer>, PlatformError> {
            self.loads.lock().expect("load lock").push(key.clone());
            Ok(self.committed(key).map(SecretBuffer::from_text))
        }

        fn stage(
            &self,
            session: SessionId,
            key: &ConnectionProfileKey,
            password: &SecretBuffer,
        ) -> Result<(), PlatformError> {
            let password = password
                .expose_text()
                .expect("fixture passwords are valid UTF-8")
                .to_owned();
            self.pending
                .lock()
                .expect("pending lock")
                .push((session, key.clone(), password));
            Ok(())
        }

        fn commit(
            &self,
            session: SessionId,
            key: &ConnectionProfileKey,
        ) -> Result<(), PlatformError> {
            let password = {
                let mut pending = self.pending.lock().expect("pending lock");
                let index = pending
                    .iter()
                    .position(|(stored_session, stored_key, _)| {
                        *stored_session == session && stored_key == key
                    })
                    .ok_or(PlatformError::CredentialNotFound)?;
                pending.remove(index).2
            };
            let mut committed = self.committed.lock().expect("credential lock");
            if let Some((_, stored_password)) = committed
                .iter_mut()
                .find(|(stored_key, _)| stored_key == key)
            {
                *stored_password = password;
            } else {
                committed.push((key.clone(), password));
            }
            Ok(())
        }

        fn discard(&self, session: SessionId) -> Result<(), PlatformError> {
            self.pending
                .lock()
                .expect("pending lock")
                .retain(|(stored_session, _, _)| *stored_session != session);
            Ok(())
        }

        fn delete(&self, key: &ConnectionProfileKey) -> Result<(), PlatformError> {
            if std::mem::take(&mut *self.fail_delete.lock().expect("failure lock")) {
                return Err(PlatformError::Unavailable);
            }
            self.committed
                .lock()
                .expect("credential lock")
                .retain(|(stored_key, _)| stored_key != key);
            Ok(())
        }

        fn purge_pending(&self) -> Result<(), PlatformError> {
            self.pending.lock().expect("pending lock").clear();
            Ok(())
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

        let permit = slot
            .begin_connect(first)
            .expect("first connect is accepted");
        let mut lifecycle =
            StartedTestSession::from_permit(permit, first, RecordingCleanup::default());
        slot.begin_disconnect(first)
            .expect("first session begins disconnect");
        let completed = lifecycle.complete().expect("all resources are reclaimed");
        slot.finish_cleanup(&completed)
            .expect("matching cleanup completion releases the slot");
        assert!(slot.begin_connect(second).is_ok());
    }

    #[test]
    fn cleanup_completion_for_another_session_does_not_release_disconnect_slot() {
        let first = SessionId::allocate();
        let other = SessionId::allocate();
        let second = SessionId::allocate();
        let mut slot = ActiveSessionSlot::default();
        let _first_permit = slot
            .begin_connect(first)
            .expect("first connect is accepted");
        slot.begin_disconnect(first)
            .expect("first session begins disconnect");
        let mut other_slot = ActiveSessionSlot::default();
        let other_permit = other_slot
            .begin_connect(other)
            .expect("other start is reserved");
        let mut other_lifecycle =
            StartedTestSession::from_permit(other_permit, other, RecordingCleanup::default());
        let other_completion = other_lifecycle
            .complete()
            .expect("other started session cleanup completes");

        assert!(slot.finish_cleanup(&other_completion).is_err());
        assert!(slot.begin_connect(second).is_err());
    }

    #[test]
    fn failed_coordinator_cleanup_keeps_disconnect_slot_occupied() {
        let first = SessionId::allocate();
        let second = SessionId::allocate();
        let mut slot = ActiveSessionSlot::default();
        let permit = slot
            .begin_connect(first)
            .expect("first connect is accepted");
        let mut lifecycle = StartedTestSession::from_permit(
            permit,
            first,
            RecordingCleanup::failing_at(CleanupError::ShutdownWriter),
        );

        slot.begin_disconnect(first)
            .expect("first session begins disconnect");
        assert!(matches!(
            lifecycle.complete(),
            Err(CleanupError::ShutdownWriter)
        ));
        assert!(slot.begin_connect(second).is_err());
    }

    #[test]
    fn completion_from_a_different_valid_start_with_same_session_id_cannot_release_slot() {
        let session_id = SessionId::allocate();
        let mut slot = ActiveSessionSlot::default();
        let permit = slot
            .begin_connect(session_id)
            .expect("the product slot reserves one start");
        let mut other_slot = ActiveSessionSlot::default();
        let other_permit = other_slot
            .begin_connect(session_id)
            .expect("another slot issues a distinct reservation");
        let mut lifecycle =
            StartedTestSession::from_permit(permit, session_id, RecordingCleanup::default());
        let mut other_lifecycle =
            StartedTestSession::from_permit(other_permit, session_id, RecordingCleanup::default());
        slot.begin_disconnect(session_id)
            .expect("the product session begins disconnect");

        let other_completion = other_lifecycle
            .complete()
            .expect("the other start reclaims only its own resources");
        assert!(slot.finish_cleanup(&other_completion).is_err());
        assert!(slot.is_occupied());

        let completion = lifecycle
            .complete()
            .expect("the product start reclaims its bound resources");
        assert!(slot.finish_cleanup(&completion).is_ok());
    }

    #[test]
    fn stale_completion_is_rejected_after_a_new_start_with_the_same_session_id() {
        let session_id = SessionId::allocate();
        let mut slot = ActiveSessionSlot::default();
        let first_permit = slot
            .begin_connect(session_id)
            .expect("first start is reserved");
        let mut first_lifecycle =
            StartedTestSession::from_permit(first_permit, session_id, RecordingCleanup::default());
        slot.begin_disconnect(session_id)
            .expect("first session disconnects");
        let stale_completion = first_lifecycle.complete().expect("first cleanup completes");
        slot.finish_cleanup(&stale_completion)
            .expect("first completion releases only the first reservation");

        let second_permit = slot
            .begin_connect(session_id)
            .expect("same session ID receives a new opaque reservation");
        let mut second_lifecycle =
            StartedTestSession::from_permit(second_permit, session_id, RecordingCleanup::default());
        slot.begin_disconnect(session_id)
            .expect("second session disconnects");

        assert!(slot.finish_cleanup(&stale_completion).is_err());
        assert!(slot.is_occupied());
        let second_completion = second_lifecycle
            .complete()
            .expect("second cleanup completes");
        assert!(slot.finish_cleanup(&second_completion).is_ok());
    }

    #[test]
    fn controller_consumes_original_launch_rollback_and_accepts_a_fresh_connect() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller = AppController::connection_form(complete_form());
        let AppAction::StartSession(request, permit) = controller
            .handle_intent(
                complete_form()
                    .take_submission(&catalog)
                    .expect("valid first submission"),
                &catalog,
                &store,
            )
            .expect("first connect is accepted")
            .expect("first connect starts one reservation")
        else {
            panic!("connect uses the session start transaction");
        };
        let session_id = request.session_id;
        assert!(request.credentials.is_some());
        let mut coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let rollback = match coordinator.start(permit, TargetSystem::MacOs, request, |_| {
            Err(ProtocolError::Terminal)
        }) {
            SessionStartOutcome::Started(_) => {
                panic!("failed launch must roll back instead of issuing cleanup")
            }
            SessionStartOutcome::LaunchRolledBack(rollback) => rollback,
        };
        controller
            .consume_launch_rollback(&rollback)
            .expect("the original rollback releases its connecting reservation");
        assert!(matches!(
            controller.page(),
            AppPage::Failed { code, draft } if code == "terminal" && draft.username == "test-user"
        ));

        let AppAction::StartSession(fresh_request, _fresh_permit) = controller
            .handle_intent(
                complete_form()
                    .take_submission(&catalog)
                    .expect("fresh submission owns a fresh password"),
                &catalog,
                &store,
            )
            .expect("fresh connect is accepted by the same controller")
            .expect("fresh connect creates a new reservation")
        else {
            panic!("fresh connect uses the session start transaction");
        };
        assert_ne!(fresh_request.session_id, session_id);
    }

    #[test]
    fn mismatched_duplicate_and_stale_launch_rollbacks_leave_the_current_slot_untouched() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller = AppController::connection_form(complete_form());
        let AppAction::StartSession(request, permit) = controller
            .handle_intent(
                complete_form()
                    .take_submission(&catalog)
                    .expect("valid first submission"),
                &catalog,
                &store,
            )
            .expect("first connect is accepted")
            .expect("first connect starts one reservation")
        else {
            panic!("connect uses the session start transaction");
        };
        let session_id = request.session_id;

        let foreign_permit = frd_session::reserve_session_start(session_id).1;
        let mut foreign_coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let foreign_rollback = match foreign_coordinator.start(
            foreign_permit,
            TargetSystem::MacOs,
            test_connect_request(session_id),
            |_| Err(ProtocolError::Terminal),
        ) {
            SessionStartOutcome::Started(_) => panic!("foreign launch must fail"),
            SessionStartOutcome::LaunchRolledBack(rollback) => rollback,
        };
        assert!(controller
            .consume_launch_rollback(&foreign_rollback)
            .is_err());

        let mut coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let original_rollback =
            match coordinator.start(permit, TargetSystem::MacOs, request, |_| {
                Err(ProtocolError::Terminal)
            }) {
                SessionStartOutcome::Started(_) => panic!("original launch must fail"),
                SessionStartOutcome::LaunchRolledBack(rollback) => rollback,
            };
        controller
            .consume_launch_rollback(&original_rollback)
            .expect("matching rollback releases only its reservation");
        assert!(controller
            .consume_launch_rollback(&original_rollback)
            .is_err());

        let AppAction::StartSession(_fresh_request, _fresh_permit) = controller
            .handle_intent(
                complete_form()
                    .take_submission(&catalog)
                    .expect("fresh submission"),
                &catalog,
                &store,
            )
            .expect("fresh connect starts")
            .expect("fresh reservation is returned")
        else {
            panic!("fresh connect uses the session start transaction");
        };
        assert!(controller
            .consume_launch_rollback(&original_rollback)
            .is_err());
        assert_eq!(
            controller
                .handle_intent(
                    complete_form()
                        .take_submission(&catalog)
                        .expect("blocked third submission"),
                    &catalog,
                    &store,
                )
                .err(),
            Some(AppControllerError::SessionAlreadyActive)
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
    fn presented_session_ignores_late_stages_without_closing_input() {
        let session_id = SessionId::allocate();
        let mut controller = AppController::awaiting_first_frame(session_id, 3);
        controller.handle_presentation(PresentationEvent::FramePresented {
            session_id,
            generation: 3,
            revision: 9,
            completeness: FrameCompleteness::FullBaseline,
        });

        for stage in [
            ConnectionStage::Connecting,
            ConnectionStage::TransportReady,
            ConnectionStage::AwaitingIdentityDecision,
        ] {
            controller.handle_session_event(SessionEvent::StageChanged(stage));
            assert!(matches!(controller.page(), AppPage::RemoteSession { .. }));
            assert!(controller
                .route_input(frd_core::InputEvent::ReleaseAll)
                .is_some());
        }
    }

    #[test]
    fn presented_apple_disconnecting_stage_closes_input_once_and_waits_for_bound_cleanup() {
        let session_id = SessionId::allocate();
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let (mut controller, permit) =
            AppController::awaiting_first_frame_with_start(session_id, 3);
        let mut lifecycle =
            StartedTestSession::from_permit(permit, session_id, RecordingCleanup::default());
        controller.handle_presentation(PresentationEvent::FramePresented {
            session_id,
            generation: 3,
            revision: 9,
            completeness: FrameCompleteness::FullBaseline,
        });
        controller.set_platform_capabilities(PlatformCapabilities {
            dynamic_resolution: true,
            clipboard_read: true,
            clipboard_write: true,
            remote_audio: true,
            text_input: true,
        });
        controller.set_product_policy(ProductPolicy {
            dynamic_resolution: true,
            clipboard_read: true,
            clipboard_write: true,
            remote_audio: true,
            text_input: true,
        });
        controller.handle_session_event(SessionEvent::CapabilitiesChanged(
            frd_protocol_api::SessionCapabilities {
                dynamic_resolution: true,
                clipboard_read: true,
                clipboard_write: true,
                remote_audio: true,
                text_input: true,
            },
        ));
        controller.handle_session_event(SessionEvent::Clipboard(ClipboardPayload::new(vec![0x22])));
        controller.handle_session_event(SessionEvent::AudioState(AudioState::Playing));
        controller.handle_server_identity_challenge(challenge(session_id, 7, [0x11; 32]));
        assert!(controller
            .route_input(frd_core::InputEvent::ReleaseAll)
            .is_some());
        assert!(controller.effective_capabilities().remote_audio);
        assert!(controller.current_server_identity_challenge().is_some());

        controller.handle_session_event(SessionEvent::StageChanged(ConnectionStage::Disconnecting));
        controller.handle_session_event(SessionEvent::StageChanged(ConnectionStage::Disconnecting));
        controller
            .handle_session_event(SessionEvent::Closed(frd_protocol_api::ProtocolExit::Closed));

        assert!(matches!(controller.page(), AppPage::Disconnecting { .. }));
        assert!(controller
            .route_input(frd_core::InputEvent::ReleaseAll)
            .is_none());
        assert!(controller.take_inbound_clipboard().is_none());
        assert_eq!(controller.audio_state(), &AudioState::Unavailable);
        assert_eq!(
            controller.effective_capabilities(),
            frd_protocol_api::SessionCapabilities::default()
        );
        assert!(controller.current_server_identity_challenge().is_none());
        assert!(matches!(
            controller.handle_intent(AppIntent::Disconnect, &catalog, &store),
            Ok(None)
        ));
        let blocked = complete_form()
            .take_submission(&catalog)
            .expect("retry submission");
        assert_eq!(
            controller.handle_intent(blocked, &catalog, &store).err(),
            Some(AppControllerError::SessionAlreadyActive)
        );

        controller
            .finish_session_cleanup(
                lifecycle
                    .complete()
                    .expect("bound start resources complete cleanup"),
            )
            .expect("bound completion releases the slot");
        assert!(matches!(controller.page(), AppPage::ConnectionForm(_)));
        let retry = complete_form()
            .take_submission(&catalog)
            .expect("retry submission");
        assert!(matches!(
            controller.handle_intent(retry, &catalog, &store),
            Ok(Some(AppAction::StartSession(_, _)))
        ));
    }

    #[test]
    fn repeated_cancel_is_a_successful_noop_after_one_disconnect_command() {
        let session_id = SessionId::allocate();
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller = AppController::awaiting_first_frame(session_id, 1);

        assert!(matches!(
            controller
                .handle_intent(AppIntent::CancelConnect, &catalog, &store)
                .expect("first cancel succeeds"),
            Some(AppAction::SessionCommand(SessionCommand::Disconnect))
        ));
        assert!(matches!(
            controller.handle_intent(AppIntent::CancelConnect, &catalog, &store),
            Ok(None)
        ));
    }

    #[test]
    fn repeated_disconnect_is_a_successful_noop_after_one_disconnect_command() {
        let session_id = SessionId::allocate();
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller = AppController::awaiting_first_frame(session_id, 1);
        controller.handle_presentation(PresentationEvent::FramePresented {
            session_id,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::FullBaseline,
        });

        assert!(matches!(
            controller
                .handle_intent(AppIntent::Disconnect, &catalog, &store)
                .expect("first disconnect succeeds"),
            Some(AppAction::SessionCommand(SessionCommand::Disconnect))
        ));
        assert!(matches!(
            controller.handle_intent(AppIntent::Disconnect, &catalog, &store),
            Ok(None)
        ));
    }

    #[test]
    fn cancel_then_disconnect_emits_only_the_first_disconnect_command() {
        let session_id = SessionId::allocate();
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller = AppController::awaiting_first_frame(session_id, 1);

        assert!(matches!(
            controller
                .handle_intent(AppIntent::CancelConnect, &catalog, &store)
                .expect("cancel succeeds"),
            Some(AppAction::SessionCommand(SessionCommand::Disconnect))
        ));
        assert!(matches!(
            controller.handle_intent(AppIntent::Disconnect, &catalog, &store),
            Ok(None)
        ));
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
    fn cancel_then_late_full_baseline_does_not_enter_remote() {
        let session_id = SessionId::allocate();
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller = AppController::awaiting_first_frame(session_id, 3);

        controller
            .handle_intent(AppIntent::CancelConnect, &catalog, &store)
            .expect("cancel enters disconnecting");
        assert!(matches!(controller.page(), AppPage::Disconnecting { .. }));

        controller.handle_presentation(PresentationEvent::FramePresented {
            session_id,
            generation: 3,
            revision: 9,
            completeness: FrameCompleteness::FullBaseline,
        });

        assert!(matches!(controller.page(), AppPage::Disconnecting { .. }));
    }

    #[test]
    fn disconnect_then_route_input_returns_none() {
        let session_id = SessionId::allocate();
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller = AppController::awaiting_first_frame(session_id, 3);
        controller.handle_presentation(PresentationEvent::FramePresented {
            session_id,
            generation: 3,
            revision: 9,
            completeness: FrameCompleteness::FullBaseline,
        });
        assert!(controller
            .route_input(frd_core::InputEvent::ReleaseAll)
            .is_some());

        controller
            .handle_intent(AppIntent::Disconnect, &catalog, &store)
            .expect("disconnect enters disconnecting");

        assert!(controller
            .route_input(frd_core::InputEvent::ReleaseAll)
            .is_none());
    }

    #[test]
    fn disconnecting_then_late_transport_ready_and_generation_do_not_resurrect() {
        let session_id = SessionId::allocate();
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let mut controller = AppController::awaiting_first_frame(session_id, 3);

        controller
            .handle_intent(AppIntent::Disconnect, &catalog, &store)
            .expect("disconnect enters disconnecting");
        controller
            .handle_session_event(SessionEvent::StageChanged(ConnectionStage::TransportReady));
        controller.handle_session_event(SessionEvent::SurfaceGenerationChanged {
            session_id,
            generation: 4,
            size: PixelSize::new(1024, 768).expect("valid size"),
        });

        assert!(matches!(controller.page(), AppPage::Disconnecting { .. }));
        assert!(controller
            .route_input(frd_core::InputEvent::ReleaseAll)
            .is_none());
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
    fn cleanup_and_reconnect_do_not_expose_prior_session_state() {
        let first = SessionId::allocate();
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let store = RecordingStore::default();
        let (mut controller, permit) = AppController::awaiting_first_frame_with_start(first, 1);
        let mut lifecycle =
            StartedTestSession::from_permit(permit, first, RecordingCleanup::default());
        controller.handle_session_event(SessionEvent::CapabilitiesChanged(
            frd_protocol_api::SessionCapabilities {
                dynamic_resolution: true,
                clipboard_read: true,
                clipboard_write: true,
                remote_audio: true,
                text_input: true,
            },
        ));
        controller.handle_session_event(SessionEvent::Clipboard(ClipboardPayload::new(vec![0x22])));
        controller.handle_session_event(SessionEvent::AudioState(AudioState::Playing));
        controller.handle_server_identity_challenge(challenge(first, 7, [0x11; 32]));

        controller
            .handle_intent(AppIntent::Disconnect, &catalog, &store)
            .expect("disconnect begins cleanup");
        assert!(controller.take_inbound_clipboard().is_none());
        assert_eq!(controller.audio_state(), &AudioState::Unavailable);
        assert_eq!(
            controller.effective_capabilities(),
            frd_protocol_api::SessionCapabilities::default()
        );
        assert!(controller.current_server_identity_challenge().is_none());

        controller
            .finish_session_cleanup(
                lifecycle
                    .complete()
                    .expect("first started session cleanup completes"),
            )
            .expect("cleanup releases first session");
        let second = complete_form()
            .take_submission(&catalog)
            .expect("second submission");
        assert!(matches!(
            controller.handle_intent(second, &catalog, &store),
            Ok(Some(AppAction::StartSession(_, _)))
        ));
        assert!(controller.take_inbound_clipboard().is_none());
        assert_eq!(controller.audio_state(), &AudioState::Unavailable);
        assert_eq!(
            controller.effective_capabilities(),
            frd_protocol_api::SessionCapabilities::default()
        );
        assert!(controller.current_server_identity_challenge().is_none());
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
            evaluate_server_identity(Some([0x11; 32]), [0x22; 32]),
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
            evaluate_server_identity(Some([0x11; 32]), [0x11; 32]),
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
            evaluate_server_identity(None, pin),
        )
    }

    fn test_connect_request(session_id: SessionId) -> ConnectRequest {
        ConnectRequest {
            session_id,
            endpoint: Endpoint::new("mac.example", 5900).expect("valid endpoint"),
            protocol_id: ProtocolId::apple_hpss_mvs(),
            credentials: None,
            saved_server_pin: None,
        }
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
        loads: Mutex<usize>,
        stores: Mutex<Vec<(ProtocolId, Endpoint, [u8; 32])>>,
    }

    impl RecordingStore {
        fn with_saved_pin(saved_pin: [u8; 32]) -> Self {
            Self {
                saved_pin: Some(saved_pin),
                loads: Mutex::new(0),
                stores: Mutex::new(Vec::new()),
            }
        }

        fn load_count(&self) -> usize {
            *self.loads.lock().expect("load count lock")
        }
    }

    impl ServerIdentityStore for RecordingStore {
        fn load_pin(
            &self,
            _: &ProtocolId,
            _: &Endpoint,
        ) -> Result<Option<[u8; 32]>, PlatformError> {
            *self.loads.lock().expect("load count lock") += 1;
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
