//! 平台无关的会话启动、媒体工作线程与取消清理；窗口壳仅通过端口和 wake 接入。

use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use frd_core::{ProtocolId, SessionId, TargetSystem};
use frd_frame::{
    EnqueuedSurfaceUpdate, FrameMailbox, FrameTransaction, FrameTransactionCompiler,
    FrameTransactionError, SurfaceUpdate,
};
use frd_media_api::{
    AudioOutput, AudioOutputError, MediaFrame, MediaPublishError, MediaPublisher,
    VideoStreamIdentity,
};
use frd_protocol_api::{
    ConnectRequest, MailboxSurfacePublisher, ProtocolCatalog, ProtocolError, ProtocolExit,
    ProtocolFactory, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionCommand, SessionEvent,
};
use frd_session::{
    CleanupComplete, CleanupError, CleanupOperations, SessionCleanupHandle, SessionCoordinator,
    SessionStartFailure, SessionStartOutcome, SessionStartPermit,
};

use crate::cleanup::{
    spawn_cleanup, BackgroundCleanupFailure, BackgroundCleanupOutcome, CleanupPolicy,
    PendingCleanup,
};
use crate::frame_metrics::{checked_mailbox_age, BatchMetricContext};
use crate::video_decode_worker::{
    VideoDecodeLoadSnapshot, VideoDecodeSender, VideoDecodeWorker, VideoFrameToken,
    VideoStreamAdmission, VideoWorkerEvent, VideoWorkerEvents,
};

const FRAME_MAILBOX_ENTRY_LIMIT: usize = 256;
const FRAME_MAILBOX_PIXEL_LIMIT: usize = 64 * 1024 * 1024;
const MEDIA_MAILBOX_ENTRY_LIMIT: usize = 16;
const APPLE_HIGH_PERFORMANCE_PROTOCOL_ID: &str = "apple-high-performance";

pub trait WakeSink: Send + Sync {
    fn wake(&self) -> Result<(), ProtocolError>;
}

pub trait AudioOutputFactory: Send + Sync {
    fn open(&self) -> Result<Box<dyn AudioOutput>, AudioOutputError>;
}

#[cfg(test)]
pub(crate) enum TestLaunchOutcome {
    Started,
    LaunchRolledBack(SessionStartFailure),
}

/// 窗口壳接收后台启动事务后的结果；外部壳可直接匹配所有分支。
///
/// ```
/// use frd_shell_desktop::AcceptedLaunchOutcome;
/// fn started(outcome: AcceptedLaunchOutcome) -> bool {
///     match outcome {
///         AcceptedLaunchOutcome::Started => true,
///         AcceptedLaunchOutcome::LaunchRolledBack(_)
///         | AcceptedLaunchOutcome::CancelledStarted
///         | AcceptedLaunchOutcome::CancelledLaunchRolledBack(_) => false,
///     }
/// }
/// assert!(started(AcceptedLaunchOutcome::Started));
/// ```
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
    video_sender: Option<VideoDecodeSender>,
    video_events: VideoWorkerEvents,
}

struct LiveSessionPorts {
    session_id: SessionId,
    protocol_id: ProtocolId,
    commands: mpsc::Sender<SessionCommand>,
    events: mpsc::Receiver<SessionEvent>,
    mailbox: Arc<Mutex<FrameMailbox>>,
    video_sender: Option<VideoDecodeSender>,
    video_events: VideoWorkerEvents,
    frame_compiler: FrameTransactionCompiler,
    frame_presentation_attached: bool,
}

impl PendingLiveSessionPorts {
    fn accept(self) -> LiveSessionPorts {
        LiveSessionPorts {
            session_id: self.session_id,
            protocol_id: self.protocol_id,
            commands: self.commands,
            events: self.events,
            mailbox: self.mailbox,
            video_sender: self.video_sender,
            video_events: self.video_events,
            frame_compiler: FrameTransactionCompiler::new(self.session_id),
            frame_presentation_attached: true,
        }
    }
}

pub(crate) struct CompiledFrameDrain {
    pub(crate) transactions: Vec<FrameTransaction>,
    pub(crate) metrics: BatchMetricContext,
}

#[derive(Debug)]
pub(crate) struct FrameCompileFailure {
    pub(crate) error: FrameTransactionError,
    pub(crate) metrics: BatchMetricContext,
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
    pub(crate) fn complete_test_launch(
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

    pub(crate) fn active_high_performance_session_id(&self) -> Option<SessionId> {
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

    pub fn video_decode_load_snapshot(
        &self,
        identity: VideoStreamIdentity,
        generation: u64,
    ) -> Option<VideoDecodeLoadSnapshot> {
        self.active.as_ref().and_then(|active| {
            (active.session_id == identity.session_id)
                .then_some(active.video_sender.as_ref())
                .flatten()?
                .video_decode_load_snapshot(identity, generation)
        })
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

    pub(crate) fn drain_frame_transactions(
        &mut self,
    ) -> Result<CompiledFrameDrain, FrameCompileFailure> {
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
        if !active.frame_presentation_attached {
            if let Ok(mut mailbox) = active.mailbox.lock() {
                while mailbox.pop_enqueued().is_some() {}
            }
            return Ok(CompiledFrameDrain {
                transactions: Vec::new(),
                metrics: BatchMetricContext {
                    batch_started_at: std::time::Instant::now(),
                    source_update_count: 0,
                    oldest_age: None,
                    transaction_count: 0,
                },
            });
        }
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

    pub(crate) fn retire_frame_presentation(&mut self, session_id: SessionId) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        if active.session_id != session_id {
            return false;
        }
        active.frame_presentation_attached = false;
        active.frame_compiler = FrameTransactionCompiler::new(session_id);
        if let Ok(mut mailbox) = active.mailbox.lock() {
            while mailbox.pop_enqueued().is_some() {}
        }
        true
    }

    pub(crate) fn active_session_id(&self) -> Option<SessionId> {
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
        Some(Box::new(DesktopMediaPublisher::new(
            media_tx,
            video_sender.clone(),
        ))),
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
            video_sender: Some(video_sender),
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

#[cfg(test)]
impl SessionHost {
    pub(crate) fn new_with_test_spawner(
        factories: impl IntoIterator<Item = Arc<dyn ProtocolFactory>>,
        wake: Arc<dyn WakeSink>,
        audio_factory: Arc<dyn AudioOutputFactory>,
        worker_spawner: Arc<dyn test_support::WorkerSpawner>,
    ) -> Self {
        Self::new_with_spawner(
            factories,
            wake,
            audio_factory,
            Arc::new(test_support::SpawnerAdapter(worker_spawner)),
        )
    }

    pub(crate) fn attach_frame_test_mailbox(
        &mut self,
        session_id: SessionId,
        mailbox: Arc<Mutex<FrameMailbox>>,
    ) {
        let (commands, _command_rx) = mpsc::channel();
        let (_event_tx, events) = mpsc::channel();
        self.active = Some(
            PendingLiveSessionPorts {
                session_id,
                protocol_id: ProtocolId::apple_hpss_mvs(),
                commands,
                events,
                mailbox,
                video_sender: None,
                video_events: VideoWorkerEvents::new(None),
            }
            .accept(),
        );
    }

    pub(crate) fn test_mailbox(&self) -> Arc<Mutex<FrameMailbox>> {
        self.active.as_ref().unwrap().mailbox.clone()
    }
}

// 跨模块呈现回归仅使用这些测试端口，不公开生产状态或工作线程实现。
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum WorkerKind {
        Audio,
        Protocol,
    }

    pub(crate) trait WorkerSpawner: Send + Sync {
        fn spawn(
            &self,
            kind: WorkerKind,
            name: String,
            work: Box<dyn FnOnce() + Send>,
        ) -> io::Result<JoinHandle<()>>;
    }

    pub(super) struct SpawnerAdapter(pub(super) Arc<dyn WorkerSpawner>);

    impl super::WorkerSpawner for SpawnerAdapter {
        fn spawn(
            &self,
            kind: super::WorkerKind,
            name: String,
            work: Box<dyn FnOnce() + Send>,
        ) -> io::Result<JoinHandle<()>> {
            let kind = match kind {
                super::WorkerKind::Audio => WorkerKind::Audio,
                super::WorkerKind::Protocol => WorkerKind::Protocol,
            };
            self.0.spawn(kind, name, work)
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum AudioWorkerExit {
        Closed,
        Failed,
    }

    pub(crate) fn drain_audio_media(
        factory: Arc<dyn AudioOutputFactory>,
        media: mpsc::Receiver<MediaFrame>,
    ) -> AudioWorkerExit {
        match super::drain_audio_media(factory, media) {
            super::AudioWorkerExit::Closed => AudioWorkerExit::Closed,
            super::AudioWorkerExit::Failed => AudioWorkerExit::Failed,
        }
    }

    pub(crate) fn media_publisher(
        audio: mpsc::SyncSender<MediaFrame>,
        video: VideoDecodeSender,
    ) -> impl MediaPublisher {
        DesktopMediaPublisher::new(audio, video)
    }
}
