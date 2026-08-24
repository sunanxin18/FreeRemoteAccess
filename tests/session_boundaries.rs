use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use freeremotedesk::app::connection::{
    validate_connection, ConnectionRequest, ServiceKind, ValidatedConnection,
};
use freeremotedesk::core::{FrameRect, RemotePixelFormat, RenderUpdate};
use freeremotedesk::protocols::ProtocolAdapter;
use freeremotedesk::session::{
    backpressure::RenderUpdateQueue,
    engine::{
        ProtocolContext, SessionCommand, SessionEngine, SessionEvent, SessionMailboxLimits,
        SessionModel, SessionPhase, UiWakeHandle,
    },
};
use secrecy::SecretString;

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

fn test_connection() -> ValidatedConnection {
    validate_connection(ConnectionRequest {
        service: ServiceKind::LinuxVnc,
        host: "render-budget.example".to_owned(),
        port: None,
        username: "desktop-user".to_owned(),
        password: SecretString::from("test-secret".to_owned()),
        domain: None,
    })
    .unwrap()
}

#[derive(Debug)]
struct TestWake {
    notifications: Sender<()>,
}

impl UiWakeHandle for TestWake {
    fn wake(&self) -> Result<(), freeremotedesk::session::SessionError> {
        self.notifications
            .send(())
            .map_err(|_| freeremotedesk::session::SessionError::new("test_wake_closed"))?;
        Ok(())
    }
}

struct ByteBudgetAdapter {
    render_burst_finished: Sender<()>,
}

impl ProtocolAdapter for ByteBudgetAdapter {
    fn run(
        self: Box<Self>,
        _context: ProtocolContext,
        _commands: Receiver<SessionCommand>,
        events: freeremotedesk::session::SessionEventSink,
    ) -> Result<(), freeremotedesk::session::SessionError> {
        events.emit(SessionEvent::Connecting)?;
        events.emit(SessionEvent::SurfaceReset {
            generation: 1,
            width: 4,
            height: 1,
            format: RemotePixelFormat::Bgra8Srgb,
        })?;
        events.emit(SessionEvent::Render(
            RenderUpdate::reset(1, 4, 1, RemotePixelFormat::Bgra8Srgb).unwrap(),
        ))?;
        events.emit(SessionEvent::Connected { generation: 1 })?;
        events.emit(SessionEvent::Render(rect_update(1, 8)))?;
        events.emit(SessionEvent::Render(rect_update(1, 8)))?;
        let error = events
            .emit(SessionEvent::Render(rect_update(1, 4)))
            .unwrap_err();
        self.render_burst_finished.send(()).map_err(|_| {
            freeremotedesk::session::SessionError::new("test_adapter_signal_closed")
        })?;
        Err(error)
    }
}

#[test]
fn engine_fails_closed_when_adapter_render_bytes_exceed_aggregate_budget() {
    let (wake_sender, wake_receiver) = crossbeam_channel::unbounded();
    let (burst_sender, burst_receiver) = crossbeam_channel::bounded(1);
    let engine = SessionEngine::spawn_with_mailbox_limits(
        Box::new(ByteBudgetAdapter {
            render_burst_finished: burst_sender,
        }),
        ProtocolContext::new(test_connection()),
        Arc::new(TestWake {
            notifications: wake_sender,
        }),
        SessionMailboxLimits::new(8, 8, 8, 16).unwrap(),
    )
    .unwrap();

    burst_receiver.recv_timeout(Duration::from_secs(1)).unwrap();

    let mut received = Vec::new();
    loop {
        match engine.try_next_event() {
            Ok(Some(event)) => received.push(event),
            Ok(None) => wake_receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(error) if error.code() == "session_event_channel_closed" => break,
            Err(error) => panic!("unexpected session error: {error}"),
        }
    }

    assert!(matches!(received[0], SessionEvent::Connecting));
    assert!(matches!(
        received[1],
        SessionEvent::SurfaceReset { generation: 1, .. }
    ));
    assert!(matches!(
        received[2],
        SessionEvent::Render(RenderUpdate::Reset { generation: 1, .. })
    ));
    assert!(matches!(
        received[3],
        SessionEvent::Connected { generation: 1 }
    ));
    assert!(matches!(
        received[4],
        SessionEvent::Render(RenderUpdate::DirtyRect { ref pixels, .. }) if pixels.len() == 8
    ));
    assert!(matches!(
        received[5],
        SessionEvent::Render(RenderUpdate::DirtyRect { ref pixels, .. }) if pixels.len() == 8
    ));
    assert!(matches!(
        received[6],
        SessionEvent::Failed {
            code: "render_queue_full"
        }
    ));
    assert_eq!(received.len(), 7);

    engine.join().unwrap();
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
