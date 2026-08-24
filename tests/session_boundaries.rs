use std::sync::{mpsc, Arc, Barrier};
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

struct ScriptAdapter {
    events: Vec<SessionEvent>,
    emitted_all: Sender<()>,
}

impl ProtocolAdapter for ScriptAdapter {
    fn run(
        self: Box<Self>,
        _context: ProtocolContext,
        _commands: Receiver<SessionCommand>,
        events: freeremotedesk::session::SessionEventSink,
    ) -> Result<(), freeremotedesk::session::SessionError> {
        for event in self.events {
            events.emit(event)?;
        }
        self.emitted_all
            .send(())
            .map_err(|_| freeremotedesk::session::SessionError::new("test_adapter_signal_closed"))
    }
}

struct FailedGenerationSwitchAdapter {
    emitted_all: Sender<()>,
}

struct FullEventMailboxAdapter {
    emitted_all: Sender<()>,
}

impl ProtocolAdapter for FullEventMailboxAdapter {
    fn run(
        self: Box<Self>,
        _context: ProtocolContext,
        _commands: Receiver<SessionCommand>,
        events: freeremotedesk::session::SessionEventSink,
    ) -> Result<(), freeremotedesk::session::SessionError> {
        events.emit(SessionEvent::Connecting)?;
        events.emit(SessionEvent::Bell)?;
        let error = events
            .emit(SessionEvent::ClipboardText("overflow".to_owned()))
            .unwrap_err();
        self.emitted_all.send(()).map_err(|_| {
            freeremotedesk::session::SessionError::new("test_adapter_signal_closed")
        })?;
        Err(error)
    }
}

impl ProtocolAdapter for FailedGenerationSwitchAdapter {
    fn run(
        self: Box<Self>,
        _context: ProtocolContext,
        _commands: Receiver<SessionCommand>,
        events: freeremotedesk::session::SessionEventSink,
    ) -> Result<(), freeremotedesk::session::SessionError> {
        for event in [
            SessionEvent::Connecting,
            SessionEvent::SurfaceReset {
                generation: 1,
                width: 1,
                height: 1,
                format: RemotePixelFormat::Bgra8Srgb,
            },
            SessionEvent::Render(
                RenderUpdate::reset(1, 1, 1, RemotePixelFormat::Bgra8Srgb).unwrap(),
            ),
            SessionEvent::Connected { generation: 1 },
            SessionEvent::SurfaceReset {
                generation: 2,
                width: 1,
                height: 1,
                format: RemotePixelFormat::Bgra8Srgb,
            },
        ] {
            events.emit(event)?;
        }
        let error = events
            .emit(SessionEvent::Render(rect_update(2, 12)))
            .unwrap_err();
        self.emitted_all.send(()).map_err(|_| {
            freeremotedesk::session::SessionError::new("test_adapter_signal_closed")
        })?;
        Err(error)
    }
}

struct CommandBackpressureAdapter {
    ready: Sender<()>,
    release: mpsc::Receiver<()>,
}

impl ProtocolAdapter for CommandBackpressureAdapter {
    fn run(
        self: Box<Self>,
        _context: ProtocolContext,
        commands: Receiver<SessionCommand>,
        events: freeremotedesk::session::SessionEventSink,
    ) -> Result<(), freeremotedesk::session::SessionError> {
        events.emit(SessionEvent::Connecting)?;
        self.ready.send(()).map_err(|_| {
            freeremotedesk::session::SessionError::new("test_adapter_signal_closed")
        })?;
        self.release.recv().map_err(|_| {
            freeremotedesk::session::SessionError::new("test_adapter_release_closed")
        })?;
        commands.recv().map_err(|_| {
            freeremotedesk::session::SessionError::new("test_command_channel_closed")
        })?;
        Ok(())
    }
}

struct ConcurrentRenderAdapter {
    emitted_all: Sender<()>,
}

impl ProtocolAdapter for ConcurrentRenderAdapter {
    fn run(
        self: Box<Self>,
        _context: ProtocolContext,
        _commands: Receiver<SessionCommand>,
        events: freeremotedesk::session::SessionEventSink,
    ) -> Result<(), freeremotedesk::session::SessionError> {
        for _ in 0..32 {
            events.emit(SessionEvent::Render(rect_update(1, 1024 * 1024)))?;
        }

        let barrier = Arc::new(Barrier::new(3));
        let first_events = events.clone();
        let first_barrier = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_events.emit(SessionEvent::Render(
                RenderUpdate::reset(2, 1, 1, RemotePixelFormat::Bgra8Srgb).unwrap(),
            ))
        });
        let second_events = events.clone();
        let second_barrier = Arc::clone(&barrier);
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_events.emit(SessionEvent::Render(
                RenderUpdate::reset(2, 1, 1, RemotePixelFormat::Bgra8Srgb).unwrap(),
            ))
        });
        barrier.wait();
        first.join().map_err(|_| {
            freeremotedesk::session::SessionError::new("test_render_thread_panicked")
        })??;
        second.join().map_err(|_| {
            freeremotedesk::session::SessionError::new("test_render_thread_panicked")
        })??;
        self.emitted_all
            .send(())
            .map_err(|_| freeremotedesk::session::SessionError::new("test_adapter_signal_closed"))
    }
}

fn drain_events(engine: &SessionEngine, wake_receiver: &Receiver<()>) -> Vec<SessionEvent> {
    let mut received = Vec::new();
    loop {
        match engine.try_next_event() {
            Ok(Some(event)) => received.push(event),
            Ok(None) => wake_receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(error) if error.code() == "session_event_channel_closed" => break,
            Err(error) => panic!("unexpected session error: {error}"),
        }
    }
    received
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
fn generation_switch_evicts_stale_render_without_desynchronizing_event_order() {
    let (wake_sender, wake_receiver) = crossbeam_channel::unbounded();
    let (emitted_sender, emitted_receiver) = crossbeam_channel::bounded(1);
    let engine = SessionEngine::spawn_with_mailbox_limits(
        Box::new(ScriptAdapter {
            events: vec![
                SessionEvent::Connecting,
                SessionEvent::SurfaceReset {
                    generation: 1,
                    width: 1,
                    height: 1,
                    format: RemotePixelFormat::Bgra8Srgb,
                },
                SessionEvent::Render(
                    RenderUpdate::reset(1, 1, 1, RemotePixelFormat::Bgra8Srgb).unwrap(),
                ),
                SessionEvent::Connected { generation: 1 },
                SessionEvent::SurfaceReset {
                    generation: 2,
                    width: 1,
                    height: 1,
                    format: RemotePixelFormat::Bgra8Srgb,
                },
                SessionEvent::Render(
                    RenderUpdate::reset(2, 1, 1, RemotePixelFormat::Bgra8Srgb).unwrap(),
                ),
                SessionEvent::Connected { generation: 2 },
            ],
            emitted_all: emitted_sender,
        }),
        ProtocolContext::new(test_connection()),
        Arc::new(TestWake {
            notifications: wake_sender,
        }),
        SessionMailboxLimits::new(8, 8, 8, 64).unwrap(),
    )
    .unwrap();

    emitted_receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    let received = drain_events(&engine, &wake_receiver);

    assert!(matches!(received[0], SessionEvent::Connecting));
    assert!(matches!(
        received[1],
        SessionEvent::SurfaceReset { generation: 1, .. }
    ));
    assert!(matches!(
        received[2],
        SessionEvent::Connected { generation: 1 }
    ));
    assert!(matches!(
        received[3],
        SessionEvent::SurfaceReset { generation: 2, .. }
    ));
    assert!(matches!(
        received[4],
        SessionEvent::Render(RenderUpdate::Reset { generation: 2, .. })
    ));
    assert!(matches!(
        received[5],
        SessionEvent::Connected { generation: 2 }
    ));
    assert_eq!(received.len(), 6);

    engine.join().unwrap();
}

#[test]
fn rejected_generation_switch_keeps_prior_render_and_reports_terminal_failure() {
    let (wake_sender, wake_receiver) = crossbeam_channel::unbounded();
    let (emitted_sender, emitted_receiver) = crossbeam_channel::bounded(1);
    let engine = SessionEngine::spawn_with_mailbox_limits(
        Box::new(FailedGenerationSwitchAdapter {
            emitted_all: emitted_sender,
        }),
        ProtocolContext::new(test_connection()),
        Arc::new(TestWake {
            notifications: wake_sender,
        }),
        SessionMailboxLimits::new(8, 8, 8, 8).unwrap(),
    )
    .unwrap();

    emitted_receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    let received = drain_events(&engine, &wake_receiver);

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
        SessionEvent::SurfaceReset { generation: 2, .. }
    ));
    assert!(matches!(
        received[5],
        SessionEvent::Failed {
            code: "render_update_exceeds_budget"
        }
    ));
    assert_eq!(received.len(), 6);

    engine.join().unwrap();
}

#[test]
fn full_event_mailbox_reserves_terminal_failure_delivery() {
    let (wake_sender, wake_receiver) = crossbeam_channel::unbounded();
    let (emitted_sender, emitted_receiver) = crossbeam_channel::bounded(1);
    let engine = SessionEngine::spawn_with_mailbox_limits(
        Box::new(FullEventMailboxAdapter {
            emitted_all: emitted_sender,
        }),
        ProtocolContext::new(test_connection()),
        Arc::new(TestWake {
            notifications: wake_sender,
        }),
        SessionMailboxLimits::new(8, 2, 2, 64).unwrap(),
    )
    .unwrap();

    emitted_receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    let received = drain_events(&engine, &wake_receiver);

    assert!(matches!(received[0], SessionEvent::Connecting));
    assert!(matches!(received[1], SessionEvent::Bell));
    assert!(matches!(
        received[2],
        SessionEvent::Failed {
            code: "session_event_channel_full"
        }
    ));
    assert_eq!(received.len(), 3);

    engine.join().unwrap();
}

#[test]
fn full_command_mailbox_returns_without_blocking_high_frequency_input() {
    let (wake_sender, _wake_receiver) = crossbeam_channel::unbounded();
    let (ready_sender, ready_receiver) = crossbeam_channel::bounded(1);
    let (release_sender, release_receiver) = mpsc::channel();
    let engine = SessionEngine::spawn_with_mailbox_limits(
        Box::new(CommandBackpressureAdapter {
            ready: ready_sender,
            release: release_receiver,
        }),
        ProtocolContext::new(test_connection()),
        Arc::new(TestWake {
            notifications: wake_sender,
        }),
        SessionMailboxLimits::new(1, 8, 8, 64).unwrap(),
    )
    .unwrap();
    ready_receiver.recv_timeout(Duration::from_secs(1)).unwrap();

    engine
        .send(SessionCommand::Pointer {
            x: 1,
            y: 1,
            buttons: 0,
        })
        .unwrap();

    std::thread::scope(|scope| {
        let (outcome_sender, outcome_receiver) = crossbeam_channel::bounded(1);
        let engine_ref = &engine;
        let send = scope.spawn(move || {
            outcome_sender
                .send(engine_ref.send(SessionCommand::Resize {
                    width: 2,
                    height: 2,
                }))
                .unwrap();
        });
        let immediate = outcome_receiver.recv_timeout(Duration::from_millis(100));
        release_sender.send(()).unwrap();
        send.join().unwrap();

        let error = immediate.expect("满命令邮箱不能阻塞 UI 线程").unwrap_err();
        assert_eq!(error.code(), "session_command_channel_full");
    });

    engine.join().unwrap();
}

#[test]
fn concurrent_render_emission_does_not_fail_on_transient_mailbox_lock_contention() {
    let (wake_sender, wake_receiver) = crossbeam_channel::unbounded();
    let (emitted_sender, emitted_receiver) = crossbeam_channel::bounded(1);
    let engine = SessionEngine::spawn_with_mailbox_limits(
        Box::new(ConcurrentRenderAdapter {
            emitted_all: emitted_sender,
        }),
        ProtocolContext::new(test_connection()),
        Arc::new(TestWake {
            notifications: wake_sender,
        }),
        SessionMailboxLimits::new(8, 64, 64, 64 * 1024 * 1024).unwrap(),
    )
    .unwrap();

    emitted_receiver
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    let received = drain_events(&engine, &wake_receiver);

    assert_eq!(
        received
            .iter()
            .filter(|event| matches!(event, SessionEvent::Failed { .. }))
            .count(),
        0
    );
    assert_eq!(
        received
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    SessionEvent::Render(RenderUpdate::Reset { generation: 2, .. })
                )
            })
            .count(),
        1
    );

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
