use std::collections::{HashMap, VecDeque};
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use frd_media_api::{
    ChromaFormat, DecodeOutcome, DecodedVideoFrame, EncodedVideoAccessUnit, VideoDecodeError,
    VideoDecodeErrorCode, VideoDecodeQuery, VideoDecoder, VideoDecoderRegistry,
    VideoDecoderSelection, VideoPixelFormat, VideoStreamConfig, VideoStreamIdentity,
    VideoTimestamp,
};

pub const VIDEO_ACCESS_UNIT_ENTRY_LIMIT: usize = 64;
pub const VIDEO_ACCESS_UNIT_BYTE_LIMIT: usize = 32 * 1024 * 1024;
const VIDEO_EVENT_LIMIT: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoDecoderDiagnostics(VideoDecoderSelection);

impl VideoDecoderDiagnostics {
    pub fn selection(&self) -> &VideoDecoderSelection {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoFrameToken {
    identity: VideoStreamIdentity,
    generation: u64,
    timestamp: VideoTimestamp,
    publication_id: u64,
}

impl VideoFrameToken {
    pub const fn identity(&self) -> VideoStreamIdentity {
        self.identity
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn timestamp(&self) -> VideoTimestamp {
        self.timestamp
    }

    pub const fn publication_id(&self) -> u64 {
        self.publication_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedVideoFrameHandoff {
    token: VideoFrameToken,
    frame: DecodedVideoFrame,
}

impl DecodedVideoFrameHandoff {
    pub const fn token(&self) -> &VideoFrameToken {
        &self.token
    }

    pub const fn frame(&self) -> &DecodedVideoFrame {
        &self.frame
    }

    pub fn into_parts(self) -> (VideoFrameToken, DecodedVideoFrame) {
        (self.token, self.frame)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VideoWorkerEvent {
    BackendSelected {
        identity: VideoStreamIdentity,
        generation: u64,
        diagnostics: VideoDecoderDiagnostics,
    },
    FrameDecoded(DecodedVideoFrameHandoff),
    DecodeFailed {
        identity: VideoStreamIdentity,
        generation: u64,
        code: VideoDecodeErrorCode,
        after_first_frame: bool,
    },
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoWorkerSendError {
    Full,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoWorkerShutdownError {
    TimedOut,
    Panicked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StreamEpoch {
    identity: VideoStreamIdentity,
    generation: u64,
    serial: u64,
}

enum VideoWorkerCommand {
    Config {
        epoch: StreamEpoch,
        config: VideoStreamConfig,
    },
    AccessUnit {
        epoch: StreamEpoch,
        access_unit: EncodedVideoAccessUnit,
    },
}

impl VideoWorkerCommand {
    #[cfg(test)]
    fn into_access_unit(self) -> Option<EncodedVideoAccessUnit> {
        match self {
            Self::AccessUnit { access_unit, .. } => Some(access_unit),
            Self::Config { .. } => None,
        }
    }
}

struct VideoInputState {
    pending_config: Option<(StreamEpoch, VideoStreamConfig)>,
    access_units: VecDeque<(StreamEpoch, EncodedVideoAccessUnit)>,
    access_unit_bytes: usize,
    closed: bool,
}

#[derive(Clone)]
struct VideoInputQueue {
    shared: Arc<(Mutex<VideoInputState>, Condvar)>,
}

impl VideoInputQueue {
    fn new() -> Self {
        Self {
            shared: Arc::new((
                Mutex::new(VideoInputState {
                    pending_config: None,
                    access_units: VecDeque::new(),
                    access_unit_bytes: 0,
                    closed: false,
                }),
                Condvar::new(),
            )),
        }
    }

    fn try_push_config(
        &self,
        epoch: StreamEpoch,
        config: VideoStreamConfig,
    ) -> Result<(), VideoWorkerSendError> {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video input queue mutex poisoned");
        if state.closed {
            return Err(VideoWorkerSendError::Closed);
        }
        state.pending_config = Some((epoch, config));
        state.access_units.clear();
        state.access_unit_bytes = 0;
        wake.notify_one();
        Ok(())
    }

    fn try_push_tagged_access_unit(
        &self,
        epoch: StreamEpoch,
        access_unit: EncodedVideoAccessUnit,
    ) -> Result<(), VideoWorkerSendError> {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video input queue mutex poisoned");
        if state.closed {
            return Err(VideoWorkerSendError::Closed);
        }
        let next_bytes = state
            .access_unit_bytes
            .checked_add(access_unit.bytes().len())
            .unwrap_or(usize::MAX);
        let saturated = state.access_units.len() >= VIDEO_ACCESS_UNIT_ENTRY_LIMIT
            || next_bytes > VIDEO_ACCESS_UNIT_BYTE_LIMIT;
        if saturated && !access_unit.random_access() {
            return Err(VideoWorkerSendError::Full);
        }
        if saturated {
            state.access_units.clear();
            state.access_unit_bytes = 0;
        }
        state.access_unit_bytes += access_unit.bytes().len();
        state.access_units.push_back((epoch, access_unit));
        wake.notify_one();
        Ok(())
    }

    #[cfg(test)]
    fn try_push_access_unit(
        &self,
        access_unit: EncodedVideoAccessUnit,
    ) -> Result<(), VideoWorkerSendError> {
        let epoch = StreamEpoch {
            identity: access_unit.identity(),
            generation: access_unit.generation(),
            serial: 1,
        };
        self.try_push_tagged_access_unit(epoch, access_unit)
    }

    fn pop(&self) -> Option<VideoWorkerCommand> {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video input queue mutex poisoned");
        loop {
            if let Some((epoch, config)) = state.pending_config.take() {
                return Some(VideoWorkerCommand::Config { epoch, config });
            }
            if let Some((epoch, access_unit)) = state.access_units.pop_front() {
                state.access_unit_bytes -= access_unit.bytes().len();
                return Some(VideoWorkerCommand::AccessUnit { epoch, access_unit });
            }
            if state.closed {
                return None;
            }
            state = wake
                .wait(state)
                .expect("video input queue mutex poisoned while waiting");
        }
    }

    #[cfg(test)]
    fn try_pop(&self) -> Option<VideoWorkerCommand> {
        let (lock, _) = &*self.shared;
        let mut state = lock.lock().expect("video input queue mutex poisoned");
        if let Some((epoch, config)) = state.pending_config.take() {
            return Some(VideoWorkerCommand::Config { epoch, config });
        }
        state.access_units.pop_front().map(|(epoch, access_unit)| {
            state.access_unit_bytes -= access_unit.bytes().len();
            VideoWorkerCommand::AccessUnit { epoch, access_unit }
        })
    }

    fn close(&self) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video input queue mutex poisoned");
        state.closed = true;
        state.pending_config = None;
        state.access_units.clear();
        state.access_unit_bytes = 0;
        wake.notify_all();
    }

    fn is_closed(&self) -> bool {
        self.shared
            .0
            .lock()
            .expect("video input queue mutex poisoned")
            .closed
    }

    fn latest_epoch(&self) -> Option<StreamEpoch> {
        let state = self
            .shared
            .0
            .lock()
            .expect("video input queue mutex poisoned");
        state
            .pending_config
            .as_ref()
            .map(|(epoch, _)| *epoch)
            .or_else(|| state.access_units.back().map(|(epoch, _)| *epoch))
    }

    fn has_pending_config(&self) -> bool {
        self.shared
            .0
            .lock()
            .expect("video input queue mutex poisoned")
            .pending_config
            .is_some()
    }
}

struct StreamPresentationState {
    epoch: StreamEpoch,
    latest_token: Option<VideoFrameToken>,
    latest_frame: Option<DecodedVideoFrame>,
    delivered_token: Option<VideoFrameToken>,
    ready: bool,
}

struct VideoEventState {
    controls: VecDeque<VideoWorkerEvent>,
    streams: HashMap<VideoStreamIdentity, StreamPresentationState>,
    next_epoch: u64,
    next_publication: u64,
}

#[derive(Clone)]
pub struct VideoWorkerEvents {
    shared: Arc<(Mutex<VideoEventState>, Condvar)>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl VideoWorkerEvents {
    pub(crate) fn new(wake: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        Self {
            shared: Arc::new((
                Mutex::new(VideoEventState {
                    controls: VecDeque::new(),
                    streams: HashMap::new(),
                    next_epoch: 1,
                    next_publication: 1,
                }),
                Condvar::new(),
            )),
            wake,
        }
    }

    fn accept_config(&self, identity: VideoStreamIdentity, generation: u64) -> StreamEpoch {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video event mutex poisoned");
        let serial = state.next_epoch;
        state.next_epoch = state
            .next_epoch
            .checked_add(1)
            .expect("video epoch exhausted");
        let epoch = StreamEpoch {
            identity,
            generation,
            serial,
        };
        state.streams.insert(
            identity,
            StreamPresentationState {
                epoch,
                latest_token: None,
                latest_frame: None,
                delivered_token: None,
                ready: false,
            },
        );
        wake.notify_all();
        epoch
    }

    fn current_epoch(&self, identity: VideoStreamIdentity, generation: u64) -> Option<StreamEpoch> {
        self.shared
            .0
            .lock()
            .expect("video event mutex poisoned")
            .streams
            .get(&identity)
            .filter(|stream| stream.epoch.generation == generation)
            .map(|stream| stream.epoch)
    }

    fn is_current(&self, epoch: StreamEpoch) -> bool {
        self.current_epoch(epoch.identity, epoch.generation) == Some(epoch)
    }

    fn publish_selected(&self, epoch: StreamEpoch, diagnostics: VideoDecoderDiagnostics) {
        if !self.is_current(epoch) {
            return;
        }
        self.publish_control(VideoWorkerEvent::BackendSelected {
            identity: epoch.identity,
            generation: epoch.generation,
            diagnostics,
        });
    }

    fn publish_failure(
        &self,
        epoch: StreamEpoch,
        code: VideoDecodeErrorCode,
        after_first_frame: bool,
    ) {
        if !self.is_current(epoch) {
            return;
        }
        self.publish_control(VideoWorkerEvent::DecodeFailed {
            identity: epoch.identity,
            generation: epoch.generation,
            code,
            after_first_frame,
        });
    }

    fn publish_control(&self, event: VideoWorkerEvent) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video event mutex poisoned");
        while state.controls.len() >= VIDEO_EVENT_LIMIT {
            state.controls.pop_front();
        }
        state.controls.push_back(event);
        wake.notify_all();
        drop(state);
        self.notify_wake();
    }

    fn publish_frame(&self, epoch: StreamEpoch, frame: DecodedVideoFrame) {
        let input = frame.as_input();
        if input.identity != epoch.identity || input.generation != epoch.generation {
            return;
        }
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video event mutex poisoned");
        if state
            .streams
            .get(&epoch.identity)
            .map(|stream| stream.epoch)
            != Some(epoch)
        {
            return;
        }
        let publication_id = state.next_publication;
        state.next_publication = state
            .next_publication
            .checked_add(1)
            .expect("video publication id exhausted");
        let token = VideoFrameToken {
            identity: epoch.identity,
            generation: epoch.generation,
            timestamp: input.timestamp,
            publication_id,
        };
        let stream = state
            .streams
            .get_mut(&epoch.identity)
            .expect("current stream remains present");
        stream.latest_token = Some(token);
        stream.latest_frame = Some(frame);
        stream.delivered_token = None;
        wake.notify_all();
        drop(state);
        self.notify_wake();
    }

    pub fn take_latest_frame(
        &self,
        identity: VideoStreamIdentity,
    ) -> Option<DecodedVideoFrameHandoff> {
        let mut state = self.shared.0.lock().expect("video event mutex poisoned");
        take_frame_from_state(&mut state, identity)
    }

    pub fn has_latest_frame(&self, identity: VideoStreamIdentity) -> bool {
        self.shared
            .0
            .lock()
            .expect("video event mutex poisoned")
            .streams
            .get(&identity)
            .is_some_and(|stream| stream.latest_frame.is_some())
    }

    #[cfg(test)]
    fn latest_timestamp(&self, identity: VideoStreamIdentity) -> Option<VideoTimestamp> {
        self.shared
            .0
            .lock()
            .expect("video event mutex poisoned")
            .streams
            .get(&identity)
            .and_then(|stream| stream.latest_token)
            .map(|token| token.timestamp)
    }

    pub fn try_recv(&self) -> Option<VideoWorkerEvent> {
        let mut state = self.shared.0.lock().expect("video event mutex poisoned");
        pop_event(&mut state)
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<VideoWorkerEvent> {
        let deadline = Instant::now() + timeout;
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video event mutex poisoned");
        loop {
            if let Some(event) = pop_event(&mut state) {
                return Some(event);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (next, wait) = wake
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .expect("video event mutex poisoned while waiting");
            state = next;
            if wait.timed_out() && !event_available(&state) {
                return None;
            }
        }
    }

    pub fn confirm_presented(&self, token: &VideoFrameToken) -> Result<(), VideoDecodeErrorCode> {
        let mut state = self.shared.0.lock().expect("video event mutex poisoned");
        let Some(stream) = state.streams.get_mut(&token.identity) else {
            return Err(VideoDecodeErrorCode::StaleStreamOrGeneration);
        };
        if stream.epoch.generation != token.generation
            || stream.latest_token != Some(*token)
            || stream.delivered_token != Some(*token)
        {
            return Err(VideoDecodeErrorCode::StaleStreamOrGeneration);
        }
        stream.ready = true;
        Ok(())
    }

    pub fn is_ready(&self, identity: VideoStreamIdentity, generation: u64) -> bool {
        self.shared
            .0
            .lock()
            .expect("video event mutex poisoned")
            .streams
            .get(&identity)
            .is_some_and(|stream| stream.epoch.generation == generation && stream.ready)
    }

    fn publish_stopped(&self) {
        self.publish_control(VideoWorkerEvent::Stopped);
    }

    fn notify_wake(&self) {
        if let Some(wake) = &self.wake {
            wake();
        }
    }
}

fn event_available(state: &VideoEventState) -> bool {
    !state.controls.is_empty()
        || state
            .streams
            .values()
            .any(|stream| stream.latest_frame.is_some())
}

fn pop_event(state: &mut VideoEventState) -> Option<VideoWorkerEvent> {
    if !matches!(state.controls.front(), Some(VideoWorkerEvent::Stopped)) {
        if let Some(event) = state.controls.pop_front() {
            return Some(event);
        }
    }
    let identity = state
        .streams
        .iter()
        .filter_map(|(identity, stream)| {
            stream
                .latest_frame
                .as_ref()
                .zip(stream.latest_token)
                .map(|(_, token)| (*identity, token.publication_id))
        })
        .min_by_key(|(_, publication_id)| *publication_id)
        .map(|(identity, _)| identity);
    if let Some(identity) = identity {
        return take_frame_from_state(state, identity).map(VideoWorkerEvent::FrameDecoded);
    }
    let event = state.controls.pop_front()?;
    if event == VideoWorkerEvent::Stopped {
        state.streams.clear();
    }
    Some(event)
}

fn take_frame_from_state(
    state: &mut VideoEventState,
    identity: VideoStreamIdentity,
) -> Option<DecodedVideoFrameHandoff> {
    let stream = state.streams.get_mut(&identity)?;
    let token = stream.latest_token?;
    let frame = stream.latest_frame.take()?;
    stream.delivered_token = Some(token);
    Some(DecodedVideoFrameHandoff { token, frame })
}

struct StreamRoute {
    input: VideoInputQueue,
    started: bool,
}

struct VideoRouterState {
    streams: HashMap<VideoStreamIdentity, StreamRoute>,
    closed: bool,
}

#[derive(Clone)]
struct VideoRouter {
    shared: Arc<(Mutex<VideoRouterState>, Condvar)>,
    events: VideoWorkerEvents,
}

impl VideoRouter {
    fn new(events: VideoWorkerEvents) -> Self {
        Self {
            shared: Arc::new((
                Mutex::new(VideoRouterState {
                    streams: HashMap::new(),
                    closed: false,
                }),
                Condvar::new(),
            )),
            events,
        }
    }

    fn try_send_config(&self, config: VideoStreamConfig) -> Result<(), VideoWorkerSendError> {
        let input = config.as_input();
        let identity = input.identity;
        let generation = input.generation;
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video router mutex poisoned");
        if state.closed {
            return Err(VideoWorkerSendError::Closed);
        }
        let replace = state
            .streams
            .get(&identity)
            .is_none_or(|route| route.input.is_closed());
        if replace {
            state.streams.insert(
                identity,
                StreamRoute {
                    input: VideoInputQueue::new(),
                    started: false,
                },
            );
        }
        let epoch = self.events.accept_config(identity, generation);
        let result = state
            .streams
            .get(&identity)
            .expect("stream route was inserted")
            .input
            .try_push_config(epoch, config);
        if result.is_ok() {
            wake.notify_all();
        }
        result
    }

    fn try_send_access_unit(
        &self,
        access_unit: EncodedVideoAccessUnit,
    ) -> Result<(), VideoWorkerSendError> {
        let (lock, _) = &*self.shared;
        let state = lock.lock().expect("video router mutex poisoned");
        if state.closed {
            return Err(VideoWorkerSendError::Closed);
        }
        let Some(epoch) = self
            .events
            .current_epoch(access_unit.identity(), access_unit.generation())
        else {
            return Ok(());
        };
        let Some(route) = state.streams.get(&access_unit.identity()) else {
            return Ok(());
        };
        route.input.try_push_tagged_access_unit(epoch, access_unit)
    }

    fn close(&self) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video router mutex poisoned");
        if state.closed {
            return;
        }
        state.closed = true;
        for route in state.streams.values() {
            route.input.close();
        }
        wake.notify_all();
    }

    fn finish_stream(&self, identity: VideoStreamIdentity, input: &VideoInputQueue) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video router mutex poisoned");
        if let Some(route) = state
            .streams
            .get_mut(&identity)
            .filter(|route| Arc::ptr_eq(&route.input.shared, &input.shared))
        {
            route.started = false;
            if !input.has_pending_config() {
                input.close();
            }
        }
        wake.notify_all();
    }

    fn abort_stream(&self, identity: VideoStreamIdentity, input: &VideoInputQueue) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video router mutex poisoned");
        if let Some(route) = state
            .streams
            .get_mut(&identity)
            .filter(|route| Arc::ptr_eq(&route.input.shared, &input.shared))
        {
            route.started = false;
            input.close();
        }
        wake.notify_all();
    }
}

#[derive(Clone)]
pub struct VideoDecodeSender {
    router: VideoRouter,
}

impl VideoDecodeSender {
    pub fn try_send_config(&self, config: VideoStreamConfig) -> Result<(), VideoWorkerSendError> {
        self.router.try_send_config(config)
    }

    pub fn try_send_access_unit(
        &self,
        access_unit: EncodedVideoAccessUnit,
    ) -> Result<(), VideoWorkerSendError> {
        self.router.try_send_access_unit(access_unit)
    }
}

type StreamRegistryLoader = Arc<
    dyn Fn(VideoStreamIdentity) -> Result<VideoDecoderRegistry, VideoDecodeError> + Send + Sync,
>;

#[cfg(test)]
type RegistryLoader = Box<dyn FnOnce() -> Result<VideoDecoderRegistry, VideoDecodeError> + Send>;

pub struct VideoDecodeWorker {
    sender: VideoDecodeSender,
    events: VideoWorkerEvents,
    supervisor: Option<JoinHandle<()>>,
}

impl VideoDecodeWorker {
    pub fn spawn(wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        Self::spawn_inner(
            Arc::new(|_identity| {
                frd_video_ffmpeg::FfmpegBackend::load()
                    .map(|backend| VideoDecoderRegistry::new(vec![Box::new(backend)]))
            }),
            Some(wake),
        )
    }

    #[cfg(test)]
    pub(crate) fn spawn_with_registry_loader(loader: RegistryLoader) -> io::Result<Self> {
        let loader = Arc::new(Mutex::new(Some(loader)));
        Self::spawn_with_stream_registry_loader(Arc::new(move |_identity| {
            let loader = loader
                .lock()
                .expect("test registry loader mutex poisoned")
                .take()
                .ok_or_else(|| VideoDecodeError::new(VideoDecodeErrorCode::BackendUnavailable))?;
            loader()
        }))
    }

    #[cfg(test)]
    pub(crate) fn spawn_with_stream_registry_loader(
        loader: StreamRegistryLoader,
    ) -> io::Result<Self> {
        Self::spawn_inner(loader, None)
    }

    fn spawn_inner(
        loader: StreamRegistryLoader,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> io::Result<Self> {
        let events = VideoWorkerEvents::new(wake);
        let router = VideoRouter::new(events.clone());
        let supervisor_router = router.clone();
        let supervisor_events = events.clone();
        let supervisor = std::thread::Builder::new()
            .name("frd-video-supervisor".to_string())
            .spawn(move || {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    run_supervisor(supervisor_router.clone(), supervisor_events.clone(), loader)
                }));
                supervisor_router.close();
                supervisor_events.publish_stopped();
            })?;
        Ok(Self {
            sender: VideoDecodeSender { router },
            events,
            supervisor: Some(supervisor),
        })
    }

    pub fn sender(&self) -> VideoDecodeSender {
        self.sender.clone()
    }

    pub fn events(&self) -> VideoWorkerEvents {
        self.events.clone()
    }

    pub fn request_stop(&self) {
        self.sender.router.close();
    }

    pub fn poll_join(&mut self) -> Result<bool, VideoWorkerShutdownError> {
        let Some(supervisor) = self.supervisor.as_ref() else {
            return Ok(true);
        };
        if !supervisor.is_finished() {
            return Ok(false);
        }
        self.supervisor
            .take()
            .expect("finished video supervisor remains owned")
            .join()
            .map(|_| true)
            .map_err(|_| VideoWorkerShutdownError::Panicked)
    }

    pub fn join_timeout(mut self, timeout: Duration) -> Result<(), VideoWorkerShutdownError> {
        self.request_stop();
        let deadline = Instant::now() + timeout;
        loop {
            if self.poll_join()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(VideoWorkerShutdownError::TimedOut);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Drop for VideoDecodeWorker {
    fn drop(&mut self) {
        self.request_stop();
    }
}

fn run_supervisor(router: VideoRouter, events: VideoWorkerEvents, loader: StreamRegistryLoader) {
    let mut workers: HashMap<VideoStreamIdentity, JoinHandle<()>> = HashMap::new();
    loop {
        let finished = workers
            .iter()
            .filter_map(|(identity, worker)| worker.is_finished().then_some(*identity))
            .collect::<Vec<_>>();
        for identity in finished {
            if let Some(worker) = workers.remove(&identity) {
                let _ = worker.join();
            }
        }

        let (closed, starts) = {
            let (lock, wake) = &*router.shared;
            let mut state = lock.lock().expect("video router mutex poisoned");
            let starts = state
                .streams
                .iter_mut()
                .filter_map(|(identity, route)| {
                    (!route.started && !workers.contains_key(identity)).then(|| {
                        route.started = true;
                        (*identity, route.input.clone())
                    })
                })
                .collect::<Vec<_>>();
            if starts.is_empty() && !state.closed {
                let (next, _) = wake
                    .wait_timeout(state, Duration::from_millis(25))
                    .expect("video router mutex poisoned while waiting");
                state = next;
            }
            (state.closed, starts)
        };

        for (identity, input) in starts {
            let worker_events = events.clone();
            let worker_loader = loader.clone();
            let worker_router = router.clone();
            let worker_input = input.clone();
            match std::thread::Builder::new()
                .name(format!(
                    "frd-video-{}-{}",
                    identity.session_id.get(),
                    identity.stream_id
                ))
                .spawn(move || {
                    let _ = catch_unwind(AssertUnwindSafe(|| {
                        run_stream_worker(
                            identity,
                            worker_input.clone(),
                            worker_events,
                            worker_loader,
                        )
                    }));
                    worker_router.finish_stream(identity, &worker_input);
                }) {
                Ok(worker) => {
                    workers.insert(identity, worker);
                }
                Err(_) => {
                    if let Some(epoch) = input.latest_epoch() {
                        events.publish_failure(
                            epoch,
                            VideoDecodeErrorCode::BackendUnavailable,
                            false,
                        );
                    }
                    router.abort_stream(identity, &input);
                }
            }
        }

        if closed {
            for worker in workers.into_values() {
                let _ = worker.join();
            }
            return;
        }
    }
}

struct ActiveDecoder {
    epoch: StreamEpoch,
    decoder: Box<dyn VideoDecoder>,
    after_first_frame: bool,
}

fn run_stream_worker(
    identity: VideoStreamIdentity,
    input: VideoInputQueue,
    events: VideoWorkerEvents,
    loader: StreamRegistryLoader,
) {
    let mut registry = None;
    let mut active: Option<ActiveDecoder> = None;
    while let Some(command) = input.pop() {
        match command {
            VideoWorkerCommand::Config { epoch, config } => {
                if epoch.identity != identity {
                    continue;
                }
                if let Some(mut previous) = active.take() {
                    if let Ok(frames) = previous.decoder.flush() {
                        publish_current_frames(&events, &mut previous, frames);
                    }
                }
                if registry.is_none() {
                    match loader(identity) {
                        Ok(loaded) => registry = Some(loaded),
                        Err(error) => {
                            events.publish_failure(epoch, error.code(), false);
                            if events.is_current(epoch) {
                                return;
                            }
                            continue;
                        }
                    }
                }
                let created = registry
                    .as_ref()
                    .expect("loaded registry remains available")
                    .select_and_create(&query_for_config(&config), &config);
                match created {
                    Ok(created) => {
                        let (selection, decoder) = created.into_parts();
                        events.publish_selected(epoch, VideoDecoderDiagnostics(selection));
                        active = Some(ActiveDecoder {
                            epoch,
                            decoder,
                            after_first_frame: false,
                        });
                    }
                    Err(error) => {
                        events.publish_failure(epoch, error.code(), false);
                        if events.is_current(epoch) {
                            return;
                        }
                    }
                }
            }
            VideoWorkerCommand::AccessUnit { epoch, access_unit } => {
                let Some(decoder) = active.as_mut() else {
                    continue;
                };
                if decoder.epoch != epoch || !events.is_current(epoch) {
                    continue;
                }
                match decoder.decoder.submit(access_unit) {
                    Ok(DecodeOutcome::NeedMoreData) => {}
                    Ok(DecodeOutcome::Frames(frames)) => {
                        publish_current_frames(&events, decoder, frames)
                    }
                    Err(error) => {
                        events.publish_failure(epoch, error.code(), decoder.after_first_frame);
                        if events.is_current(epoch) {
                            return;
                        }
                        active = None;
                    }
                }
            }
        }
    }

    if let Some(mut active) = active {
        if let Ok(frames) = active.decoder.flush() {
            publish_current_frames(&events, &mut active, frames);
        }
    }
}

fn query_for_config(config: &VideoStreamConfig) -> VideoDecodeQuery {
    let input = config.as_input();
    let preferred_outputs: Box<[VideoPixelFormat]> = match (input.chroma, input.bit_depth) {
        (ChromaFormat::Yuv444, 8) => vec![VideoPixelFormat::Yuv444P8].into_boxed_slice(),
        (ChromaFormat::Yuv420, 8) => {
            vec![VideoPixelFormat::Nv12, VideoPixelFormat::Yuv420P8].into_boxed_slice()
        }
        (ChromaFormat::Yuv420, 10) => vec![VideoPixelFormat::P010].into_boxed_slice(),
        _ => Box::default(),
    };
    VideoDecodeQuery {
        codec: input.codec,
        profile: input.profile,
        chroma: input.chroma,
        bit_depth: input.bit_depth,
        coded_size: input.coded_size,
        frame_rate: None,
        preferred_outputs,
    }
}

fn publish_current_frames(
    events: &VideoWorkerEvents,
    active: &mut ActiveDecoder,
    frames: Box<[DecodedVideoFrame]>,
) {
    for frame in frames {
        let input = frame.as_input();
        if input.identity == active.epoch.identity && input.generation == active.epoch.generation {
            active.after_first_frame = true;
            events.publish_frame(active.epoch, frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use frd_core::{PixelRect, PixelSize, SessionId};
    use frd_media_api::{
        ChromaFormat, ChromaLocation, DecodeOutcome, DecodedVideoFrame, DecodedVideoFrameInput,
        EncodedVideoAccessUnit, VideoBackendAvailability, VideoBackendId, VideoBackendKind,
        VideoBitstreamFormat, VideoCapabilityProvider, VideoCodec, VideoColorimetry,
        VideoDecodeCapability, VideoDecodeError, VideoDecodeErrorCode, VideoDecodeQuery,
        VideoDecodeSupport, VideoDecoder, VideoDecoderFactory, VideoDecoderRegistry,
        VideoParameterSets, VideoPixelFormat, VideoPlane, VideoProfile, VideoRange,
        VideoStreamConfig, VideoStreamConfigInput, VideoStreamIdentity, VideoTimeBase,
        VideoTimestamp,
    };

    use super::{
        VideoDecodeWorker, VideoInputQueue, VideoWorkerEvent, VideoWorkerSendError,
        VIDEO_ACCESS_UNIT_BYTE_LIMIT, VIDEO_ACCESS_UNIT_ENTRY_LIMIT,
    };

    #[test]
    fn input_queue_enforces_access_unit_count_and_byte_budgets() {
        let count_queue = VideoInputQueue::new();
        for tick in 0..VIDEO_ACCESS_UNIT_ENTRY_LIMIT {
            assert_eq!(
                count_queue.try_push_access_unit(test_au(7, tick as u64, false, 1)),
                Ok(())
            );
        }
        assert_eq!(
            count_queue.try_push_access_unit(test_au(7, 65, false, 1)),
            Err(VideoWorkerSendError::Full)
        );

        let byte_queue = VideoInputQueue::new();
        let chunk = 2 * 1024 * 1024;
        for tick in 0..(VIDEO_ACCESS_UNIT_BYTE_LIMIT / chunk) {
            assert_eq!(
                byte_queue.try_push_access_unit(test_au(7, tick as u64, false, chunk)),
                Ok(())
            );
        }
        assert_eq!(
            byte_queue.try_push_access_unit(test_au(7, 17, false, 1)),
            Err(VideoWorkerSendError::Full)
        );
    }

    #[test]
    fn saturated_queue_replaces_inter_frames_with_latest_random_access_point() {
        let queue = VideoInputQueue::new();
        for tick in 0..VIDEO_ACCESS_UNIT_ENTRY_LIMIT {
            queue
                .try_push_access_unit(test_au(7, tick as u64, false, 1))
                .unwrap();
        }

        queue
            .try_push_access_unit(test_au(7, 99, true, 3))
            .expect("最新 random-access AU 必须成为恢复点");

        let command = queue.pop().expect("队列应保留恢复点");
        let access_unit = command.into_access_unit().expect("应只剩 AU");
        assert!(access_unit.random_access());
        assert_eq!(access_unit.timestamp().ticks, 99);
        assert!(queue.try_pop().is_none());
    }

    #[test]
    fn stale_generation_is_discarded_before_decoder_submit() {
        let submits = Arc::new(AtomicUsize::new(0));
        let worker = worker_with_script(DecoderScript::Echo, submits.clone());
        worker.sender().try_send_config(test_config(7)).unwrap();
        recv_backend_selected(&worker);

        worker
            .sender()
            .try_send_access_unit(test_au(6, 1, true, 1))
            .unwrap();
        std::thread::sleep(Duration::from_millis(30));

        assert_eq!(submits.load(Ordering::Acquire), 0);
        assert!(worker.events().try_recv().is_none());
        stop(worker);
    }

    #[test]
    fn latest_frame_slot_keeps_only_the_latest_current_generation_frame() {
        let submits = Arc::new(AtomicUsize::new(0));
        let worker = worker_with_script(DecoderScript::Echo, submits.clone());
        worker.sender().try_send_config(test_config(7)).unwrap();
        recv_backend_selected(&worker);
        for tick in 1..=3 {
            worker
                .sender()
                .try_send_access_unit(test_au(7, tick, tick == 1, 1))
                .unwrap();
        }
        wait_until(|| {
            submits.load(Ordering::Acquire) == 3
                && worker
                    .events()
                    .latest_timestamp(test_identity())
                    .map(|timestamp| timestamp.ticks)
                    == Some(3)
        });

        let VideoWorkerEvent::FrameDecoded(frame) = recv_event(&worker) else {
            panic!("应收到 latest frame");
        };
        assert_eq!(frame.frame().as_input().generation, 7);
        assert_eq!(frame.frame().as_input().timestamp.ticks, 3);
        assert!(worker.events().try_recv().is_none());
        stop(worker);
    }

    #[test]
    fn ready_is_not_emitted_until_current_generation_frame_is_accepted() {
        let worker = worker_with_script(DecoderScript::Echo, Arc::new(AtomicUsize::new(0)));
        worker.sender().try_send_config(test_config(7)).unwrap();
        recv_backend_selected(&worker);
        worker
            .sender()
            .try_send_access_unit(test_au(7, 1, true, 1))
            .unwrap();
        let VideoWorkerEvent::FrameDecoded(frame) = recv_event(&worker) else {
            panic!("应收到 decoded frame handoff");
        };
        let identity = test_identity();
        assert!(!worker.events().is_ready(identity, 7));
        worker.events().confirm_presented(frame.token()).unwrap();
        assert!(worker.events().is_ready(identity, 7));
        stop(worker);
    }

    #[test]
    fn decoder_failure_reports_whether_a_frame_was_already_decoded() {
        let before = worker_with_script(DecoderScript::FailBefore, Arc::new(AtomicUsize::new(0)));
        before.sender().try_send_config(test_config(7)).unwrap();
        recv_backend_selected(&before);
        before
            .sender()
            .try_send_access_unit(test_au(7, 1, true, 1))
            .unwrap();
        assert_eq!(
            recv_event(&before),
            VideoWorkerEvent::DecodeFailed {
                identity: test_identity(),
                generation: 7,
                code: VideoDecodeErrorCode::DecodeFailedBeforeFirstFrame,
                after_first_frame: false,
            }
        );
        stop(before);

        let after = worker_with_script(DecoderScript::FrameThenFail, Arc::new(AtomicUsize::new(0)));
        after.sender().try_send_config(test_config(7)).unwrap();
        recv_backend_selected(&after);
        after
            .sender()
            .try_send_access_unit(test_au(7, 1, true, 1))
            .unwrap();
        assert!(matches!(
            recv_event(&after),
            VideoWorkerEvent::FrameDecoded(_)
        ));
        after
            .sender()
            .try_send_access_unit(test_au(7, 2, false, 1))
            .unwrap();
        assert_eq!(
            recv_event(&after),
            VideoWorkerEvent::DecodeFailed {
                identity: test_identity(),
                generation: 7,
                code: VideoDecodeErrorCode::DecodeFailedAfterFirstFrame,
                after_first_frame: true,
            }
        );
        stop(after);
    }

    #[test]
    fn backend_load_failure_is_reported_without_panicking_the_process() {
        let worker = VideoDecodeWorker::spawn_with_registry_loader(Box::new(|| {
            Err(VideoDecodeError::new(
                VideoDecodeErrorCode::BackendUnavailable,
            ))
        }))
        .expect("worker thread 应可启动");

        worker.sender().try_send_config(test_config(7)).unwrap();

        assert_eq!(
            recv_event(&worker),
            VideoWorkerEvent::DecodeFailed {
                identity: test_identity(),
                generation: 7,
                code: VideoDecodeErrorCode::BackendUnavailable,
                after_first_frame: false,
            }
        );
        stop(worker);
    }

    #[test]
    fn shutdown_flushes_once_publishes_the_latest_frame_and_stops_within_deadline() {
        let flushes = Arc::new(AtomicUsize::new(0));
        let worker = VideoDecodeWorker::spawn_with_registry_loader(Box::new({
            let flushes = flushes.clone();
            move || {
                Ok(registry(
                    DecoderScript::FlushFrame(flushes),
                    Arc::new(AtomicUsize::new(0)),
                ))
            }
        }))
        .expect("worker thread 应可启动");
        worker.sender().try_send_config(test_config(7)).unwrap();
        recv_backend_selected(&worker);

        worker.request_stop();
        let events = collect_until_stopped(&worker);
        worker
            .join_timeout(Duration::from_secs(1))
            .expect("worker 应在单一短 deadline 内退出");

        assert_eq!(flushes.load(Ordering::Acquire), 1);
        assert!(events
            .iter()
            .any(|event| matches!(event, VideoWorkerEvent::FrameDecoded(frame) if frame.frame().as_input().generation == 7)));
        assert_eq!(events.last(), Some(&VideoWorkerEvent::Stopped));
    }

    #[test]
    fn same_session_stream_backend_loads_are_isolated() {
        let session_id = SessionId::allocate();
        let blocked = identity_for(session_id, 1);
        let healthy = identity_for(session_id, 2);
        let (load_entered_tx, load_entered_rx) = std::sync::mpsc::channel();
        let (release_load_tx, release_load_rx) = std::sync::mpsc::channel();
        let release_load_rx = Arc::new(Mutex::new(release_load_rx));
        let worker = VideoDecodeWorker::spawn_with_stream_registry_loader(Arc::new({
            let release_load_rx = release_load_rx.clone();
            move |identity| {
                if identity == blocked {
                    load_entered_tx.send(()).unwrap();
                    release_load_rx.lock().unwrap().recv().unwrap();
                }
                Ok(registry(DecoderScript::Echo, Arc::new(AtomicUsize::new(0))))
            }
        }))
        .expect("supervisor 应启动");

        worker
            .sender()
            .try_send_config(test_config_for(blocked, 7))
            .unwrap();
        load_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("stream 1 loader 应进入阻塞点");
        worker
            .sender()
            .try_send_config(test_config_for(healthy, 7))
            .unwrap();
        recv_backend_selected_for(&worker, healthy, 7);
        worker
            .sender()
            .try_send_access_unit(test_au_for(healthy, 7, 1, true, 1))
            .unwrap();

        let handoff = recv_frame_for(&worker, healthy);
        assert_eq!(handoff.frame().as_input().identity, healthy);

        release_load_tx.send(()).unwrap();
        stop(worker);
    }

    #[test]
    fn same_session_stream_submits_are_isolated() {
        let session_id = SessionId::allocate();
        let blocked = identity_for(session_id, 11);
        let healthy = identity_for(session_id, 12);
        let (submit_entered_tx, submit_entered_rx) = std::sync::mpsc::channel();
        let (release_submit_tx, release_submit_rx) = std::sync::mpsc::channel();
        let release_submit_rx = Arc::new(Mutex::new(release_submit_rx));
        let worker = VideoDecodeWorker::spawn_with_stream_registry_loader(Arc::new({
            let release_submit_rx = release_submit_rx.clone();
            move |identity| {
                let script = if identity == blocked {
                    DecoderScript::BlockingSubmit {
                        entered: submit_entered_tx.clone(),
                        release: release_submit_rx.clone(),
                        block_after_calls: 0,
                    }
                } else {
                    DecoderScript::Echo
                };
                Ok(registry(script, Arc::new(AtomicUsize::new(0))))
            }
        }))
        .expect("supervisor 应启动");

        for identity in [blocked, healthy] {
            worker
                .sender()
                .try_send_config(test_config_for(identity, 7))
                .unwrap();
            recv_backend_selected_for(&worker, identity, 7);
        }
        worker
            .sender()
            .try_send_access_unit(test_au_for(blocked, 7, 1, true, 1))
            .unwrap();
        submit_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("stream 11 submit 应进入阻塞点");
        worker
            .sender()
            .try_send_access_unit(test_au_for(healthy, 7, 1, true, 1))
            .unwrap();

        assert_eq!(
            recv_frame_for(&worker, healthy).frame().as_input().identity,
            healthy
        );

        release_submit_tx.send(()).unwrap();
        stop(worker);
    }

    #[test]
    fn config_ingress_invalidates_ready_and_drops_late_flush_before_codec_unblocks() {
        let identity = test_identity();
        let (flush_entered_tx, flush_entered_rx) = std::sync::mpsc::channel();
        let (release_flush_tx, release_flush_rx) = std::sync::mpsc::channel();
        let release_flush_rx = Arc::new(Mutex::new(release_flush_rx));
        let worker = VideoDecodeWorker::spawn_with_stream_registry_loader(Arc::new({
            let release_flush_rx = release_flush_rx.clone();
            move |_identity| {
                Ok(registry(
                    DecoderScript::BlockingFlush {
                        entered: flush_entered_tx.clone(),
                        release: release_flush_rx.clone(),
                        blocked: Arc::new(AtomicBool::new(false)),
                    },
                    Arc::new(AtomicUsize::new(0)),
                ))
            }
        }))
        .expect("supervisor 应启动");
        worker
            .sender()
            .try_send_config(test_config_for(identity, 7))
            .unwrap();
        recv_backend_selected_for(&worker, identity, 7);
        worker
            .sender()
            .try_send_access_unit(test_au_for(identity, 7, 1, true, 1))
            .unwrap();
        let first = recv_frame_for(&worker, identity);
        worker.events().confirm_presented(first.token()).unwrap();
        assert!(worker.events().is_ready(identity, 7));

        worker
            .sender()
            .try_send_access_unit(test_au_for(identity, 7, 2, false, 1))
            .unwrap();
        wait_until(|| worker.events().has_latest_frame(identity));
        worker
            .sender()
            .try_send_config(test_config_for(identity, 8))
            .unwrap();

        assert!(!worker.events().is_ready(identity, 7));
        assert!(!worker.events().has_latest_frame(identity));
        flush_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("旧 decoder flush 应阻塞在 codec thread");
        release_flush_tx.send(()).unwrap();
        recv_backend_selected_for(&worker, identity, 8);
        assert_no_frame_for_generation(&worker, identity, 7);
        stop(worker);
    }

    #[test]
    fn config_ingress_drops_a_late_submit_from_the_previous_epoch() {
        let identity = test_identity();
        let (submit_entered_tx, submit_entered_rx) = std::sync::mpsc::channel();
        let (release_submit_tx, release_submit_rx) = std::sync::mpsc::channel();
        let release_submit_rx = Arc::new(Mutex::new(release_submit_rx));
        let worker = VideoDecodeWorker::spawn_with_stream_registry_loader(Arc::new({
            let release_submit_rx = release_submit_rx.clone();
            move |_identity| {
                Ok(registry(
                    DecoderScript::BlockingSubmit {
                        entered: submit_entered_tx.clone(),
                        release: release_submit_rx.clone(),
                        block_after_calls: 0,
                    },
                    Arc::new(AtomicUsize::new(0)),
                ))
            }
        }))
        .expect("supervisor 应启动");
        worker
            .sender()
            .try_send_config(test_config_for(identity, 7))
            .unwrap();
        recv_backend_selected_for(&worker, identity, 7);
        worker
            .sender()
            .try_send_access_unit(test_au_for(identity, 7, 1, true, 1))
            .unwrap();
        submit_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("旧 submit 应进入阻塞点");

        worker
            .sender()
            .try_send_config(test_config_for(identity, 8))
            .unwrap();
        assert!(!worker.events().has_latest_frame(identity));
        release_submit_tx.send(()).unwrap();
        recv_backend_selected_for(&worker, identity, 8);
        assert_no_frame_for_generation(&worker, identity, 7);
        stop(worker);
    }

    #[test]
    fn control_event_saturation_cannot_evict_the_latest_frame_slot() {
        let identity = test_identity();
        let events = super::VideoWorkerEvents::new(None);
        let epoch = events.accept_config(identity, 7);
        events.publish_frame(epoch, test_frame_for(identity, 7, 41));
        for _ in 0..(super::VIDEO_EVENT_LIMIT + 8) {
            events.publish_failure(
                epoch,
                VideoDecodeErrorCode::DecodeFailedAfterFirstFrame,
                true,
            );
        }

        let handoff = events
            .take_latest_frame(identity)
            .expect("控制事件洪峰不得驱逐 latest frame");
        assert_eq!(handoff.frame().as_input().timestamp.ticks, 41);

        events.accept_config(identity, 8);
        assert!(events.take_latest_frame(identity).is_none());
    }

    #[test]
    fn presentation_confirmation_requires_the_latest_delivered_stream_frame_token() {
        let session_id = SessionId::allocate();
        let stream_a = identity_for(session_id, 21);
        let stream_b = identity_for(session_id, 22);
        let events = super::VideoWorkerEvents::new(None);
        let epoch_a = events.accept_config(stream_a, 7);
        events.publish_frame(epoch_a, test_frame_for(stream_a, 7, 1));
        let first_a = events.take_latest_frame(stream_a).unwrap();
        events.publish_frame(epoch_a, test_frame_for(stream_a, 7, 2));
        let second_a = events.take_latest_frame(stream_a).unwrap();

        assert_eq!(
            events.confirm_presented(first_a.token()),
            Err(VideoDecodeErrorCode::StaleStreamOrGeneration)
        );
        events.confirm_presented(second_a.token()).unwrap();
        assert!(events.is_ready(stream_a, 7));

        let epoch_b = events.accept_config(stream_b, 7);
        events.publish_frame(epoch_b, test_frame_for(stream_b, 7, 1));
        let first_b = events.take_latest_frame(stream_b).unwrap();
        let wrong_stream = super::VideoFrameToken {
            identity: stream_b,
            ..*second_a.token()
        };
        assert_eq!(
            events.confirm_presented(&wrong_stream),
            Err(VideoDecodeErrorCode::StaleStreamOrGeneration)
        );
        assert!(!events.is_ready(stream_b, 7));
        events.confirm_presented(first_b.token()).unwrap();
        assert!(events.is_ready(stream_b, 7));
    }

    fn worker_with_script(script: DecoderScript, submits: Arc<AtomicUsize>) -> VideoDecodeWorker {
        VideoDecodeWorker::spawn_with_registry_loader(Box::new(move || {
            Ok(registry(script, submits))
        }))
        .expect("worker thread 应可启动")
    }

    fn registry(script: DecoderScript, submits: Arc<AtomicUsize>) -> VideoDecoderRegistry {
        VideoDecoderRegistry::new(vec![Box::new(FakeFactory { script, submits })])
    }

    #[derive(Clone)]
    enum DecoderScript {
        Echo,
        FailBefore,
        FrameThenFail,
        FlushFrame(Arc<AtomicUsize>),
        BlockingSubmit {
            entered: std::sync::mpsc::Sender<()>,
            release: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
            block_after_calls: usize,
        },
        BlockingFlush {
            entered: std::sync::mpsc::Sender<()>,
            release: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
            blocked: Arc<AtomicBool>,
        },
    }

    struct FakeFactory {
        script: DecoderScript,
        submits: Arc<AtomicUsize>,
    }

    impl VideoCapabilityProvider for FakeFactory {
        fn backend_id(&self) -> VideoBackendId {
            VideoBackendId::new("fake-software")
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
                max_coded_size: PixelSize::new(8192, 8192).unwrap(),
                output_formats: vec![VideoPixelFormat::Yuv444P8].into_boxed_slice(),
                requires_bitstream_conversion: false,
            })
        }
    }

    impl VideoDecoderFactory for FakeFactory {
        fn create(
            &self,
            config: &VideoStreamConfig,
        ) -> Result<Box<dyn VideoDecoder>, VideoDecodeError> {
            Ok(Box::new(FakeDecoder {
                config: config.clone(),
                script: self.script.clone(),
                submits: self.submits.clone(),
            }))
        }
    }

    struct FakeDecoder {
        config: VideoStreamConfig,
        script: DecoderScript,
        submits: Arc<AtomicUsize>,
    }

    impl VideoDecoder for FakeDecoder {
        fn submit(
            &mut self,
            access_unit: EncodedVideoAccessUnit,
        ) -> Result<DecodeOutcome, VideoDecodeError> {
            let call = self.submits.load(Ordering::Acquire);
            let outcome = match self.script {
                DecoderScript::Echo => Ok(DecodeOutcome::Frames(
                    vec![test_frame_for(
                        access_unit.identity(),
                        access_unit.generation(),
                        access_unit.timestamp().ticks,
                    )]
                    .into_boxed_slice(),
                )),
                DecoderScript::FailBefore => Err(VideoDecodeError::new(
                    VideoDecodeErrorCode::DecodeFailedBeforeFirstFrame,
                )),
                DecoderScript::FrameThenFail if call == 0 => Ok(DecodeOutcome::Frames(
                    vec![test_frame(
                        access_unit.generation(),
                        access_unit.timestamp().ticks,
                    )]
                    .into_boxed_slice(),
                )),
                DecoderScript::FrameThenFail => Err(VideoDecodeError::new(
                    VideoDecodeErrorCode::DecodeFailedAfterFirstFrame,
                )),
                DecoderScript::FlushFrame(_) | DecoderScript::BlockingFlush { .. } => {
                    Ok(DecodeOutcome::Frames(
                        vec![test_frame_for(
                            access_unit.identity(),
                            access_unit.generation(),
                            access_unit.timestamp().ticks,
                        )]
                        .into_boxed_slice(),
                    ))
                }
                DecoderScript::BlockingSubmit {
                    ref entered,
                    ref release,
                    block_after_calls,
                } => {
                    if call >= block_after_calls {
                        entered.send(()).unwrap();
                        release.lock().unwrap().recv().unwrap();
                    }
                    Ok(DecodeOutcome::Frames(
                        vec![test_frame_for(
                            access_unit.identity(),
                            access_unit.generation(),
                            access_unit.timestamp().ticks,
                        )]
                        .into_boxed_slice(),
                    ))
                }
            };
            self.submits.fetch_add(1, Ordering::Release);
            outcome
        }

        fn flush(&mut self) -> Result<Box<[DecodedVideoFrame]>, VideoDecodeError> {
            match &self.script {
                DecoderScript::FlushFrame(flushes) => {
                    flushes.fetch_add(1, Ordering::AcqRel);
                    Ok(vec![test_frame(self.config.as_input().generation, 99)].into_boxed_slice())
                }
                DecoderScript::BlockingFlush {
                    entered,
                    release,
                    blocked,
                } => {
                    if !blocked.swap(true, Ordering::AcqRel) {
                        entered.send(()).unwrap();
                        release.lock().unwrap().recv().unwrap();
                    }
                    Ok(vec![test_frame_for(
                        self.config.as_input().identity,
                        self.config.as_input().generation,
                        500,
                    )]
                    .into_boxed_slice())
                }
                _ => Ok(Box::default()),
            }
        }

        fn reset(&mut self, generation: u64) -> Result<(), VideoDecodeError> {
            let mut input = self.config.as_input().clone();
            input.generation = generation;
            self.config = VideoStreamConfig::try_new(input).unwrap();
            Ok(())
        }
    }

    fn recv_backend_selected(worker: &VideoDecodeWorker) {
        assert!(matches!(
            recv_event(worker),
            VideoWorkerEvent::BackendSelected { .. }
        ));
    }

    fn recv_backend_selected_for(
        worker: &VideoDecodeWorker,
        identity: VideoStreamIdentity,
        generation: u64,
    ) {
        loop {
            let event = recv_event(worker);
            if matches!(
                event,
                VideoWorkerEvent::BackendSelected {
                    identity: actual_identity,
                    generation: actual_generation,
                    ..
                } if actual_identity == identity && actual_generation == generation
            ) {
                return;
            }
        }
    }

    fn recv_frame_for(
        worker: &VideoDecodeWorker,
        identity: VideoStreamIdentity,
    ) -> super::DecodedVideoFrameHandoff {
        loop {
            if let VideoWorkerEvent::FrameDecoded(handoff) = recv_event(worker) {
                if handoff.token().identity() == identity {
                    return handoff;
                }
            }
        }
    }

    fn assert_no_frame_for_generation(
        worker: &VideoDecodeWorker,
        identity: VideoStreamIdentity,
        generation: u64,
    ) {
        assert!(worker
            .events()
            .take_latest_frame(identity)
            .is_none_or(|handoff| handoff.token().generation() != generation));
    }

    fn recv_event(worker: &VideoDecodeWorker) -> VideoWorkerEvent {
        worker
            .events()
            .recv_timeout(Duration::from_secs(1))
            .expect("应在 deadline 内收到 worker 事件")
    }

    fn collect_until_stopped(worker: &VideoDecodeWorker) -> Vec<VideoWorkerEvent> {
        let mut events = Vec::new();
        loop {
            let event = recv_event(worker);
            let stopped = event == VideoWorkerEvent::Stopped;
            events.push(event);
            if stopped {
                return events;
            }
        }
    }

    fn stop(worker: VideoDecodeWorker) {
        worker.request_stop();
        let _ = collect_until_stopped(&worker);
        worker
            .join_timeout(Duration::from_secs(1))
            .expect("测试 worker 应有界退出");
    }

    fn wait_until(condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !condition() {
            assert!(Instant::now() < deadline, "测试条件应在 deadline 内成立");
            std::thread::yield_now();
        }
    }

    fn test_config(generation: u64) -> VideoStreamConfig {
        test_config_for(test_identity(), generation)
    }

    fn test_config_for(identity: VideoStreamIdentity, generation: u64) -> VideoStreamConfig {
        VideoStreamConfig::try_new(VideoStreamConfigInput {
            identity,
            generation,
            codec: VideoCodec::Hevc,
            profile: VideoProfile::HevcMain4448,
            chroma: ChromaFormat::Yuv444,
            bit_depth: 8,
            coded_size: PixelSize::new(2, 2).unwrap(),
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

    fn test_identity() -> VideoStreamIdentity {
        static SESSION_ID: OnceLock<SessionId> = OnceLock::new();
        VideoStreamIdentity {
            session_id: *SESSION_ID.get_or_init(SessionId::allocate),
            stream_id: 5,
        }
    }

    fn identity_for(session_id: SessionId, stream_id: u32) -> VideoStreamIdentity {
        VideoStreamIdentity {
            session_id,
            stream_id,
        }
    }

    fn test_au(
        generation: u64,
        ticks: u64,
        random_access: bool,
        bytes: usize,
    ) -> EncodedVideoAccessUnit {
        test_au_for(test_identity(), generation, ticks, random_access, bytes)
    }

    fn test_au_for(
        identity: VideoStreamIdentity,
        generation: u64,
        ticks: u64,
        random_access: bool,
        bytes: usize,
    ) -> EncodedVideoAccessUnit {
        EncodedVideoAccessUnit::try_new(
            identity,
            generation,
            VideoTimestamp {
                ticks,
                timescale: NonZeroU32::new(90_000).unwrap(),
            },
            random_access,
            vec![0x26; bytes].into_boxed_slice(),
        )
        .unwrap()
    }

    fn test_frame(generation: u64, ticks: u64) -> DecodedVideoFrame {
        test_frame_for(test_identity(), generation, ticks)
    }

    fn test_frame_for(
        identity: VideoStreamIdentity,
        generation: u64,
        ticks: u64,
    ) -> DecodedVideoFrame {
        let plane = || VideoPlane::try_new(2, 2, 2, vec![0x80; 4].into_boxed_slice()).unwrap();
        DecodedVideoFrame::try_new(DecodedVideoFrameInput {
            identity,
            generation,
            timestamp: VideoTimestamp {
                ticks,
                timescale: NonZeroU32::new(90_000).unwrap(),
            },
            coded_size: PixelSize::new(2, 2).unwrap(),
            visible_rect: PixelRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
            format: VideoPixelFormat::Yuv444P8,
            planes: vec![plane(), plane(), plane()].into_boxed_slice(),
        })
        .unwrap()
    }
}
