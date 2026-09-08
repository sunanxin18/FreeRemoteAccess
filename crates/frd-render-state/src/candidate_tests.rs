use super::*;
use frd_frame::{FrameReset, FrameRevision, PixelBuffer};

fn startup(session_id: SessionId, generation: u64) -> FrameTransaction {
    FrameTransaction::Startup {
        earliest_constituent_enqueue_at: std::time::Instant::now(),
        reset: FrameReset {
            session_id,
            generation,
            size: PixelSize::new(1, 1).unwrap(),
            format: PixelFormat::Bgrx8UnormSrgb,
        },
        revision: FrameRevision {
            session_id,
            generation,
            revision: 1,
            patches: vec![PixelPatch {
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                stride_bytes: 4,
                pixels: PixelBuffer::new(vec![1, 2, 3, 0]),
            }],
            completeness: FrameCompleteness::FullBaseline,
        },
    }
}

#[test]
fn dropped_candidate_preserves_metadata_and_allows_exact_retry() {
    let session = SessionId::allocate();
    let mut state = RemoteUpdateState::default();
    let first = state
        .prepare_batch(vec![startup(session, 1)])
        .unwrap()
        .commit();
    let pending = state.pending_receipt();
    let candidate = state.prepare_batch(vec![startup(session, 2)]).unwrap();
    assert_eq!(candidate.identity().generation, 2);
    assert_eq!(candidate.operations().len(), 3);
    drop(candidate);
    assert_eq!(state.current_generation(), Some(1));
    assert_eq!(state.last_damage_revision(), 1);
    assert_eq!(state.pending_receipt(), pending);
    assert!(!state.baseline_presented());
    let retried = state
        .prepare_batch(vec![startup(session, 2)])
        .unwrap()
        .commit();
    assert_eq!(retried.installed_surface.unwrap().generation, 2);
    assert_eq!(
        state.confirm_presented(first.final_boundary.unwrap()),
        Err(TransactionError::StalePresentationReceipt)
    );
    let receipt = retried.final_boundary.unwrap();
    state.confirm_presented(receipt).unwrap();
    assert!(state.baseline_presented());
    assert_eq!(
        state.confirm_presented(receipt),
        Err(TransactionError::StalePresentationReceipt)
    );
}

#[test]
fn planning_failure_does_not_publish_partial_batch() {
    let session = SessionId::allocate();
    let mut state = RemoteUpdateState::default();
    let error = state
        .prepare_batch(vec![startup(session, 1), startup(session, 1)])
        .err()
        .unwrap();
    assert_eq!(error.error, TransactionError::StaleUpdate);
    assert_eq!(state.current_generation(), None);
    assert_eq!(state.pending_receipt(), None);
    state
        .prepare_batch(vec![startup(session, 1)])
        .unwrap()
        .commit();
    assert_eq!(state.current_generation(), Some(1));
}

#[test]
fn recovery_candidate_drop_retains_exact_recovery_requirement() {
    let session = SessionId::allocate();
    let mut state = RemoteUpdateState::default();
    state
        .prepare_batch(vec![startup(session, 3)])
        .unwrap()
        .commit();
    assert_eq!(
        state.invalidate_for_device_loss(),
        RecoveryRequirement::ResetAndFullSnapshot {
            session_id: session,
            generation: 3
        }
    );
    drop(state.prepare_batch(vec![startup(session, 3)]).unwrap());
    assert_eq!(
        state
            .prepare_batch(vec![startup(session, 4)])
            .err()
            .unwrap()
            .error,
        TransactionError::StaleUpdate
    );
    assert_eq!(state.pending_receipt(), None);
    state
        .prepare_batch(vec![startup(session, 3)])
        .unwrap()
        .commit();
    assert_eq!(state.current_generation(), Some(3));
}
