mod controller;

pub use controller::{ActiveSessionSlot, AppController, IdentityDecisionError, ProductPolicy};
pub use frd_protocol_api::PresentationEvent;
use frd_ui_model::ConnectionSubmission;
pub use frd_ui_model::Page as AppPage;

/// UI 只在此低频语义边界驱动会话；不包含输入或帧数据。
pub enum AppIntent {
    Connect(ConnectionSubmission),
    CancelConnect,
    Disconnect,
    ReturnToConnection,
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use frd_core::SessionId;
    use frd_frame::FrameCompleteness;
    use frd_platform_api::{PlatformError, ServerIdentityStore};

    use frd_protocol_api::{
        AudioState, ClipboardPayload, Endpoint, ProtocolCatalog, ProtocolId,
        ServerIdentityChallenge, ServerIdentityDecision, ServerIdentityValidation, SessionEvent,
    };
    use frd_session::{CleanupError, CleanupOperations, SessionCoordinator};

    use super::{ActiveSessionSlot, AppController, AppPage, PresentationEvent};

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
        stores: Mutex<Vec<(ProtocolId, Endpoint, [u8; 32])>>,
    }

    impl ServerIdentityStore for RecordingStore {
        fn load_pin(
            &self,
            _: &ProtocolId,
            _: &Endpoint,
        ) -> Result<Option<[u8; 32]>, PlatformError> {
            Ok(None)
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
