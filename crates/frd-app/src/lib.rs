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
        Endpoint, ProtocolId, ServerIdentityChallenge, ServerIdentityDecision,
        ServerIdentityValidation,
    };

    use super::{ActiveSessionSlot, AppController, AppPage, PresentationEvent};

    #[test]
    fn active_session_slot_rejects_a_second_session_until_cleanup_finishes() {
        let first = SessionId::allocate();
        let second = SessionId::allocate();
        let mut slot = ActiveSessionSlot::default();

        assert!(slot.begin_connect(first).is_ok());
        assert!(slot.begin_connect(second).is_err());
        slot.finish_cleanup(first);
        assert!(slot.begin_connect(second).is_ok());
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
        assert_eq!(*store.pin.lock().expect("pin lock"), None);

        controller
            .resolve_server_identity_with_store(
                session_id,
                7,
                ServerIdentityDecision::TrustAndRemember,
                &store,
            )
            .expect("current challenge is persisted");
        assert_eq!(*store.pin.lock().expect("pin lock"), Some([0x11; 32]));
    }

    fn challenge(
        session_id: SessionId,
        challenge_id: u64,
        pin: [u8; 32],
    ) -> ServerIdentityChallenge {
        ServerIdentityChallenge {
            session_id,
            challenge_id,
            protocol_id: ProtocolId::apple_hpss_mvs(),
            endpoint: Endpoint::new("mac.example", 5900).expect("valid endpoint"),
            sha256_fingerprint: pin,
            subject: "mac.example".to_owned(),
            issuer: "local test".to_owned(),
            validation: ServerIdentityValidation::Unknown,
        }
    }

    #[derive(Default)]
    struct RecordingStore {
        pin: Mutex<Option<[u8; 32]>>,
    }

    impl ServerIdentityStore for RecordingStore {
        fn load_pin(
            &self,
            _: &ProtocolId,
            _: &Endpoint,
        ) -> Result<Option<[u8; 32]>, PlatformError> {
            Ok(*self.pin.lock().expect("pin lock"))
        }

        fn store_pin(
            &self,
            _: &ProtocolId,
            _: &Endpoint,
            pin: [u8; 32],
        ) -> Result<(), PlatformError> {
            *self.pin.lock().expect("pin lock") = Some(pin);
            Ok(())
        }
    }
}
