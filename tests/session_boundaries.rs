use freeremotedesk::core::{FrameRect, RemotePixelFormat, RenderUpdate};
use freeremotedesk::session::{
    backpressure::RenderUpdateQueue,
    engine::{SessionEvent, SessionModel, SessionPhase},
};

fn rect_update(generation: u64, byte_len: usize) -> RenderUpdate {
    assert_eq!(byte_len % 4, 0);
    let width = u32::try_from(byte_len / 4).unwrap();
    RenderUpdate::dirty_rect(
        generation,
        FrameRect::new(0, 0, width, 1).unwrap(),
        RemotePixelFormat::Bgra8Srgb,
        width * 4,
        vec![0; byte_len].into_boxed_slice(),
    )
    .unwrap()
}

#[test]
fn full_queue_replaces_older_pending_present_for_same_generation() {
    let mut queue = RenderUpdateQueue::with_limits(2, 1024).unwrap();

    queue.push(RenderUpdate::present(4)).unwrap();
    queue.push(RenderUpdate::present(4)).unwrap();

    assert_eq!(queue.len(), 1);
}

#[test]
fn reset_discards_queued_updates_from_older_generations() {
    let mut queue = RenderUpdateQueue::with_limits(8, 4096).unwrap();
    queue.push(rect_update(2, 64)).unwrap();

    queue
        .push(RenderUpdate::reset(3, 4, 4, RemotePixelFormat::Bgra8Srgb).unwrap())
        .unwrap();

    assert!(queue.iter().all(|update| update.generation() >= 3));
}

#[test]
fn oversized_update_is_rejected_without_mutating_queue() {
    let mut queue = RenderUpdateQueue::with_limits(4, 32).unwrap();

    let error = queue.push(rect_update(1, 64)).unwrap_err();

    assert_eq!(error.code(), "render_update_exceeds_budget");
    assert!(queue.is_empty());
}

#[test]
fn session_rejects_connected_before_surface_reset() {
    let mut model = SessionModel::default();
    model.apply(SessionEvent::Connecting).unwrap();

    let error = model
        .apply(SessionEvent::Connected { generation: 1 })
        .unwrap_err();

    assert_eq!(error.code(), "connected_without_surface");
    assert_eq!(model.snapshot().phase(), SessionPhase::Connecting);
}

#[test]
fn session_requires_matching_generation_before_connected() {
    let mut model = SessionModel::default();
    model.apply(SessionEvent::Connecting).unwrap();
    model
        .apply(SessionEvent::SurfaceReset {
            generation: 3,
            width: 1920,
            height: 1080,
            format: RemotePixelFormat::Bgra8Srgb,
        })
        .unwrap();

    let error = model
        .apply(SessionEvent::Connected { generation: 2 })
        .unwrap_err();

    assert_eq!(error.code(), "connected_generation_mismatch");
    assert_eq!(model.snapshot().phase(), SessionPhase::SurfaceReady);
}
