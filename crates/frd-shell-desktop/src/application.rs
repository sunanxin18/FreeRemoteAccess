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
    PointerButton, ProtocolId, SessionId, TargetSystem,
};
use frd_frame::{
    EnqueuedSurfaceUpdate, FrameCompleteness, FrameMailbox, FrameReset, FrameRevision,
    FrameTransaction, FrameTransactionCompiler, FrameTransactionError, PixelBuffer, PixelFormat,
    PixelPatch, SurfaceUpdate,
};
use frd_media_api::{
    AudioOutput, AudioOutputError, DecodedVideoFrame, MediaFrame, MediaPublishError,
    MediaPublisher, MediaStageDiagnostic, MediaStageTrace, VideoDecodeErrorCode,
    VideoStreamIdentity,
};
use frd_protocol_api::{
    ConnectRequest, MailboxSurfacePublisher, ProtocolCatalog, ProtocolError, ProtocolExit,
    ProtocolFactory, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionCommand, SessionEvent,
};
use frd_render_wgpu::{
    BatchApplyFailure, BatchApplyOutcome, BatchApplySuccess, GpuContext, RecoveryRequirement,
    RemoteRenderer, VideoPresentationReceipt, VideoRenderer, VideoRendererError, VideoStreamEpoch,
};
use frd_session::{
    CleanupComplete, CleanupError, CleanupOperations, SessionCleanupHandle, SessionCoordinator,
    SessionStartFailure, SessionStartOutcome, SessionStartPermit,
};
use frd_ui_model::{
    CapabilityGlyphState, ConnectionGlyph, IslandAction, LaunchOptions, Page, SessionChromeModel,
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
    checked_mailbox_age, BatchMetricContext, FramePipelineMetrics, MetricIdentity,
};
#[cfg(test)]
use crate::frame_metrics_sink::MetricSinkError;
use crate::input::{hid_usage_from_key_code, KeyboardDomain, KeyboardPreDispatch};
use crate::lifecycle::{
    execute_presentation_recovery, OcclusionAction, PresentationLifecycle, PresentationOperation,
    PresentationRecoveryBackend, PresentationRecoveryContext, PresentationRecoveryFailure,
};
use crate::platform::PlatformWindowChrome;
use crate::presentation_timing::{PresentationTimingKey, PresentationTimingTracker};
use crate::repaint::{RepaintPlan, RepaintScheduler};
use crate::ui_fonts::system_font_definitions;
use crate::video_decode_worker::{
    VideoDecodeSender, VideoDecodeWorker, VideoFrameToken, VideoStreamAdmission, VideoWorkerEvent,
    VideoWorkerEvents,
};
use crate::{
    ChromeGeometrySnapshot, ChromeHitMap, ChromeHitTarget, ChromeLayouts, ChromeRect,
    ControlIslandPlacement, InputGate, InputOwnership, InputRouter, WindowChromeAdapter,
    WindowChromeCommand, TITLE_BAR_HEIGHT_POINTS,
};

const FRAME_MAILBOX_ENTRY_LIMIT: usize = 256;
const FRAME_MAILBOX_PIXEL_LIMIT: usize = 64 * 1024 * 1024;
const MEDIA_MAILBOX_ENTRY_LIMIT: usize = 16;
const PENDING_LAUNCH_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const HP_INPUT_DIAGNOSTICS_ENV: &str = "FRD_APPLE_HP_INPUT_DIAGNOSTICS";
const HP_INPUT_DIAGNOSTICS_QUEUE_LIMIT: usize = 32;
const APPLE_HIGH_PERFORMANCE_PROTOCOL_ID: &str = "apple-high-performance";
const TEST_SESSION_CHROME: SessionChromeModel = SessionChromeModel {
    connection: ConnectionGlyph::Connected,
    diagnostics: None,
    presentation_timing: None,
    audio: CapabilityGlyphState::Unavailable,
    clipboard: CapabilityGlyphState::Unavailable,
    action: Some(IslandAction::Disconnect),
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HighPerformanceInputShellLine {
    Stage { stage: &'static str, count: u64 },
}

impl std::fmt::Display for HighPerformanceInputShellLine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stage { stage, count } => {
                write!(formatter, "[apple-hp-input] stage={stage} count={count}")
            }
        }
    }
}

#[derive(Default)]
struct HighPerformanceInputShellDiagnostics {
    lines: Option<mpsc::SyncSender<HighPerformanceInputShellLine>>,
    session_id: Option<SessionId>,
    shell_physical_accepted: u64,
    command_enqueued: u64,
}

impl HighPerformanceInputShellDiagnostics {
    fn from_environment() -> Self {
        if !std::env::var_os(HP_INPUT_DIAGNOSTICS_ENV).is_some_and(|value| value == "1") {
            return Self::default();
        }
        let (lines, receiver) = mpsc::sync_channel(HP_INPUT_DIAGNOSTICS_QUEUE_LIMIT);
        let reporter = std::thread::Builder::new()
            .name("frd-hp-input-shell-diagnostic".to_owned())
            .spawn(move || {
                while let Ok(line) = receiver.recv() {
                    eprintln!("{line}");
                }
            });
        reporter
            .is_ok()
            .then_some(Self {
                lines: Some(lines),
                ..Self::default()
            })
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn enabled_for_test(capacity: usize) -> (Self, mpsc::Receiver<HighPerformanceInputShellLine>) {
        let (lines, receiver) = mpsc::sync_channel(capacity);
        (
            Self {
                lines: Some(lines),
                ..Self::default()
            },
            receiver,
        )
    }

    fn is_enabled(&self) -> bool {
        self.lines.is_some()
    }

    fn observe_accepted(&mut self, session_id: SessionId) {
        self.select_session(session_id);
        self.shell_physical_accepted = self.shell_physical_accepted.saturating_add(1);
        self.enqueue_stage("shell_physical_accepted", self.shell_physical_accepted);
    }

    fn observe_enqueued(&mut self, session_id: SessionId) {
        self.select_session(session_id);
        self.command_enqueued = self.command_enqueued.saturating_add(1);
        self.enqueue_stage("command_enqueued", self.command_enqueued);
    }

    fn select_session(&mut self, session_id: SessionId) {
        if self.session_id != Some(session_id) {
            self.session_id = Some(session_id);
            self.shell_physical_accepted = 0;
            self.command_enqueued = 0;
        }
    }

    fn enqueue_stage(&self, stage: &'static str, count: u64) {
        if count.is_power_of_two() {
            if let Some(lines) = &self.lines {
                let _ = lines.try_send(HighPerformanceInputShellLine::Stage { stage, count });
            }
        }
    }
}

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

fn schedule_egui_repaint(event: &WindowEvent, repaint: bool, request_redraw: impl FnOnce()) {
    if repaint && !matches!(event, WindowEvent::RedrawRequested) {
        request_redraw();
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
        ports: PendingLiveSessionPorts,
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

struct DesktopMediaPublisher {
    audio: mpsc::SyncSender<MediaFrame>,
    video: VideoDecodeSender,
}

impl DesktopMediaPublisher {
    fn new(audio: mpsc::SyncSender<MediaFrame>, video: VideoDecodeSender) -> Self {
        Self { audio, video }
    }
}

impl MediaPublisher for DesktopMediaPublisher {
    fn publish(&self, frame: MediaFrame) -> Result<(), MediaPublishError> {
        match frame {
            frame @ MediaFrame::Pcm { .. } => {
                self.audio.try_send(frame).map_err(|error| match error {
                    mpsc::TrySendError::Full(_) => MediaPublishError::Full,
                    mpsc::TrySendError::Disconnected(_) => MediaPublishError::Closed,
                })
            }
            MediaFrame::VideoConfig(config) => self
                .video
                .try_send_config(config)
                .map_err(map_video_publish_error),
            MediaFrame::EncodedVideo(access_unit) => self
                .video
                .try_send_access_unit(access_unit)
                .map_err(map_video_publish_error),
        }
    }
}

fn map_video_publish_error(
    error: crate::video_decode_worker::VideoWorkerSendError,
) -> MediaPublishError {
    match error {
        crate::video_decode_worker::VideoWorkerSendError::Full => MediaPublishError::Full,
        crate::video_decode_worker::VideoWorkerSendError::Closed => MediaPublishError::Closed,
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

struct PendingLiveSessionPorts {
    session_id: SessionId,
    protocol_id: ProtocolId,
    commands: mpsc::Sender<SessionCommand>,
    events: mpsc::Receiver<SessionEvent>,
    mailbox: Arc<Mutex<FrameMailbox>>,
    video_events: VideoWorkerEvents,
}

struct LiveSessionPorts {
    session_id: SessionId,
    protocol_id: ProtocolId,
    commands: mpsc::Sender<SessionCommand>,
    events: mpsc::Receiver<SessionEvent>,
    mailbox: Arc<Mutex<FrameMailbox>>,
    video_events: VideoWorkerEvents,
    frame_compiler: FrameTransactionCompiler,
}

impl PendingLiveSessionPorts {
    fn accept(self) -> LiveSessionPorts {
        LiveSessionPorts {
            session_id: self.session_id,
            protocol_id: self.protocol_id,
            commands: self.commands,
            events: self.events,
            mailbox: self.mailbox,
            video_events: self.video_events,
            frame_compiler: FrameTransactionCompiler::new(self.session_id),
        }
    }
}

struct CompiledFrameDrain {
    transactions: Vec<FrameTransaction>,
    metrics: BatchMetricContext,
}

#[derive(Debug)]
struct FrameCompileFailure {
    error: FrameTransactionError,
    metrics: BatchMetricContext,
}

struct LiveSessionCleanup {
    commands: Option<mpsc::Sender<SessionCommand>>,
    protocol_worker: Option<JoinHandle<()>>,
    audio_worker: Option<JoinHandle<()>>,
    video_worker: Option<VideoDecodeWorker>,
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
        if let Some(worker) = &self.video_worker {
            worker.request_stop();
        }
        Ok(())
    }

    fn join_workers_and_audio(&mut self) -> Result<(), CleanupError> {
        let (protocol_pending, protocol_panicked) = poll_worker(&mut self.protocol_worker);
        let (audio_pending, audio_panicked) = poll_worker(&mut self.audio_worker);
        let (video_pending, video_panicked) = poll_video_worker(&mut self.video_worker);
        if protocol_pending
            || audio_pending
            || video_pending
            || protocol_panicked
            || audio_panicked
            || video_panicked
        {
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

fn poll_video_worker(worker: &mut Option<VideoDecodeWorker>) -> (bool, bool) {
    let Some(video) = worker.as_mut() else {
        return (false, false);
    };
    match video.poll_join() {
        Ok(false) => (true, false),
        Ok(true) => {
            worker.take();
            (false, false)
        }
        Err(_) => {
            worker.take();
            (false, true)
        }
    }
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
                self.active = Some(ports.accept());
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

    pub fn drain_video_worker_events(&mut self) -> Vec<(SessionId, VideoWorkerEvent)> {
        let Some(active) = self.active.as_ref() else {
            return Vec::new();
        };
        let mut events = Vec::new();
        while let Some(event) = active.video_events.try_recv() {
            events.push((active.session_id, event));
        }
        events
    }

    pub fn drain_video_admissions(&mut self) -> Vec<(SessionId, VideoStreamAdmission)> {
        let Some(active) = self.active.as_ref() else {
            return Vec::new();
        };
        let mut admissions = Vec::new();
        while let Some(admission) = active.video_events.try_recv_admission() {
            admissions.push((active.session_id, admission));
        }
        admissions
    }

    pub fn active_protocol_id(&self) -> Option<ProtocolId> {
        self.active
            .as_ref()
            .map(|active| active.protocol_id.clone())
    }

    fn active_high_performance_session_id(&self) -> Option<SessionId> {
        self.active.as_ref().and_then(|active| {
            (active.protocol_id.as_str() == APPLE_HIGH_PERFORMANCE_PROTOCOL_ID)
                .then_some(active.session_id)
        })
    }

    pub fn confirm_video_presented(&self, token: &VideoFrameToken) -> Result<(), SessionHostError> {
        self.active
            .as_ref()
            .ok_or(SessionHostError::NoActiveSession)?
            .video_events
            .confirm_presented(token)
            .map_err(|_| SessionHostError::CommandClosed)
    }

    pub fn video_is_ready(&self, identity: VideoStreamIdentity, generation: u64) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.video_events.is_ready(identity, generation))
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

    fn drain_frame_transactions(&mut self) -> Result<CompiledFrameDrain, FrameCompileFailure> {
        let Some(active) = self.active.as_mut() else {
            return Ok(CompiledFrameDrain {
                transactions: Vec::new(),
                metrics: BatchMetricContext {
                    batch_started_at: std::time::Instant::now(),
                    source_update_count: 0,
                    oldest_age: None,
                    transaction_count: 0,
                },
            });
        };
        let pre_call_buffered_count = active.frame_compiler.buffered_source_update_count();
        let pre_call_earliest = active.frame_compiler.earliest_buffered_enqueue_at();
        let updates = {
            let Ok(mut mailbox) = active.mailbox.lock() else {
                return Ok(CompiledFrameDrain {
                    transactions: Vec::new(),
                    metrics: BatchMetricContext {
                        batch_started_at: std::time::Instant::now(),
                        source_update_count: 0,
                        oldest_age: None,
                        transaction_count: 0,
                    },
                });
            };
            let mut updates = Vec::with_capacity(mailbox.len());
            while let Some(update) = mailbox.pop_enqueued() {
                updates.push(update);
            }
            updates
        };
        if updates.is_empty() {
            return Ok(CompiledFrameDrain {
                transactions: Vec::new(),
                metrics: BatchMetricContext {
                    batch_started_at: std::time::Instant::now(),
                    source_update_count: pre_call_buffered_count,
                    oldest_age: None,
                    transaction_count: 0,
                },
            });
        }
        let drained_count = updates.len();
        let current_earliest = updates.iter().map(|entry| entry.enqueued_at).min();
        let batch_started_at = std::time::Instant::now();
        match active.frame_compiler.compile(updates) {
            Ok(transactions) => {
                let source_update_count = if transactions.is_empty() {
                    pre_call_buffered_count.saturating_add(drained_count)
                } else {
                    transactions
                        .iter()
                        .map(FrameTransaction::source_update_count)
                        .sum()
                };
                let earliest = transactions
                    .iter()
                    .map(FrameTransaction::earliest_constituent_enqueue_at)
                    .min();
                let oldest_age =
                    earliest.and_then(|earliest| checked_mailbox_age(batch_started_at, earliest));
                Ok(CompiledFrameDrain {
                    metrics: BatchMetricContext {
                        batch_started_at,
                        source_update_count,
                        oldest_age,
                        transaction_count: transactions.len(),
                    },
                    transactions,
                })
            }
            Err(error) => {
                let earliest = match (pre_call_earliest, current_earliest) {
                    (Some(left), Some(right)) => Some(left.min(right)),
                    (Some(value), None) | (None, Some(value)) => Some(value),
                    (None, None) => None,
                };
                Err(FrameCompileFailure {
                    error,
                    metrics: BatchMetricContext {
                        batch_started_at,
                        source_update_count: pre_call_buffered_count.saturating_add(drained_count),
                        oldest_age: earliest
                            .and_then(|earliest| checked_mailbox_age(batch_started_at, earliest)),
                        transaction_count: 0,
                    },
                })
            }
        }
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
) -> Result<
    (
        LiveSessionCleanup,
        PendingLiveSessionPorts,
        ProtocolStartBarrier,
    ),
    ProtocolError,
> {
    let session_id = request.session_id;
    let protocol_id = request.protocol_id.clone();
    let (command_tx, command_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let mailbox = Arc::new(Mutex::new(FrameMailbox::new(
        FRAME_MAILBOX_ENTRY_LIMIT,
        FRAME_MAILBOX_PIXEL_LIMIT,
    )));
    let video_wake = wake.clone();
    let video_worker = VideoDecodeWorker::spawn(Arc::new(move || {
        let _ = video_wake.wake();
    }))
    .map_err(|_| ProtocolError::Terminal)?;
    let video_sender = video_worker.sender();
    let video_events = video_worker.events();
    let (media_tx, media_rx) = mpsc::sync_channel(MEDIA_MAILBOX_ENTRY_LIMIT);
    let runtime = ProtocolRuntime::new(
        session_id,
        command_rx,
        Box::new(ChannelEventSink(event_tx.clone())),
        Box::new(MailboxSurfacePublisher::new(mailbox.clone())),
        Some(Box::new(DesktopMediaPublisher::new(media_tx, video_sender))),
        Box::new(SharedWake(wake.clone())),
    );
    let session = match factory.create(request, runtime) {
        Ok(session) => session,
        Err(error) => {
            stop_unpublished_video_worker(video_worker);
            return Err(error);
        }
    };
    if cancelled.load(Ordering::Acquire) {
        stop_unpublished_video_worker(video_worker);
        return Err(ProtocolError::Terminal);
    }

    let (audio_start_tx, audio_start_rx) = mpsc::channel();
    let audio_events = event_tx.clone();
    let audio_wake = wake.clone();
    let audio_worker = match worker_spawner.spawn(
        WorkerKind::Audio,
        format!("frd-audio-{}", session_id.get()),
        Box::new(move || {
            if audio_start_rx.recv().is_err() {
                return;
            }
            let degraded = match catch_unwind(AssertUnwindSafe(|| {
                drain_audio_media(audio_factory, media_rx)
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
    ) {
        Ok(worker) => worker,
        Err(_) => {
            stop_unpublished_video_worker(video_worker);
            return Err(ProtocolError::Terminal);
        }
    };
    if cancelled.load(Ordering::Acquire) {
        drop(audio_start_tx);
        let _ = audio_worker.join();
        stop_unpublished_video_worker(video_worker);
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
            stop_unpublished_video_worker(video_worker);
            return Err(ProtocolError::Terminal);
        }
    };
    let _ = audio_start_tx.send(());

    Ok((
        LiveSessionCleanup {
            commands: Some(command_tx.clone()),
            protocol_worker: Some(protocol_worker),
            audio_worker: Some(audio_worker),
            video_worker: Some(video_worker),
            mailbox: Some(mailbox.clone()),
        },
        PendingLiveSessionPorts {
            session_id,
            protocol_id,
            commands: command_tx,
            events: event_rx,
            mailbox,
            video_events,
        },
        start_barrier,
    ))
}

fn stop_unpublished_video_worker(worker: VideoDecodeWorker) {
    worker.request_stop();
    let _ = worker.join_timeout(std::time::Duration::from_secs(1));
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AudioWorkerExit {
    Closed,
    Failed,
}

fn drain_audio_media(
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
            Ok(MediaFrame::VideoConfig(_) | MediaFrame::EncodedVideo(_)) => {}
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
        match frame {
            MediaFrame::Pcm {
                sample_rate_hz,
                channels,
                samples,
            } => {
                if output
                    .enqueue_pcm(sample_rate_hz, channels, samples)
                    .is_err()
                {
                    return AudioWorkerExit::Failed;
                }
            }
            MediaFrame::VideoConfig(_) | MediaFrame::EncodedVideo(_) => {}
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RemoteBinding {
    session_id: SessionId,
    generation: u64,
    size: PixelSize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VideoBinding {
    identity: VideoStreamIdentity,
    generation: u64,
    size: PixelSize,
}

impl VideoBinding {
    fn remote_binding(self) -> RemoteBinding {
        RemoteBinding {
            session_id: self.identity.session_id,
            generation: self.generation,
            size: self.size,
        }
    }
}

fn content_viewport_for_surface(
    remote: PixelSize,
    drawable: PixelSize,
    remote_area: PixelRect,
) -> Option<ContentViewport> {
    ContentViewport::fit_in(remote, drawable, remote_area)
}

fn detach_video_for_matching_failure(
    active: &mut Option<VideoBinding>,
    identity: VideoStreamIdentity,
    generation: u64,
) -> bool {
    if active.is_some_and(|video| video.identity == identity && video.generation == generation) {
        *active = None;
        return true;
    }
    false
}

struct VideoFailureAction {
    command: SessionCommand,
    error: ProtocolError,
}

fn video_decode_terminal_code(code: VideoDecodeErrorCode) -> &'static str {
    match code {
        VideoDecodeErrorCode::BackendUnavailable => "video_backend_unavailable",
        VideoDecodeErrorCode::ExactProfileChromaBitDepthUnsupported => {
            "video_exact_profile_chroma_bit_depth_unsupported"
        }
        VideoDecodeErrorCode::OutputFormatUnsupported => "video_output_format_unsupported",
        VideoDecodeErrorCode::DecoderCreationFailed => "video_decoder_creation_failed",
        VideoDecodeErrorCode::MalformedOrOverBudgetAccessUnit => {
            "video_malformed_or_over_budget_access_unit"
        }
        VideoDecodeErrorCode::StaleStreamOrGeneration => "video_stale_stream_or_generation",
        VideoDecodeErrorCode::DecodeFailedBeforeFirstFrame => {
            "video_decode_failed_before_first_frame"
        }
        VideoDecodeErrorCode::DecodeFailedAfterFirstFrame => {
            "video_decode_failed_after_first_frame"
        }
        VideoDecodeErrorCode::DecodedFrameLayoutInvalid => "video_decoded_frame_layout_invalid",
        VideoDecodeErrorCode::FramePublicationFailed => "video_frame_publication_failed",
        VideoDecodeErrorCode::BackendVersionMismatch => "video_backend_version_mismatch",
    }
}

fn admit_video_surface_owner(
    owner: &mut Option<VideoStreamEpoch>,
    admission: VideoStreamAdmission,
) -> bool {
    let candidate = VideoStreamEpoch {
        identity: admission.identity,
        generation: admission.generation,
    };
    match *owner {
        None => {
            *owner = Some(candidate);
            true
        }
        Some(current) if current.identity == candidate.identity => {
            if candidate.generation < current.generation {
                return false;
            }
            *owner = Some(candidate);
            true
        }
        Some(_) => false,
    }
}

fn video_surface_owner_matches(
    owner: Option<VideoStreamEpoch>,
    identity: VideoStreamIdentity,
    generation: u64,
) -> bool {
    owner
        == Some(VideoStreamEpoch {
            identity,
            generation,
        })
}

#[derive(Default)]
struct OwnedVideoStageTrace {
    owner: Option<VideoStreamEpoch>,
    trace: MediaStageTrace,
}

impl OwnedVideoStageTrace {
    fn admit(&mut self, owner: VideoStreamEpoch) {
        if self.owner == Some(owner) {
            return;
        }
        self.owner = Some(owner);
        self.trace = MediaStageTrace::default();
    }

    fn observe_uploaded(&mut self, owner: VideoStreamEpoch, size: PixelSize) -> bool {
        if self.owner != Some(owner) {
            return false;
        }
        self.trace.observe(MediaStageDiagnostic::FrameUploaded {
            generation: owner.generation,
            stream_id: owner.identity.stream_id,
            width: size.width,
            height: size.height,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VideoSurfaceOwnerTransition {
    WorkerStopped,
    CleanupStarted,
    CleanupComplete,
}

fn transition_video_surface_owner(
    owner: &mut Option<VideoStreamEpoch>,
    transition: VideoSurfaceOwnerTransition,
) {
    match transition {
        VideoSurfaceOwnerTransition::WorkerStopped
        | VideoSurfaceOwnerTransition::CleanupStarted => {}
        VideoSurfaceOwnerTransition::CleanupComplete => *owner = None,
    }
}

trait VideoSurfaceDrainTarget {
    fn video_owner(&self) -> Option<VideoStreamEpoch>;
    fn video_owner_mut(&mut self) -> &mut Option<VideoStreamEpoch>;
    fn active_video_mut(&mut self) -> &mut Option<VideoBinding>;
    fn reset_for_admission(&mut self, epoch: VideoStreamEpoch);
    fn configure_video_stream(&mut self, epoch: VideoStreamEpoch)
        -> Result<(), VideoRendererError>;
    fn upload_video_frame(
        &mut self,
        token: VideoFrameToken,
        frame: DecodedVideoFrame,
    ) -> Result<(), VideoRendererError>;
    fn detach_after_video_failure(&mut self);
    fn video_worker_stopped(&mut self);
}

#[derive(Default)]
struct VideoSurfaceDrainOutcome {
    ui_redraw_needed: bool,
    frame_redraw_needed: bool,
    failure: Option<(SessionId, VideoFailureAction)>,
}

fn drain_video_surface_events<T: VideoSurfaceDrainTarget>(
    target: &mut T,
    admissions: Vec<(SessionId, VideoStreamAdmission)>,
    events: Vec<(SessionId, VideoWorkerEvent)>,
    active_protocol_id: Option<ProtocolId>,
) -> Result<VideoSurfaceDrainOutcome, FatalReport> {
    let mut outcome = VideoSurfaceDrainOutcome {
        ui_redraw_needed: !events.is_empty(),
        ..VideoSurfaceDrainOutcome::default()
    };
    for (session_id, admission) in admissions {
        if admission.identity.session_id != session_id {
            continue;
        }
        let epoch = VideoStreamEpoch {
            identity: admission.identity,
            generation: admission.generation,
        };
        if admit_video_surface_owner(target.video_owner_mut(), admission) {
            target.reset_for_admission(epoch);
        }
    }

    for (session_id, event) in events {
        match event {
            VideoWorkerEvent::BackendSelected {
                identity,
                generation,
                ..
            } if identity.session_id == session_id => {
                let epoch = VideoStreamEpoch {
                    identity,
                    generation,
                };
                if target.video_owner() != Some(epoch) {
                    continue;
                }
                match target.configure_video_stream(epoch) {
                    Ok(()) | Err(VideoRendererError::StaleStreamOrGeneration) => {}
                    Err(error) => {
                        return Err(FatalReport::presentation(
                            PresentationOperation::Redraw,
                            PresentError::from(error),
                            None,
                            None,
                        ));
                    }
                }
            }
            VideoWorkerEvent::FrameDecoded(handoff) => {
                let (token, frame) = handoff.into_parts();
                let input = frame.as_input();
                if token.identity().session_id != session_id
                    || token.identity() != input.identity
                    || token.generation() != input.generation
                    || token.timestamp() != input.timestamp
                    || !video_surface_owner_matches(
                        target.video_owner(),
                        token.identity(),
                        token.generation(),
                    )
                {
                    continue;
                }
                match target.upload_video_frame(token, frame) {
                    Ok(()) => outcome.frame_redraw_needed = true,
                    Err(VideoRendererError::StaleStreamOrGeneration) => {}
                    Err(error) => {
                        return Err(FatalReport::presentation(
                            PresentationOperation::Redraw,
                            PresentError::from(error),
                            None,
                            None,
                        ));
                    }
                }
            }
            VideoWorkerEvent::DecodeFailed {
                identity,
                generation,
                code,
                ..
            } => {
                let Some(protocol_id) = active_protocol_id.clone() else {
                    continue;
                };
                let disconnect = disconnect_after_matching_video_failure(
                    target.video_owner(),
                    target.active_video_mut(),
                    identity,
                    generation,
                    code,
                    protocol_id,
                );
                if disconnect.is_some() {
                    target.detach_after_video_failure();
                    outcome.failure = disconnect.map(|action| (session_id, action));
                }
            }
            VideoWorkerEvent::Stopped => target.video_worker_stopped(),
            VideoWorkerEvent::BackendSelected { .. } => {}
        }
    }
    Ok(outcome)
}

fn disconnect_after_matching_video_failure(
    configured: Option<VideoStreamEpoch>,
    active: &mut Option<VideoBinding>,
    identity: VideoStreamIdentity,
    generation: u64,
    code: VideoDecodeErrorCode,
    protocol_id: ProtocolId,
) -> Option<VideoFailureAction> {
    if !video_surface_owner_matches(configured, identity, generation) {
        return None;
    }
    let _ = detach_video_for_matching_failure(active, identity, generation);
    Some(VideoFailureAction {
        command: SessionCommand::Disconnect,
        error: ProtocolError::adapter(protocol_id, video_decode_terminal_code(code)),
    })
}

enum WindowPresentation {
    Pixel(Option<frd_protocol_api::PresentationEvent>),
    Video(Option<VideoPresentationReceipt>),
}

fn take_exact_presented_value<R: PartialEq, T>(
    pending: &mut Option<(R, T)>,
    presented: &R,
) -> Option<T> {
    if pending
        .as_ref()
        .is_some_and(|(receipt, _)| receipt == presented)
    {
        return pending.take().map(|(_, value)| value);
    }
    None
}

fn video_ready_event_after_confirmation<T, E>(
    candidate: Option<T>,
    confirm: impl FnOnce(&T) -> Result<(), E>,
    event: impl FnOnce(&T) -> frd_protocol_api::PresentationEvent,
) -> Option<frd_protocol_api::PresentationEvent> {
    let candidate = candidate?;
    confirm(&candidate).ok()?;
    Some(event(&candidate))
}

fn apply_compiled_drain(
    transactions: Vec<FrameTransaction>,
    apply: impl FnOnce(Vec<FrameTransaction>) -> Result<BatchApplySuccess, BatchApplyFailure>,
) -> Result<BatchApplySuccess, BatchApplyFailure> {
    debug_assert!(!transactions.is_empty());
    apply(transactions)
}

fn accept_batch_outcome(
    remote: &mut Option<RemoteBinding>,
    pending_texture_writes: &mut PendingTextureWrites,
    outcome: &BatchApplyOutcome,
) -> bool {
    if let Some(installed) = outcome.installed_surface {
        *remote = Some(RemoteBinding {
            session_id: installed.session_id,
            generation: installed.generation,
            size: installed.size,
        });
    }
    pending_texture_writes.record_batch(outcome.had_texture_writes);
    outcome.final_boundary.is_some_and(|boundary| {
        remote.is_some_and(|binding| {
            binding.session_id == boundary.session_id && binding.generation == boundary.generation
        })
    })
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RuntimeDrainOutcome {
    ui_redraw_needed: bool,
    frame_redraw_needed: bool,
}

impl RuntimeDrainOutcome {
    fn any_redraw(self) -> bool {
        self.ui_redraw_needed || self.frame_redraw_needed
    }
}

enum FrameDrainFailure {
    Compile(FrameTransactionError),
    Render(BatchApplyFailure),
}

trait FrameBatchFailureTarget {
    fn block_remote_input(&mut self);
    fn detach_remote_surface(&mut self);
    fn clear_pending_texture_writes(&mut self);
}

fn terminate_failed_frame_batch(
    target: &mut impl FrameBatchFailureTarget,
    failure: FrameDrainFailure,
) -> FatalReport {
    target.block_remote_input();
    target.detach_remote_surface();
    target.clear_pending_texture_writes();
    match failure {
        FrameDrainFailure::Compile(error) => FatalReport::frame_transaction(error),
        FrameDrainFailure::Render(failure) => FatalReport::frame_batch(&failure),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeDrainCallback {
    Wake,
    RedrawRequested,
}

trait RuntimeDrainCallbackTarget {
    fn request_redraw(&mut self);
    fn render_now(&mut self);
    fn terminate(&mut self, report: FatalReport);
}

fn dispatch_runtime_drain(
    callback: RuntimeDrainCallback,
    result: Result<RuntimeDrainOutcome, FatalReport>,
    target: &mut impl RuntimeDrainCallbackTarget,
) {
    match (callback, result) {
        (_, Err(report)) => target.terminate(report),
        (RuntimeDrainCallback::Wake, Ok(outcome)) if outcome.any_redraw() => {
            target.request_redraw();
        }
        (RuntimeDrainCallback::Wake, Ok(_)) => {}
        (RuntimeDrainCallback::RedrawRequested, Ok(_)) => target.render_now(),
    }
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
    fn record_batch(&mut self, had_texture_writes: bool) {
        self.pending |= had_texture_writes;
    }

    fn clear(&mut self) {
        self.pending = false;
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
    video_renderer: VideoRenderer,
    compositor: PresentationCompositor,
    egui_context: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    physical_size: PixelSize,
    remote_area: Option<PixelRect>,
    chrome_layouts: Option<ChromeLayouts>,
    chrome_hit_map: Option<ChromeHitMap>,
    cursor_position: Option<(u32, u32)>,
    lifecycle: PresentationLifecycle,
    remote: Option<RemoteBinding>,
    video_owner: Option<VideoStreamEpoch>,
    video: Option<VideoBinding>,
    pending_video: Option<(VideoPresentationReceipt, VideoFrameToken)>,
    video_stage_trace: OwnedVideoStageTrace,
    dpi_transition: DpiTransition,
    pending_texture_writes: PendingTextureWrites,
    focus_session_chrome: bool,
}

impl DesktopWindowState {
    fn refresh_chrome_geometry(&mut self) -> Option<ChromeLayouts> {
        let insets = self.chrome.native_insets(&self.window);
        let remote_area =
            persistent_session_panel_content_rect(self.physical_size, self.window.scale_factor())?;
        let layouts = ChromeGeometrySnapshot::new(
            self.physical_size.width,
            self.physical_size.height,
            self.window.scale_factor(),
            insets,
        )?
        .with_window_capabilities(self.chrome.capabilities())
        .layouts(ControlIslandPlacement::default(), true)?;
        self.remote_area = Some(remote_area);
        self.chrome_layouts = Some(layouts.clone());
        Some(layouts)
    }
}

fn persistent_session_panel_content_rect(
    physical_size: PixelSize,
    scale_factor: f64,
) -> Option<PixelRect> {
    if !scale_factor.is_finite() || scale_factor <= 0.0 {
        return None;
    }
    let panel_height = (TITLE_BAR_HEIGHT_POINTS * scale_factor).ceil();
    if !panel_height.is_finite()
        || panel_height <= 0.0
        || panel_height >= f64::from(physical_size.height)
    {
        return None;
    }
    let panel_height = panel_height as u32;
    Some(PixelRect {
        x: 0,
        y: panel_height,
        width: physical_size.width,
        height: physical_size.height - panel_height,
    })
}

impl VideoSurfaceDrainTarget for DesktopWindowState {
    fn video_owner(&self) -> Option<VideoStreamEpoch> {
        self.video_owner
    }

    fn video_owner_mut(&mut self) -> &mut Option<VideoStreamEpoch> {
        &mut self.video_owner
    }

    fn active_video_mut(&mut self) -> &mut Option<VideoBinding> {
        &mut self.video
    }

    fn reset_for_admission(&mut self, epoch: VideoStreamEpoch) {
        self.video_renderer.detach();
        self.video = None;
        self.pending_video = None;
        self.video_stage_trace.admit(epoch);
    }

    fn configure_video_stream(
        &mut self,
        epoch: VideoStreamEpoch,
    ) -> Result<(), VideoRendererError> {
        self.video_renderer.configure_stream(epoch)?;
        self.video = None;
        self.pending_video = None;
        Ok(())
    }

    fn upload_video_frame(
        &mut self,
        token: VideoFrameToken,
        frame: DecodedVideoFrame,
    ) -> Result<(), VideoRendererError> {
        let upload = self.video_renderer.upload_frame(frame)?;
        self.video_stage_trace.observe_uploaded(
            VideoStreamEpoch {
                identity: token.identity(),
                generation: token.generation(),
            },
            upload.layout.visible_size(),
        );
        self.video = Some(VideoBinding {
            identity: token.identity(),
            generation: token.generation(),
            size: upload.layout.visible_size(),
        });
        self.pending_video = Some((upload.receipt, token));
        self.pending_texture_writes.record_batch(true);
        Ok(())
    }

    fn detach_after_video_failure(&mut self) {
        self.video_renderer.detach();
        self.pending_video = None;
    }

    fn video_worker_stopped(&mut self) {
        self.video_renderer.detach();
        transition_video_surface_owner(
            &mut self.video_owner,
            VideoSurfaceOwnerTransition::WorkerStopped,
        );
        self.video = None;
        self.pending_video = None;
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
    hp_input_diagnostics: HighPerformanceInputShellDiagnostics,
    metrics: FramePipelineMetrics,
    presentation_timing: PresentationTimingTracker,
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

struct ApplicationDrainCallbacks<'a> {
    application: &'a mut DesktopApplication,
    event_loop: &'a ActiveEventLoop,
}

impl<'a> ApplicationDrainCallbacks<'a> {
    fn new(application: &'a mut DesktopApplication, event_loop: &'a ActiveEventLoop) -> Self {
        Self {
            application,
            event_loop,
        }
    }
}

impl RuntimeDrainCallbackTarget for ApplicationDrainCallbacks<'_> {
    fn request_redraw(&mut self) {
        self.application.request_redraw();
    }

    fn render_now(&mut self) {
        self.application.render();
    }

    fn terminate(&mut self, report: FatalReport) {
        self.application
            .handle_application_fatal(self.event_loop, report);
    }
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

impl FrameBatchFailureTarget for DesktopApplication {
    fn block_remote_input(&mut self) {
        self.block_and_release_input_for_fatal();
    }

    fn detach_remote_surface(&mut self) {
        if let Some(window) = self.window.as_mut() {
            window.renderer.detach();
            window.video_renderer.detach();
            window.remote = None;
            window.video_owner = None;
            window.video = None;
            window.pending_video = None;
        }
    }

    fn clear_pending_texture_writes(&mut self) {
        if let Some(window) = self.window.as_mut() {
            window.pending_texture_writes.clear();
        }
    }
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
            hp_input_diagnostics: HighPerformanceInputShellDiagnostics::from_environment(),
            metrics,
            presentation_timing: PresentationTimingTracker::default(),
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
            hp_input_diagnostics: HighPerformanceInputShellDiagnostics::from_environment(),
            metrics: FramePipelineMetrics::disabled(),
            presentation_timing: PresentationTimingTracker::default(),
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
        let video_renderer = VideoRenderer::new(gpu.clone()).map_err(|_| {
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
            video_renderer,
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
            chrome_layouts: None,
            chrome_hit_map: None,
            cursor_position: None,
            lifecycle: PresentationLifecycle::new(physical_size),
            remote: None,
            video_owner: None,
            video: None,
            pending_video: None,
            video_stage_trace: OwnedVideoStageTrace::default(),
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

    fn drain_runtime(&mut self) -> Result<RuntimeDrainOutcome, FatalReport> {
        let events = self.sessions.drain_session_events();
        let mut outcome = RuntimeDrainOutcome {
            ui_redraw_needed: !events.is_empty(),
            frame_redraw_needed: false,
        };
        let mut cleanup_needed = false;
        let mut detach_remote = false;
        for (session_id, event) in events {
            match &event {
                SessionEvent::SurfaceGenerationChanged {
                    session_id,
                    generation,
                    ..
                } => {
                    self.metrics.observe_generation(*session_id, *generation);
                    self.presentation_timing.reset();
                }
                SessionEvent::FrameResponseTiming(timing) => {
                    self.metrics.observe_frame_response_timing(*timing)
                }
                SessionEvent::StageChanged(frd_protocol_api::ConnectionStage::Disconnecting)
                | SessionEvent::Error(_)
                | SessionEvent::Closed(_) => {
                    self.metrics.clear_input_probe();
                    self.presentation_timing.reset();
                }
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

        let video_admissions = self.sessions.drain_video_admissions();
        if !video_admissions.is_empty() {
            self.presentation_timing.reset();
            self.launch.controller_mut().clear_presentation_timing();
        }
        let video_events = self.sessions.drain_video_worker_events();
        let active_protocol_id = self.sessions.active_protocol_id();
        let video_drain = if let Some(window) = self.window.as_mut() {
            drain_video_surface_events(window, video_admissions, video_events, active_protocol_id)?
        } else {
            VideoSurfaceDrainOutcome {
                ui_redraw_needed: !video_events.is_empty(),
                ..VideoSurfaceDrainOutcome::default()
            }
        };
        outcome.ui_redraw_needed |= video_drain.ui_redraw_needed;
        outcome.frame_redraw_needed |= video_drain.frame_redraw_needed;
        let video_failure = video_drain.failure;
        if let Some((session_id, action)) = video_failure {
            self.block_and_release_input();
            self.launch
                .controller_mut()
                .handle_session_event_with_stores(
                    session_id,
                    SessionEvent::Error(action.error),
                    self.stores.as_app_stores(),
                );
            let _ = self.sessions.send_command(action.command);
            detach_remote = true;
            cleanup_needed = true;
        }

        if detach_remote {
            self.metrics.detach();
            if let Some(window) = self.window.as_mut() {
                window.renderer.detach();
                window.video_renderer.detach();
                window.remote = None;
                transition_video_surface_owner(
                    &mut window.video_owner,
                    VideoSurfaceOwnerTransition::CleanupStarted,
                );
                window.video = None;
                window.pending_video = None;
                window.pending_texture_writes.clear();
            }
        }
        if cleanup_needed {
            self.start_background_cleanup();
        }

        // SessionEvent is deliberately reduced before frame mailbox data.
        let drain = match self.sessions.drain_frame_transactions() {
            Ok(drain) => drain,
            Err(failure) => {
                self.metrics
                    .observe_compile_failure(failure.metrics, failure.error);
                return Err(terminate_failed_frame_batch(
                    self,
                    FrameDrainFailure::Compile(failure.error),
                ));
            }
        };
        if drain.transactions.is_empty() {
            self.publish_metrics_failure();
            return Ok(outcome);
        }
        let metrics = drain.metrics;
        let Some(window) = self.window.as_mut() else {
            self.publish_metrics_failure();
            return Ok(outcome);
        };
        let applied = apply_compiled_drain(drain.transactions, |transactions| {
            window.renderer.apply_update_batch(transactions)
        });
        let success = match applied {
            Ok(success) => success,
            Err(failure) => {
                self.metrics.observe_batch_failure(metrics, &failure);
                return Err(terminate_failed_frame_batch(
                    self,
                    FrameDrainFailure::Render(failure),
                ));
            }
        };
        self.metrics
            .observe_batch_success(metrics, &success.outcome, &success.scope);
        let window = self
            .window
            .as_mut()
            .expect("a successful renderer batch retains the window");
        outcome.frame_redraw_needed = accept_batch_outcome(
            &mut window.remote,
            &mut window.pending_texture_writes,
            &success.outcome,
        );
        if window.pending_texture_writes.take_for_blocked_present(
            window.lifecycle.accepts_redraw(),
            window.dpi_transition.is_pending(),
        ) {
            let _ = window.gpu.queue().submit(std::iter::empty());
        }
        self.publish_metrics_failure();
        Ok(outcome)
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
        self.send_input_with_hp_diagnostics(event, None);
    }

    fn send_input_with_hp_diagnostics(
        &mut self,
        event: frd_core::InputEvent,
        hp_session_id: Option<SessionId>,
    ) {
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
                    if let Some(session_id) = hp_session_id {
                        self.hp_input_diagnostics.observe_enqueued(session_id);
                    }
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
        let remote = if let Some(video) = window.video {
            video.remote_binding()
        } else {
            window.remote?
        };
        content_viewport_for_surface(remote.size, window.physical_size, window.remote_area?)
    }

    fn send_viewport_changed(&self) {
        let Some((session_id, generation)) = self.input.interactive_epoch() else {
            return;
        };
        let Some(remote) = self.window.as_ref().and_then(|window| {
            window
                .video
                .map(VideoBinding::remote_binding)
                .or(window.remote)
        }) else {
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
        let Some(chrome_layouts) = window.refresh_chrome_geometry() else {
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
        let mut window_command = None;
        let mut rendered_hit_rects = Vec::new();
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
                        let window_capabilities = window.chrome.capabilities();
                        let scale = window.window.scale_factor() as f32;
                        let to_logical = |rect: ChromeRect| {
                            egui::Rect::from_min_size(
                                egui::pos2(rect.x as f32 / scale, rect.y as f32 / scale),
                                egui::vec2(rect.width as f32 / scale, rect.height as f32 / scale),
                            )
                        };
                        let island_rect = chrome_layouts
                            .overlay
                            .island_rect
                            .map(to_logical)
                            .expect("visible floating chrome geometry includes the island");
                        let reveal_line_rect = to_logical(chrome_layouts.overlay.reveal_line_rect);
                        let result = frd_ui_egui::show_control_island(
                            ui.ctx(),
                            frd_ui_egui::ControlIslandRenderInput {
                                model: chrome,
                                window_capabilities,
                                visible: true,
                                maximized: window_maximized,
                                island_rect,
                                reveal_line_rect,
                                focus_first: focus_session_chrome,
                                opaque_material: false,
                            },
                        );
                        rendered_hit_rects = result.hit_rects;
                        if result.window_move_requested {
                            window_command = Some(WindowChromeCommand::BeginMove);
                        }
                        match result.action {
                            Some(IslandAction::CancelConnect) => {
                                intent = Some(AppIntent::CancelConnect);
                            }
                            Some(IslandAction::Disconnect) => {
                                intent = Some(AppIntent::Disconnect);
                            }
                            Some(action) => {
                                window_command = window_command_for_island_action(action);
                            }
                            None => {}
                        }
                    }
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
        let scale_factor = window.window.scale_factor();
        let island_actions = rendered_hit_rects
            .into_iter()
            .map(|(rect, action)| {
                logical_rect_to_physical(rect, scale_factor).map(|rect| (rect, action))
            })
            .collect::<Option<Vec<_>>>();
        if let Some(hit_map) = island_actions.and_then(|island_actions| {
            ChromeHitMap::candidate(
                chrome_layouts.remote.content_rect,
                island_actions,
                chrome_layouts.overlay.island_reposition_handle,
                chrome_layouts.overlay.window_move_region,
                Vec::new(),
            )
        }) {
            window.chrome.publish_hit_map(hit_map.clone());
            window.chrome_hit_map = Some(hit_map);
        }
        if let Some(command) = window_command {
            if let Err(error) = window.chrome.execute(&window.window, command) {
                eprintln!(
                    "窗口操作失败（FRD-WIN-SHELL-002: window_chrome_command_failed）：{error:?}"
                );
            }
        }
        window
            .egui_state
            .handle_platform_output(&window.window, output.platform_output);
        let paint_jobs = egui_context.tessellate(output.shapes, output.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [window.physical_size.width, window.physical_size.height],
            pixels_per_point: output.pixels_per_point,
        };
        let remote_viewport = window.remote.and_then(|remote| {
            content_viewport_for_surface(remote.size, window.physical_size, window.remote_area?)
        });
        let video_viewport = window.video.and_then(|video| {
            content_viewport_for_surface(video.size, window.physical_size, window.remote_area?)
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
        // 重绘热路径只借用 GPU 上下文；compositor、renderer 与 egui renderer
        // 是互不重叠的字段，不需要为闭包延长所有权而复制整组 wgpu handle。
        let gpu = &window.gpu;
        let egui_renderer = &mut window.egui_renderer;
        let overlay = |encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView| {
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
        };
        let render_result =
            if video_viewport.is_some() && window.video_renderer.frame_layout().is_some() {
                window
                    .compositor
                    .render_video_in(&mut window.video_renderer, video_viewport, overlay, &hook)
                    .map(WindowPresentation::Video)
            } else {
                window
                    .compositor
                    .render_in(&mut window.renderer, remote_viewport, overlay, &hook)
                    .map(WindowPresentation::Pixel)
            };
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

        let mut video_token_to_confirm = None;
        let video_presented_egui_context = window.egui_context.clone();
        let presentation_error = match render_result {
            Ok(WindowPresentation::Pixel(Some(event))) => {
                let (session_id, generation, revision, completeness) = match &event {
                    frd_protocol_api::PresentationEvent::FramePresented {
                        session_id,
                        generation,
                        revision,
                        completeness,
                        ..
                    } => (*session_id, *generation, *revision, *completeness),
                    frd_protocol_api::PresentationEvent::Timing(_) => {
                        unreachable!("pixel renderer only publishes frame-presented events")
                    }
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
            Ok(WindowPresentation::Video(Some(receipt))) => {
                if let Some(token) = take_exact_presented_value(&mut window.pending_video, &receipt)
                {
                    video_token_to_confirm = Some(token);
                    None
                } else {
                    Some(PresentError::Renderer(
                        frd_render_wgpu::RendererError::StalePresentationReceipt,
                    ))
                }
            }
            Ok(WindowPresentation::Pixel(None) | WindowPresentation::Video(None)) => None,
            Err(error) => Some(error),
        };
        let mut video_timing = None;
        let video_presentation = video_ready_event_after_confirmation(
            video_token_to_confirm,
            |token| {
                // worker 可能在 present 前已发布更新的 latest-frame token；此时精确确认
                // 必须被拒绝，但它是正常背压竞争，不是 surface/GPU 致命错误。
                self.sessions
                    .confirm_video_presented(token)
                    .map_err(|_| ())?;
                self.sessions
                    .video_is_ready(token.identity(), token.generation())
                    .then_some(())
                    .ok_or(())?;
                if let Some(local_ingress_at) = token.local_ingress_at() {
                    video_timing = self.presentation_timing.observe(
                        PresentationTimingKey {
                            identity: token.identity(),
                            generation: token.generation(),
                            worker_epoch_serial: token.worker_epoch_serial(),
                        },
                        local_ingress_at,
                        std::time::Instant::now(),
                    );
                }
                Ok::<(), ()>(())
            },
            |token| frd_protocol_api::PresentationEvent::FramePresented {
                session_id: token.identity().session_id,
                generation: token.generation(),
                revision: token.publication_id(),
                completeness: FrameCompleteness::FullBaseline,
            },
        );
        if let Some(event) = video_presentation {
            let frd_protocol_api::PresentationEvent::FramePresented {
                session_id,
                generation,
                revision,
                completeness,
            } = event.clone()
            else {
                unreachable!("video readiness only creates frame-presented events")
            };
            debug_assert_eq!(completeness, FrameCompleteness::FullBaseline);
            self.metrics.observe_full_baseline_presented(
                MetricIdentity {
                    session_id,
                    generation,
                },
                revision,
                std::time::Instant::now(),
            );
            self.launch.controller_mut().handle_presentation(event);
            if matches!(
                self.launch.controller().page(),
                AppPage::RemoteSession { .. }
            ) {
                if let Some(release) = self.input.set_gate(InputGate::Interactive {
                    session_id,
                    generation,
                }) {
                    self.send_input(release);
                }
                video_presented_egui_context.memory_mut(|memory| memory.stop_text_input());
                self.send_viewport_changed();
                self.request_redraw();
            }
        }
        if let Some(timing) = video_timing {
            self.launch
                .controller_mut()
                .handle_presentation(frd_protocol_api::PresentationEvent::Timing(timing));
            self.request_redraw();
        }
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
                if let Some(window) = self.window.as_mut() {
                    match window
                        .renderer
                        .apply_update_batch(test_texture_transactions(*session_id))
                    {
                        Ok(success) => {
                            let _ = accept_batch_outcome(
                                &mut window.remote,
                                &mut window.pending_texture_writes,
                                &success.outcome,
                            );
                        }
                        Err(failure) => {
                            let _ = self.proxy.send_event(DesktopUserEvent::ApplicationFatal(
                                FatalReport::frame_batch(&failure),
                            ));
                            return;
                        }
                    }
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
                    .map(|window| {
                        window
                            .renderer
                            .apply_update_batch(test_texture_transactions(session_id))
                    });
                match restore {
                    Ok(Ok(batch)) => {
                        let window = self
                            .window
                            .as_mut()
                            .expect("offline recovery retains its window");
                        let _ = accept_batch_outcome(
                            &mut window.remote,
                            &mut window.pending_texture_writes,
                            &batch.outcome,
                        );
                    }
                    Ok(Err(failure)) => {
                        let _ = self.proxy.send_event(DesktopUserEvent::ApplicationFatal(
                            FatalReport::frame_batch(&failure),
                        ));
                        return;
                    }
                    Err(recovery) => {
                        self.publish_presentation_fatal(PresentationRecoveryFailure {
                            operation: context.operation(),
                            source,
                            retry: None,
                            recovery: Some(recovery),
                        });
                        return;
                    }
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
                    let hp_session_id = (self.hp_input_diagnostics.is_enabled()
                        && matches!(input, frd_core::InputEvent::PhysicalKey { .. }))
                    .then(|| self.sessions.active_high_performance_session_id())
                    .flatten();
                    if let Some(session_id) = hp_session_id {
                        self.hp_input_diagnostics.observe_accepted(session_id);
                    }
                    self.send_input_with_hp_diagnostics(input, hp_session_id);
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
        let content_viewport = self.content_viewport();
        let ownership = self.window.as_ref().and_then(|window| {
            effective_pointer_keyboard_ownership(
                window.chrome_hit_map.as_ref()?,
                content_viewport,
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
                if let Some(window) = self.window.as_mut() {
                    transition_video_surface_owner(
                        &mut window.video_owner,
                        VideoSurfaceOwnerTransition::CleanupComplete,
                    );
                }
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
                let result = self.drain_runtime();
                let mut callbacks = ApplicationDrainCallbacks::new(self, event_loop);
                dispatch_runtime_drain(RuntimeDrainCallback::Wake, result, &mut callbacks);
                drop(callbacks);
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
        schedule_egui_repaint(&event, response.repaint, || window.window.request_redraw());
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
                let result = self.drain_runtime();
                let mut callbacks = ApplicationDrainCallbacks::new(self, event_loop);
                dispatch_runtime_drain(
                    RuntimeDrainCallback::RedrawRequested,
                    result,
                    &mut callbacks,
                );
                drop(callbacks);
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
        self.window
            .video_renderer
            .recover_device(recovered.1.clone())?;
        self.window.video = None;
        self.window.pending_video = None;
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

fn logical_rect_to_physical(rect: egui::Rect, scale_factor: f64) -> Option<ChromeRect> {
    if !rect.is_finite() || !scale_factor.is_finite() || scale_factor <= 0.0 {
        return None;
    }
    let scale = scale_factor as f32;
    let min_x = (rect.min.x * scale).floor();
    let min_y = (rect.min.y * scale).floor();
    let max_x = (rect.max.x * scale).ceil();
    let max_y = (rect.max.y * scale).ceil();
    if min_x < 0.0
        || min_y < 0.0
        || max_x <= min_x
        || max_y <= min_y
        || max_x > u32::MAX as f32
        || max_y > u32::MAX as f32
    {
        return None;
    }
    Some(ChromeRect {
        x: min_x as u32,
        y: min_y as u32,
        width: (max_x - min_x) as u32,
        height: (max_y - min_y) as u32,
    })
}

fn window_command_for_island_action(action: IslandAction) -> Option<WindowChromeCommand> {
    match action {
        IslandAction::MinimizeWindow => Some(WindowChromeCommand::Minimize),
        IslandAction::ToggleMaximizeWindow => Some(WindowChromeCommand::ToggleMaximize),
        IslandAction::CloseWindow => Some(WindowChromeCommand::Close),
        IslandAction::ShowSystemMenu => Some(WindowChromeCommand::ShowSystemMenu),
        IslandAction::ShowConnectionDetails
        | IslandAction::CancelConnect
        | IslandAction::Disconnect
        | IslandAction::ToggleRemoteAudio
        | IslandAction::OpenClipboard => None,
    }
}

fn pointer_keyboard_ownership(
    hit_map: &ChromeHitMap,
    content_viewport: Option<ContentViewport>,
    position: (u32, u32),
) -> Option<InputOwnership> {
    match hit_map.hit_test(position) {
        Some(ChromeHitTarget::IslandAction(_)) => Some(InputOwnership::Ui),
        Some(ChromeHitTarget::RemoteContent) => {
            let rect = content_viewport?.content;
            let in_content = position.0 >= rect.x
                && position.1 >= rect.y
                && position.0 < rect.x.saturating_add(rect.width)
                && position.1 < rect.y.saturating_add(rect.height);
            Some(if in_content {
                InputOwnership::Remote
            } else {
                InputOwnership::Ui
            })
        }
        Some(
            ChromeHitTarget::IslandRepositionHandle
            | ChromeHitTarget::WindowMoveRegion
            | ChromeHitTarget::NativeChrome,
        )
        | None => None,
    }
}

fn effective_pointer_keyboard_ownership(
    hit_map: &ChromeHitMap,
    content_viewport: Option<ContentViewport>,
    position: (u32, u32),
    consumed_by_egui: bool,
    interactive: bool,
) -> Option<InputOwnership> {
    match pointer_keyboard_ownership(hit_map, content_viewport, position) {
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

fn test_texture_transactions(session_id: SessionId) -> Vec<FrameTransaction> {
    vec![FrameTransaction::Startup {
        earliest_constituent_enqueue_at: std::time::Instant::now(),
        reset: FrameReset {
            session_id,
            generation: 1,
            size: PixelSize::new(2, 2).expect("test texture size is non-zero"),
            format: PixelFormat::Bgrx8UnormSrgb,
        },
        revision: FrameRevision {
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
            completeness: FrameCompleteness::FullBaseline,
        },
    }]
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
    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use frd_app::{
        AppAction, AppController, AppIntent, AppPlatformStores, PresentationEvent, ProductPolicy,
    };
    use frd_core::{
        ContentViewport, Endpoint, InputEvent, KeyState, Modifiers, PhysicalKeyCode, PixelRect,
        PixelSize, ProtocolId, SecretBuffer, SessionId, TargetSystem,
    };
    use frd_frame::{
        EnqueuedSurfaceUpdate, FrameCompleteness, FrameReset, FrameRevision, FrameTransaction,
        FrameTransactionError, PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate,
    };
    use frd_media_api::{
        AudioOutput, AudioOutputError, ChromaFormat, ChromaLocation, DecodeOutcome,
        DecodedVideoFrame, DecodedVideoFrameInput, EncodedVideoAccessUnit, MediaFrame,
        MediaPublisher, VideoBackendAvailability, VideoBackendId, VideoBackendKind,
        VideoBitstreamFormat, VideoCapabilityProvider, VideoCodec, VideoColorimetry,
        VideoDecodeCapability, VideoDecodeError, VideoDecodeErrorCode, VideoDecodeQuery,
        VideoDecodeSupport, VideoDecoder, VideoDecoderFactory, VideoDecoderRegistry,
        VideoParameterSets, VideoPixelFormat, VideoPlane, VideoProfile, VideoRange,
        VideoStreamConfig, VideoStreamConfigInput, VideoStreamIdentity, VideoTimeBase,
        VideoTimestamp,
    };
    use frd_platform_api::{PlatformCapabilities, PlatformError, ServerIdentityStore};
    use frd_protocol_api::{
        ConnectRequest, ConnectionStage, CredentialRequirements, Credentials, ProtocolCatalog,
        ProtocolDescriptor, ProtocolError, ProtocolExit, ProtocolFactory, ProtocolRuntime,
        ProtocolSession, SessionCapabilities, SessionCommand, SessionEvent,
    };
    use frd_render_wgpu::{
        BatchApplyOutcome, BatchApplySuccess, BatchScopeDiagnostics, GpuScopeObservation,
        InstalledSurface, PresentationReceipt, VideoStreamEpoch,
    };
    use frd_session::reserve_session_start;
    use frd_ui_model::{ConnectionDraft, ConnectionForm, ProtocolChoice};
    use winit::event::{Ime, WindowEvent};

    use super::{
        accept_batch_outcome, apply_compiled_drain, detach_video_for_matching_failure,
        dispatch_runtime_drain, initialize_metrics_before_session_launch,
        mark_texture_deltas_applied, take_exact_presented_value, terminate_failed_frame_batch,
        AcceptedLaunchOutcome, ApplicationExitState, AudioOutputFactory, FrameBatchFailureTarget,
        FrameDrainFailure, PendingLiveSessionPorts, RemoteBinding, RuntimeDrainCallback,
        RuntimeDrainCallbackTarget, RuntimeDrainOutcome, RuntimeWakeGate, SessionHost,
        TestLaunchOutcome, UnavailableCredentialStore, UnavailableProfileStore, VideoBinding,
        VideoSurfaceDrainTarget, WakeSink, WorkerKind, WorkerSpawner,
    };
    use crate::frame_metrics_sink::MetricSinkError;

    #[test]
    fn hp_keyboard_shell_diagnostics_reset_both_stages_for_each_session() {
        let first_session = SessionId::allocate();
        let second_session = SessionId::allocate();
        let (mut diagnostics, lines) =
            super::HighPerformanceInputShellDiagnostics::enabled_for_test(8);

        diagnostics.observe_accepted(first_session);
        diagnostics.observe_enqueued(first_session);
        diagnostics.observe_accepted(first_session);
        diagnostics.observe_accepted(second_session);
        diagnostics.observe_enqueued(second_session);

        assert_eq!(
            lines.try_iter().collect::<Vec<_>>(),
            vec![
                super::HighPerformanceInputShellLine::Stage {
                    stage: "shell_physical_accepted",
                    count: 1,
                },
                super::HighPerformanceInputShellLine::Stage {
                    stage: "command_enqueued",
                    count: 1,
                },
                super::HighPerformanceInputShellLine::Stage {
                    stage: "shell_physical_accepted",
                    count: 2,
                },
                super::HighPerformanceInputShellLine::Stage {
                    stage: "shell_physical_accepted",
                    count: 1,
                },
                super::HighPerformanceInputShellLine::Stage {
                    stage: "command_enqueued",
                    count: 1,
                },
            ]
        );
    }

    #[test]
    fn egui_repaint_schedules_redraw_only_for_non_redraw_requested_events() {
        let mut redraws = 0;
        super::schedule_egui_repaint(&WindowEvent::RedrawRequested, true, || redraws += 1);
        assert_eq!(redraws, 0);

        super::schedule_egui_repaint(&WindowEvent::Focused(true), true, || redraws += 1);
        assert_eq!(redraws, 1);

        redraws = 0;
        super::schedule_egui_repaint(&WindowEvent::Focused(true), false, || redraws += 1);
        assert_eq!(redraws, 0);
    }

    #[test]
    fn apple_mvs_pixel_surface_uses_aspect_fit_and_exact_pointer_mapping() {
        let viewport = super::content_viewport_for_surface(
            PixelSize::new(1920, 1080).unwrap(),
            PixelSize::new(2240, 1337).unwrap(),
            PixelRect {
                x: 20,
                y: 50,
                width: 2200,
                height: 1237,
            },
        )
        .expect("有效的 Apple Standard/MVS 像素 surface 必须有视口");

        assert_eq!(
            viewport.content,
            PixelRect {
                x: 20,
                y: 50,
                width: 2199,
                height: 1237,
            }
        );
        assert_eq!(
            viewport.map_pointer(20.0, 50.0),
            Some(frd_core::PixelPoint { x: 0, y: 0 })
        );
        assert_eq!(
            viewport.map_pointer(2218.0, 1286.0),
            Some(frd_core::PixelPoint { x: 1919, y: 1079 })
        );
    }

    #[test]
    fn high_performance_video_and_rdp_pixel_surfaces_keep_fit_behavior() {
        let drawable = PixelSize::new(2240, 1337).unwrap();
        let remote_area = PixelRect {
            x: 20,
            y: 50,
            width: 2200,
            height: 1237,
        };
        let remote = PixelSize::new(1920, 1080).unwrap();

        let high_performance_video =
            super::content_viewport_for_surface(remote, drawable, remote_area)
                .expect("有效的视频 surface 必须有视口");
        let rdp_pixel = super::content_viewport_for_surface(remote, drawable, remote_area)
            .expect("有效的 RDP 像素 surface 必须有视口");

        let expected_fit = PixelRect {
            x: 20,
            y: 50,
            width: 2199,
            height: 1237,
        };
        assert_eq!(high_performance_video.content, expected_fit);
        assert_eq!(rdp_pixel.content, expected_fit);
    }

    #[test]
    fn video_confirmation_echoes_only_the_exact_presented_receipt_value() {
        let mut pending = Some((17_u64, "exact worker token"));

        assert_eq!(take_exact_presented_value(&mut pending, &16), None);
        assert_eq!(pending, Some((17, "exact worker token")));
        assert_eq!(
            take_exact_presented_value(&mut pending, &17),
            Some("exact worker token")
        );
        assert_eq!(pending, None);
    }

    #[test]
    fn ready_is_not_emitted_until_exact_current_generation_present_confirmation() {
        let session_id = SessionId::allocate();
        let event = PresentationEvent::FramePresented {
            session_id,
            generation: 7,
            revision: 11,
            completeness: FrameCompleteness::FullBaseline,
        };

        assert_eq!(
            super::video_ready_event_after_confirmation::<u64, ()>(
                None,
                |_| Ok(()),
                |_| { event.clone() }
            ),
            None,
            "authentication, UDP activation, and AU completion do not carry a presented token"
        );
        assert_eq!(
            super::video_ready_event_after_confirmation(
                Some(11_u64),
                |_| Err(()),
                |_| { event.clone() }
            ),
            None,
            "a stale or unpresented generation cannot become Ready"
        );
        assert_eq!(
            super::video_ready_event_after_confirmation(
                Some(11_u64),
                |_| Ok::<(), ()>(()),
                |_| event.clone(),
            ),
            Some(event),
            "only exact successful present confirmation emits the neutral full-baseline event"
        );
    }

    #[test]
    fn video_stale_sibling_decode_failure_cannot_detach_active_stream() {
        let session_id = SessionId::allocate();
        let active_identity = VideoStreamIdentity {
            session_id,
            stream_id: 7,
        };
        let sibling_identity = VideoStreamIdentity {
            session_id,
            stream_id: 8,
        };
        let binding = VideoBinding {
            identity: active_identity,
            generation: 11,
            size: PixelSize::new(1920, 1080).unwrap(),
        };
        let mut active = Some(binding);

        assert!(!detach_video_for_matching_failure(
            &mut active,
            active_identity,
            10
        ));
        assert_eq!(active, Some(binding));
        assert!(!detach_video_for_matching_failure(
            &mut active,
            sibling_identity,
            11
        ));
        assert_eq!(active, Some(binding));
        assert!(detach_video_for_matching_failure(
            &mut active,
            active_identity,
            11
        ));
        assert_eq!(active, None);
    }

    #[test]
    fn config_admission_precedes_backend_failure_and_disconnects_the_surface_owner() {
        let session_id = SessionId::allocate();
        let identity = VideoStreamIdentity {
            session_id,
            stream_id: 7,
        };
        let worker = crate::video_decode_worker::VideoDecodeWorker::spawn_with_registry_loader(
            Box::new(|| {
                Err(VideoDecodeError::new(
                    VideoDecodeErrorCode::BackendUnavailable,
                ))
            }),
        )
        .unwrap();
        worker
            .sender()
            .try_send_config(test_video_config(identity, 11))
            .unwrap();

        let admission = worker
            .events()
            .recv_admission_timeout(Duration::from_secs(1))
            .expect("config acceptance must publish admission before registry selection");
        let mut owner = None;
        assert!(super::admit_video_surface_owner(&mut owner, admission));
        let failure = worker
            .events()
            .recv_timeout(Duration::from_secs(1))
            .expect("registry failure must be terminal");
        let crate::video_decode_worker::VideoWorkerEvent::DecodeFailed {
            identity: failed_identity,
            generation,
            code,
            after_first_frame: false,
        } = failure
        else {
            panic!("backend load failure must not emit BackendSelected");
        };
        let mut active = None;
        let action = super::disconnect_after_matching_video_failure(
            owner,
            &mut active,
            failed_identity,
            generation,
            code,
            ProtocolId::apple_high_performance(),
        )
        .expect("pre-selection failure of the admitted owner must terminate the mode");
        assert!(matches!(action.command, SessionCommand::Disconnect));
        assert_eq!(
            action.error,
            ProtocolError::adapter(
                ProtocolId::apple_high_performance(),
                "video_backend_unavailable"
            )
        );

        worker.request_stop();
        worker.join_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn first_admitted_surface_owner_rejects_sibling_selection_failure_and_failover() {
        let session_id = SessionId::allocate();
        let owner_identity = VideoStreamIdentity {
            session_id,
            stream_id: 1,
        };
        let sibling_identity = VideoStreamIdentity {
            session_id,
            stream_id: 2,
        };
        let mut owner = None;
        assert!(super::admit_video_surface_owner(
            &mut owner,
            crate::video_decode_worker::VideoStreamAdmission {
                identity: owner_identity,
                generation: 11,
            }
        ));
        assert!(!super::admit_video_surface_owner(
            &mut owner,
            crate::video_decode_worker::VideoStreamAdmission {
                identity: sibling_identity,
                generation: 11,
            }
        ));
        assert!(super::video_surface_owner_matches(
            owner,
            owner_identity,
            11
        ));
        assert!(!super::video_surface_owner_matches(
            owner,
            sibling_identity,
            11
        ));

        let mut active = None;
        assert!(super::disconnect_after_matching_video_failure(
            owner,
            &mut active,
            sibling_identity,
            11,
            VideoDecodeErrorCode::DecoderCreationFailed,
            ProtocolId::apple_high_performance(),
        )
        .is_none());
        assert!(super::disconnect_after_matching_video_failure(
            owner,
            &mut active,
            owner_identity,
            11,
            VideoDecodeErrorCode::DecoderCreationFailed,
            ProtocolId::apple_high_performance(),
        )
        .is_some());
        assert_eq!(
            owner,
            Some(VideoStreamEpoch {
                identity: owner_identity,
                generation: 11,
            }),
            "owner remains latched until session cleanup, so no sibling can silently fail over"
        );
    }

    fn test_decoded_video_frame(
        identity: VideoStreamIdentity,
        generation: u64,
        timestamp: VideoTimestamp,
    ) -> DecodedVideoFrame {
        let plane = || VideoPlane::try_new(2, 2, 2, vec![0x80; 4].into_boxed_slice()).unwrap();
        DecodedVideoFrame::try_new(DecodedVideoFrameInput {
            identity,
            generation,
            timestamp,
            coded_size: PixelSize::new(2, 2).unwrap(),
            visible_rect: PixelRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
            format: VideoPixelFormat::Yuv444P8,
            colorimetry: VideoColorimetry::Bt709,
            range: VideoRange::Limited,
            planes: vec![plane(), plane(), plane()].into_boxed_slice(),
        })
        .unwrap()
    }

    struct RoutingVideoFactory;

    impl VideoCapabilityProvider for RoutingVideoFactory {
        fn backend_id(&self) -> VideoBackendId {
            VideoBackendId::new("routing-test")
        }

        fn backend_kind(&self) -> VideoBackendKind {
            VideoBackendKind::Ffmpeg
        }

        fn availability(&self) -> VideoBackendAvailability {
            VideoBackendAvailability::DecoderReady
        }

        fn query(&self, _query: &VideoDecodeQuery) -> VideoDecodeSupport {
            VideoDecodeSupport::SoftwareExact(VideoDecodeCapability {
                backend_id: self.backend_id(),
                codec: VideoCodec::Hevc,
                profile: VideoProfile::HevcMain4448,
                chroma: ChromaFormat::Yuv444,
                bit_depth: 8,
                max_coded_size: PixelSize::new(2, 2).unwrap(),
                output_formats: vec![VideoPixelFormat::Yuv444P8].into_boxed_slice(),
                requires_bitstream_conversion: false,
            })
        }
    }

    impl VideoDecoderFactory for RoutingVideoFactory {
        fn create(
            &self,
            config: &VideoStreamConfig,
        ) -> Result<Box<dyn VideoDecoder>, VideoDecodeError> {
            Ok(Box::new(RoutingVideoDecoder(config.clone())))
        }
    }

    struct RoutingVideoDecoder(VideoStreamConfig);

    impl VideoDecoder for RoutingVideoDecoder {
        fn submit(
            &mut self,
            access_unit: EncodedVideoAccessUnit,
        ) -> Result<DecodeOutcome, VideoDecodeError> {
            if self.0.as_input().identity.stream_id == 1 {
                return Err(VideoDecodeError::new(
                    VideoDecodeErrorCode::DecodeFailedBeforeFirstFrame,
                ));
            }
            Ok(DecodeOutcome::Frames(
                vec![test_decoded_video_frame(
                    access_unit.identity(),
                    access_unit.generation(),
                    access_unit.timestamp(),
                )]
                .into_boxed_slice(),
            ))
        }

        fn flush(&mut self) -> Result<Box<[DecodedVideoFrame]>, VideoDecodeError> {
            Ok(Box::default())
        }

        fn reset(&mut self, _generation: u64) -> Result<(), VideoDecodeError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingVideoSurface {
        owner: Option<VideoStreamEpoch>,
        active: Option<VideoBinding>,
        configured: Vec<VideoStreamEpoch>,
        uploaded: Vec<crate::video_decode_worker::VideoFrameToken>,
        pending: Option<crate::video_decode_worker::VideoFrameToken>,
        detach_count: usize,
        stop_count: usize,
    }

    impl VideoSurfaceDrainTarget for RecordingVideoSurface {
        fn video_owner(&self) -> Option<VideoStreamEpoch> {
            self.owner
        }

        fn video_owner_mut(&mut self) -> &mut Option<VideoStreamEpoch> {
            &mut self.owner
        }

        fn active_video_mut(&mut self) -> &mut Option<VideoBinding> {
            &mut self.active
        }

        fn reset_for_admission(&mut self, _epoch: VideoStreamEpoch) {
            self.detach_count += 1;
            self.active = None;
            self.pending = None;
        }

        fn configure_video_stream(
            &mut self,
            epoch: VideoStreamEpoch,
        ) -> Result<(), frd_render_wgpu::VideoRendererError> {
            self.configured.push(epoch);
            Ok(())
        }

        fn upload_video_frame(
            &mut self,
            token: crate::video_decode_worker::VideoFrameToken,
            _frame: DecodedVideoFrame,
        ) -> Result<(), frd_render_wgpu::VideoRendererError> {
            self.uploaded.push(token);
            self.pending = Some(token);
            Ok(())
        }

        fn detach_after_video_failure(&mut self) {
            self.detach_count += 1;
            self.pending = None;
        }

        fn video_worker_stopped(&mut self) {
            self.detach_count += 1;
            self.stop_count += 1;
            super::transition_video_surface_owner(
                &mut self.owner,
                super::VideoSurfaceOwnerTransition::WorkerStopped,
            );
            self.active = None;
            self.pending = None;
        }
    }

    #[test]
    fn worker_stop_during_owner_failure_keeps_sibling_rejected_until_cleanup_completes() {
        let session_id = SessionId::allocate();
        let owner_identity = VideoStreamIdentity {
            session_id,
            stream_id: 1,
        };
        let sibling_identity = VideoStreamIdentity {
            session_id,
            stream_id: 2,
        };
        let worker =
            crate::video_decode_worker::VideoDecodeWorker::spawn_with_stream_registry_loader(
                Arc::new(|_identity| {
                    Ok(VideoDecoderRegistry::new(vec![Box::new(
                        RoutingVideoFactory,
                    )]))
                }),
            )
            .unwrap();

        worker
            .sender()
            .try_send_config(test_video_config(owner_identity, 11))
            .unwrap();
        let owner_admission = worker
            .events()
            .recv_admission_timeout(Duration::from_secs(1))
            .unwrap();
        let owner_selected = worker
            .events()
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            &owner_selected,
            crate::video_decode_worker::VideoWorkerEvent::BackendSelected {
                identity,
                generation: 11,
                ..
            } if *identity == owner_identity
        ));
        let mut surface = RecordingVideoSurface::default();
        let selected = super::drain_video_surface_events(
            &mut surface,
            vec![(session_id, owner_admission)],
            vec![(session_id, owner_selected)],
            Some(ProtocolId::apple_high_performance()),
        )
        .unwrap();
        assert!(selected.failure.is_none());
        assert_eq!(
            surface.configured,
            vec![VideoStreamEpoch {
                identity: owner_identity,
                generation: 11,
            }]
        );

        worker
            .sender()
            .try_send_access_unit(test_video_access_unit_for(owner_identity, 11, 1, 0x26))
            .unwrap();
        let owner_failure = worker
            .events()
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            &owner_failure,
            crate::video_decode_worker::VideoWorkerEvent::DecodeFailed {
                identity,
                generation: 11,
                code: VideoDecodeErrorCode::DecodeFailedBeforeFirstFrame,
                ..
            } if *identity == owner_identity
        ));
        let failed = super::drain_video_surface_events(
            &mut surface,
            Vec::new(),
            vec![
                (session_id, owner_failure),
                (
                    session_id,
                    crate::video_decode_worker::VideoWorkerEvent::Stopped,
                ),
            ],
            Some(ProtocolId::apple_high_performance()),
        )
        .unwrap();
        assert!(failed.failure.is_some());
        assert_eq!(surface.stop_count, 1);
        super::transition_video_surface_owner(
            &mut surface.owner,
            super::VideoSurfaceOwnerTransition::CleanupStarted,
        );

        worker
            .sender()
            .try_send_config(test_video_config(sibling_identity, 11))
            .unwrap();
        let sibling_admission = worker
            .events()
            .recv_admission_timeout(Duration::from_secs(1))
            .unwrap();
        let sibling_selected = worker
            .events()
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            &sibling_selected,
            crate::video_decode_worker::VideoWorkerEvent::BackendSelected {
                identity,
                generation: 11,
                ..
            } if *identity == sibling_identity
        ));
        worker
            .sender()
            .try_send_access_unit(test_video_access_unit_for(sibling_identity, 11, 2, 0x26))
            .unwrap();
        let sibling_frame = worker
            .events()
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            &sibling_frame,
            crate::video_decode_worker::VideoWorkerEvent::FrameDecoded(handoff)
                if handoff.token().identity() == sibling_identity
                    && handoff.token().generation() == 11
        ));
        let sibling = super::drain_video_surface_events(
            &mut surface,
            vec![(session_id, sibling_admission)],
            vec![(session_id, sibling_selected), (session_id, sibling_frame)],
            Some(ProtocolId::apple_high_performance()),
        )
        .unwrap();

        assert!(sibling.failure.is_none());
        assert!(!sibling.frame_redraw_needed);
        assert_eq!(surface.configured.len(), 1);
        assert!(surface.uploaded.is_empty());
        assert!(surface.pending.is_none());
        assert_eq!(surface.detach_count, 3);
        let ready = super::video_ready_event_after_confirmation::<_, ()>(
            surface.pending,
            |_| Ok(()),
            |_| PresentationEvent::FramePresented {
                session_id,
                generation: 11,
                revision: 1,
                completeness: FrameCompleteness::FullBaseline,
            },
        );
        assert_eq!(ready, None);
        let mut input = crate::InputRouter::default();
        if ready.is_some() {
            let _ = input.set_gate(crate::InputGate::Interactive {
                session_id,
                generation: 11,
            });
        }
        assert_eq!(input.interactive_epoch(), None);
        assert_eq!(
            surface.owner,
            Some(VideoStreamEpoch {
                identity: owner_identity,
                generation: 11,
            })
        );

        super::transition_video_surface_owner(
            &mut surface.owner,
            super::VideoSurfaceOwnerTransition::CleanupComplete,
        );
        assert_eq!(surface.owner, None);

        worker.request_stop();
        worker.join_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn every_video_decode_category_maps_to_a_distinct_safe_terminal_code() {
        let cases = [
            (
                VideoDecodeErrorCode::BackendUnavailable,
                "video_backend_unavailable",
            ),
            (
                VideoDecodeErrorCode::ExactProfileChromaBitDepthUnsupported,
                "video_exact_profile_chroma_bit_depth_unsupported",
            ),
            (
                VideoDecodeErrorCode::OutputFormatUnsupported,
                "video_output_format_unsupported",
            ),
            (
                VideoDecodeErrorCode::DecoderCreationFailed,
                "video_decoder_creation_failed",
            ),
            (
                VideoDecodeErrorCode::MalformedOrOverBudgetAccessUnit,
                "video_malformed_or_over_budget_access_unit",
            ),
            (
                VideoDecodeErrorCode::StaleStreamOrGeneration,
                "video_stale_stream_or_generation",
            ),
            (
                VideoDecodeErrorCode::DecodeFailedBeforeFirstFrame,
                "video_decode_failed_before_first_frame",
            ),
            (
                VideoDecodeErrorCode::DecodeFailedAfterFirstFrame,
                "video_decode_failed_after_first_frame",
            ),
            (
                VideoDecodeErrorCode::DecodedFrameLayoutInvalid,
                "video_decoded_frame_layout_invalid",
            ),
            (
                VideoDecodeErrorCode::FramePublicationFailed,
                "video_frame_publication_failed",
            ),
            (
                VideoDecodeErrorCode::BackendVersionMismatch,
                "video_backend_version_mismatch",
            ),
        ];

        for (code, expected) in cases {
            assert_eq!(super::video_decode_terminal_code(code), expected);
        }
    }

    #[test]
    fn upload_stage_trace_is_once_for_same_owner_and_rearms_only_for_a_new_owner_epoch() {
        let session_id = SessionId::allocate();
        let identity = VideoStreamIdentity {
            session_id,
            stream_id: 1,
        };
        let owner = VideoStreamEpoch {
            identity,
            generation: 7,
        };
        let mut trace = super::OwnedVideoStageTrace::default();

        trace.admit(owner);
        assert!(trace.observe_uploaded(owner, PixelSize::new(1440, 2560).unwrap()));
        trace.admit(owner);
        assert!(!trace.observe_uploaded(owner, PixelSize::new(1440, 2560).unwrap()));
        assert!(!trace.observe_uploaded(
            VideoStreamEpoch {
                identity: VideoStreamIdentity {
                    session_id,
                    stream_id: 2,
                },
                generation: 7,
            },
            PixelSize::new(1440, 2560).unwrap(),
        ));
        assert!(!trace.observe_uploaded(
            VideoStreamEpoch {
                identity,
                generation: 6,
            },
            PixelSize::new(1440, 2560).unwrap(),
        ));

        let next = VideoStreamEpoch {
            identity,
            generation: 8,
        };
        trace.admit(next);
        assert!(trace.observe_uploaded(next, PixelSize::new(1920, 1080).unwrap()));
    }

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

        pending.record_batch(true);
        assert!(pending.is_pending());
        assert!(!pending.finish_render(true, true));
        assert!(
            !pending.is_pending(),
            "an actual submit clears pending writes"
        );

        pending.record_batch(true);
        assert!(!pending.finish_render(false, false));
        assert!(
            pending.is_pending(),
            "a render error must not fake a submit"
        );
        assert!(pending.finish_render(false, true));
        assert!(!pending.is_pending(), "fallback takes the pending writes");

        assert!(!pending.finish_render(false, true));
    }

    fn test_reset(session_id: SessionId, at: Instant) -> EnqueuedSurfaceUpdate {
        EnqueuedSurfaceUpdate {
            enqueued_at: at,
            update: SurfaceUpdate::Reset {
                session_id,
                generation: 1,
                size: frd_core::PixelSize::new(2, 2).unwrap(),
                format: PixelFormat::Bgrx8UnormSrgb,
            },
        }
    }

    fn test_damage(session_id: SessionId, at: Instant) -> EnqueuedSurfaceUpdate {
        EnqueuedSurfaceUpdate {
            enqueued_at: at,
            update: SurfaceUpdate::Damage {
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
                    pixels: PixelBuffer::new(vec![0; 16]),
                }],
            },
        }
    }

    fn test_boundary(session_id: SessionId, at: Instant) -> EnqueuedSurfaceUpdate {
        EnqueuedSurfaceUpdate {
            enqueued_at: at,
            update: SurfaceUpdate::FrameBoundary {
                session_id,
                generation: 1,
                revision: 1,
                completeness: FrameCompleteness::FullBaseline,
            },
        }
    }

    fn frame_drain_host(session_id: SessionId) -> SessionHost {
        let mut host = SessionHost::new(
            std::iter::empty::<Arc<dyn ProtocolFactory>>(),
            Arc::new(CountingWake(AtomicUsize::new(0))),
            Arc::new(TestAudioFactory),
        );
        let (commands, _command_rx) = mpsc::channel();
        let (_event_tx, events) = mpsc::channel();
        let mailbox = Arc::new(Mutex::new(frd_frame::FrameMailbox::new(8, 1024)));
        host.active = Some(
            PendingLiveSessionPorts {
                session_id,
                protocol_id: ProtocolId::apple_hpss_mvs(),
                commands,
                events,
                mailbox,
                video_events: crate::video_decode_worker::VideoWorkerEvents::new(None),
            }
            .accept(),
        );
        host
    }

    fn enqueue_frame(host: &mut SessionHost, envelope: EnqueuedSurfaceUpdate) {
        let mailbox = host.active.as_ref().unwrap().mailbox.clone();
        assert_eq!(
            mailbox.lock().unwrap().push(envelope.update),
            frd_frame::PushOutcome::Queued
        );
    }

    fn successful_startup(session_id: SessionId) -> BatchApplySuccess {
        BatchApplySuccess {
            outcome: BatchApplyOutcome {
                installed_surface: Some(InstalledSurface {
                    session_id,
                    generation: 1,
                    size: frd_core::PixelSize::new(2, 2).unwrap(),
                    format: PixelFormat::Bgrx8UnormSrgb,
                }),
                uploaded_rectangles: 1,
                had_texture_writes: true,
                final_boundary: Some(PresentationReceipt {
                    session_id,
                    generation: 1,
                    revision: 1,
                    completeness: FrameCompleteness::FullBaseline,
                }),
            },
            scope: BatchScopeDiagnostics {
                observation: GpuScopeObservation {
                    begins: 1,
                    finishes: 1,
                    polls: 1,
                },
                observed_fault: None,
            },
        }
    }

    #[derive(Default)]
    struct RecordingDrainCallbacks {
        redraws: usize,
        renders: usize,
        terminations: usize,
        records: usize,
        submits: usize,
        presents: usize,
        receipts: usize,
        renderer_batch_calls: usize,
    }

    impl RuntimeDrainCallbackTarget for RecordingDrainCallbacks {
        fn request_redraw(&mut self) {
            self.redraws += 1;
        }

        fn render_now(&mut self) {
            self.renders += 1;
            self.records += 1;
            self.submits += 1;
            self.presents += 1;
            self.receipts += 1;
        }

        fn terminate(&mut self, _report: crate::fatal::FatalReport) {
            self.terminations += 1;
        }
    }

    #[test]
    fn event_only_wake_requests_one_ui_redraw_and_zero_frame_redraw() {
        let outcome = RuntimeDrainOutcome {
            ui_redraw_needed: true,
            frame_redraw_needed: false,
        };
        let mut callbacks = RecordingDrainCallbacks::default();

        dispatch_runtime_drain(RuntimeDrainCallback::Wake, Ok(outcome), &mut callbacks);

        assert_eq!(callbacks.redraws, 1);
        assert_eq!(callbacks.renderer_batch_calls, 0);
        assert!(outcome.ui_redraw_needed);
        assert!(!outcome.frame_redraw_needed);
    }

    #[test]
    fn reset_only_and_reset_damage_without_boundary_request_no_frame_redraw() {
        let session_id = SessionId::allocate();
        let at = Instant::now();
        let mut host = frame_drain_host(session_id);
        let mut renderer_batch_calls = 0;
        let mut binding: Option<RemoteBinding> = None;

        enqueue_frame(&mut host, test_reset(session_id, at));
        let reset_only = host.drain_frame_transactions().unwrap();
        if !reset_only.transactions.is_empty() {
            let _ = apply_compiled_drain(reset_only.transactions, |_| {
                renderer_batch_calls += 1;
                Ok(successful_startup(session_id))
            });
        }
        assert_eq!(reset_only.metrics.transaction_count, 0);
        assert_eq!(renderer_batch_calls, 0);
        assert!(binding.is_none());
        assert!(!RuntimeDrainOutcome::default().frame_redraw_needed);

        enqueue_frame(
            &mut host,
            test_damage(session_id, at + Duration::from_millis(1)),
        );
        let reset_damage = host.drain_frame_transactions().unwrap();
        if !reset_damage.transactions.is_empty() {
            let success = apply_compiled_drain(reset_damage.transactions, |_| {
                renderer_batch_calls += 1;
                Ok(successful_startup(session_id))
            })
            .unwrap();
            let _ = accept_batch_outcome(
                &mut binding,
                &mut super::PendingTextureWrites::default(),
                &success.outcome,
            );
        }
        assert_eq!(reset_damage.metrics.transaction_count, 0);
        assert_eq!(renderer_batch_calls, 0);
        assert!(binding.is_none());
        assert!(!RuntimeDrainOutcome::default().frame_redraw_needed);
    }

    #[test]
    fn atomic_startup_full_baseline_installs_binding_and_requests_one_frame_redraw() {
        let session_id = SessionId::allocate();
        let at = Instant::now();
        let mut host = frame_drain_host(session_id);
        enqueue_frame(&mut host, test_reset(session_id, at));
        assert!(host
            .drain_frame_transactions()
            .unwrap()
            .transactions
            .is_empty());
        enqueue_frame(
            &mut host,
            test_damage(session_id, at + Duration::from_millis(1)),
        );
        assert!(host
            .drain_frame_transactions()
            .unwrap()
            .transactions
            .is_empty());
        enqueue_frame(
            &mut host,
            test_boundary(session_id, at + Duration::from_millis(2)),
        );
        let drain = host.drain_frame_transactions().unwrap();
        assert_eq!(drain.metrics.source_update_count, 3);
        assert_eq!(drain.metrics.transaction_count, 1);
        let mut renderer_batch_calls = 0;
        let success = apply_compiled_drain(drain.transactions, |transactions| {
            renderer_batch_calls += 1;
            assert_eq!(transactions.len(), 1);
            Ok(successful_startup(session_id))
        })
        .unwrap();
        let mut binding = None;
        let mut pending = super::PendingTextureWrites::default();

        let frame_redraw_needed =
            accept_batch_outcome(&mut binding, &mut pending, &success.outcome);

        assert_eq!(renderer_batch_calls, 1);
        let binding = binding.expect("atomic Startup installs its returned surface");
        assert_eq!(binding.session_id, session_id);
        assert_eq!(binding.generation, 1);
        assert_eq!(binding.size, frd_core::PixelSize::new(2, 2).unwrap());
        assert!(frame_redraw_needed);
        assert!(pending.is_pending());
    }

    #[test]
    fn one_nonempty_compiled_drain_calls_renderer_once() {
        let session_id = SessionId::allocate();
        let transaction = FrameTransaction::Startup {
            earliest_constituent_enqueue_at: Instant::now(),
            reset: FrameReset {
                session_id,
                generation: 1,
                size: frd_core::PixelSize::new(2, 2).unwrap(),
                format: PixelFormat::Bgrx8UnormSrgb,
            },
            revision: FrameRevision {
                session_id,
                generation: 1,
                revision: 1,
                patches: Vec::new(),
                completeness: FrameCompleteness::FullBaseline,
            },
        };
        let expected = successful_startup(session_id);
        let mut calls = 0;

        let actual = apply_compiled_drain(vec![transaction], |transactions| {
            calls += 1;
            assert_eq!(transactions.len(), 1);
            Ok(expected)
        })
        .unwrap();

        assert_eq!(calls, 1);
        assert_eq!(actual, expected);
    }

    #[derive(Default)]
    struct RecordingFrameFailureTarget {
        order: Vec<&'static str>,
    }

    impl FrameBatchFailureTarget for RecordingFrameFailureTarget {
        fn block_remote_input(&mut self) {
            self.order.push("block_remote_input");
        }

        fn detach_remote_surface(&mut self) {
            self.order.push("detach_remote_surface");
        }

        fn clear_pending_texture_writes(&mut self) {
            self.order.push("clear_pending_texture_writes");
        }
    }

    #[test]
    fn frame_batch_failure_blocks_detaches_and_clears_in_exact_order() {
        let mut target = RecordingFrameFailureTarget::default();

        let report = terminate_failed_frame_batch(
            &mut target,
            FrameDrainFailure::Compile(FrameTransactionError::ForeignSession),
        );

        assert_eq!(
            target.order,
            [
                "block_remote_input",
                "detach_remote_surface",
                "clear_pending_texture_writes"
            ]
        );
        assert_eq!(report.operation(), "frame_transaction");
        assert_eq!(report.details(), "frame_transaction_foreign_session");
    }

    fn callback_fatal_report() -> crate::fatal::FatalReport {
        crate::fatal::FatalReport::frame_transaction(FrameTransactionError::ForeignSession)
    }

    #[test]
    fn fatal_wake_has_zero_redraw_record_submit_present_and_receipt() {
        let mut callbacks = RecordingDrainCallbacks::default();

        dispatch_runtime_drain(
            RuntimeDrainCallback::Wake,
            Err(callback_fatal_report()),
            &mut callbacks,
        );

        assert_eq!(callbacks.terminations, 1);
        assert_eq!(callbacks.redraws, 0);
        assert_eq!(callbacks.renders, 0);
        assert_eq!(callbacks.records, 0);
        assert_eq!(callbacks.submits, 0);
        assert_eq!(callbacks.presents, 0);
        assert_eq!(callbacks.receipts, 0);
    }

    #[test]
    fn fatal_redraw_requested_has_zero_record_submit_present_and_receipt() {
        let mut callbacks = RecordingDrainCallbacks::default();

        dispatch_runtime_drain(
            RuntimeDrainCallback::RedrawRequested,
            Err(callback_fatal_report()),
            &mut callbacks,
        );

        assert_eq!(callbacks.terminations, 1);
        assert_eq!(callbacks.renders, 0);
        assert_eq!(callbacks.records, 0);
        assert_eq!(callbacks.submits, 0);
        assert_eq!(callbacks.presents, 0);
        assert_eq!(callbacks.receipts, 0);
    }

    #[test]
    fn pointer_domain_changes_only_for_session_glyphs_and_remote_content() {
        let remote_content = PixelRect {
            x: 0,
            y: 0,
            width: 1100,
            height: 720,
        };
        let glyph = crate::ChromeRect {
            x: 500,
            y: 8,
            width: 44,
            height: 44,
        };
        let reposition = crate::ChromeRect {
            x: 448,
            y: 8,
            width: 44,
            height: 44,
        };
        let hit_map = crate::ChromeHitMap::candidate(
            remote_content,
            vec![(glyph, frd_ui_model::IslandAction::Disconnect)],
            Some(reposition),
            None,
            Vec::new(),
        )
        .unwrap();
        let viewport = ContentViewport {
            drawable: PixelSize::new(1100, 720).unwrap(),
            content: remote_content,
            remote: PixelSize::new(1100, 720).unwrap(),
        };
        let content = (remote_content.width / 2, remote_content.height / 2);

        assert_eq!(
            super::pointer_keyboard_ownership(&hit_map, Some(viewport), glyph.center()),
            Some(crate::InputOwnership::Ui)
        );
        assert_eq!(
            super::pointer_keyboard_ownership(&hit_map, Some(viewport), content),
            Some(crate::InputOwnership::Remote)
        );
        assert_eq!(
            super::effective_pointer_keyboard_ownership(
                &hit_map,
                Some(viewport),
                content,
                true,
                true
            ),
            Some(crate::InputOwnership::Ui)
        );
        assert_eq!(
            super::effective_pointer_keyboard_ownership(
                &hit_map,
                Some(viewport),
                content,
                false,
                false
            ),
            Some(crate::InputOwnership::Ui)
        );
        assert_eq!(
            super::effective_pointer_keyboard_ownership(
                &hit_map,
                Some(viewport),
                content,
                false,
                true
            ),
            Some(crate::InputOwnership::Remote)
        );
        assert_eq!(
            super::pointer_keyboard_ownership(&hit_map, Some(viewport), reposition.center()),
            None
        );
    }

    #[test]
    fn island_window_actions_map_to_exact_platform_commands() {
        use frd_ui_model::IslandAction;

        assert_eq!(
            super::window_command_for_island_action(IslandAction::MinimizeWindow),
            Some(crate::WindowChromeCommand::Minimize)
        );
        assert_eq!(
            super::window_command_for_island_action(IslandAction::ToggleMaximizeWindow),
            Some(crate::WindowChromeCommand::ToggleMaximize)
        );
        assert_eq!(
            super::window_command_for_island_action(IslandAction::CloseWindow),
            Some(crate::WindowChromeCommand::Close)
        );
        assert_eq!(
            super::window_command_for_island_action(IslandAction::ShowSystemMenu),
            Some(crate::WindowChromeCommand::ShowSystemMenu)
        );
        assert_eq!(
            super::window_command_for_island_action(IslandAction::Disconnect),
            None
        );
    }

    #[test]
    fn mvs_letterbox_cannot_claim_remote_keyboard_domain() {
        let hit_map = crate::ChromeHitMap::candidate(
            PixelRect {
                x: 0,
                y: 0,
                width: 2240,
                height: 1337,
            },
            Vec::new(),
            None,
            None,
            Vec::new(),
        )
        .unwrap();
        let viewport = ContentViewport::fit_in(
            PixelSize::new(1920, 1080).unwrap(),
            PixelSize::new(2240, 1337).unwrap(),
            PixelRect {
                x: 20,
                y: 50,
                width: 2200,
                height: 1237,
            },
        )
        .expect("有效 MVS 内容区域必须生成视口");

        assert_eq!(
            super::effective_pointer_keyboard_ownership(
                &hit_map,
                Some(viewport),
                (19, 50),
                false,
                true,
            ),
            Some(crate::InputOwnership::Ui),
            "左侧黑边不是远端内容，必须切回本地键盘域"
        );
        assert_eq!(
            super::effective_pointer_keyboard_ownership(
                &hit_map,
                Some(viewport),
                (20, 50),
                false,
                true,
            ),
            Some(crate::InputOwnership::Remote),
            "缩放后内容左上角仍必须进入远端键盘域"
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
    fn persistent_panel_keeps_remote_surface_below_reserved_row_until_overlay_cutover() {
        assert_eq!(
            super::persistent_session_panel_content_rect(PixelSize::new(1100, 720).unwrap(), 1.5),
            Some(PixelRect {
                x: 0,
                y: 66,
                width: 1100,
                height: 654,
            })
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
            std::thread::spawn(move || super::drain_audio_media(video_only_factory, video_only_rx));
        video_only_tx
            .send(MediaFrame::EncodedVideo(test_video_access_unit(1, 0xaa)))
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
            super::drain_audio_media(video_then_pcm_factory, video_then_pcm_rx)
        });
        video_then_pcm_tx
            .send(MediaFrame::EncodedVideo(test_video_access_unit(2, 0xbb)))
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

    #[test]
    fn audio_drain_remains_independent_while_video_backend_load_is_blocked() {
        let (load_entered_tx, load_entered_rx) = mpsc::channel();
        let (release_load_tx, release_load_rx) = mpsc::channel();
        let video_worker =
            crate::video_decode_worker::VideoDecodeWorker::spawn_with_registry_loader(Box::new(
                move || {
                    load_entered_tx.send(()).unwrap();
                    release_load_rx.recv().unwrap();
                    Err(VideoDecodeError::new(
                        VideoDecodeErrorCode::BackendUnavailable,
                    ))
                },
            ))
            .expect("测试 video worker 应启动");
        let (audio_tx, audio_rx) = mpsc::sync_channel(4);
        let publisher = super::DesktopMediaPublisher::new(audio_tx, video_worker.sender());
        let pcm_frames = Arc::new(Mutex::new(Vec::new()));
        let audio_factory: Arc<dyn AudioOutputFactory> = Arc::new(RecordingAudioFactory {
            open_count: Arc::new(AtomicUsize::new(0)),
            pcm_frames: pcm_frames.clone(),
        });
        let audio_worker =
            std::thread::spawn(move || super::drain_audio_media(audio_factory, audio_rx));
        let identity = VideoStreamIdentity {
            session_id: SessionId::allocate(),
            stream_id: 9,
        };

        publisher
            .publish(MediaFrame::VideoConfig(test_video_config(identity, 1)))
            .unwrap();
        load_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("video loader 应进入阻塞点");
        publisher
            .publish(MediaFrame::EncodedVideo(test_video_access_unit_for(
                identity, 1, 1, 0x26,
            )))
            .unwrap();
        publisher
            .publish(MediaFrame::Pcm {
                sample_rate_hz: 48_000,
                channels: 2,
                samples: vec![3_i16, -4_i16].into_boxed_slice(),
            })
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while pcm_frames.lock().unwrap().is_empty() {
            assert!(
                Instant::now() < deadline,
                "video loader 阻塞时 PCM 仍应独立 drain"
            );
            std::thread::yield_now();
        }
        assert_eq!(*pcm_frames.lock().unwrap(), vec![vec![3_i16, -4_i16]]);

        drop(publisher);
        assert_eq!(audio_worker.join().unwrap(), super::AudioWorkerExit::Closed);
        release_load_tx.send(()).unwrap();
        video_worker.request_stop();
        video_worker
            .join_timeout(Duration::from_secs(1))
            .expect("测试 video worker 应有界退出");
    }

    fn test_video_config(identity: VideoStreamIdentity, generation: u64) -> VideoStreamConfig {
        VideoStreamConfig::try_new(VideoStreamConfigInput {
            identity,
            generation,
            codec: VideoCodec::Hevc,
            profile: VideoProfile::HevcMain4448,
            chroma: ChromaFormat::Yuv444,
            bit_depth: 8,
            coded_size: frd_core::PixelSize::new(2, 2).unwrap(),
            visible_rect: PixelRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
            time_base: VideoTimeBase::try_new(90_000).unwrap(),
            bitstream_format: VideoBitstreamFormat::AnnexB,
            colorimetry: VideoColorimetry::Bt709,
            range: VideoRange::Limited,
            chroma_location: ChromaLocation::Left,
            parameter_sets: VideoParameterSets::try_new(
                Some(vec![0x40].into_boxed_slice()),
                vec![0x42].into_boxed_slice(),
                vec![0x44].into_boxed_slice(),
            )
            .unwrap(),
        })
        .unwrap()
    }

    fn test_video_access_unit_for(
        identity: VideoStreamIdentity,
        generation: u64,
        ticks: u64,
        byte: u8,
    ) -> EncodedVideoAccessUnit {
        EncodedVideoAccessUnit::try_new(
            identity,
            generation,
            VideoTimestamp {
                ticks,
                timescale: NonZeroU32::new(90_000).unwrap(),
            },
            true,
            vec![byte].into_boxed_slice(),
        )
        .unwrap()
    }

    fn test_video_access_unit(ticks: u64, byte: u8) -> EncodedVideoAccessUnit {
        EncodedVideoAccessUnit::try_new(
            VideoStreamIdentity {
                session_id: SessionId::allocate(),
                stream_id: 1,
            },
            1,
            VideoTimestamp {
                ticks,
                timescale: NonZeroU32::new(1_000_000).expect("测试 timebase 非零"),
            },
            true,
            vec![byte].into_boxed_slice(),
        )
        .expect("测试访问单元有效")
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
