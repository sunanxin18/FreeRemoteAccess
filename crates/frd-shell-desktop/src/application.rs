use std::cell::Cell;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use frd_app::{AppAction, AppIntent, AppLaunch, AppPage, AppPlatformStores};
use frd_compositor_wgpu::{
    PresentError, PresentationCompositor, PresentationHooks, PresentationSurface,
    PresentationSurfaceLease,
};
use frd_core::{
    ButtonState, ContentViewport, KeyState, Modifiers, PhysicalViewport, PixelRect, PixelSize,
    PointerButton, SessionId, TargetSystem,
};
use frd_frame::{
    EnqueuedSurfaceUpdate, FrameCompleteness, FrameMailbox, PixelBuffer, PixelFormat, PixelPatch,
    SurfaceUpdate,
};
use frd_media_api::{AudioOutput, AudioOutputError, MediaFrame, MediaPublishError, MediaPublisher};
use frd_protocol_api::{
    ConnectRequest, MailboxSurfacePublisher, ProtocolCatalog, ProtocolError, ProtocolExit,
    ProtocolFactory, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionCommand, SessionEvent,
};
use frd_render_wgpu::{
    ApplyOutcome, GpuContext, RecoveryRequirement, RemoteRenderer, RendererError,
};
use frd_session::{
    CleanupComplete, CleanupError, CleanupOperations, SessionCleanupHandle, SessionCoordinator,
    SessionStartFailure, SessionStartOutcome, SessionStartPermit,
};
use frd_ui_model::{
    CapabilityGlyphState, ConnectionGlyph, LaunchOptions, Page, SessionChromeAction,
    SessionChromeModel,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::cleanup::{
    spawn_cleanup, BackgroundCleanupFailure, BackgroundCleanupOutcome, CleanupPolicy,
    PendingCleanup,
};
use crate::fatal::{FatalComponent, FatalOperation, FatalReason, FatalReport};
use crate::frame_metrics::{
    checked_mailbox_age, BatchMetricContext, DrainedFrameUpdates, FramePipelineMetrics,
    MetricIdentity, SerialDrainAggregate,
};
use crate::frame_metrics_sink::MetricSinkError;
use crate::input::{hid_usage_from_key_code, KeyboardDomain, KeyboardPreDispatch};
use crate::lifecycle::{
    execute_presentation_recovery, OcclusionAction, PresentationLifecycle, PresentationOperation,
    PresentationRecoveryBackend, PresentationRecoveryContext, PresentationRecoveryFailure,
};
use crate::platform::PlatformWindowChrome;
use crate::repaint::{RepaintPlan, RepaintScheduler};
use crate::ui_fonts::system_font_definitions;
use crate::{
    ChromeHit, ChromeHitRegions, ChromeLayout, InputGate, InputOwnership, InputRouter,
    WindowChromeAdapter, TITLE_BAR_HEIGHT_POINTS,
};

const FRAME_MAILBOX_ENTRY_LIMIT: usize = 256;
const FRAME_MAILBOX_PIXEL_LIMIT: usize = 64 * 1024 * 1024;
const MEDIA_MAILBOX_ENTRY_LIMIT: usize = 16;
const PENDING_LAUNCH_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const TEST_SESSION_CHROME: SessionChromeModel = SessionChromeModel {
    connection: ConnectionGlyph::Connected,
    diagnostics: None,
    frame_response_ms: None,
    audio: CapabilityGlyphState::Unavailable,
    clipboard: CapabilityGlyphState::Unavailable,
    action: Some(SessionChromeAction::Disconnect),
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImePreDispatch<'a> {
    LocalChrome,
    Consume,
    Commit(&'a str),
}

fn classify_ime_before_egui<'a>(
    domain: KeyboardDomain,
    ime: &'a Ime,
    text_input_available: bool,
) -> ImePreDispatch<'a> {
    if domain == KeyboardDomain::LocalChrome {
        return ImePreDispatch::LocalChrome;
    }
    match ime {
        Ime::Commit(text) if text_input_available => ImePreDispatch::Commit(text),
        Ime::Commit(_) | Ime::Enabled | Ime::Preedit(_, _) | Ime::Disabled => {
            ImePreDispatch::Consume
        }
    }
}

pub trait WakeSink: Send + Sync {
    fn wake(&self) -> Result<(), ProtocolError>;
}

pub trait AudioOutputFactory: Send + Sync {
    fn open(&self) -> Result<Box<dyn AudioOutput>, AudioOutputError>;
}

#[cfg(test)]
enum TestLaunchOutcome {
    Started,
    LaunchRolledBack(SessionStartFailure),
}

pub enum AcceptedLaunchOutcome {
    Started,
    LaunchRolledBack(SessionStartFailure),
    CancelledStarted,
    CancelledLaunchRolledBack(SessionStartFailure),
}

pub struct BackgroundLaunchOutcome {
    coordinator: SessionCoordinator,
    result: BackgroundLaunchResult,
    cancelled_before_publish: bool,
}

enum BackgroundLaunchResult {
    Started {
        cleanup_handle: SessionCleanupHandle,
        ports: LiveSessionPorts,
        start_barrier: ProtocolStartBarrier,
    },
    LaunchRolledBack(SessionStartFailure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerKind {
    Audio,
    Protocol,
}

trait WorkerSpawner: Send + Sync {
    fn spawn(
        &self,
        kind: WorkerKind,
        name: String,
        work: Box<dyn FnOnce() + Send>,
    ) -> io::Result<JoinHandle<()>>;
}

struct SystemWorkerSpawner;

impl WorkerSpawner for SystemWorkerSpawner {
    fn spawn(
        &self,
        _kind: WorkerKind,
        name: String,
        work: Box<dyn FnOnce() + Send>,
    ) -> io::Result<JoinHandle<()>> {
        std::thread::Builder::new().name(name).spawn(work)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionHostError {
    NoActiveSession,
    CommandClosed,
    Cleanup(CleanupError),
    CleanupFatal(BackgroundCleanupFailure),
}

struct ChannelEventSink(mpsc::Sender<SessionEvent>);

impl RuntimeEventSink for ChannelEventSink {
    fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError> {
        self.0
            .send(event)
            .map_err(|_| ProtocolError::EventPortClosed)
    }
}

struct SharedWake(Arc<dyn WakeSink>);

impl RuntimeWake for SharedWake {
    fn wake(&self) -> Result<(), ProtocolError> {
        self.0.wake()
    }
}

struct AudioMediaPublisher(mpsc::SyncSender<MediaFrame>);

impl MediaPublisher for AudioMediaPublisher {
    fn publish(&self, frame: MediaFrame) -> Result<(), MediaPublishError> {
        self.0.try_send(frame).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => MediaPublishError::Full,
            mpsc::TrySendError::Disconnected(_) => MediaPublishError::Closed,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProtocolStartState {
    Pending,
    Started,
    Cancelled,
}

struct ProtocolStartBarrier {
    state: Arc<(Mutex<ProtocolStartState>, Condvar)>,
    resolved: bool,
}

struct ProtocolStartWaiter {
    state: Arc<(Mutex<ProtocolStartState>, Condvar)>,
}

impl ProtocolStartBarrier {
    fn new() -> (Self, ProtocolStartWaiter) {
        let state = Arc::new((Mutex::new(ProtocolStartState::Pending), Condvar::new()));
        (
            Self {
                state: state.clone(),
                resolved: false,
            },
            ProtocolStartWaiter { state },
        )
    }

    fn release(mut self) {
        self.resolve(ProtocolStartState::Started);
    }

    fn resolve(&mut self, resolution: ProtocolStartState) {
        if self.resolved {
            return;
        }
        let (lock, ready) = &*self.state;
        let mut state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *state == ProtocolStartState::Pending {
            *state = resolution;
            ready.notify_all();
        }
        self.resolved = true;
    }
}

impl Drop for ProtocolStartBarrier {
    fn drop(&mut self) {
        self.resolve(ProtocolStartState::Cancelled);
    }
}

impl ProtocolStartWaiter {
    fn wait(self) -> bool {
        let (lock, ready) = &*self.state;
        let mut state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *state == ProtocolStartState::Pending {
            state = ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *state == ProtocolStartState::Started
    }
}

struct LiveSessionPorts {
    session_id: SessionId,
    commands: mpsc::Sender<SessionCommand>,
    events: mpsc::Receiver<SessionEvent>,
    mailbox: Arc<Mutex<FrameMailbox>>,
}

struct LiveSessionCleanup {
    commands: Option<mpsc::Sender<SessionCommand>>,
    protocol_worker: Option<JoinHandle<()>>,
    audio_worker: Option<JoinHandle<()>>,
    mailbox: Option<Arc<Mutex<FrameMailbox>>>,
}

impl CleanupOperations for LiveSessionCleanup {
    fn cancel(&mut self) -> Result<(), CleanupError> {
        if let Some(commands) = &self.commands {
            let _ = commands.send(SessionCommand::Disconnect);
        }
        Ok(())
    }

    fn shutdown_writer(&mut self) -> Result<(), CleanupError> {
        drop(self.commands.take());
        Ok(())
    }

    fn join_workers_and_audio(&mut self) -> Result<(), CleanupError> {
        let (protocol_pending, protocol_panicked) = poll_worker(&mut self.protocol_worker);
        let (audio_pending, audio_panicked) = poll_worker(&mut self.audio_worker);
        if protocol_pending || audio_pending || protocol_panicked || audio_panicked {
            Err(CleanupError::JoinWorkersAndAudio)
        } else {
            Ok(())
        }
    }

    fn dispose_mailbox(&mut self) -> Result<(), CleanupError> {
        if let Some(mailbox) = self.mailbox.take() {
            let mut mailbox = mailbox.lock().map_err(|_| CleanupError::DisposeMailbox)?;
            while mailbox.pop().is_some() {}
        }
        Ok(())
    }
}

fn poll_worker(worker: &mut Option<JoinHandle<()>>) -> (bool, bool) {
    let Some(handle) = worker.as_ref() else {
        return (false, false);
    };
    if !handle.is_finished() {
        return (true, false);
    }
    let panicked = worker
        .take()
        .expect("finished worker handle remains owned")
        .join()
        .is_err();
    (false, panicked)
}

pub struct SessionHost {
    factories: Vec<Arc<dyn ProtocolFactory>>,
    coordinator: Option<SessionCoordinator>,
    wake: Arc<dyn WakeSink>,
    audio_factory: Arc<dyn AudioOutputFactory>,
    worker_spawner: Arc<dyn WorkerSpawner>,
    launch_in_flight: bool,
    launch_cancelled: Arc<AtomicBool>,
    active: Option<LiveSessionPorts>,
    cleanup_handle: Option<SessionCleanupHandle>,
    cleanup_in_flight: bool,
}

impl SessionHost {
    pub fn new(
        factories: impl IntoIterator<Item = Arc<dyn ProtocolFactory>>,
        wake: Arc<dyn WakeSink>,
        audio_factory: Arc<dyn AudioOutputFactory>,
    ) -> Self {
        Self::new_with_spawner(
            factories,
            wake,
            audio_factory,
            Arc::new(SystemWorkerSpawner),
        )
    }

    fn new_with_spawner(
        factories: impl IntoIterator<Item = Arc<dyn ProtocolFactory>>,
        wake: Arc<dyn WakeSink>,
        audio_factory: Arc<dyn AudioOutputFactory>,
        worker_spawner: Arc<dyn WorkerSpawner>,
    ) -> Self {
        let factories = factories.into_iter().collect::<Vec<_>>();
        let catalog = ProtocolCatalog::new(factories.iter().map(|factory| factory.descriptor().id));
        Self {
            factories,
            coordinator: Some(SessionCoordinator::new(catalog)),
            wake,
            audio_factory,
            worker_spawner,
            launch_in_flight: false,
            launch_cancelled: Arc::new(AtomicBool::new(false)),
            active: None,
            cleanup_handle: None,
            cleanup_in_flight: false,
        }
    }

    pub fn begin_launch(
        &mut self,
        permit: SessionStartPermit,
        target: TargetSystem,
        request: ConnectRequest,
        notify: impl Fn(BackgroundLaunchOutcome) + Send + Sync + 'static,
    ) -> Result<bool, SessionHostError> {
        if self.launch_in_flight || self.cleanup_handle.is_some() || self.cleanup_in_flight {
            return Ok(false);
        }
        let selected_factory = self
            .factories
            .iter()
            .find(|factory| factory.descriptor().id == request.protocol_id)
            .cloned();
        let pending = PendingLaunch {
            coordinator: self
                .coordinator
                .take()
                .expect("idle session host owns its coordinator"),
            permit,
            target,
            request,
        };
        let pending = Arc::new(Mutex::new(Some(pending)));
        let thread_pending = pending.clone();
        let wake = self.wake.clone();
        let audio_factory = self.audio_factory.clone();
        let worker_spawner = self.worker_spawner.clone();
        let cancelled = self.launch_cancelled.clone();
        cancelled.store(false, Ordering::Release);
        self.launch_in_flight = true;
        let notify = Arc::new(notify);
        let thread_notify = notify.clone();
        let thread_cancelled = cancelled.clone();
        let spawn_result = std::thread::Builder::new()
            .name("frd-session-launch".to_owned())
            .spawn(move || {
                let pending = take_pending_launch(&thread_pending);
                let outcome = run_background_launch(
                    pending,
                    selected_factory,
                    wake,
                    audio_factory,
                    worker_spawner,
                    thread_cancelled,
                );
                thread_notify(outcome);
            });
        if spawn_result.is_err() {
            let pending = take_pending_launch(&pending);
            notify(rollback_without_resources(
                pending,
                cancelled.load(Ordering::Acquire),
            ));
        }
        Ok(true)
    }

    pub fn cancel_pending_launch(&mut self) -> bool {
        if !self.launch_in_flight {
            return false;
        }
        self.launch_cancelled.store(true, Ordering::Release);
        true
    }

    pub fn launch_is_pending(&self) -> bool {
        self.launch_in_flight
    }

    pub fn accept_launch_outcome(
        &mut self,
        outcome: BackgroundLaunchOutcome,
        notify_cleanup: impl FnOnce(BackgroundCleanupOutcome) + Send + 'static,
    ) -> Result<AcceptedLaunchOutcome, SessionHostError> {
        if !self.launch_in_flight {
            return Err(SessionHostError::NoActiveSession);
        }
        self.launch_in_flight = false;
        let cancelled =
            outcome.cancelled_before_publish || self.launch_cancelled.swap(false, Ordering::AcqRel);
        match outcome.result {
            BackgroundLaunchResult::LaunchRolledBack(failure) => {
                self.coordinator = Some(outcome.coordinator);
                if cancelled {
                    Ok(AcceptedLaunchOutcome::CancelledLaunchRolledBack(failure))
                } else {
                    Ok(AcceptedLaunchOutcome::LaunchRolledBack(failure))
                }
            }
            BackgroundLaunchResult::Started {
                cleanup_handle,
                ports,
                start_barrier,
            } if !cancelled => {
                self.coordinator = Some(outcome.coordinator);
                self.active = Some(ports);
                self.cleanup_handle = Some(cleanup_handle);
                start_barrier.release();
                Ok(AcceptedLaunchOutcome::Started)
            }
            BackgroundLaunchResult::Started {
                cleanup_handle,
                ports,
                start_barrier,
            } => {
                drop(start_barrier);
                drop(ports);
                self.cleanup_in_flight = true;
                match spawn_cleanup(
                    PendingCleanup::new(outcome.coordinator, cleanup_handle),
                    CleanupPolicy::new(500, std::time::Duration::from_millis(10)),
                    notify_cleanup,
                ) {
                    Ok(()) => Ok(AcceptedLaunchOutcome::CancelledStarted),
                    Err(failure) => {
                        self.cleanup_in_flight = false;
                        Err(SessionHostError::CleanupFatal(failure))
                    }
                }
            }
        }
    }

    #[cfg(test)]
    fn complete_test_launch(
        &mut self,
        permit: SessionStartPermit,
        target: TargetSystem,
        request: ConnectRequest,
    ) -> TestLaunchOutcome {
        let (outcome_tx, outcome_rx) = mpsc::channel();
        assert!(self
            .begin_launch(permit, target, request, move |outcome| {
                outcome_tx.send(outcome).unwrap();
            })
            .expect("test background launch starts"));
        let outcome = outcome_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("test background launch completes");
        match self
            .accept_launch_outcome(outcome, |_| panic!("normal launch cannot start cleanup"))
            .expect("test launch outcome is current")
        {
            AcceptedLaunchOutcome::Started => TestLaunchOutcome::Started,
            AcceptedLaunchOutcome::LaunchRolledBack(failure) => {
                TestLaunchOutcome::LaunchRolledBack(failure)
            }
            AcceptedLaunchOutcome::CancelledStarted
            | AcceptedLaunchOutcome::CancelledLaunchRolledBack(_) => {
                panic!("test launch was not cancelled")
            }
        }
    }

    pub fn send_command(&self, command: SessionCommand) -> Result<(), SessionHostError> {
        self.active
            .as_ref()
            .ok_or(SessionHostError::NoActiveSession)?
            .commands
            .send(command)
            .map_err(|_| SessionHostError::CommandClosed)
    }

    pub fn drain_session_events(&mut self) -> Vec<(SessionId, SessionEvent)> {
        self.active
            .as_mut()
            .map(|active| {
                active
                    .events
                    .try_iter()
                    .map(|event| (active.session_id, event))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn drain_frame_updates(&mut self) -> Vec<SurfaceUpdate> {
        self.drain_enqueued_frame_updates()
            .into_iter()
            .map(|entry| entry.update)
            .collect()
    }

    fn drain_enqueued_frame_updates(&mut self) -> Vec<EnqueuedSurfaceUpdate> {
        let Some(active) = self.active.as_ref() else {
            return Vec::new();
        };
        let Ok(mut mailbox) = active.mailbox.lock() else {
            return Vec::new();
        };
        let mut updates = Vec::with_capacity(mailbox.len());
        while let Some(update) = mailbox.pop_enqueued() {
            updates.push(update);
        }
        updates
    }

    fn active_session_id(&self) -> Option<SessionId> {
        self.active.as_ref().map(|active| active.session_id)
    }

    pub fn is_active(&self) -> bool {
        self.launch_in_flight || self.cleanup_handle.is_some() || self.cleanup_in_flight
    }

    pub fn begin_cleanup(
        &mut self,
        notify: impl FnOnce(BackgroundCleanupOutcome) + Send + 'static,
    ) -> Result<bool, BackgroundCleanupFailure> {
        if self.cleanup_in_flight || self.launch_in_flight {
            return Ok(false);
        }
        let Some(handle) = self.cleanup_handle.take() else {
            return Ok(false);
        };
        drop(self.active.take());
        let coordinator = self
            .coordinator
            .take()
            .expect("started session owns its coordinator");
        self.cleanup_in_flight = true;
        match spawn_cleanup(
            PendingCleanup::new(coordinator, handle),
            CleanupPolicy::new(500, std::time::Duration::from_millis(10)),
            notify,
        ) {
            Ok(()) => Ok(true),
            Err(error) => {
                self.cleanup_in_flight = false;
                Err(error)
            }
        }
    }

    pub fn accept_cleanup_outcome(
        &mut self,
        outcome: BackgroundCleanupOutcome,
    ) -> Result<CleanupComplete, SessionHostError> {
        self.cleanup_in_flight = false;
        match outcome {
            BackgroundCleanupOutcome::Complete {
                coordinator,
                completion,
            } => {
                self.coordinator = Some(coordinator);
                Ok(completion)
            }
            BackgroundCleanupOutcome::Fatal(failure) => {
                Err(SessionHostError::CleanupFatal(failure))
            }
        }
    }
}

impl Drop for SessionHost {
    fn drop(&mut self) {
        self.launch_cancelled.store(true, Ordering::Release);
        if let Some(active) = self.active.as_ref() {
            let _ = active.commands.send(SessionCommand::Disconnect);
        }
    }
}

struct PendingLaunch {
    coordinator: SessionCoordinator,
    permit: SessionStartPermit,
    target: TargetSystem,
    request: ConnectRequest,
}

fn take_pending_launch(pending: &Mutex<Option<PendingLaunch>>) -> PendingLaunch {
    pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .expect("exactly one launch worker consumes the transaction")
}

fn run_background_launch(
    pending: PendingLaunch,
    selected_factory: Option<Arc<dyn ProtocolFactory>>,
    wake: Arc<dyn WakeSink>,
    audio_factory: Arc<dyn AudioOutputFactory>,
    worker_spawner: Arc<dyn WorkerSpawner>,
    cancelled: Arc<AtomicBool>,
) -> BackgroundLaunchOutcome {
    let PendingLaunch {
        mut coordinator,
        permit,
        target,
        request,
    } = pending;
    let mut launched_session = None;
    let outcome = coordinator.start(permit, target, request, |request| {
        if cancelled.load(Ordering::Acquire) {
            return Err(ProtocolError::Terminal);
        }
        let factory = selected_factory.ok_or(ProtocolError::UnregisteredProtocol)?;
        let (cleanup, ports, start_barrier) = launch_live_session(
            factory,
            wake,
            audio_factory,
            worker_spawner,
            cancelled.clone(),
            request,
        )?;
        launched_session = Some((ports, start_barrier));
        Ok(Box::new(cleanup) as Box<dyn CleanupOperations>)
    });
    let cancelled_before_publish = cancelled.load(Ordering::Acquire);
    let result = match outcome {
        SessionStartOutcome::Started(cleanup_handle) => {
            let (ports, start_barrier) =
                launched_session.expect("started transaction owns live ports and barrier");
            BackgroundLaunchResult::Started {
                cleanup_handle,
                ports,
                start_barrier,
            }
        }
        SessionStartOutcome::LaunchRolledBack(failure) => {
            debug_assert!(launched_session.is_none());
            BackgroundLaunchResult::LaunchRolledBack(failure)
        }
    };
    BackgroundLaunchOutcome {
        coordinator,
        result,
        cancelled_before_publish,
    }
}

fn rollback_without_resources(
    pending: PendingLaunch,
    cancelled_before_publish: bool,
) -> BackgroundLaunchOutcome {
    let PendingLaunch {
        mut coordinator,
        permit,
        target,
        request,
    } = pending;
    let SessionStartOutcome::LaunchRolledBack(failure) =
        coordinator.start(permit, target, request, |_| Err(ProtocolError::Terminal))
    else {
        unreachable!("resource-free launch failure cannot start a session");
    };
    BackgroundLaunchOutcome {
        coordinator,
        result: BackgroundLaunchResult::LaunchRolledBack(failure),
        cancelled_before_publish,
    }
}

fn launch_live_session(
    factory: Arc<dyn ProtocolFactory>,
    wake: Arc<dyn WakeSink>,
    audio_factory: Arc<dyn AudioOutputFactory>,
    worker_spawner: Arc<dyn WorkerSpawner>,
    cancelled: Arc<AtomicBool>,
    request: ConnectRequest,
) -> Result<(LiveSessionCleanup, LiveSessionPorts, ProtocolStartBarrier), ProtocolError> {
    let session_id = request.session_id;
    let (command_tx, command_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let mailbox = Arc::new(Mutex::new(FrameMailbox::new(
        FRAME_MAILBOX_ENTRY_LIMIT,
        FRAME_MAILBOX_PIXEL_LIMIT,
    )));
    let (media_tx, media_rx) = mpsc::sync_channel(MEDIA_MAILBOX_ENTRY_LIMIT);
    let runtime = ProtocolRuntime::new(
        session_id,
        command_rx,
        Box::new(ChannelEventSink(event_tx.clone())),
        Box::new(MailboxSurfacePublisher::new(mailbox.clone())),
        Some(Box::new(AudioMediaPublisher(media_tx))),
        Box::new(SharedWake(wake.clone())),
    );
    let session = factory.create(request, runtime)?;
    if cancelled.load(Ordering::Acquire) {
        return Err(ProtocolError::Terminal);
    }

    let (audio_start_tx, audio_start_rx) = mpsc::channel();
    let audio_events = event_tx.clone();
    let audio_wake = wake.clone();
    let audio_worker = worker_spawner
        .spawn(
            WorkerKind::Audio,
            format!("frd-audio-{}", session_id.get()),
            Box::new(move || {
                if audio_start_rx.recv().is_err() {
                    return;
                }
                let degraded = match catch_unwind(AssertUnwindSafe(|| {
                    run_audio_worker(audio_factory, media_rx)
                })) {
                    Ok(AudioWorkerExit::Closed) => false,
                    Ok(AudioWorkerExit::Failed) | Err(_) => true,
                };
                if degraded {
                    let _ = audio_events.send(SessionEvent::AudioState(
                        frd_protocol_api::AudioState::Failed,
                    ));
                    let _ = audio_wake.wake();
                }
            }),
        )
        .map_err(|_| ProtocolError::Terminal)?;
    if cancelled.load(Ordering::Acquire) {
        drop(audio_start_tx);
        let _ = audio_worker.join();
        return Err(ProtocolError::Terminal);
    }
    let (start_barrier, protocol_start_waiter) = ProtocolStartBarrier::new();
    let final_events = event_tx;
    let final_wake = wake;
    let protocol_worker = match worker_spawner.spawn(
        WorkerKind::Protocol,
        format!("frd-session-{}", session_id.get()),
        Box::new(move || {
            if !protocol_start_waiter.wait() {
                return;
            }
            let exit = catch_unwind(AssertUnwindSafe(|| session.run()))
                .unwrap_or(ProtocolExit::Failed(ProtocolError::Terminal));
            let _ = final_events.send(SessionEvent::Closed(exit));
            let _ = final_wake.wake();
        }),
    ) {
        Ok(worker) => worker,
        Err(_) => {
            // The protocol closure (and its runtime media sender) has been dropped.
            // The audio start barrier is then aborted, so no platform open can run.
            drop(audio_start_tx);
            let _ = audio_worker.join();
            return Err(ProtocolError::Terminal);
        }
    };
    let _ = audio_start_tx.send(());

    Ok((
        LiveSessionCleanup {
            commands: Some(command_tx.clone()),
            protocol_worker: Some(protocol_worker),
            audio_worker: Some(audio_worker),
            mailbox: Some(mailbox.clone()),
        },
        LiveSessionPorts {
            session_id,
            commands: command_tx,
            events: event_rx,
            mailbox,
        },
        start_barrier,
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AudioWorkerExit {
    Closed,
    Failed,
}

fn run_audio_worker(
    factory: Arc<dyn AudioOutputFactory>,
    media: mpsc::Receiver<MediaFrame>,
) -> AudioWorkerExit {
    let (sample_rate_hz, channels, samples) = loop {
        match media.recv() {
            Ok(MediaFrame::Pcm {
                sample_rate_hz,
                channels,
                samples,
            }) => break (sample_rate_hz, channels, samples),
            Ok(_) => {}
            Err(_) => return AudioWorkerExit::Closed,
        }
    };
    let Ok(mut output) = factory.open() else {
        return AudioWorkerExit::Failed;
    };
    if output
        .enqueue_pcm(sample_rate_hz, channels, samples)
        .is_err()
    {
        return AudioWorkerExit::Failed;
    }
    while let Ok(frame) = media.recv() {
        if let MediaFrame::Pcm {
            sample_rate_hz,
            channels,
            samples,
        } = frame
        {
            if output
                .enqueue_pcm(sample_rate_hz, channels, samples)
                .is_err()
            {
                return AudioWorkerExit::Failed;
            }
        }
    }
    AudioWorkerExit::Closed
}

pub enum DesktopUserEvent {
    Wake,
    Repaint,
    LaunchFinished(BackgroundLaunchOutcome),
    CleanupFinished(BackgroundCleanupOutcome),
    PresentationFatal(PresentationFailure),
    ApplicationFatal(FatalReport),
    ResizeTestTexture,
    ExitTestTexture,
    AccessKit(egui_winit::accesskit_winit::Event),
}

impl From<egui_winit::accesskit_winit::Event> for DesktopUserEvent {
    fn from(event: egui_winit::accesskit_winit::Event) -> Self {
        Self::AccessKit(event)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationFailure {
    pub operation: PresentationOperation,
    pub source: PresentError,
    pub retry: Option<PresentError>,
    pub recovery: Option<PresentError>,
}

#[derive(Default)]
struct RuntimeWakeGate {
    armed: AtomicBool,
}

impl RuntimeWakeGate {
    fn arm(&self) -> bool {
        // AcqRel elects one sender while observing the preceding Release from
        // either UI consumption or a failed-send rollback.
        self.armed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn consume(&self) {
        // Release before draining lets a concurrent publisher acquire the gate
        // and queue the next level-triggered wake without being lost.
        self.armed.store(false, Ordering::Release);
    }

    fn rollback_failed_send(&self) {
        self.armed.store(false, Ordering::Release);
    }
}

struct EventLoopWake {
    proxy: EventLoopProxy<DesktopUserEvent>,
    gate: Arc<RuntimeWakeGate>,
}

impl WakeSink for EventLoopWake {
    fn wake(&self) -> Result<(), ProtocolError> {
        if !self.gate.arm() {
            return Ok(());
        }
        self.proxy.send_event(DesktopUserEvent::Wake).map_err(|_| {
            self.gate.rollback_failed_send();
            ProtocolError::WakeFailed
        })
    }
}

struct WindowPresentationHook {
    window: Arc<Window>,
    actual_submit: Cell<bool>,
}

impl WindowPresentationHook {
    fn new(window: Arc<Window>) -> Self {
        Self {
            window,
            actual_submit: Cell::new(false),
        }
    }

    fn actual_submit(&self) -> bool {
        self.actual_submit.get()
    }
}

impl PresentationHooks for WindowPresentationHook {
    fn before_submit(&self) {
        self.actual_submit.set(true);
        self.window.pre_present_notify();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TestTextureOptions {
    pub exit_after: Option<std::time::Duration>,
    pub resize_after: Option<std::time::Duration>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TestTextureStage {
    Connection,
    RemoteSession,
}

enum DesktopMode {
    Product,
    TestTexture {
        stage: TestTextureStage,
        session_id: SessionId,
        exit_after: Option<std::time::Duration>,
        resize_after: Option<std::time::Duration>,
        driver_started: bool,
    },
}

#[derive(Clone, Copy)]
struct RemoteBinding {
    session_id: SessionId,
    generation: u64,
    size: PixelSize,
}

#[derive(Default)]
struct DpiTransition {
    pending: bool,
}

impl DpiTransition {
    fn begin(&mut self) {
        self.pending = true;
    }

    fn finish_resize(&mut self) {
        self.pending = false;
    }

    fn is_pending(&self) -> bool {
        self.pending
    }

    fn settle(&mut self, actual: PixelSize) -> Option<PixelSize> {
        self.pending.then(|| {
            self.pending = false;
            actual
        })
    }
}

#[derive(Default)]
struct PendingTextureWrites {
    pending: bool,
}

impl PendingTextureWrites {
    fn record_damage_upload(&mut self, uploaded_rectangles: usize) {
        self.pending |= uploaded_rectangles > 0;
    }

    #[cfg(test)]
    fn is_pending(&self) -> bool {
        self.pending
    }

    fn take_for_blocked_present(
        &mut self,
        accepts_redraw: bool,
        dpi_transition_pending: bool,
    ) -> bool {
        if !should_submit_pending_texture_writes(
            self.pending,
            accepts_redraw,
            dpi_transition_pending,
        ) {
            return false;
        }
        self.pending = false;
        true
    }

    fn finish_render(&mut self, actual_submit: bool, render_succeeded: bool) -> bool {
        if actual_submit {
            self.pending = false;
            return false;
        }
        if render_succeeded && self.pending {
            self.pending = false;
            return true;
        }
        false
    }
}

struct DesktopWindowState {
    chrome: PlatformWindowChrome,
    window: Arc<Window>,
    gpu: GpuContext,
    renderer: RemoteRenderer,
    compositor: PresentationCompositor,
    egui_context: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    physical_size: PixelSize,
    remote_area: Option<PixelRect>,
    chrome_layout: Option<ChromeLayout>,
    cursor_position: Option<(u32, u32)>,
    lifecycle: PresentationLifecycle,
    remote: Option<RemoteBinding>,
    dpi_transition: DpiTransition,
    pending_texture_writes: PendingTextureWrites,
    focus_session_chrome: bool,
}

impl DesktopWindowState {
    fn refresh_chrome_geometry(&mut self) -> Option<ChromeLayout> {
        self.chrome_layout = None;
        let insets = self.chrome.native_insets(&self.window);
        let layout = ChromeLayout::for_window(
            self.physical_size.width,
            self.physical_size.height,
            self.window.scale_factor(),
            insets.leading_px,
            insets.trailing_px,
        )?;
        self.remote_area = Some(layout.content_rect);
        self.chrome_layout = Some(layout);
        self.chrome.publish_hit_regions(ChromeHitRegions { layout });
        Some(layout)
    }
}

#[derive(Default)]
struct ApplicationExitState {
    when_clean: bool,
    pending_launch_deadline: Option<std::time::Instant>,
    fatal: Option<FatalReport>,
}

impl ApplicationExitState {
    fn cancel_pending_launch(
        &mut self,
        sessions: &mut SessionHost,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        if !sessions.cancel_pending_launch() {
            return None;
        }
        self.when_clean = true;
        if let Some(deadline) = self.pending_launch_deadline {
            return Some(deadline);
        }
        let deadline = now + PENDING_LAUNCH_SHUTDOWN_TIMEOUT;
        self.pending_launch_deadline = Some(deadline);
        Some(deadline)
    }

    fn launch_finished(&mut self) {
        self.pending_launch_deadline = None;
    }

    fn wait_for_cleanup(&mut self) {
        self.when_clean = true;
    }

    fn pending_launch_deadline(&self) -> Option<std::time::Instant> {
        self.pending_launch_deadline
    }

    fn should_exit(&self, sessions: &SessionHost) -> bool {
        self.when_clean && !sessions.is_active()
    }

    fn latch_fatal(&mut self, report: FatalReport) -> bool {
        if self.fatal.is_some() {
            return false;
        }
        self.pending_launch_deadline = None;
        self.fatal = Some(report);
        true
    }

    fn should_ignore_events(&self) -> bool {
        self.fatal.is_some()
    }

    fn runner_result(&self) -> Result<(), FatalReport> {
        self.fatal.clone().map_or(Ok(()), Err)
    }
}

pub struct DesktopPlatformStores {
    server_identities: Arc<dyn frd_platform_api::ServerIdentityStore>,
    profiles: Arc<dyn frd_platform_api::ConnectionProfileStore>,
    credentials: Arc<dyn frd_platform_api::SecureCredentialStore>,
}

impl DesktopPlatformStores {
    pub fn new(
        server_identities: Arc<dyn frd_platform_api::ServerIdentityStore>,
        profiles: Arc<dyn frd_platform_api::ConnectionProfileStore>,
        credentials: Arc<dyn frd_platform_api::SecureCredentialStore>,
    ) -> Self {
        Self {
            server_identities,
            profiles,
            credentials,
        }
    }

    pub fn as_app_stores(&self) -> AppPlatformStores<'_> {
        AppPlatformStores {
            server_identities: self.server_identities.as_ref(),
            profiles: self.profiles.as_ref(),
            credentials: self.credentials.as_ref(),
        }
    }
}

pub struct DesktopApplication {
    launch: AppLaunch,
    catalog: ProtocolCatalog,
    stores: DesktopPlatformStores,
    sessions: SessionHost,
    input: InputRouter,
    metrics: FramePipelineMetrics,
    proxy: EventLoopProxy<DesktopUserEvent>,
    runtime_wake_gate: Arc<RuntimeWakeGate>,
    window: Option<DesktopWindowState>,
    mode: DesktopMode,
    exit_state: ApplicationExitState,
    return_to_form_after_cancelled_launch: bool,
    repaint_scheduler: RepaintScheduler,
    armed_repaint: Option<RepaintPlan>,
    window_configuration: DesktopWindowConfiguration,
}

#[cfg(test)]
fn initialize_metrics_before_session_launch<T>(
    metrics: Result<FramePipelineMetrics, MetricSinkError>,
    launch: impl FnOnce(FramePipelineMetrics) -> T,
) -> Result<T, FatalReport> {
    metrics
        .map(launch)
        .map_err(FatalReport::frame_metrics_startup)
}

#[derive(Clone, Debug, Default)]
pub struct DesktopWindowConfiguration {
    pub icon: Option<winit::window::Icon>,
}

fn mark_texture_deltas_applied(deltas: &mut egui::TexturesDelta) {
    deltas.clear();
}

fn should_submit_pending_texture_writes(
    applied_nonempty_damage: bool,
    accepts_redraw: bool,
    dpi_transition_pending: bool,
) -> bool {
    applied_nonempty_damage && (!accepts_redraw || dpi_transition_pending)
}

#[cfg(test)]
mod dpi_transition_tests {
    use frd_core::PixelSize;

    use super::DpiTransition;

    #[test]
    fn scale_change_waits_for_the_matching_physical_size() {
        let mut transition = DpiTransition::default();
        let committed = PixelSize::new(1200, 800).unwrap();
        let resized = PixelSize::new(1800, 1200).unwrap();

        transition.begin();
        assert!(transition.is_pending());
        assert_eq!(transition.settle(committed), Some(committed));

        transition.begin();
        transition.finish_resize();
        assert!(!transition.is_pending());
        assert_eq!(transition.settle(resized), None);
    }
}

#[cfg(target_os = "windows")]
fn paint_platform_window_controls(
    ui: &egui::Ui,
    layout: ChromeLayout,
    scale_factor: f64,
    maximized: bool,
) {
    // WM_NCCALCSIZE gives WGPU the full frame, so Windows no longer paints the
    // caption glyph pixels. These are visual mirrors only: DwmDefWindowProc
    // still owns native caption hit testing, hover tooltips and Snap Layout.
    let color = ui.visuals().text_color();
    let stroke = egui::Stroke::new(1.2, color);
    let to_points = |rect: crate::ChromeRect| {
        let scale = scale_factor as f32;
        egui::Rect::from_min_size(
            egui::pos2(rect.x as f32 / scale, rect.y as f32 / scale),
            egui::vec2(rect.width as f32 / scale, rect.height as f32 / scale),
        )
    };
    if let Some(rect) = layout.minimize_button.map(to_points) {
        let center = rect.center();
        ui.painter().line_segment(
            [
                center + egui::vec2(-5.0, 3.0),
                center + egui::vec2(5.0, 3.0),
            ],
            stroke,
        );
    }
    if let Some(rect) = layout.maximize_button.map(to_points) {
        let center = rect.center();
        if maximized {
            ui.painter().rect_stroke(
                egui::Rect::from_center_size(center + egui::vec2(-1.5, 1.5), egui::vec2(8.0, 8.0)),
                0.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            ui.painter().rect_stroke(
                egui::Rect::from_center_size(center + egui::vec2(1.5, -1.5), egui::vec2(8.0, 8.0)),
                0.0,
                stroke,
                egui::StrokeKind::Inside,
            );
        } else {
            ui.painter().rect_stroke(
                egui::Rect::from_center_size(center, egui::vec2(10.0, 10.0)),
                0.0,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
    }
    if let Some(rect) = layout.close_button.map(to_points) {
        let center = rect.center();
        ui.painter().line_segment(
            [
                center + egui::vec2(-5.0, -5.0),
                center + egui::vec2(5.0, 5.0),
            ],
            stroke,
        );
        ui.painter().line_segment(
            [
                center + egui::vec2(5.0, -5.0),
                center + egui::vec2(-5.0, 5.0),
            ],
            stroke,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn paint_platform_window_controls(
    _ui: &egui::Ui,
    _layout: ChromeLayout,
    _scale_factor: f64,
    _maximized: bool,
) {
}

impl DesktopApplication {
    pub fn runner_result(&self) -> Result<(), FatalReport> {
        self.exit_state.runner_result()
    }

    pub fn set_window_configuration(&mut self, configuration: DesktopWindowConfiguration) {
        self.window_configuration = configuration;
    }

    pub fn new_product(
        launch: AppLaunch,
        factories: impl IntoIterator<Item = Arc<dyn ProtocolFactory>>,
        stores: DesktopPlatformStores,
        audio_factory: Arc<dyn AudioOutputFactory>,
        proxy: EventLoopProxy<DesktopUserEvent>,
    ) -> Self {
        let factories = factories.into_iter().collect::<Vec<_>>();
        let catalog = ProtocolCatalog::new(factories.iter().map(|factory| factory.descriptor().id));
        let runtime_wake_gate = Arc::new(RuntimeWakeGate::default());
        let wake = Arc::new(EventLoopWake {
            proxy: proxy.clone(),
            gate: runtime_wake_gate.clone(),
        });
        let (metrics, exit_state) =
            match FramePipelineMetrics::from_environment(std::time::Instant::now()) {
                Ok(metrics) => (metrics, ApplicationExitState::default()),
                Err(error) => (
                    FramePipelineMetrics::disabled(),
                    ApplicationExitState {
                        fatal: Some(FatalReport::frame_metrics_startup(error)),
                        ..ApplicationExitState::default()
                    },
                ),
            };
        Self {
            launch,
            catalog,
            stores,
            sessions: SessionHost::new(factories, wake, audio_factory),
            input: InputRouter::default(),
            metrics,
            proxy,
            runtime_wake_gate,
            window: None,
            mode: DesktopMode::Product,
            exit_state,
            return_to_form_after_cancelled_launch: false,
            repaint_scheduler: RepaintScheduler::default(),
            armed_repaint: None,
            window_configuration: DesktopWindowConfiguration::default(),
        }
    }

    pub fn new_test_texture(
        proxy: EventLoopProxy<DesktopUserEvent>,
        options: TestTextureOptions,
    ) -> Self {
        let catalog = ProtocolCatalog::new([]);
        let launch = AppLaunch::new(LaunchOptions::default(), &UnavailableCredentials, &catalog);
        let runtime_wake_gate = Arc::new(RuntimeWakeGate::default());
        let wake = Arc::new(EventLoopWake {
            proxy: proxy.clone(),
            gate: runtime_wake_gate.clone(),
        });
        let session_id = SessionId::allocate();
        Self {
            launch,
            catalog,
            stores: DesktopPlatformStores::new(
                Arc::new(UnavailableIdentityStore),
                Arc::new(UnavailableProfileStore),
                Arc::new(UnavailableCredentialStore),
            ),
            sessions: SessionHost::new(
                std::iter::empty::<Arc<dyn ProtocolFactory>>(),
                wake,
                Arc::new(UnavailableAudioFactory),
            ),
            input: InputRouter::default(),
            metrics: FramePipelineMetrics::disabled(),
            proxy,
            runtime_wake_gate,
            window: None,
            mode: DesktopMode::TestTexture {
                stage: TestTextureStage::Connection,
                session_id,
                exit_after: options.exit_after,
                resize_after: options.resize_after,
                driver_started: false,
            },
            exit_state: ApplicationExitState::default(),
            return_to_form_after_cancelled_launch: false,
            repaint_scheduler: RepaintScheduler::default(),
            armed_repaint: None,
            window_configuration: DesktopWindowConfiguration::default(),
        }
    }

    fn initialize_window(
        &self,
        event_loop: &ActiveEventLoop,
    ) -> Result<DesktopWindowState, FatalReport> {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("FreeRemoteDesk")
                        .with_window_icon(self.window_configuration.icon.clone())
                        .with_visible(false)
                        .with_inner_size(LogicalSize::new(1100.0, 720.0))
                        .with_min_inner_size(LogicalSize::new(520.0, 360.0))
                        .with_resizable(true),
                )
                .map_err(|_| {
                    FatalReport::internal(
                        FatalComponent::Window,
                        FatalOperation::Initialize,
                        FatalReason::WindowCreateFailed,
                    )
                })?,
        );
        let mut chrome = PlatformWindowChrome::new();
        chrome.configure(&window).map_err(|_| {
            FatalReport::internal(
                FatalComponent::Window,
                FatalOperation::Initialize,
                FatalReason::WindowChromeFailed,
            )
        })?;
        let physical = window.inner_size();
        let physical_size = PixelSize::new(physical.width, physical.height).ok_or_else(|| {
            FatalReport::internal(
                FatalComponent::Window,
                FatalOperation::Initialize,
                FatalReason::WindowSizeInvalid,
            )
        })?;
        let instance = dx12_instance();
        let presentation =
            PresentationSurface::create(&instance, PresentationSurfaceLease::new(window.clone()))
                .map_err(|_| {
                FatalReport::internal(
                    FatalComponent::Window,
                    FatalOperation::Initialize,
                    FatalReason::SurfaceCreateFailed,
                )
            })?;
        let gpu = pollster::block_on(presentation.request_gpu_context(instance)).map_err(|_| {
            FatalReport::internal(
                FatalComponent::Window,
                FatalOperation::Initialize,
                FatalReason::GpuUnavailable,
            )
        })?;
        let compositor = PresentationCompositor::new(presentation, gpu.clone(), physical_size)
            .map_err(|_| {
                FatalReport::internal(
                    FatalComponent::Window,
                    FatalOperation::Initialize,
                    FatalReason::CompositorConfigureFailed,
                )
            })?;
        let renderer = RemoteRenderer::new(gpu.clone()).map_err(|_| {
            FatalReport::internal(
                FatalComponent::Window,
                FatalOperation::Initialize,
                FatalReason::RendererInitializeFailed,
            )
        })?;
        let target_format = compositor.target_format().ok_or_else(|| {
            FatalReport::internal(
                FatalComponent::Window,
                FatalOperation::Initialize,
                FatalReason::SurfaceFormatUnavailable,
            )
        })?;
        let egui_context = egui::Context::default();
        egui_context.enable_accesskit();
        egui_context.set_fonts(system_font_definitions());
        let mut egui_state = egui_winit::State::new(
            egui_context.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(gpu.device().limits().max_texture_dimension_2d as usize),
        );
        egui_state.init_accesskit(event_loop, &window, self.proxy.clone());
        let egui_renderer = egui_wgpu::Renderer::new(
            gpu.device(),
            target_format,
            egui_wgpu::RendererOptions::default(),
        );
        let mut state = DesktopWindowState {
            chrome,
            window,
            gpu,
            renderer,
            compositor,
            egui_context,
            egui_state,
            egui_renderer,
            physical_size,
            remote_area: Some(PixelRect {
                x: 0,
                y: 0,
                width: physical_size.width,
                height: physical_size.height,
            }),
            chrome_layout: None,
            cursor_position: None,
            lifecycle: PresentationLifecycle::new(physical_size),
            remote: None,
            dpi_transition: DpiTransition::default(),
            pending_texture_writes: PendingTextureWrites::default(),
            focus_session_chrome: false,
        };
        state.refresh_chrome_geometry().ok_or_else(|| {
            FatalReport::internal(
                FatalComponent::Window,
                FatalOperation::Initialize,
                FatalReason::WindowChromeFailed,
            )
        })?;
        state.window.set_visible(true);
        Ok(state)
    }

    fn install_repaint_callback(&self, context: &egui::Context) {
        let proxy = self.proxy.clone();
        let scheduler = self.repaint_scheduler.clone();
        context.set_request_repaint_callback(move |request| {
            scheduler.request_after(std::time::Instant::now(), request.delay, || {
                let _ = proxy.send_event(DesktopUserEvent::Repaint);
            });
        });
    }

    fn dispatch_pending_connect(&mut self) {
        if !matches!(self.mode, DesktopMode::Product) {
            return;
        }
        if let Some(intent) = self.launch.take_connect_intent() {
            self.dispatch_intent(intent);
        }
    }

    fn dispatch_intent(&mut self, intent: AppIntent) {
        let cancelling = matches!(intent, AppIntent::CancelConnect | AppIntent::Disconnect);
        if cancelling {
            self.block_and_release_input();
        }
        if cancelling && self.sessions.launch_is_pending() {
            self.return_to_form_after_cancelled_launch = true;
            let _ = self.sessions.cancel_pending_launch();
            self.request_redraw();
            return;
        }
        let stores = self.stores.as_app_stores();
        let action = match self.launch.controller_mut().handle_intent_with_stores(
            intent,
            &self.catalog,
            stores,
        ) {
            Ok(action) => action,
            Err(error) => {
                eprintln!("应用操作失败：{error:?}");
                self.request_redraw();
                return;
            }
        };
        match action {
            Some(AppAction::SessionCommand(command)) => {
                if let Err(error) = self.sessions.send_command(command) {
                    eprintln!("会话命令发送失败：{error:?}");
                }
            }
            Some(AppAction::StartSession(request, permit)) => {
                let target = self
                    .launch
                    .controller()
                    .page()
                    .retained_draft()
                    .target_system
                    .expect("validated connection retains a target system");
                let proxy = self.proxy.clone();
                match self
                    .sessions
                    .begin_launch(permit, target, request, move |outcome| {
                        let _ = proxy.send_event(DesktopUserEvent::LaunchFinished(outcome));
                    }) {
                    Ok(true) => {}
                    Ok(false) | Err(_) => {
                        let _ = self.proxy.send_event(DesktopUserEvent::ApplicationFatal(
                            FatalReport::internal(
                                FatalComponent::Session,
                                FatalOperation::Launch,
                                FatalReason::InvalidState,
                            ),
                        ));
                    }
                }
            }
            None => {}
        }
        self.request_redraw();
    }

    fn drain_runtime(&mut self) {
        let events = self.sessions.drain_session_events();
        let mut cleanup_needed = false;
        let mut detach_remote = false;
        for (session_id, event) in events {
            match &event {
                SessionEvent::SurfaceGenerationChanged {
                    session_id,
                    generation,
                    ..
                } => self.metrics.observe_generation(*session_id, *generation),
                SessionEvent::FrameResponseTiming(timing) => {
                    self.metrics.observe_frame_response_timing(*timing)
                }
                SessionEvent::StageChanged(frd_protocol_api::ConnectionStage::Disconnecting)
                | SessionEvent::Error(_)
                | SessionEvent::Closed(_) => self.metrics.clear_input_probe(),
                _ => {}
            }
            if matches!(
                event,
                SessionEvent::CapabilitiesChanged(frd_protocol_api::SessionCapabilities {
                    text_input: false,
                    ..
                })
            ) {
                if let Some(release) = self.input.keyboard_capability_lost() {
                    self.send_input(release);
                }
            }
            if matches!(
                event,
                SessionEvent::SurfaceGenerationChanged { .. }
                    | SessionEvent::StageChanged(frd_protocol_api::ConnectionStage::Disconnecting)
                    | SessionEvent::Error(_)
                    | SessionEvent::Closed(_)
            ) {
                self.block_and_release_input();
            }
            cleanup_needed |= matches!(event, SessionEvent::Error(_) | SessionEvent::Closed(_));
            detach_remote |= matches!(
                event,
                SessionEvent::StageChanged(frd_protocol_api::ConnectionStage::Disconnecting)
                    | SessionEvent::Error(_)
                    | SessionEvent::Closed(_)
            );
            self.launch
                .controller_mut()
                .handle_session_event_with_stores(session_id, event, self.stores.as_app_stores());
        }

        if detach_remote {
            self.metrics.detach();
            if let Some(window) = self.window.as_mut() {
                window.renderer.detach();
                window.remote = None;
            }
        }
        if cleanup_needed {
            self.start_background_cleanup();
        }

        // SessionEvent is deliberately reduced before frame mailbox data.
        let updates = self.sessions.drain_enqueued_frame_updates();
        let had_events_or_frames = !updates.is_empty() || cleanup_needed || detach_remote;
        let source_update_count = updates.len();
        for entry in &updates {
            if let SurfaceUpdate::Reset {
                session_id,
                generation,
                ..
            } = &entry.update
            {
                self.metrics.clear_input_probe();
                self.metrics.observe_generation(*session_id, *generation);
            }
        }
        let mut aggregate = SerialDrainAggregate::default();
        let mut first_error: Option<RendererError> = None;
        let drain_started_at = (!updates.is_empty()).then(std::time::Instant::now);
        let oldest_age = drain_started_at.and_then(|started_at| {
            updates
                .iter()
                .map(|entry| entry.enqueued_at)
                .min()
                .and_then(|earliest| checked_mailbox_age(started_at, earliest))
        });
        if drain_started_at.is_some() && oldest_age.is_none() {
            self.metrics.invalidate(MetricSinkError::InvalidObservation);
        }
        let (updates, drain_started_at, oldest_age) =
            if let (Some(drain_started_at), Some(_)) = (drain_started_at, oldest_age) {
                let drained = DrainedFrameUpdates::new(updates, drain_started_at)
                    .expect("the borrowed timestamp validation just succeeded");
                (
                    drained.updates,
                    Some(drained.drain_started_at),
                    Some(drained.oldest_age),
                )
            } else {
                (updates, drain_started_at, oldest_age)
            };
        for entry in updates {
            let binding = match &entry.update {
                SurfaceUpdate::Reset {
                    session_id,
                    generation,
                    size,
                    ..
                } => Some(RemoteBinding {
                    session_id: *session_id,
                    generation: *generation,
                    size: *size,
                }),
                _ => None,
            };
            let Some(window) = self.window.as_mut() else {
                continue;
            };
            let before = window.gpu.scope_observation();
            let apply_result = window.renderer.apply_update(entry.update);
            let after = window.gpu.scope_observation();
            let actual_delta = after.checked_delta(before);
            match apply_result {
                Ok(outcome) => {
                    if let Some(actual_delta) = actual_delta {
                        if let Err(error) = aggregate.observe(outcome, actual_delta) {
                            self.metrics.invalidate(error);
                        }
                    } else {
                        self.metrics.invalidate(MetricSinkError::InvalidObservation);
                    }
                    if let ApplyOutcome::Damage {
                        uploaded_rectangles,
                    } = outcome
                    {
                        window
                            .pending_texture_writes
                            .record_damage_upload(uploaded_rectangles);
                    }
                    if let Some(binding) = binding {
                        window.remote = Some(binding);
                    }
                }
                Err(error) => {
                    if let Some(actual_delta) = actual_delta {
                        if let Err(metric_error) = aggregate.observe_failed_scope(actual_delta) {
                            self.metrics.invalidate(metric_error);
                        }
                    } else {
                        self.metrics.invalidate(MetricSinkError::InvalidObservation);
                    }
                    first_error.get_or_insert(error);
                    eprintln!("远端帧更新被拒绝：{error:?}");
                }
            }
        }
        if let (Some(batch_started_at), Some(oldest_age)) = (drain_started_at, oldest_age) {
            self.metrics.observe_serial_drain(
                BatchMetricContext {
                    batch_started_at,
                    source_update_count,
                    oldest_age: Some(oldest_age),
                    transaction_count: 0,
                },
                &aggregate,
                first_error,
                std::time::Instant::now(),
            );
        }
        if let Some(window) = self.window.as_mut() {
            if window.pending_texture_writes.take_for_blocked_present(
                window.lifecycle.accepts_redraw(),
                window.dpi_transition.is_pending(),
            ) {
                let _ = window.gpu.queue().submit(std::iter::empty());
            }
        }
        if had_events_or_frames {
            self.request_redraw();
        }
        self.publish_metrics_failure();
    }

    fn block_and_release_input(&mut self) {
        if let Some(event) = self.input.set_gate(InputGate::Blocked) {
            self.send_input(event);
        }
    }

    fn block_and_release_input_for_fatal(&mut self) {
        let Some(event) = self.input.set_gate(InputGate::Blocked) else {
            return;
        };
        let Some(command) = self.launch.controller().route_input(event) else {
            return;
        };
        let _ = self.sessions.send_command(command);
    }

    fn send_input(&mut self, event: frd_core::InputEvent) {
        if let Some(command) = self.launch.controller().route_input(event) {
            let probe = match &command {
                SessionCommand::Input(input)
                    if !matches!(input.event, frd_core::InputEvent::ReleaseAll) =>
                {
                    Some(MetricIdentity {
                        session_id: input.session_id,
                        generation: input.generation,
                    })
                }
                _ => None,
            };
            let accepted_at = std::time::Instant::now();
            match self.sessions.send_command(command) {
                Ok(()) => {
                    if let Some(identity) = probe {
                        self.metrics.observe_input_sent(identity, accepted_at);
                    }
                }
                Err(error) => eprintln!("远端输入发送失败：{error:?}"),
            }
        }
    }

    fn publish_metrics_failure(&mut self) {
        if let Some(error) = self.metrics.take_failure() {
            let _ = self.proxy.send_event(DesktopUserEvent::ApplicationFatal(
                FatalReport::frame_metrics_startup(error),
            ));
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            if window.lifecycle.accepts_redraw() {
                window.window.request_redraw();
            }
        }
    }

    fn commit_window_resize(&mut self, size: PixelSize) {
        let mut committed = false;
        let mut presentation_error = None;
        if let Some(window) = self.window.as_mut() {
            let result = window.compositor.resize(size);
            match window.lifecycle.finish_resize(size, result) {
                Ok(()) => {
                    window.physical_size = size;
                    window.dpi_transition.finish_resize();
                    let _ = window.refresh_chrome_geometry();
                    committed = true;
                }
                Err(error) => presentation_error = Some(error),
            }
        }
        if committed {
            self.send_viewport_changed();
            self.request_redraw();
        } else if let Some(error) = presentation_error {
            self.transition_presentation_error(
                PresentationRecoveryContext::Resize { requested: size },
                error,
            );
        }
    }

    fn content_viewport(&self) -> Option<ContentViewport> {
        let window = self.window.as_ref()?;
        let remote = window.remote?;
        ContentViewport::fit_in(remote.size, window.physical_size, window.remote_area?)
    }

    fn send_viewport_changed(&self) {
        let Some((session_id, generation)) = self.input.interactive_epoch() else {
            return;
        };
        let Some(remote) = self.window.as_ref().and_then(|window| window.remote) else {
            return;
        };
        if remote.session_id != session_id || remote.generation != generation {
            return;
        }
        let Some(viewport) = self.content_viewport() else {
            return;
        };
        let Some(viewport) =
            PhysicalViewport::new(viewport.drawable, viewport.content, viewport.remote)
        else {
            return;
        };
        let _ = self.sessions.send_command(SessionCommand::ViewportChanged {
            session_id,
            generation,
            viewport,
        });
    }

    fn render(&mut self) {
        let Some(window) = self.window.as_mut() else {
            return;
        };
        if !window.lifecycle.accepts_redraw() || window.dpi_transition.is_pending() {
            if window.pending_texture_writes.take_for_blocked_present(
                window.lifecycle.accepts_redraw(),
                window.dpi_transition.is_pending(),
            ) {
                let _ = window.gpu.queue().submit(std::iter::empty());
            }
            return;
        }
        let Some(chrome_layout) = window.refresh_chrome_geometry() else {
            window.remote_area = None;
            return;
        };
        let window_maximized = window.window.is_maximized();
        let raw_input = window.egui_state.take_egui_input(&window.window);
        let egui_context = window.egui_context.clone();
        let connection_busy = self.sessions.is_active();
        let focus_session_chrome = std::mem::take(&mut window.focus_session_chrome);
        let mode = &mut self.mode;
        let controller = self.launch.controller_mut();
        let catalog = &self.catalog;
        let mut intent = None;
        let product_chrome = controller.session_chrome();
        let mut output = egui_context.run_ui(raw_input, |root_ui| {
            egui::Panel::top("window-session-chrome")
                .exact_size(TITLE_BAR_HEIGHT_POINTS as f32)
                .frame(
                    egui::Frame::new()
                        .fill(root_ui.style().visuals.panel_fill)
                        .inner_margin(egui::Margin::symmetric(0, 4)),
                )
                .show(root_ui, |ui| {
                    let chrome = match mode {
                        DesktopMode::Product => product_chrome.as_ref(),
                        DesktopMode::TestTexture {
                            stage: TestTextureStage::RemoteSession,
                            ..
                        } => Some(&TEST_SESSION_CHROME),
                        DesktopMode::TestTexture {
                            stage: TestTextureStage::Connection,
                            ..
                        } => None,
                    };
                    if let Some(chrome) = chrome {
                        let metrics = frd_ui_egui::session_chrome_metrics();
                        ui.horizontal(|ui| {
                            ui.add_space(
                                ((ui.available_width() - metrics.total_width) / 2.0).max(0.0),
                            );
                            if let Some(action) = frd_ui_egui::show_session_chrome_with_focus(
                                ui,
                                chrome,
                                focus_session_chrome,
                            )
                            .action
                            {
                                intent = Some(match action {
                                    frd_ui_model::SessionChromeAction::Cancel => {
                                        AppIntent::CancelConnect
                                    }
                                    frd_ui_model::SessionChromeAction::Disconnect => {
                                        AppIntent::Disconnect
                                    }
                                });
                            }
                        });
                    }
                    paint_platform_window_controls(
                        ui,
                        chrome_layout,
                        window.window.scale_factor(),
                        window_maximized,
                    );
                });

            match mode {
                DesktopMode::Product => {
                    if let Some(form) = controller.connection_form_mut() {
                        egui::CentralPanel::default_margins().show(root_ui, |ui| {
                            intent = frd_ui_egui::show_connection_form_with_state(
                                ui,
                                form,
                                catalog,
                                connection_busy,
                            );
                        });
                    } else if controller.current_server_identity_challenge().is_some()
                        || matches!(controller.page(), AppPage::Failed { .. })
                    {
                        egui::CentralPanel::default_margins().show(root_ui, |ui| {
                            intent = frd_ui_egui::show_session_page(
                                ui,
                                controller.page(),
                                controller.current_server_identity_challenge(),
                            );
                        });
                    }
                }
                DesktopMode::TestTexture {
                    stage: TestTextureStage::Connection,
                    ..
                } => {
                    egui::CentralPanel::default_margins().show(root_ui, |ui| {
                        ui.heading("连接远程桌面");
                        ui.label("离线测试纹理正在初始化，不会读取凭据或连接网络。");
                    });
                }
                DesktopMode::TestTexture {
                    stage: TestTextureStage::RemoteSession,
                    ..
                } => {}
            }
        });
        window
            .egui_state
            .handle_platform_output(&window.window, output.platform_output);
        let paint_jobs = egui_context.tessellate(output.shapes, output.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [window.physical_size.width, window.physical_size.height],
            pixels_per_point: output.pixels_per_point,
        };
        let remote_viewport = window.remote.and_then(|remote| {
            ContentViewport::fit_in(remote.size, window.physical_size, window.remote_area?)
        });
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                window.egui_renderer.update_texture(
                    window.gpu.device(),
                    window.gpu.queue(),
                    *id,
                    delta,
                );
            }
        }
        let hook = WindowPresentationHook::new(window.window.clone());
        let gpu = window.gpu.clone();
        let egui_renderer = &mut window.egui_renderer;
        let render_result = window.compositor.render_in(
            &mut window.renderer,
            remote_viewport,
            |encoder, target| {
                let callbacks = egui_renderer.update_buffers(
                    gpu.device(),
                    gpu.queue(),
                    encoder,
                    &paint_jobs,
                    &screen,
                );
                debug_assert!(callbacks.is_empty(), "product UI has no egui GPU callbacks");
                let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("FreeRemoteDesk egui overlay"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                egui_renderer.render(&mut render_pass.forget_lifetime(), &paint_jobs, &screen);
            },
            &hook,
        );
        let render_succeeded = render_result.is_ok();
        let actual_submit = hook.actual_submit();
        for id in &output.textures_delta.free {
            window.egui_renderer.free_texture(id);
        }
        mark_texture_deltas_applied(&mut output.textures_delta);

        if window
            .pending_texture_writes
            .finish_render(actual_submit, render_succeeded)
        {
            let _ = window.gpu.queue().submit(std::iter::empty());
        }

        let presentation_error = match render_result {
            Ok(Some(event)) => {
                let (session_id, generation, revision, completeness) = match &event {
                    frd_protocol_api::PresentationEvent::FramePresented {
                        session_id,
                        generation,
                        revision,
                        completeness,
                        ..
                    } => (*session_id, *generation, *revision, *completeness),
                };
                let identity = MetricIdentity {
                    session_id,
                    generation,
                };
                let presented_at = std::time::Instant::now();
                if completeness == FrameCompleteness::FullBaseline {
                    self.metrics
                        .observe_full_baseline_presented(identity, revision, presented_at);
                } else {
                    self.metrics
                        .observe_presented(identity, revision, presented_at);
                }
                self.launch.controller_mut().handle_presentation(event);
                if matches!(
                    self.launch.controller().page(),
                    AppPage::RemoteSession { .. }
                ) && completeness == FrameCompleteness::FullBaseline
                {
                    let egui_context = window.egui_context.clone();
                    if let Some(release) = self.input.set_gate(InputGate::Interactive {
                        session_id,
                        generation,
                    }) {
                        self.send_input(release);
                    }
                    egui_context.memory_mut(|memory| memory.stop_text_input());
                    self.send_viewport_changed();
                    self.request_redraw();
                }
                None
            }
            Ok(None) => None,
            Err(error) => Some(error),
        };
        self.publish_metrics_failure();

        if let Some(error) = presentation_error {
            self.transition_presentation_error(PresentationRecoveryContext::Redraw, error);
        }

        if let Some(intent) = intent {
            self.dispatch_intent(intent);
        }

        if let DesktopMode::TestTexture {
            stage, session_id, ..
        } = &mut self.mode
        {
            if *stage == TestTextureStage::Connection {
                let updates = test_texture_updates(*session_id);
                if let Some(window) = self.window.as_mut() {
                    for update in updates {
                        let _ = window.renderer.apply_update(update);
                    }
                    window.remote = Some(RemoteBinding {
                        session_id: *session_id,
                        generation: 1,
                        size: PixelSize::new(2, 2).expect("test texture is non-zero"),
                    });
                    window.window.set_title("FreeRemoteDesk — 离线测试远程会话");
                }
                *stage = TestTextureStage::RemoteSession;
                self.request_redraw();
            }
        }
    }

    fn transition_presentation_error(
        &mut self,
        context: PresentationRecoveryContext,
        source: PresentError,
    ) {
        let Some(window) = self.window.as_mut() else {
            self.publish_presentation_fatal(PresentationRecoveryFailure {
                operation: context.operation(),
                source,
                retry: None,
                recovery: Some(PresentError::SurfaceDetached),
            });
            return;
        };

        let test_session_id = match &self.mode {
            DesktopMode::TestTexture { session_id, .. } => Some(*session_id),
            DesktopMode::Product => None,
        };
        let recovery = {
            let mut backend = DesktopWindowRecovery::new(window);
            execute_presentation_recovery(context, source, &mut backend)
        };
        let success = match recovery {
            Ok(success) => success,
            Err(failure) => {
                self.publish_presentation_fatal(failure);
                return;
            }
        };

        let mut publish_viewport = false;
        if let Some(size) = success.geometry_commit() {
            let Some(window) = self.window.as_mut() else {
                self.publish_presentation_fatal(PresentationRecoveryFailure {
                    operation: context.operation(),
                    source,
                    retry: None,
                    recovery: Some(PresentError::SurfaceDetached),
                });
                return;
            };
            if let Err(recovery) = window.lifecycle.finish_resize(size, Ok(())) {
                self.publish_presentation_fatal(PresentationRecoveryFailure {
                    operation: context.operation(),
                    source,
                    retry: None,
                    recovery: Some(recovery),
                });
                return;
            }
            window.physical_size = size;
            publish_viewport = success.publish_viewport();
        }

        if let Some(RecoveryRequirement::ResetAndFullSnapshot { .. }) = success.requirement {
            if let Some(session_id) = test_session_id {
                let restore = self
                    .window
                    .as_mut()
                    .ok_or(PresentError::SurfaceDetached)
                    .and_then(|window| {
                        for update in test_texture_updates(session_id) {
                            window.renderer.apply_update(update)?;
                        }
                        Ok(())
                    });
                if let Err(recovery) = restore {
                    self.publish_presentation_fatal(PresentationRecoveryFailure {
                        operation: context.operation(),
                        source,
                        retry: None,
                        recovery: Some(recovery),
                    });
                    return;
                }
            } else {
                self.publish_presentation_fatal(PresentationRecoveryFailure {
                    operation: context.operation(),
                    source,
                    retry: None,
                    recovery: None,
                });
                return;
            }
        }
        if publish_viewport {
            self.send_viewport_changed();
        }
        if success.request_redraw() {
            self.request_redraw();
        }
    }

    fn publish_presentation_fatal(&self, failure: PresentationRecoveryFailure) {
        let _ = self
            .proxy
            .send_event(DesktopUserEvent::PresentationFatal(PresentationFailure {
                operation: failure.operation,
                source: failure.source,
                retry: failure.retry,
                recovery: failure.recovery,
            }));
    }

    fn handle_remote_window_event(&mut self, event: &WindowEvent, consumed_by_egui: bool) {
        let Some(viewport) = self.content_viewport() else {
            return;
        };
        let pointer_ownership = if consumed_by_egui {
            InputOwnership::Ui
        } else {
            InputOwnership::Remote
        };
        let keyboard_ownership = if consumed_by_egui
            || self.input.keyboard_domain() == KeyboardDomain::LocalChrome
            || !self.launch.controller().effective_capabilities().text_input
        {
            InputOwnership::Ui
        } else {
            InputOwnership::Remote
        };
        let input = match event {
            WindowEvent::CursorMoved { position, .. } => self.input.pointer_moved(
                position.x as f32,
                position.y as f32,
                viewport,
                pointer_ownership,
            ),
            WindowEvent::MouseInput { state, button, .. } => {
                map_mouse_button(*button).and_then(|button| {
                    self.input.pointer_button(
                        button,
                        map_button_state(*state),
                        viewport,
                        pointer_ownership,
                    )
                })
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (horizontal, vertical) = wheel_signs(*delta);
                self.input
                    .wheel(horizontal, vertical, viewport, pointer_ownership)
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                hid_usage_from_key_code(code).and_then(|usage| {
                    self.input.key(
                        frd_core::PhysicalKeyCode::from_usb_hid_usage(usage),
                        map_key_state(event.state),
                        keyboard_ownership,
                    )
                })
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.input.text(text.clone(), keyboard_ownership)
            }
            _ => None,
        };
        if let Some(input) = input {
            self.send_input(input);
        }
    }

    fn handle_keyboard_before_egui(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::KeyboardInput {
            event,
            is_synthetic,
            ..
        } = event
        else {
            if let WindowEvent::Ime(ime) = event {
                match classify_ime_before_egui(
                    self.input.keyboard_domain(),
                    ime,
                    self.launch.controller().effective_capabilities().text_input,
                ) {
                    ImePreDispatch::LocalChrome => return false,
                    ImePreDispatch::Consume => return true,
                    ImePreDispatch::Commit(text) => {
                        if let Some(input) =
                            self.input.text(text.to_owned(), InputOwnership::Remote)
                        {
                            self.send_input(input);
                        }
                        return true;
                    }
                }
            }
            return false;
        };

        let code = match event.physical_key {
            PhysicalKey::Code(code) => Some(code),
            PhysicalKey::Unidentified(_) => None,
        };
        let physical = code
            .and_then(hid_usage_from_key_code)
            .map(frd_core::PhysicalKeyCode::from_usb_hid_usage);
        let key_state = map_key_state(event.state);
        let local_shortcut = code.is_some_and(|code| {
            local_chrome_shortcut(code, event.state, event.repeat, self.input.modifiers())
        });
        let remote_allowed = self.launch.controller().effective_capabilities().text_input;
        match self.input.dispatch_key_event(
            physical,
            key_state,
            *is_synthetic,
            remote_allowed,
            local_shortcut,
        ) {
            KeyboardPreDispatch::Consume => true,
            KeyboardPreDispatch::Remote(input) => {
                if let Some(input) = input {
                    self.send_input(input);
                }
                true
            }
            KeyboardPreDispatch::EnterLocalChrome(release) => {
                if let Some(release) = release {
                    self.send_input(release);
                }
                if let Some(window) = self.window.as_mut() {
                    window.egui_context.memory_mut(|memory| {
                        memory.stop_text_input();
                    });
                    window.focus_session_chrome = true;
                    window.window.request_redraw();
                }
                true
            }
            KeyboardPreDispatch::EnterRemoteSurface => {
                self.clear_local_keyboard_focus();
                true
            }
            KeyboardPreDispatch::LocalChrome => false,
        }
    }

    fn clear_local_keyboard_focus(&mut self) {
        if let Some(window) = self.window.as_mut() {
            window
                .egui_context
                .memory_mut(|memory| memory.stop_text_input());
            window.window.request_redraw();
        }
    }

    fn update_keyboard_domain_from_pointer(&mut self, event: &WindowEvent, consumed_by_egui: bool) {
        if !matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) {
            return;
        }
        let interactive = self.input.interactive_epoch().is_some();
        let ownership = self.window.as_ref().and_then(|window| {
            effective_pointer_keyboard_ownership(
                window.chrome_layout?,
                window.cursor_position?,
                consumed_by_egui,
                interactive,
            )
        });
        let Some(ownership) = ownership else {
            return;
        };
        if let Some(release) = self.input.pointer_pressed_for_domain(ownership) {
            self.send_input(release);
        }
        if ownership == InputOwnership::Remote {
            self.clear_local_keyboard_focus();
        }
    }

    fn begin_shutdown(&mut self, event_loop: &ActiveEventLoop) {
        self.block_and_release_input();
        if let Some(deadline) = self
            .exit_state
            .cancel_pending_launch(&mut self.sessions, std::time::Instant::now())
        {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            return;
        }
        if self.sessions.is_active() {
            self.exit_state.wait_for_cleanup();
            if !matches!(self.launch.controller().page(), Page::Disconnecting { .. }) {
                self.dispatch_intent(AppIntent::Disconnect);
            }
            self.start_background_cleanup();
        } else {
            event_loop.exit();
        }
    }

    fn start_background_cleanup(&mut self) {
        let proxy = self.proxy.clone();
        if let Err(failure) = self.sessions.begin_cleanup(move |outcome| {
            let _ = proxy.send_event(DesktopUserEvent::CleanupFinished(outcome));
        }) {
            let _ = self.proxy.send_event(DesktopUserEvent::CleanupFinished(
                BackgroundCleanupOutcome::Fatal(failure),
            ));
        }
    }

    fn handle_launch_finished(
        &mut self,
        event_loop: &ActiveEventLoop,
        outcome: BackgroundLaunchOutcome,
    ) {
        self.exit_state.launch_finished();
        let proxy = self.proxy.clone();
        let accepted = self
            .sessions
            .accept_launch_outcome(outcome, move |outcome| {
                let _ = proxy.send_event(DesktopUserEvent::CleanupFinished(outcome));
            });
        match accepted {
            Ok(AcceptedLaunchOutcome::Started) => {
                if let Some(session_id) = self.sessions.active_session_id() {
                    self.metrics.begin_session(session_id);
                }
            }
            Ok(AcceptedLaunchOutcome::LaunchRolledBack(failure)) => {
                let stores = self.stores.as_app_stores();
                if self
                    .launch
                    .controller_mut()
                    .consume_launch_rollback_with_stores(&failure, stores)
                    .is_err()
                {
                    self.handle_application_fatal(
                        event_loop,
                        FatalReport::internal(
                            FatalComponent::Application,
                            FatalOperation::LaunchAccept,
                            FatalReason::InvalidState,
                        ),
                    );
                    return;
                }
            }
            Ok(AcceptedLaunchOutcome::CancelledStarted) => {
                let stores = self.stores.as_app_stores();
                let cancel_failed = !matches!(
                    self.launch.controller_mut().handle_intent_with_stores(
                        AppIntent::CancelConnect,
                        &self.catalog,
                        stores,
                    ),
                    Ok(Some(AppAction::SessionCommand(_))) | Ok(None)
                );
                if cancel_failed {
                    self.handle_application_fatal(
                        event_loop,
                        FatalReport::internal(
                            FatalComponent::Application,
                            FatalOperation::LaunchCancel,
                            FatalReason::InvalidState,
                        ),
                    );
                    return;
                }
                self.return_to_form_after_cancelled_launch = false;
            }
            Ok(AcceptedLaunchOutcome::CancelledLaunchRolledBack(failure)) => {
                let stores = self.stores.as_app_stores();
                if self
                    .launch
                    .controller_mut()
                    .consume_launch_rollback_with_stores(&failure, stores)
                    .is_err()
                {
                    self.handle_application_fatal(
                        event_loop,
                        FatalReport::internal(
                            FatalComponent::Application,
                            FatalOperation::LaunchCancel,
                            FatalReason::InvalidState,
                        ),
                    );
                    return;
                }
                if self.return_to_form_after_cancelled_launch && !self.exit_state.when_clean {
                    let stores = self.stores.as_app_stores();
                    let _ = self.launch.controller_mut().handle_intent_with_stores(
                        AppIntent::ReturnToConnection,
                        &self.catalog,
                        stores,
                    );
                }
                self.return_to_form_after_cancelled_launch = false;
            }
            Err(SessionHostError::CleanupFatal(failure)) => {
                self.handle_application_fatal(event_loop, FatalReport::cleanup(failure));
                return;
            }
            Err(_) => {
                self.handle_application_fatal(
                    event_loop,
                    FatalReport::internal(
                        FatalComponent::Session,
                        FatalOperation::LaunchAccept,
                        FatalReason::InvalidState,
                    ),
                );
                return;
            }
        }
        self.maybe_finish_exit(event_loop);
        self.request_redraw();
    }

    fn handle_cleanup_finished(
        &mut self,
        event_loop: &ActiveEventLoop,
        outcome: BackgroundCleanupOutcome,
    ) {
        match self.sessions.accept_cleanup_outcome(outcome) {
            Ok(completion) => {
                if self
                    .launch
                    .controller_mut()
                    .finish_session_cleanup_with_stores(completion, self.stores.as_app_stores())
                    .is_err()
                {
                    self.handle_application_fatal(
                        event_loop,
                        FatalReport::internal(
                            FatalComponent::Application,
                            FatalOperation::Cleanup,
                            FatalReason::InvalidState,
                        ),
                    );
                    return;
                }
            }
            Err(SessionHostError::CleanupFatal(failure)) => {
                self.handle_application_fatal(event_loop, FatalReport::cleanup(failure));
                return;
            }
            Err(_) => {
                self.handle_application_fatal(
                    event_loop,
                    FatalReport::internal(
                        FatalComponent::Session,
                        FatalOperation::Cleanup,
                        FatalReason::InvalidState,
                    ),
                );
                return;
            }
        }
        self.maybe_finish_exit(event_loop);
        self.request_redraw();
    }

    fn detach_window(&mut self) {
        self.metrics.detach();
        self.repaint_scheduler.shutdown();
        self.armed_repaint = None;
        if let Some(mut window) = self.window.take() {
            window.lifecycle.destroy();
            window.compositor.detach();
            window.renderer.detach();
        }
    }

    fn handle_presentation_fatal(
        &mut self,
        event_loop: &ActiveEventLoop,
        failure: PresentationFailure,
    ) {
        self.handle_application_fatal(
            event_loop,
            FatalReport::presentation(
                failure.operation,
                failure.source,
                failure.retry,
                failure.recovery,
            ),
        );
    }

    fn handle_application_fatal(&mut self, event_loop: &ActiveEventLoop, report: FatalReport) {
        if !self.exit_state.latch_fatal(report) {
            event_loop.exit();
            return;
        }
        // Fatal is monotonic. Latch first so no subsequent callback can install
        // a queued launch result while teardown remains on this call stack.
        self.metrics.fatal();
        self.block_and_release_input_for_fatal();
        self.detach_window();
        let _ = self.sessions.cancel_pending_launch();
        let _ = self.sessions.send_command(SessionCommand::Disconnect);
        event_loop.exit();
    }

    fn maybe_finish_exit(&self, event_loop: &ActiveEventLoop) {
        if self.exit_state.should_exit(&self.sessions) {
            event_loop.exit();
        }
    }

    fn synchronize_repaint_deadline(&mut self, event_loop: &ActiveEventLoop) {
        self.armed_repaint = self.repaint_scheduler.take_plan();
        let now = std::time::Instant::now();
        match self.armed_repaint {
            Some(plan) if plan.deadline <= now => {
                if self.repaint_scheduler.fire(plan, now) {
                    self.request_redraw();
                }
                self.armed_repaint = None;
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(plan) => event_loop.set_control_flow(ControlFlow::WaitUntil(plan.deadline)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

impl ApplicationHandler<DesktopUserEvent> for DesktopApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.exit_state.should_ignore_events() {
            event_loop.exit();
            return;
        }
        event_loop.set_control_flow(ControlFlow::Wait);
        if self.window.is_none() {
            match self.initialize_window(event_loop) {
                Ok(window) => {
                    self.install_repaint_callback(&window.egui_context);
                    self.window = Some(window);
                    self.request_redraw();
                    self.dispatch_pending_connect();
                }
                Err(report) => {
                    self.handle_application_fatal(event_loop, report);
                    return;
                }
            }
        }
        if let DesktopMode::TestTexture {
            exit_after,
            resize_after,
            driver_started,
            ..
        } = &mut self.mode
        {
            if !*driver_started && (exit_after.is_some() || resize_after.is_some()) {
                *driver_started = true;
                spawn_test_texture_driver(self.proxy.clone(), *resize_after, *exit_after);
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: DesktopUserEvent) {
        if self.exit_state.should_ignore_events() {
            event_loop.exit();
            return;
        }
        match event {
            DesktopUserEvent::Wake => {
                self.runtime_wake_gate.consume();
                self.drain_runtime();
                self.request_redraw();
                self.maybe_finish_exit(event_loop);
            }
            DesktopUserEvent::Repaint => self.synchronize_repaint_deadline(event_loop),
            DesktopUserEvent::LaunchFinished(outcome) => {
                self.handle_launch_finished(event_loop, outcome)
            }
            DesktopUserEvent::CleanupFinished(outcome) => {
                self.handle_cleanup_finished(event_loop, outcome)
            }
            DesktopUserEvent::PresentationFatal(failure) => {
                self.handle_presentation_fatal(event_loop, failure)
            }
            DesktopUserEvent::ApplicationFatal(report) => {
                self.handle_application_fatal(event_loop, report)
            }
            DesktopUserEvent::ResizeTestTexture => {
                if matches!(self.mode, DesktopMode::TestTexture { .. }) {
                    if let Some(window) = self.window.as_ref() {
                        let _ = window
                            .window
                            .request_inner_size(winit::dpi::PhysicalSize::new(960_u32, 640_u32));
                    }
                }
            }
            DesktopUserEvent::ExitTestTexture => event_loop.exit(),
            DesktopUserEvent::AccessKit(event) => {
                if matches!(
                    &event.window_event,
                    egui_winit::accesskit_winit::WindowEvent::ActionRequested(_)
                ) {
                    if let Some(release) = self.input.enter_local_chrome() {
                        self.send_input(release);
                    }
                }
                let Some(window) = self
                    .window
                    .as_mut()
                    .filter(|window| window.window.id() == event.window_id)
                else {
                    return;
                };
                match event.window_event {
                    egui_winit::accesskit_winit::WindowEvent::InitialTreeRequested => {
                        window.window.request_redraw();
                    }
                    egui_winit::accesskit_winit::WindowEvent::ActionRequested(request) => {
                        window.egui_state.on_accesskit_action_request(request);
                        window.window.request_redraw();
                    }
                    egui_winit::accesskit_winit::WindowEvent::AccessibilityDeactivated => {}
                }
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.exit_state.should_ignore_events() {
            event_loop.exit();
            return;
        }
        let Some(window) = self.window.as_mut() else {
            return;
        };
        if window.window.id() != window_id {
            return;
        }
        if self.handle_keyboard_before_egui(&event) {
            return;
        }
        let Some(window) = self.window.as_mut() else {
            return;
        };
        let response = window.egui_state.on_window_event(&window.window, &event);
        if response.repaint {
            window.window.request_redraw();
        }
        if let WindowEvent::CursorMoved { position, .. } = &event {
            let position = (position.x >= 0.0 && position.y >= 0.0)
                .then_some((position.x as u32, position.y as u32));
            if let Some(window) = self.window.as_mut() {
                window.cursor_position = position;
            }
        }
        match &event {
            WindowEvent::CloseRequested => {
                self.begin_shutdown(event_loop);
                return;
            }
            WindowEvent::Destroyed => {
                self.detach_window();
                self.begin_shutdown(event_loop);
                return;
            }
            WindowEvent::Focused(true) => self.input.focus_gained(),
            WindowEvent::Focused(false) => {
                if let Some(release) = self.input.focus_lost() {
                    self.send_input(release);
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(release) = self.input.cursor_left() {
                    self.send_input(release);
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.input.set_modifiers(map_modifiers(modifiers.state()));
            }
            WindowEvent::Resized(size) => {
                #[cfg(target_os = "windows")]
                let minimized = self
                    .window
                    .as_ref()
                    .and_then(|window| window.window.is_minimized());
                if let Some(size) = PixelSize::new(size.width, size.height) {
                    self.commit_window_resize(size);
                } else if let Some(window) = self.window.as_mut() {
                    window.compositor.pause_presenting();
                }
                #[cfg(target_os = "windows")]
                if let Some(minimized) = minimized {
                    self.metrics
                        .observe_window_minimized(minimized, std::time::Instant::now());
                    self.publish_metrics_failure();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                let refresh_result = self.window.as_mut().map(|window| {
                    window.dpi_transition.begin();
                    window.chrome.refresh_for_dpi(&window.window)
                });
                if refresh_result.is_some_and(|result| result.is_err()) {
                    eprintln!(
                        "标题栏缩放刷新失败（FRD-WIN-SHELL-001: window_chrome_dpi_refresh_failed）"
                    );
                }
            }
            WindowEvent::Occluded(occluded) => {
                self.metrics
                    .observe_occluded(*occluded, std::time::Instant::now());
                self.publish_metrics_failure();
                let action = self
                    .window
                    .as_mut()
                    .map(|window| window.lifecycle.set_occluded(*occluded))
                    .unwrap_or(OcclusionAction::None);
                match action {
                    OcclusionAction::None => {}
                    OcclusionAction::Pause => {
                        if let Some(window) = self.window.as_mut() {
                            window.compositor.pause_presenting();
                        }
                    }
                    OcclusionAction::ResumeAndRedraw => {
                        let committed = self
                            .window
                            .as_ref()
                            .map(|window| window.lifecycle.committed_size());
                        let result = self.window.as_mut().map_or(
                            Err(PresentError::SurfaceDetached),
                            |window| {
                                window.compositor.resize(
                                    committed.expect("resume retains its committed drawable"),
                                )
                            },
                        );
                        match result {
                            Ok(()) => self.request_redraw(),
                            Err(error) => self.transition_presentation_error(
                                PresentationRecoveryContext::OcclusionResume {
                                    committed: committed
                                        .expect("resume retains its committed drawable"),
                                },
                                error,
                            ),
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.drain_runtime();
                self.render();
                self.maybe_finish_exit(event_loop);
                return;
            }
            _ => {}
        }
        self.update_keyboard_domain_from_pointer(&event, response.consumed);
        self.handle_remote_window_event(&event, response.consumed);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.metrics.close();
        self.repaint_scheduler.shutdown();
        if let Some(release) = self.input.shutdown() {
            self.send_input(release);
        }
        if self.sessions.is_active() {
            let _ = self.sessions.send_command(SessionCommand::Disconnect);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.exit_state.should_ignore_events() {
            event_loop.exit();
            return;
        }
        let now = std::time::Instant::now();
        self.metrics.advance_phase(now);
        self.publish_metrics_failure();
        if self
            .exit_state
            .pending_launch_deadline()
            .is_some_and(|deadline| now >= deadline && self.sessions.launch_is_pending())
        {
            self.handle_application_fatal(
                event_loop,
                FatalReport::internal(
                    FatalComponent::Session,
                    FatalOperation::Shutdown,
                    FatalReason::ShutdownTimeout,
                ),
            );
            return;
        }
        let pending_dpi_size = self.window.as_mut().and_then(|window| {
            let actual = window.window.inner_size();
            let actual = PixelSize::new(actual.width, actual.height)?;
            window.dpi_transition.settle(actual)
        });
        if let Some(size) = pending_dpi_size {
            let geometry_only = self
                .window
                .as_ref()
                .is_some_and(|window| window.physical_size == size);
            if geometry_only {
                if let Some(window) = self.window.as_mut() {
                    let _ = window.refresh_chrome_geometry();
                }
                self.send_viewport_changed();
                self.request_redraw();
            } else {
                self.commit_window_resize(size);
            }
        }
        if let Some(plan) = self.armed_repaint {
            if now >= plan.deadline {
                self.armed_repaint = None;
                if self.repaint_scheduler.fire(plan, now) {
                    self.request_redraw();
                }
            }
        }
        let next_deadline = self
            .armed_repaint
            .map(|plan| plan.deadline)
            .into_iter()
            .chain(self.exit_state.pending_launch_deadline())
            .chain(self.metrics.next_deadline(now))
            .min();
        event_loop.set_control_flow(
            next_deadline
                .map(ControlFlow::WaitUntil)
                .unwrap_or(ControlFlow::Wait),
        );
    }
}

fn spawn_test_texture_driver(
    proxy: EventLoopProxy<DesktopUserEvent>,
    resize_after: Option<std::time::Duration>,
    exit_after: Option<std::time::Duration>,
) {
    let _ = std::thread::Builder::new()
        .name("frd-test-texture-driver".to_owned())
        .spawn(move || {
            let started = std::time::Instant::now();
            if let Some(delay) =
                resize_after.filter(|delay| exit_after.is_none_or(|exit| *delay < exit))
            {
                std::thread::sleep(delay.saturating_sub(started.elapsed()));
                if proxy
                    .send_event(DesktopUserEvent::ResizeTestTexture)
                    .is_err()
                {
                    return;
                }
            }
            if let Some(delay) = exit_after {
                std::thread::sleep(delay.saturating_sub(started.elapsed()));
                let _ = proxy.send_event(DesktopUserEvent::ExitTestTexture);
            }
        });
}

fn dx12_instance() -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::DX12;
    wgpu::Instance::new(descriptor)
}

struct DesktopWindowRecovery<'a> {
    window: &'a mut DesktopWindowState,
    recovered: Option<(Option<RecoveryRequirement>, GpuContext)>,
}

impl<'a> DesktopWindowRecovery<'a> {
    fn new(window: &'a mut DesktopWindowState) -> Self {
        Self {
            window,
            recovered: None,
        }
    }
}

impl PresentationRecoveryBackend for DesktopWindowRecovery<'_> {
    fn recover_gpu(&mut self) -> Result<(), PresentError> {
        let recovered = pollster::block_on(
            self.window
                .compositor
                .recover_gpu_with_new_instance(&mut self.window.renderer, dx12_instance()),
        )?;
        self.recovered = Some(recovered);
        Ok(())
    }

    fn configure(&mut self, size: PixelSize) -> Result<(), PresentError> {
        self.window.compositor.resize(size)
    }

    fn finish_gpu_recovery(&mut self) -> Result<Option<RecoveryRequirement>, PresentError> {
        let (requirement, gpu) = self
            .recovered
            .take()
            .expect("successful recovery owns the replacement GPU context");
        let target_format = self
            .window
            .compositor
            .target_format()
            .ok_or(PresentError::SurfaceUnsupported)?;
        self.window.egui_renderer = egui_wgpu::Renderer::new(
            gpu.device(),
            target_format,
            egui_wgpu::RendererOptions::default(),
        );
        self.window
            .egui_context
            .set_fonts(system_font_definitions());
        self.window.gpu = gpu;
        Ok(requirement)
    }
}

fn map_mouse_button(button: MouseButton) -> Option<PointerButton> {
    match button {
        MouseButton::Left => Some(PointerButton::Primary),
        MouseButton::Middle => Some(PointerButton::Middle),
        MouseButton::Right => Some(PointerButton::Secondary),
        MouseButton::Back => Some(PointerButton::Back),
        MouseButton::Forward => Some(PointerButton::Forward),
        MouseButton::Other(_) => None,
    }
}

fn pointer_keyboard_ownership(
    layout: ChromeLayout,
    position: (u32, u32),
) -> Option<InputOwnership> {
    match layout.hit_test(position.0, position.1) {
        ChromeHit::Connection
        | ChromeHit::Audio
        | ChromeHit::Clipboard
        | ChromeHit::SessionAction => Some(InputOwnership::Ui),
        ChromeHit::Client => {
            let rect = layout.content_rect;
            let in_content = position.0 >= rect.x
                && position.1 >= rect.y
                && position.0 < rect.x.saturating_add(rect.width)
                && position.1 < rect.y.saturating_add(rect.height);
            in_content.then_some(InputOwnership::Remote)
        }
        ChromeHit::Drag | ChromeHit::Minimize | ChromeHit::Maximize | ChromeHit::Close => None,
    }
}

fn effective_pointer_keyboard_ownership(
    layout: ChromeLayout,
    position: (u32, u32),
    consumed_by_egui: bool,
    interactive: bool,
) -> Option<InputOwnership> {
    match pointer_keyboard_ownership(layout, position) {
        Some(InputOwnership::Remote) if consumed_by_egui || !interactive => {
            Some(InputOwnership::Ui)
        }
        ownership => ownership,
    }
}

#[cfg(target_os = "windows")]
fn local_chrome_shortcut(
    code: winit::keyboard::KeyCode,
    state: ElementState,
    repeat: bool,
    modifiers: Modifiers,
) -> bool {
    code == winit::keyboard::KeyCode::Home
        && state == ElementState::Pressed
        && !repeat
        && modifiers.control
        && modifiers.alt
}

#[cfg(not(target_os = "windows"))]
fn local_chrome_shortcut(
    _code: winit::keyboard::KeyCode,
    _state: ElementState,
    _repeat: bool,
    _modifiers: Modifiers,
) -> bool {
    false
}

fn map_button_state(state: ElementState) -> ButtonState {
    match state {
        ElementState::Pressed => ButtonState::Pressed,
        ElementState::Released => ButtonState::Released,
    }
}

fn map_key_state(state: ElementState) -> KeyState {
    match state {
        ElementState::Pressed => KeyState::Pressed,
        ElementState::Released => KeyState::Released,
    }
}

fn map_modifiers(modifiers: ModifiersState) -> Modifiers {
    Modifiers {
        shift: modifiers.shift_key(),
        control: modifiers.control_key(),
        alt: modifiers.alt_key(),
        meta: modifiers.super_key(),
    }
}

fn wheel_signs(delta: MouseScrollDelta) -> (i8, i8) {
    let (horizontal, vertical) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (x, y),
        MouseScrollDelta::PixelDelta(position) => (position.x as f32, position.y as f32),
    };
    (axis_sign(horizontal), axis_sign(vertical))
}

fn axis_sign(value: f32) -> i8 {
    if value > 0.0 {
        1
    } else if value < 0.0 {
        -1
    } else {
        0
    }
}

fn test_texture_updates(session_id: SessionId) -> [SurfaceUpdate; 3] {
    [
        SurfaceUpdate::Reset {
            session_id,
            generation: 1,
            size: PixelSize::new(2, 2).expect("test texture size is non-zero"),
            format: PixelFormat::Bgrx8UnormSrgb,
        },
        SurfaceUpdate::Damage {
            session_id,
            generation: 1,
            revision: 1,
            patches: vec![PixelPatch {
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                },
                stride_bytes: 8,
                pixels: PixelBuffer::new(vec![
                    0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0,
                ]),
            }],
        },
        SurfaceUpdate::FrameBoundary {
            session_id,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::FullBaseline,
        },
    ]
}

struct UnavailableCredentials;

impl frd_platform_api::CredentialProvider for UnavailableCredentials {
    fn load_username(
        &self,
        _provider: &frd_core::CredentialProviderId,
    ) -> Result<String, frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::CredentialProviderFailed)
    }

    fn load_password(
        &self,
        _provider: &frd_core::CredentialProviderId,
    ) -> Result<frd_core::SecretBuffer, frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::CredentialProviderFailed)
    }
}

struct UnavailableIdentityStore;

impl frd_platform_api::ServerIdentityStore for UnavailableIdentityStore {
    fn load_pin(
        &self,
        _protocol: &frd_core::ProtocolId,
        _endpoint: &frd_core::Endpoint,
    ) -> Result<Option<[u8; 32]>, frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::Unavailable)
    }

    fn store_pin(
        &self,
        _protocol: &frd_core::ProtocolId,
        _endpoint: &frd_core::Endpoint,
        _pin: [u8; 32],
    ) -> Result<(), frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::Unavailable)
    }
}

struct UnavailableProfileStore;

impl frd_platform_api::ConnectionProfileStore for UnavailableProfileStore {
    fn list(
        &self,
    ) -> Result<Vec<frd_platform_api::SavedConnectionProfile>, frd_platform_api::PlatformError>
    {
        Err(frd_platform_api::PlatformError::Unavailable)
    }

    fn upsert(
        &self,
        _profile: &frd_platform_api::SavedConnectionProfile,
    ) -> Result<(), frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::Unavailable)
    }

    fn delete(
        &self,
        _key: &frd_platform_api::ConnectionProfileKey,
    ) -> Result<(), frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::Unavailable)
    }
}

struct UnavailableCredentialStore;

impl frd_platform_api::SecureCredentialStore for UnavailableCredentialStore {
    fn load(
        &self,
        _key: &frd_platform_api::ConnectionProfileKey,
    ) -> Result<Option<frd_core::SecretBuffer>, frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::Unavailable)
    }

    fn stage(
        &self,
        _session: SessionId,
        _key: &frd_platform_api::ConnectionProfileKey,
        _password: &frd_core::SecretBuffer,
    ) -> Result<(), frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::Unavailable)
    }

    fn commit(
        &self,
        _session: SessionId,
        _key: &frd_platform_api::ConnectionProfileKey,
    ) -> Result<(), frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::Unavailable)
    }

    fn discard(&self, _session: SessionId) -> Result<(), frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::Unavailable)
    }

    fn delete(
        &self,
        _key: &frd_platform_api::ConnectionProfileKey,
    ) -> Result<(), frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::Unavailable)
    }

    fn purge_pending(&self) -> Result<(), frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::Unavailable)
    }
}

struct UnavailableAudioFactory;

impl AudioOutputFactory for UnavailableAudioFactory {
    fn open(&self) -> Result<Box<dyn AudioOutput>, AudioOutputError> {
        Err(AudioOutputError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use frd_app::{
        AppAction, AppController, AppIntent, AppPlatformStores, PresentationEvent, ProductPolicy,
    };
    use frd_core::{
        Endpoint, InputEvent, KeyState, Modifiers, PhysicalKeyCode, PixelRect, ProtocolId,
        SecretBuffer, SessionId, TargetSystem,
    };
    use frd_frame::{FrameCompleteness, PixelFormat};
    use frd_media_api::{AudioOutput, AudioOutputError, MediaFrame};
    use frd_platform_api::{PlatformCapabilities, PlatformError, ServerIdentityStore};
    use frd_protocol_api::{
        ConnectRequest, ConnectionStage, CredentialRequirements, Credentials, ProtocolCatalog,
        ProtocolDescriptor, ProtocolError, ProtocolExit, ProtocolFactory, ProtocolRuntime,
        ProtocolSession, SessionCapabilities, SessionCommand, SessionEvent,
    };
    use frd_session::reserve_session_start;
    use frd_ui_model::{ConnectionDraft, ConnectionForm, ProtocolChoice};
    use winit::event::Ime;

    use super::{
        initialize_metrics_before_session_launch, mark_texture_deltas_applied,
        AcceptedLaunchOutcome, ApplicationExitState, AudioOutputFactory, RuntimeWakeGate,
        SessionHost, TestLaunchOutcome, UnavailableCredentialStore, UnavailableProfileStore,
        WakeSink, WorkerKind, WorkerSpawner,
    };
    use crate::frame_metrics_sink::MetricSinkError;

    fn assert_metric_startup_fatal_before_launch(error: MetricSinkError) {
        let launches = AtomicUsize::new(0);
        let result = initialize_metrics_before_session_launch(Err(error), |_| {
            launches.fetch_add(1, Ordering::Relaxed);
        });
        let report = result.unwrap_err();
        assert_eq!(launches.load(Ordering::Relaxed), 0);
        assert_eq!(report.component(), "application");
        assert_eq!(report.operation(), "frame_metrics");
        assert_eq!(report.reason(), "frame_metrics_configuration_invalid");
        assert_eq!(report.details(), "none");
    }

    #[test]
    fn partial_metric_configuration_is_fatal_before_session_launch() {
        assert_metric_startup_fatal_before_launch(MetricSinkError::InvalidConfiguration);
    }

    #[test]
    fn invalid_metric_configuration_is_fatal_before_session_launch() {
        assert_metric_startup_fatal_before_launch(MetricSinkError::InvalidConfiguration);
    }

    #[test]
    fn runtime_wake_gate_coalesces_until_consumed_and_rolls_back_failed_send() {
        let gate = RuntimeWakeGate::default();

        assert!(gate.arm(), "the first publisher must send a wake event");
        assert!(!gate.arm(), "an armed gate must coalesce later wakes");

        gate.consume();
        assert!(gate.arm(), "consuming a wake must re-arm event delivery");

        gate.rollback_failed_send();
        assert!(gate.arm(), "a failed send must permit a retry");
        assert!(!gate.arm(), "the successful retry must arm the gate");
    }

    #[test]
    fn pending_texture_writes_submit_only_when_present_is_blocked() {
        assert!(super::should_submit_pending_texture_writes(
            true, false, false
        ));
        assert!(!super::should_submit_pending_texture_writes(
            false, false, false
        ));
        assert!(!super::should_submit_pending_texture_writes(
            true, true, false
        ));
        assert!(super::should_submit_pending_texture_writes(
            true, true, true
        ));
    }

    #[test]
    fn pending_texture_write_state_tracks_actual_and_fallback_submits() {
        let mut pending = super::PendingTextureWrites::default();

        pending.record_damage_upload(1);
        assert!(pending.is_pending());
        assert!(!pending.finish_render(true, true));
        assert!(
            !pending.is_pending(),
            "an actual submit clears pending writes"
        );

        pending.record_damage_upload(1);
        assert!(!pending.finish_render(false, false));
        assert!(
            pending.is_pending(),
            "a render error must not fake a submit"
        );
        assert!(pending.finish_render(false, true));
        assert!(!pending.is_pending(), "fallback takes the pending writes");

        assert!(!pending.finish_render(false, true));
    }

    #[test]
    fn pointer_domain_changes_only_for_session_glyphs_and_remote_content() {
        let layout = crate::ChromeLayout::for_window(1100, 720, 1.0, 0, 144).unwrap();
        let glyph = layout.session_buttons[0].center();
        let content = (
            layout.content_rect.x + layout.content_rect.width / 2,
            layout.content_rect.y + layout.content_rect.height / 2,
        );

        assert_eq!(
            super::pointer_keyboard_ownership(layout, glyph),
            Some(crate::InputOwnership::Ui)
        );
        assert_eq!(
            super::pointer_keyboard_ownership(layout, content),
            Some(crate::InputOwnership::Remote)
        );
        assert_eq!(
            super::effective_pointer_keyboard_ownership(layout, content, true, true),
            Some(crate::InputOwnership::Ui)
        );
        assert_eq!(
            super::effective_pointer_keyboard_ownership(layout, content, false, false),
            Some(crate::InputOwnership::Ui)
        );
        assert_eq!(
            super::effective_pointer_keyboard_ownership(layout, content, false, true),
            Some(crate::InputOwnership::Remote)
        );
        assert_eq!(super::pointer_keyboard_ownership(layout, (100, 20)), None);
        assert_eq!(
            super::pointer_keyboard_ownership(
                layout,
                layout.minimize_button.expect("Windows layout").center(),
            ),
            None
        );
    }

    #[test]
    fn remote_ime_commits_once_and_suppresses_non_commit_state_from_egui() {
        assert_eq!(
            super::classify_ime_before_egui(
                crate::input::KeyboardDomain::RemoteSurface,
                &Ime::Commit("中".to_owned()),
                true,
            ),
            super::ImePreDispatch::Commit("中")
        );
        for ime in [
            Ime::Enabled,
            Ime::Preedit("中".to_owned(), Some((0, 3))),
            Ime::Disabled,
        ] {
            assert_eq!(
                super::classify_ime_before_egui(
                    crate::input::KeyboardDomain::RemoteSurface,
                    &ime,
                    true,
                ),
                super::ImePreDispatch::Consume
            );
        }
        assert_eq!(
            super::classify_ime_before_egui(
                crate::input::KeyboardDomain::RemoteSurface,
                &Ime::Commit("blocked".to_owned()),
                false,
            ),
            super::ImePreDispatch::Consume
        );
        assert_eq!(
            super::classify_ime_before_egui(
                crate::input::KeyboardDomain::LocalChrome,
                &Ime::Commit("local".to_owned()),
                true,
            ),
            super::ImePreDispatch::LocalChrome
        );
    }

    #[test]
    fn applied_egui_texture_deltas_are_empty_before_drop() {
        let mut deltas = egui::TexturesDelta::default();
        deltas.free.insert(egui::TextureId::Managed(7));

        mark_texture_deltas_applied(&mut deltas);

        assert!(deltas.set.is_empty());
        assert!(deltas.free.is_empty());
    }

    #[test]
    fn remote_area_starts_below_the_dpi_scaled_titlebar() {
        let layout = crate::ChromeLayout::for_window(1100, 720, 1.5, 0, 144).unwrap();
        assert_eq!(
            layout.content_rect,
            PixelRect {
                x: 0,
                y: 66,
                width: 1100,
                height: 654,
            }
        );
    }

    struct CountingWake(AtomicUsize);

    impl WakeSink for CountingWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    struct TestAudioFactory;

    impl AudioOutputFactory for TestAudioFactory {
        fn open(&self) -> Result<Box<dyn AudioOutput>, AudioOutputError> {
            Ok(Box::new(TestAudioOutput))
        }
    }

    struct CountingAudioFactory(Arc<AtomicUsize>);

    impl AudioOutputFactory for CountingAudioFactory {
        fn open(&self) -> Result<Box<dyn AudioOutput>, AudioOutputError> {
            self.0.fetch_add(1, Ordering::AcqRel);
            Ok(Box::new(TestAudioOutput))
        }
    }

    struct RecordingAudioFactory {
        open_count: Arc<AtomicUsize>,
        pcm_frames: Arc<Mutex<Vec<Vec<i16>>>>,
    }

    impl AudioOutputFactory for RecordingAudioFactory {
        fn open(&self) -> Result<Box<dyn AudioOutput>, AudioOutputError> {
            self.open_count.fetch_add(1, Ordering::AcqRel);
            Ok(Box::new(RecordingAudioOutput {
                pcm_frames: self.pcm_frames.clone(),
            }))
        }
    }

    struct InjectedProtocolSpawnFailure {
        live_workers: Arc<AtomicUsize>,
        audio_spawned: mpsc::Sender<()>,
        allow_audio_reclaim: Mutex<Option<mpsc::Receiver<()>>>,
    }

    impl WorkerSpawner for InjectedProtocolSpawnFailure {
        fn spawn(
            &self,
            kind: WorkerKind,
            name: String,
            work: Box<dyn FnOnce() + Send>,
        ) -> io::Result<JoinHandle<()>> {
            match kind {
                WorkerKind::Protocol => Err(io::Error::other("injected protocol spawn failure")),
                WorkerKind::Audio => {
                    let live_workers = self.live_workers.clone();
                    let audio_spawned = self.audio_spawned.clone();
                    let allow_audio_reclaim = self
                        .allow_audio_reclaim
                        .lock()
                        .unwrap()
                        .take()
                        .expect("one audio worker is spawned");
                    std::thread::Builder::new().name(name).spawn(move || {
                        live_workers.fetch_add(1, Ordering::AcqRel);
                        audio_spawned.send(()).unwrap();
                        work();
                        allow_audio_reclaim.recv().unwrap();
                        live_workers.fetch_sub(1, Ordering::AcqRel);
                    })
                }
            }
        }
    }

    struct TestIdentityStore;

    impl ServerIdentityStore for TestIdentityStore {
        fn load_pin(
            &self,
            _protocol: &ProtocolId,
            _endpoint: &Endpoint,
        ) -> Result<Option<[u8; 32]>, PlatformError> {
            Ok(None)
        }

        fn store_pin(
            &self,
            _protocol: &ProtocolId,
            _endpoint: &Endpoint,
            _pin: [u8; 32],
        ) -> Result<(), PlatformError> {
            Ok(())
        }
    }

    static TEST_UNAVAILABLE_PROFILES: UnavailableProfileStore = UnavailableProfileStore;
    static TEST_UNAVAILABLE_CREDENTIALS: UnavailableCredentialStore = UnavailableCredentialStore;

    fn test_app_stores(identity: &TestIdentityStore) -> AppPlatformStores<'_> {
        AppPlatformStores {
            server_identities: identity,
            profiles: &TEST_UNAVAILABLE_PROFILES,
            credentials: &TEST_UNAVAILABLE_CREDENTIALS,
        }
    }

    struct TestAudioOutput;

    impl AudioOutput for TestAudioOutput {
        fn enqueue_pcm(
            &mut self,
            _sample_rate_hz: u32,
            _channels: u8,
            _samples: Box<[i16]>,
        ) -> Result<(), AudioOutputError> {
            Ok(())
        }
    }

    struct RecordingAudioOutput {
        pcm_frames: Arc<Mutex<Vec<Vec<i16>>>>,
    }

    impl AudioOutput for RecordingAudioOutput {
        fn enqueue_pcm(
            &mut self,
            _sample_rate_hz: u32,
            _channels: u8,
            samples: Box<[i16]>,
        ) -> Result<(), AudioOutputError> {
            self.pcm_frames.lock().unwrap().push(samples.into_vec());
            Ok(())
        }
    }

    struct OpenFailureAudioFactory;

    impl AudioOutputFactory for OpenFailureAudioFactory {
        fn open(&self) -> Result<Box<dyn AudioOutput>, AudioOutputError> {
            Err(AudioOutputError::Unavailable)
        }
    }

    enum FailingAudioMode {
        EnqueueError,
        Panic,
    }

    struct FailingAudioFactory(FailingAudioMode);

    impl AudioOutputFactory for FailingAudioFactory {
        fn open(&self) -> Result<Box<dyn AudioOutput>, AudioOutputError> {
            Ok(Box::new(FailingAudioOutput(match self.0 {
                FailingAudioMode::EnqueueError => FailingAudioMode::EnqueueError,
                FailingAudioMode::Panic => FailingAudioMode::Panic,
            })))
        }
    }

    struct FailingAudioOutput(FailingAudioMode);

    impl AudioOutput for FailingAudioOutput {
        fn enqueue_pcm(
            &mut self,
            _sample_rate_hz: u32,
            _channels: u8,
            _samples: Box<[i16]>,
        ) -> Result<(), AudioOutputError> {
            match self.0 {
                FailingAudioMode::EnqueueError => Err(AudioOutputError::Closed),
                FailingAudioMode::Panic => panic!("sanitized test audio panic"),
            }
        }
    }

    struct BlockingFactory {
        worker_started: mpsc::Sender<()>,
    }

    struct ProtocolStartProbeFactory {
        protocol_id: ProtocolId,
        run_entered: mpsc::Sender<()>,
        publication_complete: mpsc::Sender<()>,
        session_dropped: mpsc::Sender<()>,
        run_count: Arc<AtomicUsize>,
        drop_count: Arc<AtomicUsize>,
    }

    impl ProtocolFactory for ProtocolStartProbeFactory {
        fn descriptor(&self) -> ProtocolDescriptor {
            ProtocolDescriptor {
                id: self.protocol_id.clone(),
                display_name: "protocol-start-probe".to_owned(),
                default_port: 5900,
                credential_requirements: CredentialRequirements::username_password(),
            }
        }

        fn create(
            &self,
            request: ConnectRequest,
            runtime: ProtocolRuntime,
        ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
            Ok(Box::new(ProtocolStartProbeSession {
                session_id: request.session_id,
                runtime,
                run_entered: self.run_entered.clone(),
                publication_complete: self.publication_complete.clone(),
                session_dropped: self.session_dropped.clone(),
                run_count: self.run_count.clone(),
                drop_count: self.drop_count.clone(),
            }))
        }
    }

    struct ProtocolStartProbeSession {
        session_id: SessionId,
        runtime: ProtocolRuntime,
        run_entered: mpsc::Sender<()>,
        publication_complete: mpsc::Sender<()>,
        session_dropped: mpsc::Sender<()>,
        run_count: Arc<AtomicUsize>,
        drop_count: Arc<AtomicUsize>,
    }

    impl Drop for ProtocolStartProbeSession {
        fn drop(&mut self) {
            self.drop_count.fetch_add(1, Ordering::AcqRel);
            let _ = self.session_dropped.send(());
        }
    }

    impl ProtocolSession for ProtocolStartProbeSession {
        fn run(mut self: Box<Self>) -> ProtocolExit {
            self.run_entered
                .send(())
                .expect("test observer remains available");
            self.run_count.fetch_add(1, Ordering::AcqRel);
            self.runtime
                .publish_event(SessionEvent::StageChanged(ConnectionStage::TransportReady))
                .expect("probe event port is installed");
            self.runtime
                .begin_generation(
                    self.session_id,
                    1,
                    frd_core::PixelSize::new(2, 2).expect("test probe size is valid"),
                    PixelFormat::Bgrx8UnormSrgb,
                )
                .expect("probe Reset port is installed");
            self.publication_complete
                .send(())
                .expect("test publication observer remains available");
            ProtocolExit::Closed
        }
    }

    struct ObservedBlockingFactory {
        create_count: Arc<AtomicUsize>,
        drop_count: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        create_entered: Option<mpsc::Sender<()>>,
        release_create: Mutex<Option<mpsc::Receiver<()>>>,
        worker_started: mpsc::Sender<()>,
    }

    impl ProtocolFactory for ObservedBlockingFactory {
        fn descriptor(&self) -> ProtocolDescriptor {
            ProtocolDescriptor {
                id: ProtocolId::apple_hpss_mvs(),
                display_name: "observed-test-protocol".to_owned(),
                default_port: 5900,
                credential_requirements: CredentialRequirements::username_password(),
            }
        }

        fn create(
            &self,
            _request: ConnectRequest,
            runtime: ProtocolRuntime,
        ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
            self.create_count.fetch_add(1, Ordering::AcqRel);
            if let Some(create_entered) = &self.create_entered {
                create_entered.send(()).unwrap();
            }
            if let Some(release_create) = self.release_create.lock().unwrap().take() {
                release_create.recv().unwrap();
            }
            Ok(Box::new(ObservedBlockingSession {
                runtime,
                worker_started: self.worker_started.clone(),
                drop_count: self.drop_count.clone(),
                stop: self.stop.clone(),
            }))
        }
    }

    struct ObservedBlockingSession {
        runtime: ProtocolRuntime,
        worker_started: mpsc::Sender<()>,
        drop_count: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
    }

    impl Drop for ObservedBlockingSession {
        fn drop(&mut self) {
            self.drop_count.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl ProtocolSession for ObservedBlockingSession {
        fn run(mut self: Box<Self>) -> ProtocolExit {
            self.worker_started.send(()).unwrap();
            loop {
                if self.stop.load(Ordering::Acquire) {
                    return ProtocolExit::Closed;
                }
                if matches!(
                    self.runtime.try_next_command(),
                    Some(SessionCommand::Disconnect)
                ) {
                    return ProtocolExit::Closed;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    struct FailOnceFactory {
        fail_next: AtomicBool,
        worker_started: mpsc::Sender<()>,
    }

    impl ProtocolFactory for FailOnceFactory {
        fn descriptor(&self) -> ProtocolDescriptor {
            ProtocolDescriptor {
                id: ProtocolId::apple_hpss_mvs(),
                display_name: "fail-once-protocol".to_owned(),
                default_port: 5900,
                credential_requirements: CredentialRequirements::username_password(),
            }
        }

        fn create(
            &self,
            _request: ConnectRequest,
            runtime: ProtocolRuntime,
        ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
            if self.fail_next.swap(false, Ordering::AcqRel) {
                return Err(ProtocolError::Adapter {
                    protocol_id: ProtocolId::apple_hpss_mvs(),
                    code: "test_factory_failed",
                });
            }
            Ok(Box::new(BlockingSession {
                runtime,
                worker_started: self.worker_started.clone(),
            }))
        }
    }

    impl ProtocolFactory for BlockingFactory {
        fn descriptor(&self) -> ProtocolDescriptor {
            ProtocolDescriptor {
                id: ProtocolId::apple_hpss_mvs(),
                display_name: "test-protocol".to_owned(),
                default_port: 5900,
                credential_requirements: CredentialRequirements::username_password(),
            }
        }

        fn create(
            &self,
            _request: ConnectRequest,
            runtime: ProtocolRuntime,
        ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
            Ok(Box::new(BlockingSession {
                runtime,
                worker_started: self.worker_started.clone(),
            }))
        }
    }

    struct BlockingSession {
        runtime: ProtocolRuntime,
        worker_started: mpsc::Sender<()>,
    }

    struct PanicFactory;

    impl ProtocolFactory for PanicFactory {
        fn descriptor(&self) -> ProtocolDescriptor {
            ProtocolDescriptor {
                id: ProtocolId::apple_hpss_mvs(),
                display_name: "panic-protocol".to_owned(),
                default_port: 5900,
                credential_requirements: CredentialRequirements::username_password(),
            }
        }

        fn create(
            &self,
            _request: ConnectRequest,
            _runtime: ProtocolRuntime,
        ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
            Ok(Box::new(PanicSession))
        }
    }

    struct PanicSession;

    impl ProtocolSession for PanicSession {
        fn run(self: Box<Self>) -> ProtocolExit {
            panic!("sanitized test protocol panic")
        }
    }

    struct MediaFactory;

    impl ProtocolFactory for MediaFactory {
        fn descriptor(&self) -> ProtocolDescriptor {
            ProtocolDescriptor {
                id: ProtocolId::apple_hpss_mvs(),
                display_name: "media-protocol".to_owned(),
                default_port: 5900,
                credential_requirements: CredentialRequirements::username_password(),
            }
        }

        fn create(
            &self,
            _request: ConnectRequest,
            runtime: ProtocolRuntime,
        ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
            Ok(Box::new(MediaSession { runtime }))
        }
    }

    struct NoMediaFactory;

    impl ProtocolFactory for NoMediaFactory {
        fn descriptor(&self) -> ProtocolDescriptor {
            ProtocolDescriptor {
                id: ProtocolId::apple_hpss_mvs(),
                display_name: "no-media-protocol".to_owned(),
                default_port: 5900,
                credential_requirements: CredentialRequirements::username_password(),
            }
        }

        fn create(
            &self,
            _request: ConnectRequest,
            runtime: ProtocolRuntime,
        ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
            Ok(Box::new(NoMediaSession { runtime }))
        }
    }

    struct MediaSession {
        runtime: ProtocolRuntime,
    }

    struct NoMediaSession {
        runtime: ProtocolRuntime,
    }

    struct UnsupportedInputFactory;

    impl ProtocolFactory for UnsupportedInputFactory {
        fn descriptor(&self) -> ProtocolDescriptor {
            ProtocolDescriptor {
                id: ProtocolId::apple_hpss_mvs(),
                display_name: "unsupported-input-protocol".to_owned(),
                default_port: 5900,
                credential_requirements: CredentialRequirements::username_password(),
            }
        }

        fn create(
            &self,
            request: ConnectRequest,
            runtime: ProtocolRuntime,
        ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
            Ok(Box::new(UnsupportedInputSession {
                session_id: request.session_id,
                runtime,
            }))
        }
    }

    struct UnsupportedInputSession {
        session_id: SessionId,
        runtime: ProtocolRuntime,
    }

    impl ProtocolSession for UnsupportedInputSession {
        fn run(mut self: Box<Self>) -> ProtocolExit {
            self.runtime
                .publish_event(SessionEvent::StageChanged(ConnectionStage::TransportReady))
                .unwrap();
            self.runtime
                .publish_event(SessionEvent::CapabilitiesChanged(SessionCapabilities {
                    dynamic_resolution: true,
                    clipboard_read: false,
                    clipboard_write: false,
                    remote_audio: false,
                    text_input: false,
                }))
                .unwrap();
            self.runtime
                .begin_generation(
                    self.session_id,
                    1,
                    frd_core::PixelSize::new(2, 2).unwrap(),
                    PixelFormat::Bgrx8UnormSrgb,
                )
                .unwrap();
            loop {
                match self.runtime.try_next_command() {
                    Some(SessionCommand::Input(_)) => {
                        return ProtocolExit::Failed(ProtocolError::Adapter {
                            protocol_id: ProtocolId::apple_hpss_mvs(),
                            code: "unsupported_keyboard_input",
                        });
                    }
                    Some(SessionCommand::Disconnect) => return ProtocolExit::Closed,
                    _ => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        }
    }

    impl ProtocolSession for MediaSession {
        fn run(mut self: Box<Self>) -> ProtocolExit {
            let _ = self.runtime.try_publish_optional_media(MediaFrame::Pcm {
                sample_rate_hz: 48_000,
                channels: 2,
                samples: vec![1_i16, -1_i16].into_boxed_slice(),
            });
            loop {
                if matches!(
                    self.runtime.try_next_command(),
                    Some(SessionCommand::Disconnect)
                ) {
                    return ProtocolExit::Closed;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    impl ProtocolSession for NoMediaSession {
        fn run(mut self: Box<Self>) -> ProtocolExit {
            loop {
                if matches!(
                    self.runtime.try_next_command(),
                    Some(SessionCommand::Disconnect)
                ) {
                    return ProtocolExit::Closed;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    impl ProtocolSession for BlockingSession {
        fn run(mut self: Box<Self>) -> ProtocolExit {
            self.worker_started.send(()).expect("test observer alive");
            loop {
                if matches!(
                    self.runtime.try_next_command(),
                    Some(SessionCommand::Disconnect)
                ) {
                    return ProtocolExit::Closed;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    #[test]
    fn connect_action_starts_a_real_worker_without_blocking_the_event_loop() {
        let (worker_started_tx, worker_started_rx) = mpsc::channel();
        let wake = Arc::new(CountingWake(AtomicUsize::new(0)));
        let mut host = SessionHost::new(
            [Arc::new(BlockingFactory {
                worker_started: worker_started_tx,
            }) as Arc<dyn ProtocolFactory>],
            wake.clone(),
            Arc::new(TestAudioFactory),
        );
        let session_id = SessionId::allocate();
        let (_owner, permit) = reserve_session_start(session_id);
        let request = ConnectRequest {
            session_id,
            endpoint: Endpoint::new("test.invalid", 5900).expect("valid test endpoint"),
            protocol_id: ProtocolId::apple_hpss_mvs(),
            credentials: Some(Credentials {
                username: "test-user".to_owned(),
                password: SecretBuffer::new(vec![0x41]).take(),
            }),
            saved_server_pin: None,
        };

        let before = Instant::now();
        let outcome = host.complete_test_launch(permit, TargetSystem::MacOs, request);
        assert!(before.elapsed() < Duration::from_millis(250));
        assert!(matches!(outcome, TestLaunchOutcome::Started));
        worker_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("protocol worker starts asynchronously");

        let before_cleanup = Instant::now();
        let cleanup = complete_background_cleanup(&mut host);
        assert!(
            before_cleanup.elapsed() < Duration::from_millis(250),
            "the caller waits for a typed event, never a worker join"
        );
        assert_eq!(cleanup.session_id(), session_id);
        assert!(wake.0.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn protocol_spawn_failure_returns_from_dispatch_before_off_loop_rollback_reclaims_audio() {
        let (audio_spawned_tx, audio_spawned_rx) = mpsc::channel();
        let (allow_audio_reclaim_tx, allow_audio_reclaim_rx) = mpsc::channel();
        let live_workers = Arc::new(AtomicUsize::new(0));
        let audio_factory = Arc::new(CountingAudioFactory(Arc::new(AtomicUsize::new(0))));
        let mut host = SessionHost::new_with_spawner(
            [Arc::new(BlockingFactory {
                worker_started: mpsc::channel().0,
            }) as Arc<dyn ProtocolFactory>],
            Arc::new(CountingWake(AtomicUsize::new(0))),
            audio_factory.clone(),
            Arc::new(InjectedProtocolSpawnFailure {
                live_workers: live_workers.clone(),
                audio_spawned: audio_spawned_tx,
                allow_audio_reclaim: Mutex::new(Some(allow_audio_reclaim_rx)),
            }),
        );
        let session_id = SessionId::allocate();
        let (_owner, permit) = reserve_session_start(session_id);
        let request = test_request(session_id);
        let (outcome_tx, outcome_rx) = mpsc::channel();

        let before = Instant::now();
        assert!(host
            .begin_launch(permit, TargetSystem::MacOs, request, move |outcome| {
                outcome_tx.send(outcome).unwrap();
            })
            .expect("background launch thread starts"));
        assert!(
            before.elapsed() < Duration::from_millis(250),
            "ApplicationHandler dispatch must not wait for partial rollback"
        );
        audio_spawned_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the injected partial audio worker starts");
        assert_eq!(live_workers.load(Ordering::Acquire), 1);
        assert!(matches!(
            outcome_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(
            audio_factory.0.load(Ordering::Acquire),
            0,
            "the start barrier forbids platform audio open before protocol spawn succeeds"
        );

        allow_audio_reclaim_tx.send(()).unwrap();
        let outcome = outcome_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("rollback is published after partial resources are reclaimed");
        assert_eq!(live_workers.load(Ordering::Acquire), 0);
        assert!(matches!(
            host.accept_launch_outcome(outcome, |_| panic!("rollback has no cleanup worker")),
            Ok(AcceptedLaunchOutcome::LaunchRolledBack(_))
        ));
        assert!(!host.is_active());
    }

    #[test]
    fn protocol_worker_waits_until_live_ports_are_installed_for_apple_and_rdp() {
        assert_protocol_worker_waits_until_live_ports_are_installed(
            TargetSystem::MacOs,
            ProtocolId::apple_hpss_mvs(),
        );
        assert_protocol_worker_waits_until_live_ports_are_installed(
            TargetSystem::Windows,
            ProtocolId::rdp(),
        );
    }

    #[test]
    fn cancelled_launch_drops_barrier_without_running_protocol() {
        assert_cancelled_started_probe_launch(TargetSystem::MacOs, ProtocolId::apple_hpss_mvs());
    }

    #[test]
    fn close_after_started_outcome_but_before_acceptance_never_installs_stale_active_ports() {
        assert_cancelled_started_probe_launch(TargetSystem::MacOs, ProtocolId::apple_hpss_mvs());
    }

    #[test]
    fn fatal_while_launch_is_pending_latches_without_waiting_or_a_deadline() {
        let create_count = Arc::new(AtomicUsize::new(0));
        let drop_count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (create_entered_tx, create_entered_rx) = mpsc::channel();
        let (release_create_tx, release_create_rx) = mpsc::channel();
        let mut host = test_host(
            Arc::new(ObservedBlockingFactory {
                create_count: create_count.clone(),
                drop_count: drop_count.clone(),
                stop,
                create_entered: Some(create_entered_tx),
                release_create: Mutex::new(Some(release_create_rx)),
                worker_started: mpsc::channel().0,
            }),
            Arc::new(TestAudioFactory),
        );
        let session_id = SessionId::allocate();
        let (_owner, permit) = reserve_session_start(session_id);
        let (launch_tx, launch_rx) = mpsc::channel();
        host.begin_launch(
            permit,
            TargetSystem::MacOs,
            test_request(session_id),
            move |outcome| launch_tx.send(outcome).unwrap(),
        )
        .expect("background launch starts");
        create_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("fatal is injected while factory creation owns the request");

        let mut exit = ApplicationExitState::default();
        let report = crate::fatal::FatalReport::internal(
            crate::fatal::FatalComponent::Application,
            crate::fatal::FatalOperation::Launch,
            crate::fatal::FatalReason::InvalidState,
        );
        assert!(exit.latch_fatal(report.clone()));
        assert!(exit.should_ignore_events());
        assert_eq!(exit.pending_launch_deadline(), None);
        assert!(host.cancel_pending_launch());
        assert!(matches!(
            launch_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(exit.runner_result(), Err(report));

        release_create_tx.send(()).unwrap();

        let outcome = launch_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("background transaction may finish after fatal exit is already selected");
        assert!(exit.should_ignore_events());
        drop(outcome);
        assert!(host.active.is_none());
        assert_eq!(create_count.load(Ordering::Acquire), 1);
        assert_eq!(drop_count.load(Ordering::Acquire), 1);
    }

    #[test]
    fn fatal_after_started_is_queued_ignores_the_late_event_without_installing_ports() {
        let run_count = Arc::new(AtomicUsize::new(0));
        let drop_count = Arc::new(AtomicUsize::new(0));
        let (run_entered_tx, run_entered_rx) = mpsc::channel();
        let (publication_complete_tx, _publication_complete_rx) = mpsc::channel();
        let (session_dropped_tx, session_dropped_rx) = mpsc::channel();
        let mut host = test_host(
            Arc::new(ProtocolStartProbeFactory {
                protocol_id: ProtocolId::apple_hpss_mvs(),
                run_entered: run_entered_tx,
                publication_complete: publication_complete_tx,
                session_dropped: session_dropped_tx,
                run_count: run_count.clone(),
                drop_count: drop_count.clone(),
            }),
            Arc::new(TestAudioFactory),
        );
        let session_id = SessionId::allocate();
        let (_owner, permit) = reserve_session_start(session_id);
        let (launch_tx, launch_rx) = mpsc::channel();
        host.begin_launch(
            permit,
            TargetSystem::MacOs,
            test_request(session_id),
            move |outcome| launch_tx.send(outcome).unwrap(),
        )
        .expect("background launch starts");
        let outcome = launch_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Started is queued before the fatal transition");
        assert!(matches!(
            run_entered_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        let mut exit = ApplicationExitState::default();
        let report = crate::fatal::FatalReport::internal(
            crate::fatal::FatalComponent::Application,
            crate::fatal::FatalOperation::Launch,
            crate::fatal::FatalReason::InvalidState,
        );
        assert!(exit.latch_fatal(report.clone()));
        assert!(host.cancel_pending_launch());
        assert_eq!(exit.pending_launch_deadline(), None);
        assert!(exit.should_ignore_events());
        drop(outcome);
        assert!(host.active.is_none());
        assert_eq!(run_count.load(Ordering::Acquire), 0);
        assert_eq!(exit.runner_result(), Err(report));
        session_dropped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("dropping the queued launch cancels the waiting session");
        assert_eq!(drop_count.load(Ordering::Acquire), 1);
    }

    #[test]
    fn fatal_latch_discards_an_existing_graceful_shutdown_deadline() {
        let now = Instant::now();
        let mut exit = ApplicationExitState {
            pending_launch_deadline: Some(now + Duration::from_secs(5)),
            ..ApplicationExitState::default()
        };
        let report = crate::fatal::FatalReport::internal(
            crate::fatal::FatalComponent::Application,
            crate::fatal::FatalOperation::Shutdown,
            crate::fatal::FatalReason::InvalidState,
        );

        assert!(exit.latch_fatal(report));
        assert_eq!(exit.pending_launch_deadline(), None);
    }

    #[test]
    fn factory_failure_rolls_back_partial_runtime_resources_and_allows_a_fresh_launch() {
        let (worker_started_tx, worker_started_rx) = mpsc::channel();
        let mut host = test_host(
            Arc::new(FailOnceFactory {
                fail_next: AtomicBool::new(true),
                worker_started: worker_started_tx,
            }),
            Arc::new(TestAudioFactory),
        );

        let first_session = SessionId::allocate();
        let (_first_owner, first_permit) = reserve_session_start(first_session);
        let first_request = ConnectRequest {
            session_id: first_session,
            endpoint: Endpoint::new("test.invalid", 5900).unwrap(),
            protocol_id: ProtocolId::apple_hpss_mvs(),
            credentials: Some(Credentials {
                username: "test-user".to_owned(),
                password: SecretBuffer::new(vec![0x41]).take(),
            }),
            saved_server_pin: None,
        };
        let TestLaunchOutcome::LaunchRolledBack(failure) =
            host.complete_test_launch(first_permit, TargetSystem::MacOs, first_request)
        else {
            panic!("factory failure must return only after rollback");
        };
        assert!(matches!(
            failure.error(),
            ProtocolError::Adapter {
                code: "test_factory_failed",
                ..
            }
        ));
        assert!(!host.is_active());

        let second_session = launch_test_session(&mut host);
        worker_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("fresh request starts after rollback reclaimed the coordinator");
        let completion = complete_background_cleanup(&mut host);
        assert_eq!(completion.session_id(), second_session);
    }

    #[test]
    fn unsupported_keyboard_input_never_reaches_or_closes_the_fake_adapter_session() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let mut form = ConnectionForm::new(ConnectionDraft {
            target_system: Some(TargetSystem::MacOs),
            address: "test.invalid".to_owned(),
            port: Some(5900),
            protocol: ProtocolChoice::Explicit(ProtocolId::apple_hpss_mvs()),
            username: "test-user".to_owned(),
        });
        form.set_password(SecretBuffer::new(vec![0x41]));
        let mut controller = AppController::connection_form(form);
        controller.set_platform_capabilities(PlatformCapabilities {
            dynamic_resolution: true,
            clipboard_read: false,
            clipboard_write: false,
            remote_audio: false,
            text_input: true,
        });
        controller.set_product_policy(ProductPolicy {
            dynamic_resolution: true,
            clipboard_read: false,
            clipboard_write: false,
            remote_audio: false,
            text_input: true,
        });

        let intent = controller
            .connection_form_mut()
            .expect("connection form remains editable")
            .take_connect_intent(&catalog)
            .expect("complete form creates one connection intent");
        let identity_store = TestIdentityStore;
        let stores = test_app_stores(&identity_store);
        let AppAction::StartSession(request, permit) = controller
            .handle_intent_with_stores(intent, &catalog, stores)
            .expect("controller accepts the connection")
            .expect("connection starts one session")
        else {
            panic!("connection must start through the shared transaction");
        };
        let session_id = request.session_id;
        let mut host = test_host(
            Arc::new(UnsupportedInputFactory),
            Arc::new(TestAudioFactory),
        );
        assert!(matches!(
            host.complete_test_launch(permit, TargetSystem::MacOs, request),
            TestLaunchOutcome::Started
        ));

        let events = wait_for_event(&mut host, |event| {
            matches!(event, SessionEvent::SurfaceGenerationChanged { .. })
        });
        for (origin, event) in events {
            controller.handle_session_event_with_stores(origin, event, stores);
        }
        controller.handle_presentation(PresentationEvent::FramePresented {
            session_id,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::FullBaseline,
        });

        let unsupported_key = InputEvent::PhysicalKey {
            code: PhysicalKeyCode::from_usb_hid_usage(30),
            state: KeyState::Pressed,
            modifiers: Modifiers::default(),
        };
        if let Some(command) = controller.route_input(unsupported_key) {
            host.send_command(command)
                .expect("session command channel open");
        }
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            !host
                .drain_session_events()
                .iter()
                .any(|(_, event)| matches!(event, SessionEvent::Closed(_))),
            "an unsupported key must not reach the fail-closed fake adapter"
        );

        let Some(AppAction::SessionCommand(disconnect)) = controller
            .handle_intent_with_stores(AppIntent::Disconnect, &catalog, stores)
            .expect("disconnect is valid while remote")
        else {
            panic!("disconnect must emit exactly one protocol command");
        };
        host.send_command(disconnect)
            .expect("disconnect reaches the worker");
        let completion = complete_background_cleanup(&mut host);
        controller
            .finish_session_cleanup_with_stores(completion, stores)
            .expect("shared cleanup token releases the controller slot");
    }

    #[test]
    fn protocol_worker_panic_publishes_one_sanitized_terminal_event_and_cleans_up() {
        let mut host = test_host(Arc::new(PanicFactory), Arc::new(TestAudioFactory));
        let session_id = launch_test_session(&mut host);

        let events = wait_for_event(&mut host, |event| {
            matches!(
                event,
                SessionEvent::Closed(ProtocolExit::Failed(ProtocolError::Terminal))
            )
        });
        assert_eq!(
            events
                .iter()
                .filter(|(_, event)| matches!(event, SessionEvent::Closed(_)))
                .count(),
            1
        );

        let completion = complete_background_cleanup(&mut host);
        assert_eq!(completion.session_id(), session_id);
    }

    #[test]
    fn drained_session_events_retain_their_originating_session_id() {
        let mut host = test_host(Arc::new(PanicFactory), Arc::new(TestAudioFactory));
        let session_id = launch_test_session(&mut host);

        let events = wait_for_event(&mut host, |event| matches!(event, SessionEvent::Closed(_)));

        assert!(events.iter().all(|(origin, _event)| *origin == session_id));
    }

    #[test]
    fn audio_open_failure_is_one_degradation_and_session_cleanup_stays_safe() {
        assert_audio_degradation_and_cleanup(Arc::new(OpenFailureAudioFactory));
    }

    #[test]
    fn audio_enqueue_failure_is_one_degradation_and_session_cleanup_stays_safe() {
        assert_audio_degradation_and_cleanup(Arc::new(FailingAudioFactory(
            FailingAudioMode::EnqueueError,
        )));
    }

    #[test]
    fn audio_worker_panic_is_one_sanitized_degradation_and_session_cleanup_stays_safe() {
        assert_audio_degradation_and_cleanup(Arc::new(FailingAudioFactory(
            FailingAudioMode::Panic,
        )));
    }

    #[test]
    fn audio_worker_opens_device_only_after_first_frame() {
        let no_media_opens = Arc::new(AtomicUsize::new(0));
        let mut no_media_host = test_host(
            Arc::new(NoMediaFactory),
            Arc::new(CountingAudioFactory(no_media_opens.clone())),
        );
        let no_media_session = launch_test_session(&mut no_media_host);
        std::thread::sleep(Duration::from_millis(25));
        assert_eq!(
            no_media_opens.load(Ordering::Acquire),
            0,
            "a session that publishes no media must not open platform audio"
        );
        let no_media_cleanup = complete_background_cleanup(&mut no_media_host);
        assert_eq!(no_media_cleanup.session_id(), no_media_session);
        assert_eq!(no_media_opens.load(Ordering::Acquire), 0);

        let media_opens = Arc::new(AtomicUsize::new(0));
        let mut media_host = test_host(
            Arc::new(MediaFactory),
            Arc::new(CountingAudioFactory(media_opens.clone())),
        );
        let media_session = launch_test_session(&mut media_host);
        let deadline = Instant::now() + Duration::from_secs(1);
        while media_opens.load(Ordering::Acquire) != 1 {
            assert!(
                Instant::now() < deadline,
                "the first media frame must eventually open platform audio"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(media_opens.load(Ordering::Acquire), 1);
        let media_cleanup = complete_background_cleanup(&mut media_host);
        assert_eq!(media_cleanup.session_id(), media_session);
        assert_eq!(media_opens.load(Ordering::Acquire), 1);
    }

    #[test]
    fn audio_worker_ignores_video_until_first_pcm_frame() {
        let video_only_opens = Arc::new(AtomicUsize::new(0));
        let (video_only_tx, video_only_rx) = mpsc::channel();
        let video_only_factory: Arc<dyn AudioOutputFactory> =
            Arc::new(CountingAudioFactory(video_only_opens.clone()));
        let video_only_worker =
            std::thread::spawn(move || super::run_audio_worker(video_only_factory, video_only_rx));
        video_only_tx
            .send(MediaFrame::EncodedVideo {
                timestamp_us: 1,
                bytes: vec![0xaa].into_boxed_slice(),
            })
            .unwrap();
        drop(video_only_tx);
        assert_eq!(
            video_only_worker.join().unwrap(),
            super::AudioWorkerExit::Closed
        );
        assert_eq!(
            video_only_opens.load(Ordering::Acquire),
            0,
            "a video-only session must not open platform audio"
        );

        let video_then_pcm_opens = Arc::new(AtomicUsize::new(0));
        let pcm_frames = Arc::new(Mutex::new(Vec::new()));
        let (video_then_pcm_tx, video_then_pcm_rx) = mpsc::channel();
        let video_then_pcm_factory: Arc<dyn AudioOutputFactory> = Arc::new(RecordingAudioFactory {
            open_count: video_then_pcm_opens.clone(),
            pcm_frames: pcm_frames.clone(),
        });
        let video_then_pcm_worker = std::thread::spawn(move || {
            super::run_audio_worker(video_then_pcm_factory, video_then_pcm_rx)
        });
        video_then_pcm_tx
            .send(MediaFrame::EncodedVideo {
                timestamp_us: 2,
                bytes: vec![0xbb].into_boxed_slice(),
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(25));
        assert_eq!(
            video_then_pcm_opens.load(Ordering::Acquire),
            0,
            "video must not open platform audio before PCM arrives"
        );
        video_then_pcm_tx
            .send(MediaFrame::Pcm {
                sample_rate_hz: 48_000,
                channels: 2,
                samples: vec![7_i16, -8_i16].into_boxed_slice(),
            })
            .unwrap();
        drop(video_then_pcm_tx);
        assert_eq!(
            video_then_pcm_worker.join().unwrap(),
            super::AudioWorkerExit::Closed
        );
        assert_eq!(video_then_pcm_opens.load(Ordering::Acquire), 1);
        assert_eq!(*pcm_frames.lock().unwrap(), vec![vec![7_i16, -8_i16]]);
    }

    fn assert_audio_degradation_and_cleanup(audio: Arc<dyn AudioOutputFactory>) {
        let mut host = test_host(Arc::new(MediaFactory), audio);
        let session_id = launch_test_session(&mut host);
        let events = wait_for_event(&mut host, |event| {
            matches!(
                event,
                SessionEvent::AudioState(frd_protocol_api::AudioState::Failed)
            )
        });
        std::thread::sleep(Duration::from_millis(10));
        let mut all_events = events;
        all_events.extend(host.drain_session_events());
        assert_eq!(
            all_events
                .iter()
                .filter(|(_, event)| {
                    matches!(
                        event,
                        SessionEvent::AudioState(frd_protocol_api::AudioState::Failed)
                    )
                })
                .count(),
            1
        );
        assert!(
            host.is_active(),
            "audio degradation must not close the desktop"
        );
        assert!(!all_events
            .iter()
            .any(|(_, event)| matches!(event, SessionEvent::Closed(_))));

        let completion = complete_background_cleanup(&mut host);
        assert_eq!(completion.session_id(), session_id);
    }

    fn test_host(
        protocol: Arc<dyn ProtocolFactory>,
        audio: Arc<dyn AudioOutputFactory>,
    ) -> SessionHost {
        SessionHost::new(
            [protocol],
            Arc::new(CountingWake(AtomicUsize::new(0))),
            audio,
        )
    }

    fn assert_protocol_worker_waits_until_live_ports_are_installed(
        target: TargetSystem,
        protocol_id: ProtocolId,
    ) {
        let (run_entered_tx, run_entered_rx) = mpsc::channel();
        let (publication_complete_tx, publication_complete_rx) = mpsc::channel();
        let (session_dropped_tx, session_dropped_rx) = mpsc::channel();
        let run_count = Arc::new(AtomicUsize::new(0));
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut host = test_host(
            Arc::new(ProtocolStartProbeFactory {
                protocol_id: protocol_id.clone(),
                run_entered: run_entered_tx,
                publication_complete: publication_complete_tx,
                session_dropped: session_dropped_tx,
                run_count: run_count.clone(),
                drop_count: drop_count.clone(),
            }),
            Arc::new(TestAudioFactory),
        );
        let session_id = SessionId::allocate();
        let (_owner, permit) = reserve_session_start(session_id);
        let (launch_tx, launch_rx) = mpsc::channel();
        host.begin_launch(
            permit,
            target,
            test_request_for_protocol(session_id, protocol_id),
            move |outcome| launch_tx.send(outcome).unwrap(),
        )
        .expect("background launch starts");
        let outcome = launch_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("background launch completes");
        assert!(matches!(
            run_entered_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(host.active.is_none());

        assert!(matches!(
            host.accept_launch_outcome(outcome, |_| panic!("normal launch cannot start cleanup")),
            Ok(AcceptedLaunchOutcome::Started)
        ));
        run_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("protocol starts after live ports are installed");
        publication_complete_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("protocol publishes its ordinary event and Reset");
        assert_eq!(run_count.load(Ordering::Acquire), 1);
        assert!(host.active.is_some());
        assert!(host.drain_session_events().iter().any(|(_, event)| {
            matches!(
                event,
                SessionEvent::StageChanged(ConnectionStage::TransportReady)
            )
        }));
        assert!(host
            .drain_frame_updates()
            .iter()
            .any(|update| matches!(update, frd_frame::SurfaceUpdate::Reset { .. })));
        session_dropped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("one-shot probe session exits after publication");
        let completion = complete_background_cleanup(&mut host);
        assert_eq!(completion.session_id(), session_id);
        assert_eq!(drop_count.load(Ordering::Acquire), 1);
    }

    fn assert_cancelled_started_probe_launch(target: TargetSystem, protocol_id: ProtocolId) {
        let (run_entered_tx, run_entered_rx) = mpsc::channel();
        let (publication_complete_tx, _publication_complete_rx) = mpsc::channel();
        let (session_dropped_tx, session_dropped_rx) = mpsc::channel();
        let run_count = Arc::new(AtomicUsize::new(0));
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut host = test_host(
            Arc::new(ProtocolStartProbeFactory {
                protocol_id: protocol_id.clone(),
                run_entered: run_entered_tx,
                publication_complete: publication_complete_tx,
                session_dropped: session_dropped_tx,
                run_count: run_count.clone(),
                drop_count: drop_count.clone(),
            }),
            Arc::new(TestAudioFactory),
        );
        let session_id = SessionId::allocate();
        let (_owner, permit) = reserve_session_start(session_id);
        let (launch_tx, launch_rx) = mpsc::channel();
        host.begin_launch(
            permit,
            target,
            test_request_for_protocol(session_id, protocol_id),
            move |outcome| launch_tx.send(outcome).unwrap(),
        )
        .expect("background launch starts");
        let outcome = launch_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Started reaches the application thread");
        assert!(matches!(
            run_entered_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        assert!(host.cancel_pending_launch());
        let (cleanup_tx, cleanup_rx) = mpsc::channel();
        assert!(matches!(
            host.accept_launch_outcome(outcome, move |outcome| {
                cleanup_tx.send(outcome).unwrap();
            }),
            Ok(AcceptedLaunchOutcome::CancelledStarted)
        ));
        assert!(
            host.active.is_none(),
            "stale ports must never become active"
        );
        let cleanup = cleanup_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("cancelled start is reclaimed by the bounded cleanup worker");
        let completion = host
            .accept_cleanup_outcome(cleanup)
            .expect("cancelled launch cleanup completes");
        assert_eq!(completion.session_id(), session_id);
        session_dropped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("cancelled waiter drops its protocol session");
        assert_eq!(run_count.load(Ordering::Acquire), 0);
        assert_eq!(drop_count.load(Ordering::Acquire), 1);
        assert!(!host.is_active());
    }

    fn launch_test_session(host: &mut SessionHost) -> SessionId {
        let session_id = SessionId::allocate();
        let (_owner, permit) = reserve_session_start(session_id);
        let request = test_request(session_id);
        assert!(matches!(
            host.complete_test_launch(permit, TargetSystem::MacOs, request),
            TestLaunchOutcome::Started
        ));
        session_id
    }

    fn test_request(session_id: SessionId) -> ConnectRequest {
        test_request_for_protocol(session_id, ProtocolId::apple_hpss_mvs())
    }

    fn test_request_for_protocol(session_id: SessionId, protocol_id: ProtocolId) -> ConnectRequest {
        ConnectRequest {
            session_id,
            endpoint: Endpoint::new("test.invalid", 5900).expect("valid test endpoint"),
            protocol_id,
            credentials: None,
            saved_server_pin: None,
        }
    }

    fn wait_for_event(
        host: &mut SessionHost,
        predicate: impl Fn(&SessionEvent) -> bool,
    ) -> Vec<(SessionId, SessionEvent)> {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut observed = Vec::new();
        loop {
            observed.extend(host.drain_session_events());
            if observed.iter().any(|(_, event)| predicate(event)) {
                return observed;
            }
            assert!(
                Instant::now() < deadline,
                "worker must publish the expected event"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn complete_background_cleanup(host: &mut SessionHost) -> frd_session::CleanupComplete {
        let (outcome_tx, outcome_rx) = mpsc::channel();
        assert!(host
            .begin_cleanup(move |outcome| outcome_tx.send(outcome).unwrap())
            .expect("cleanup worker starts"));
        assert!(!host
            .begin_cleanup(|_| panic!("a repeated close must not start a second cleanup worker"))
            .expect("repeated close is an idempotent no-op"));
        let outcome = outcome_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("background cleanup reports completion");
        host.accept_cleanup_outcome(outcome)
            .expect("cleanup completes without a fatal state")
    }
}
