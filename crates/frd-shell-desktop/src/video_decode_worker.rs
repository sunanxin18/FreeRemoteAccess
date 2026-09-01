use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::io::{self, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, Once};
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
const VIDEO_STREAM_IDENTITY_LIMIT: usize = 4;
const VIDEO_EVENT_LIMIT: usize = 64;
const DECODER_BOUNDARY_PANIC_CODE: &str = "freeremotedesk_decoder_boundary_panic";

static INSTALL_DECODER_BOUNDARY_PANIC_HOOK: Once = Once::new();

thread_local! {
    static DECODER_BOUNDARY_DEPTH: Cell<usize> = const { Cell::new(0) };
}

struct DecoderBoundaryGuard;

impl DecoderBoundaryGuard {
    fn enter() -> Self {
        DECODER_BOUNDARY_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self
    }
}

impl Drop for DecoderBoundaryGuard {
    fn drop(&mut self) {
        let _ = DECODER_BOUNDARY_DEPTH.try_with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

fn install_decoder_boundary_panic_hook() {
    INSTALL_DECODER_BOUNDARY_PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            let guarded = DECODER_BOUNDARY_DEPTH
                .try_with(|depth| depth.get() != 0)
                .unwrap_or(false);
            if guarded {
                let _ = writeln!(io::stderr().lock(), "{DECODER_BOUNDARY_PANIC_CODE}");
            } else {
                previous(panic_info);
            }
        }));
    });
}

fn call_decoder_boundary<T>(
    call: impl FnOnce() -> Result<T, VideoDecodeError>,
) -> Result<T, VideoDecodeError> {
    let _guard = DecoderBoundaryGuard::enter();
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(result) => result,
        Err(payload) => {
            drop(payload);
            Err(VideoDecodeError::new(
                VideoDecodeErrorCode::BackendUnavailable,
            ))
        }
    }
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoStreamAdmission {
    pub identity: VideoStreamIdentity,
    pub generation: u64,
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
    worker_epoch: Option<StreamEpoch>,
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
                    worker_epoch: None,
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
                state.worker_epoch = Some(epoch);
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
            state.worker_epoch = Some(epoch);
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

    fn worker_epoch(&self) -> Option<StreamEpoch> {
        self.shared
            .0
            .lock()
            .expect("video input queue mutex poisoned")
            .worker_epoch
    }
}

struct StreamPresentationState {
    epoch: StreamEpoch,
    latest_token: Option<VideoFrameToken>,
    latest_frame: Option<DecodedVideoFrame>,
    latest_sequence: Option<u64>,
    delivered_token: Option<VideoFrameToken>,
    ready: bool,
    terminal: bool,
}

struct ControlPublication {
    sequence: u64,
    epoch: Option<StreamEpoch>,
    event: VideoWorkerEvent,
}

struct VideoEventState {
    admissions: VecDeque<VideoStreamAdmission>,
    controls: VecDeque<ControlPublication>,
    streams: HashMap<VideoStreamIdentity, StreamPresentationState>,
    next_epoch: u64,
    next_sequence: u64,
}

#[derive(Clone)]
pub struct VideoWorkerEvents {
    shared: Arc<(Mutex<VideoEventState>, Condvar)>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    before_control_enqueue: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
}

impl VideoWorkerEvents {
    pub(crate) fn new(wake: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        Self {
            shared: Arc::new((
                Mutex::new(VideoEventState {
                    admissions: VecDeque::new(),
                    controls: VecDeque::new(),
                    streams: HashMap::new(),
                    next_epoch: 1,
                    next_sequence: 1,
                }),
                Condvar::new(),
            )),
            wake,
            #[cfg(test)]
            before_control_enqueue: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(test)]
    fn set_before_control_enqueue(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self
            .before_control_enqueue
            .lock()
            .expect("control publication hook mutex poisoned") = Some(hook);
    }

    #[cfg(test)]
    fn invoke_before_control_enqueue(&self) {
        let hook = self
            .before_control_enqueue
            .lock()
            .expect("control publication hook mutex poisoned")
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn accept_config(&self, identity: VideoStreamIdentity, generation: u64) -> StreamEpoch {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video event mutex poisoned");
        let epoch = accept_config_in_state(&mut state, identity, generation);
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
        self.publish_epoch_control(
            epoch,
            VideoWorkerEvent::BackendSelected {
                identity: epoch.identity,
                generation: epoch.generation,
                diagnostics,
            },
            false,
        );
    }

    fn publish_failure(
        &self,
        epoch: StreamEpoch,
        code: VideoDecodeErrorCode,
        after_first_frame: bool,
    ) {
        self.publish_epoch_control(
            epoch,
            VideoWorkerEvent::DecodeFailed {
                identity: epoch.identity,
                generation: epoch.generation,
                code,
                after_first_frame,
            },
            true,
        );
    }

    fn publish_epoch_control(&self, epoch: StreamEpoch, event: VideoWorkerEvent, terminal: bool) {
        #[cfg(test)]
        self.invoke_before_control_enqueue();
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video event mutex poisoned");
        if !enqueue_epoch_control(&mut state, epoch, event, terminal) {
            return;
        }
        wake.notify_all();
        drop(state);
        self.notify_wake();
    }

    fn publish_panic_failure(&self, input: &VideoInputQueue) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video event mutex poisoned");
        let epoch = input.worker_epoch();
        let Some(epoch) = epoch else {
            return;
        };
        if state
            .streams
            .get(&epoch.identity)
            .is_none_or(|stream| stream.epoch != epoch || stream.terminal)
        {
            return;
        }
        let event = VideoWorkerEvent::DecodeFailed {
            identity: epoch.identity,
            generation: epoch.generation,
            code: VideoDecodeErrorCode::BackendUnavailable,
            after_first_frame: false,
        };
        let published = enqueue_epoch_control(&mut state, epoch, event, true);
        debug_assert!(published);
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
        let publication_id = take_next_sequence(&mut state);
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
        stream.latest_sequence = Some(publication_id);
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

    pub fn try_recv_admission(&self) -> Option<VideoStreamAdmission> {
        self.shared
            .0
            .lock()
            .expect("video event mutex poisoned")
            .admissions
            .pop_front()
    }

    pub fn recv_admission_timeout(&self, timeout: Duration) -> Option<VideoStreamAdmission> {
        let deadline = Instant::now() + timeout;
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video event mutex poisoned");
        loop {
            if let Some(admission) = state.admissions.pop_front() {
                return Some(admission);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (next, wait) = wake
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .expect("video event mutex poisoned while waiting for admission");
            state = next;
            if wait.timed_out() && state.admissions.is_empty() {
                return None;
            }
        }
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
        if stream.terminal {
            stream.latest_token = None;
            stream.delivered_token = None;
            stream.ready = false;
        } else {
            stream.ready = true;
        }
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
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video event mutex poisoned");
        let sequence = take_next_sequence(&mut state);
        while state.controls.len() >= VIDEO_EVENT_LIMIT {
            state.controls.pop_front();
        }
        state.controls.push_back(ControlPublication {
            sequence,
            epoch: None,
            event: VideoWorkerEvent::Stopped,
        });
        wake.notify_all();
        drop(state);
        self.notify_wake();
    }

    fn remove_terminal_if_drained(&self, identity: VideoStreamIdentity) -> bool {
        let mut state = self.shared.0.lock().expect("video event mutex poisoned");
        let drained = state.streams.get(&identity).is_some_and(|stream| {
            stream.terminal
                && stream.latest_frame.is_none()
                && stream.latest_token.is_none()
                && stream.delivered_token.is_none()
                && !stream.ready
                && !state.controls.iter().any(|control| {
                    control
                        .epoch
                        .is_some_and(|epoch| epoch.identity == identity)
                })
        });
        if drained {
            state.streams.remove(&identity);
        }
        drained
    }

    fn notify_wake(&self) {
        if let Some(wake) = &self.wake {
            wake();
        }
    }
}

fn accept_config_in_state(
    state: &mut VideoEventState,
    identity: VideoStreamIdentity,
    generation: u64,
) -> StreamEpoch {
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
    state
        .controls
        .retain(|control| control.epoch.is_none_or(|old| old.identity != identity));
    state.streams.insert(
        identity,
        StreamPresentationState {
            epoch,
            latest_token: None,
            latest_frame: None,
            latest_sequence: None,
            delivered_token: None,
            ready: false,
            terminal: false,
        },
    );
    epoch
}

fn enqueue_admission(state: &mut VideoEventState, epoch: StreamEpoch) {
    state
        .admissions
        .retain(|admission| admission.identity != epoch.identity);
    while state.admissions.len() >= VIDEO_STREAM_IDENTITY_LIMIT {
        state.admissions.pop_front();
    }
    state.admissions.push_back(VideoStreamAdmission {
        identity: epoch.identity,
        generation: epoch.generation,
    });
}

fn enqueue_epoch_control(
    state: &mut VideoEventState,
    epoch: StreamEpoch,
    event: VideoWorkerEvent,
    terminal: bool,
) -> bool {
    if state
        .streams
        .get(&epoch.identity)
        .map(|stream| stream.epoch)
        != Some(epoch)
    {
        return false;
    }
    let sequence = take_next_sequence(state);
    if terminal {
        let stream = state
            .streams
            .get_mut(&epoch.identity)
            .expect("current stream remains present");
        stream.terminal = true;
        stream.ready = false;
        if stream.latest_frame.is_none() {
            stream.latest_token = None;
            stream.delivered_token = None;
        }
    }
    while state.controls.len() >= VIDEO_EVENT_LIMIT {
        state.controls.pop_front();
    }
    state.controls.push_back(ControlPublication {
        sequence,
        epoch: Some(epoch),
        event,
    });
    true
}

fn take_next_sequence(state: &mut VideoEventState) -> u64 {
    let sequence = state.next_sequence;
    state.next_sequence = state
        .next_sequence
        .checked_add(1)
        .expect("video publication sequence exhausted");
    sequence
}

fn event_available(state: &VideoEventState) -> bool {
    !state.controls.is_empty()
        || state
            .streams
            .values()
            .any(|stream| stream.latest_frame.is_some())
}

fn pop_event(state: &mut VideoEventState) -> Option<VideoWorkerEvent> {
    let frame = state
        .streams
        .iter()
        .filter_map(|(identity, stream)| {
            stream
                .latest_frame
                .as_ref()
                .zip(stream.latest_sequence)
                .map(|(_, sequence)| (*identity, sequence))
        })
        .min_by_key(|(_, sequence)| *sequence);
    let control_sequence = state.controls.front().map(|control| control.sequence);
    if let Some((identity, frame_sequence)) = frame {
        if control_sequence.is_none_or(|control_sequence| frame_sequence < control_sequence) {
            return take_frame_from_state(state, identity).map(VideoWorkerEvent::FrameDecoded);
        }
    }
    let control = state.controls.pop_front()?;
    if control.event == VideoWorkerEvent::Stopped {
        state.streams.clear();
    }
    Some(control.event)
}

fn take_frame_from_state(
    state: &mut VideoEventState,
    identity: VideoStreamIdentity,
) -> Option<DecodedVideoFrameHandoff> {
    let stream = state.streams.get_mut(&identity)?;
    let token = stream.latest_token?;
    let frame = stream.latest_frame.take()?;
    stream.latest_sequence = None;
    stream.delivered_token = Some(token);
    Some(DecodedVideoFrameHandoff { token, frame })
}

struct StreamRoute {
    input: VideoInputQueue,
    started: bool,
    terminal: bool,
    joined: bool,
    restart_pending: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamWorkerExit {
    Returned,
    Panicked,
}

struct VideoRouterState {
    streams: HashMap<VideoStreamIdentity, StreamRoute>,
    closed: bool,
    #[cfg(test)]
    spawn_count: usize,
    #[cfg(test)]
    supervisor_revision: u64,
}

#[derive(Clone)]
struct VideoRouter {
    shared: Arc<(Mutex<VideoRouterState>, Condvar)>,
    events: VideoWorkerEvents,
    #[cfg(test)]
    before_stream_finish: Arc<Mutex<Option<Arc<dyn Fn(VideoStreamIdentity) + Send + Sync>>>>,
}

impl VideoRouter {
    fn new(events: VideoWorkerEvents) -> Self {
        Self {
            shared: Arc::new((
                Mutex::new(VideoRouterState {
                    streams: HashMap::new(),
                    closed: false,
                    #[cfg(test)]
                    spawn_count: 0,
                    #[cfg(test)]
                    supervisor_revision: 0,
                }),
                Condvar::new(),
            )),
            events,
            #[cfg(test)]
            before_stream_finish: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(test)]
    fn set_before_stream_finish(&self, hook: Arc<dyn Fn(VideoStreamIdentity) + Send + Sync>) {
        *self
            .before_stream_finish
            .lock()
            .expect("stream finish hook mutex poisoned") = Some(hook);
    }

    #[cfg(test)]
    fn invoke_before_stream_finish(&self, identity: VideoStreamIdentity) {
        let hook = self
            .before_stream_finish
            .lock()
            .expect("stream finish hook mutex poisoned")
            .clone();
        if let Some(hook) = hook {
            hook(identity);
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
        cleanup_drained_streams(&mut state, &self.events);
        if !state.streams.contains_key(&identity)
            && state.streams.len() >= VIDEO_STREAM_IDENTITY_LIMIT
        {
            return Err(VideoWorkerSendError::Full);
        }
        let (output_lock, output_wake) = &*self.events.shared;
        let mut output = output_lock.lock().expect("video event mutex poisoned");
        let output_terminal = output
            .streams
            .get(&identity)
            .is_some_and(|stream| stream.terminal);
        let replace = state
            .streams
            .get(&identity)
            .is_none_or(|route| route.terminal || output_terminal || route.input.is_closed());
        if replace {
            state.streams.insert(
                identity,
                StreamRoute {
                    input: VideoInputQueue::new(),
                    started: false,
                    terminal: false,
                    joined: false,
                    restart_pending: false,
                },
            );
        }
        let epoch = accept_config_in_state(&mut output, identity, generation);
        let result = state
            .streams
            .get(&identity)
            .expect("stream route was inserted")
            .input
            .try_push_config(epoch, config);
        if result.is_ok() {
            enqueue_admission(&mut output, epoch);
        }
        output_wake.notify_all();
        drop(output);
        if result.is_ok() {
            self.events.notify_wake();
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
        let output = self
            .events
            .shared
            .0
            .lock()
            .expect("video event mutex poisoned");
        let Some(output_stream) = output.streams.get(&access_unit.identity()) else {
            return Ok(());
        };
        if output_stream.terminal {
            return Err(VideoWorkerSendError::Closed);
        }
        if output_stream.epoch.generation != access_unit.generation() {
            return Ok(());
        }
        let current_epoch = output_stream.epoch;
        let Some(route) = state.streams.get(&access_unit.identity()) else {
            return Ok(());
        };
        if route.terminal {
            return Err(VideoWorkerSendError::Closed);
        }
        route
            .input
            .try_push_tagged_access_unit(current_epoch, access_unit)
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

    fn finish_stream(
        &self,
        identity: VideoStreamIdentity,
        input: &VideoInputQueue,
        exit: StreamWorkerExit,
    ) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video router mutex poisoned");
        if let Some(route) = state
            .streams
            .get_mut(&identity)
            .filter(|route| Arc::ptr_eq(&route.input.shared, &input.shared))
        {
            if exit == StreamWorkerExit::Panicked {
                self.events.publish_panic_failure(input);
            }
            if input.has_pending_config() {
                route.restart_pending = true;
                route.terminal = false;
            } else {
                route.restart_pending = false;
                route.terminal = true;
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
            route.terminal = true;
            route.joined = true;
            route.restart_pending = false;
            input.close();
        }
        cleanup_drained_streams(&mut state, &self.events);
        wake.notify_all();
    }

    fn reap_stream(
        &self,
        identity: VideoStreamIdentity,
        input: &VideoInputQueue,
        exit: StreamWorkerExit,
    ) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video router mutex poisoned");
        if let Some(route) = state
            .streams
            .get_mut(&identity)
            .filter(|route| Arc::ptr_eq(&route.input.shared, &input.shared))
        {
            if exit == StreamWorkerExit::Panicked {
                self.events.publish_panic_failure(input);
                if input.has_pending_config() {
                    route.restart_pending = true;
                    route.terminal = false;
                } else {
                    route.restart_pending = false;
                    route.terminal = true;
                    input.close();
                }
            }
            route.started = false;
            route.joined = true;
            if route.restart_pending {
                route.restart_pending = false;
                route.terminal = false;
            }
        }
        cleanup_drained_streams(&mut state, &self.events);
        wake.notify_all();
    }

    #[cfg(test)]
    fn record_spawn(&self) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video router mutex poisoned");
        state.spawn_count += 1;
        wake.notify_all();
    }

    #[cfg(test)]
    fn record_supervisor_revision(&self) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video router mutex poisoned");
        state.supervisor_revision += 1;
        wake.notify_all();
    }
}

fn cleanup_drained_streams(state: &mut VideoRouterState, events: &VideoWorkerEvents) {
    let candidates = state
        .streams
        .iter()
        .filter_map(|(identity, route)| (route.terminal && route.joined).then_some(*identity))
        .collect::<Vec<_>>();
    for identity in candidates {
        if events.remove_terminal_if_drained(identity) {
            state.streams.remove(&identity);
        }
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
        install_decoder_boundary_panic_hook();
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
    struct RunningWorker {
        input: VideoInputQueue,
        thread: JoinHandle<StreamWorkerExit>,
    }

    let mut workers: HashMap<VideoStreamIdentity, RunningWorker> = HashMap::new();
    loop {
        #[cfg(test)]
        router.record_supervisor_revision();
        let finished = workers
            .iter()
            .filter_map(|(identity, worker)| worker.thread.is_finished().then_some(*identity))
            .collect::<Vec<_>>();
        for identity in finished {
            if let Some(worker) = workers.remove(&identity) {
                let exit = worker.thread.join().unwrap_or(StreamWorkerExit::Panicked);
                router.reap_stream(identity, &worker.input, exit);
            }
        }

        let (closed, starts) = {
            let (lock, wake) = &*router.shared;
            let mut state = lock.lock().expect("video router mutex poisoned");
            cleanup_drained_streams(&mut state, &events);
            let starts = state
                .streams
                .iter_mut()
                .filter_map(|(identity, route)| {
                    (!route.started
                        && !route.terminal
                        && !route.input.is_closed()
                        && !workers.contains_key(identity))
                    .then(|| {
                        route.started = true;
                        route.joined = false;
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
            #[cfg(test)]
            router.record_spawn();
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
                    let exit = match catch_unwind(AssertUnwindSafe(|| {
                        run_stream_worker(
                            identity,
                            worker_input.clone(),
                            worker_events.clone(),
                            worker_loader,
                        )
                    })) {
                        Ok(()) => StreamWorkerExit::Returned,
                        Err(_) => StreamWorkerExit::Panicked,
                    };
                    #[cfg(test)]
                    worker_router.invoke_before_stream_finish(identity);
                    worker_router.finish_stream(identity, &worker_input, exit);
                    exit
                }) {
                Ok(worker) => {
                    workers.insert(
                        identity,
                        RunningWorker {
                            input,
                            thread: worker,
                        },
                    );
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
                let _ = worker.thread.join();
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
                    match call_decoder_boundary(|| previous.decoder.flush()) {
                        Ok(frames) => publish_current_frames(&events, &mut previous, frames),
                        Err(error) => {
                            events.publish_failure(epoch, error.code(), previous.after_first_frame);
                            return;
                        }
                    }
                }
                if registry.is_none() {
                    match call_decoder_boundary(|| loader(identity)) {
                        Ok(loaded) => registry = Some(loaded),
                        Err(error) => {
                            events.publish_failure(epoch, error.code(), false);
                            return;
                        }
                    }
                }
                let query = query_for_config(&config);
                let created = call_decoder_boundary(|| {
                    registry
                        .as_ref()
                        .expect("loaded registry remains available")
                        .select_and_create(&query, &config)
                });
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
                        return;
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
                match call_decoder_boundary(|| decoder.decoder.submit(access_unit)) {
                    Ok(DecodeOutcome::NeedMoreData) => {}
                    Ok(DecodeOutcome::Frames(frames)) => {
                        publish_current_frames(&events, decoder, frames)
                    }
                    Err(error) => {
                        events.publish_failure(epoch, error.code(), decoder.after_first_frame);
                        return;
                    }
                }
            }
        }
    }

    if let Some(mut active) = active {
        match call_decoder_boundary(|| active.decoder.flush()) {
            Ok(frames) => publish_current_frames(&events, &mut active, frames),
            Err(error) => {
                events.publish_failure(active.epoch, error.code(), active.after_first_frame)
            }
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
    use std::process::Command;
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

    #[test]
    fn stale_control_publication_is_rechecked_atomically_after_config_ingress() {
        let identity = test_identity();
        let events = super::VideoWorkerEvents::new(None);
        let old_epoch = events.accept_config(identity, 7);
        let selection = registry(DecoderScript::Echo, Arc::new(AtomicUsize::new(0)))
            .select(&super::query_for_config(&test_config_for(identity, 7)))
            .unwrap();
        let diagnostics = super::VideoDecoderDiagnostics(selection);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        events.set_before_control_enqueue(Arc::new({
            let release_rx = release_rx.clone();
            move || {
                entered_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            }
        }));

        let publisher = {
            let events = events.clone();
            std::thread::spawn(move || events.publish_selected(old_epoch, diagnostics))
        };
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("旧 control publication 应到达 enqueue barrier");
        events.accept_config(identity, 8);
        release_tx.send(()).unwrap();
        publisher.join().unwrap();

        assert!(events.try_recv().is_none(), "旧 epoch control 不得迟到入队");
    }

    #[test]
    fn stale_failure_publication_is_rechecked_atomically_after_config_ingress() {
        let identity = test_identity();
        let events = super::VideoWorkerEvents::new(None);
        let old_epoch = events.accept_config(identity, 7);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        events.set_before_control_enqueue(Arc::new({
            let release_rx = release_rx.clone();
            move || {
                entered_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            }
        }));

        let publisher = {
            let events = events.clone();
            std::thread::spawn(move || {
                events.publish_failure(
                    old_epoch,
                    VideoDecodeErrorCode::DecodeFailedBeforeFirstFrame,
                    false,
                )
            })
        };
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("旧 failure publication 应到达 enqueue barrier");
        events.accept_config(identity, 8);
        release_tx.send(()).unwrap();
        publisher.join().unwrap();

        assert!(events.try_recv().is_none(), "旧 epoch failure 不得迟到入队");
    }

    #[test]
    fn fatal_stream_stays_terminal_until_one_new_config_restarts_it_once() {
        let identity = test_identity();
        let loader_calls = Arc::new(AtomicUsize::new(0));
        let worker = VideoDecodeWorker::spawn_with_stream_registry_loader(Arc::new({
            let loader_calls = loader_calls.clone();
            move |_identity| {
                let call = loader_calls.fetch_add(1, Ordering::AcqRel);
                Ok(registry(
                    if call == 0 {
                        DecoderScript::FailBefore
                    } else {
                        DecoderScript::Echo
                    },
                    Arc::new(AtomicUsize::new(0)),
                ))
            }
        }))
        .unwrap();
        worker
            .sender()
            .try_send_config(test_config_for(identity, 7))
            .unwrap();
        recv_backend_selected_for(&worker, identity, 7);
        worker
            .sender()
            .try_send_access_unit(test_au_for(identity, 7, 1, true, 1))
            .unwrap();
        recv_decode_failed_for(&worker, identity, 7);

        wait_for_supervisor_revisions(&worker, 3);
        assert_eq!(worker_spawn_count(&worker), 1);
        assert_eq!(loader_calls.load(Ordering::Acquire), 1);

        worker
            .sender()
            .try_send_config(test_config_for(identity, 8))
            .unwrap();
        recv_backend_selected_for(&worker, identity, 8);
        wait_for_supervisor_revisions(&worker, 1);
        assert_eq!(worker_spawn_count(&worker), 2);
        assert_eq!(loader_calls.load(Ordering::Acquire), 2);
        stop(worker);
    }

    #[test]
    fn fifth_retained_encoded_stream_identity_is_rejected_without_worker_allocation() {
        let session_id = SessionId::allocate();
        let worker = VideoDecodeWorker::spawn_with_stream_registry_loader(Arc::new(|_identity| {
            Err(VideoDecodeError::new(
                VideoDecodeErrorCode::BackendUnavailable,
            ))
        }))
        .unwrap();
        for stream_id in 1..=4 {
            worker
                .sender()
                .try_send_config(test_config_for(identity_for(session_id, stream_id), 7))
                .unwrap();
        }

        assert_eq!(
            worker
                .sender()
                .try_send_config(test_config_for(identity_for(session_id, 5), 7,)),
            Err(super::VideoWorkerSendError::Full)
        );
        wait_for_spawn_count(&worker, 4);
        assert_eq!(worker_spawn_count(&worker), 4);
        stop(worker);
    }

    #[test]
    fn drained_terminal_identities_release_capacity_and_churn_stays_bounded() {
        let session_id = SessionId::allocate();
        let worker = VideoDecodeWorker::spawn_with_stream_registry_loader(Arc::new(|_identity| {
            Err(VideoDecodeError::new(
                VideoDecodeErrorCode::BackendUnavailable,
            ))
        }))
        .unwrap();

        for stream_id in 1..=12 {
            let identity = identity_for(session_id, stream_id);
            worker
                .sender()
                .try_send_config(test_config_for(identity, 7))
                .expect("已 drain 的 terminal identity 应释放容量");
            recv_decode_failed_for(&worker, identity, 7);
            wait_for_supervisor_revisions(&worker, 2);
            let (router_count, event_count) = retained_identity_counts(&worker);
            assert!(router_count <= 4, "router identity 必须保持有界");
            assert!(event_count <= 4, "output identity 必须保持有界");
        }

        assert_eq!(worker_spawn_count(&worker), 12);
        stop(worker);
    }

    #[test]
    fn terminal_without_a_retained_frame_clears_the_old_delivered_token() {
        let session_id = SessionId::allocate();
        let worker = VideoDecodeWorker::spawn_with_stream_registry_loader(Arc::new(|_identity| {
            Ok(registry(
                DecoderScript::FrameThenFail,
                Arc::new(AtomicUsize::new(0)),
            ))
        }))
        .unwrap();
        let mut first_token = None;
        for stream_id in 1..=4 {
            let identity = identity_for(session_id, stream_id);
            worker
                .sender()
                .try_send_config(test_config_for(identity, 7))
                .unwrap();
            recv_backend_selected_for(&worker, identity, 7);
            worker
                .sender()
                .try_send_access_unit(test_au_for(identity, 7, 1, true, 1))
                .unwrap();
            let frame = recv_frame_for(&worker, identity);
            first_token.get_or_insert(*frame.token());
            worker
                .sender()
                .try_send_access_unit(test_au_for(identity, 7, 2, false, 1))
                .unwrap();
            recv_decode_failed_for(&worker, identity, 7);
        }
        wait_for_supervisor_revisions(&worker, 2);

        let fifth = identity_for(session_id, 5);
        worker
            .sender()
            .try_send_config(test_config_for(fifth, 7))
            .expect("无 retained frame 的 terminal identity 应立即释放容量");
        assert_eq!(
            worker
                .events()
                .confirm_presented(&first_token.expect("应记录旧 delivered token")),
            Err(VideoDecodeErrorCode::StaleStreamOrGeneration)
        );
        stop(worker);
    }

    #[test]
    fn retained_frame_publication_precedes_a_later_fatal_control() {
        let identity = test_identity();
        let events = super::VideoWorkerEvents::new(None);
        let epoch = events.accept_config(identity, 7);
        events.publish_frame(epoch, test_frame_for(identity, 7, 41));
        events.publish_failure(
            epoch,
            VideoDecodeErrorCode::DecodeFailedAfterFirstFrame,
            true,
        );

        assert!(matches!(
            events.try_recv(),
            Some(VideoWorkerEvent::FrameDecoded(frame))
                if frame.frame().as_input().timestamp.ticks == 41
        ));
        assert!(matches!(
            events.try_recv(),
            Some(VideoWorkerEvent::DecodeFailed {
                identity: actual_identity,
                generation: 7,
                after_first_frame: true,
                ..
            }) if actual_identity == identity
        ));
    }

    #[test]
    fn published_fatal_closes_au_admission_and_replaces_the_old_input_before_finish() {
        let identity = test_identity();
        let loader_calls = Arc::new(AtomicUsize::new(0));
        let worker = VideoDecodeWorker::spawn_with_stream_registry_loader(Arc::new({
            let loader_calls = loader_calls.clone();
            move |_identity| {
                let call = loader_calls.fetch_add(1, Ordering::AcqRel);
                Ok(registry(
                    if call == 0 {
                        DecoderScript::FailBefore
                    } else {
                        DecoderScript::Echo
                    },
                    Arc::new(AtomicUsize::new(0)),
                ))
            }
        }))
        .unwrap();
        let (finish_entered_tx, finish_entered_rx) = std::sync::mpsc::channel();
        let (release_finish_tx, release_finish_rx) = std::sync::mpsc::channel();
        let release_finish_rx = Arc::new(Mutex::new(release_finish_rx));
        let blocked_once = Arc::new(AtomicBool::new(false));
        worker.sender.router.set_before_stream_finish(Arc::new({
            let release_finish_rx = release_finish_rx.clone();
            let blocked_once = blocked_once.clone();
            move |actual_identity| {
                if actual_identity == identity && !blocked_once.swap(true, Ordering::AcqRel) {
                    finish_entered_tx.send(()).unwrap();
                    release_finish_rx.lock().unwrap().recv().unwrap();
                }
            }
        }));

        worker
            .sender()
            .try_send_config(test_config_for(identity, 7))
            .unwrap();
        recv_backend_selected_for(&worker, identity, 7);
        let old_input = worker
            .sender
            .router
            .shared
            .0
            .lock()
            .unwrap()
            .streams
            .get(&identity)
            .unwrap()
            .input
            .shared
            .clone();
        worker
            .sender()
            .try_send_access_unit(test_au_for(identity, 7, 1, true, 1))
            .unwrap();
        recv_decode_failed_for(&worker, identity, 7);
        finish_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("fatal worker 应在 finish 前同步停住");

        let au_result = worker
            .sender()
            .try_send_access_unit(test_au_for(identity, 7, 2, false, 1));
        let config_result = worker
            .sender()
            .try_send_config(test_config_for(identity, 8));
        let new_input = worker
            .sender
            .router
            .shared
            .0
            .lock()
            .unwrap()
            .streams
            .get(&identity)
            .unwrap()
            .input
            .shared
            .clone();
        release_finish_tx.send(()).unwrap();
        recv_backend_selected_for(&worker, identity, 8);
        wait_for_spawn_count(&worker, 2);

        assert_eq!(au_result, Err(VideoWorkerSendError::Closed));
        assert_eq!(config_result, Ok(()));
        assert!(
            !Arc::ptr_eq(&old_input, &new_input),
            "terminal publication 后 config 必须建立新 input"
        );
        assert_eq!(loader_calls.load(Ordering::Acquire), 2);
        stop(worker);
    }

    #[test]
    fn stale_fatal_still_exits_and_restarts_a_pending_config_on_one_new_worker() {
        let identity = test_identity();
        let loader_calls = Arc::new(AtomicUsize::new(0));
        let (failure_entered_tx, failure_entered_rx) = std::sync::mpsc::channel();
        let (release_failure_tx, release_failure_rx) = std::sync::mpsc::channel();
        let release_failure_rx = Arc::new(Mutex::new(release_failure_rx));
        let worker = VideoDecodeWorker::spawn_with_stream_registry_loader(Arc::new({
            let loader_calls = loader_calls.clone();
            let release_failure_rx = release_failure_rx.clone();
            move |_identity| {
                let call = loader_calls.fetch_add(1, Ordering::AcqRel);
                Ok(registry(
                    if call == 0 {
                        DecoderScript::BlockingFailure {
                            entered: failure_entered_tx.clone(),
                            release: release_failure_rx.clone(),
                        }
                    } else {
                        DecoderScript::Echo
                    },
                    Arc::new(AtomicUsize::new(0)),
                ))
            }
        }))
        .unwrap();

        worker
            .sender()
            .try_send_config(test_config_for(identity, 7))
            .unwrap();
        recv_backend_selected_for(&worker, identity, 7);
        worker
            .sender()
            .try_send_access_unit(test_au_for(identity, 7, 1, true, 1))
            .unwrap();
        failure_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("旧 decoder failure 应到达同步点");
        worker
            .sender()
            .try_send_config(test_config_for(identity, 8))
            .unwrap();
        release_failure_tx.send(()).unwrap();

        recv_backend_selected_for(&worker, identity, 8);
        wait_for_spawn_count(&worker, 2);
        assert_eq!(loader_calls.load(Ordering::Acquire), 2);
        assert!(worker.events().try_recv().is_none());
        stop(worker);
    }

    #[test]
    fn stale_submit_panic_preserves_a_pending_config_for_one_new_worker() {
        let identity = test_identity();
        let loader_calls = Arc::new(AtomicUsize::new(0));
        let (panic_entered_tx, panic_entered_rx) = std::sync::mpsc::channel();
        let (release_panic_tx, release_panic_rx) = std::sync::mpsc::channel();
        let release_panic_rx = Arc::new(Mutex::new(release_panic_rx));
        let worker = VideoDecodeWorker::spawn_with_stream_registry_loader(Arc::new({
            let loader_calls = loader_calls.clone();
            let release_panic_rx = release_panic_rx.clone();
            move |_identity| {
                let call = loader_calls.fetch_add(1, Ordering::AcqRel);
                Ok(registry(
                    if call == 0 {
                        DecoderScript::BlockingPanic {
                            entered: panic_entered_tx.clone(),
                            release: release_panic_rx.clone(),
                        }
                    } else {
                        DecoderScript::Echo
                    },
                    Arc::new(AtomicUsize::new(0)),
                ))
            }
        }))
        .unwrap();

        worker
            .sender()
            .try_send_config(test_config_for(identity, 7))
            .unwrap();
        recv_backend_selected_for(&worker, identity, 7);
        worker
            .sender()
            .try_send_access_unit(test_au_for(identity, 7, 1, true, 1))
            .unwrap();
        panic_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("旧 decoder panic 应到达同步点");
        worker
            .sender()
            .try_send_config(test_config_for(identity, 8))
            .unwrap();
        release_panic_tx.send(()).unwrap();

        recv_backend_selected_for(&worker, identity, 8);
        wait_for_spawn_count(&worker, 2);
        assert_eq!(loader_calls.load(Ordering::Acquire), 2);
        assert!(worker.events().try_recv().is_none());
        stop(worker);
    }

    #[test]
    fn loader_panics_are_terminal_and_four_drained_identities_release_the_cap() {
        assert_panics_release_capacity(PanicSite::Loader);
    }

    #[test]
    fn submit_panics_are_terminal_and_four_drained_identities_release_the_cap() {
        assert_panics_release_capacity(PanicSite::Submit);
    }

    #[test]
    fn flush_panics_are_terminal_and_four_drained_identities_release_the_cap() {
        assert_panics_release_capacity(PanicSite::Flush);
    }

    #[test]
    fn decoder_boundary_panic_hook_hides_secrets_and_forwards_unguarded_panics() {
        const SAFE_CODE: &str = "freeremotedesk_decoder_boundary_panic";
        const GUARDED_CASES: [(&str, &str); 4] = [
            ("loader", "sensitive loader panic details must not escape"),
            ("selection", "sensitive selection panic details"),
            ("submit", "sensitive submit panic details"),
            ("flush", "sensitive flush panic details"),
        ];

        for (mode, secret) in GUARDED_CASES {
            let output = run_panic_hook_child(mode);
            assert!(
                output.status.success(),
                "guarded {mode} child 应完成既有 terminal/cap 回收断言，status={}",
                output.status
            );
            let stderr = String::from_utf8(output.stderr).expect("child stderr 应为 UTF-8");
            assert!(
                !stderr.contains(secret),
                "guarded {mode} child stderr 不得包含 panic payload"
            );
            let lines = stderr
                .lines()
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>();
            assert_eq!(
                lines,
                vec![SAFE_CODE; 4],
                "guarded {mode} child stderr 只能包含每次 panic 的固定安全码"
            );
        }

        let output = run_panic_hook_child("unguarded");
        assert!(
            !output.status.success(),
            "unguarded child panic 应保持 test process failure"
        );
        let stderr = String::from_utf8(output.stderr).expect("child stderr 应为 UTF-8");
        assert!(
            stderr.contains("freeremotedesk_unguarded_panic_sentinel"),
            "unguarded panic 应转发到 previous hook"
        );
        assert!(
            !stderr.contains(SAFE_CODE),
            "unguarded panic 不得被 decoder boundary hook 改写"
        );
    }

    #[test]
    fn decoder_boundary_panic_hook_child() {
        let Ok(mode) = std::env::var("FRD_VIDEO_DECODER_PANIC_HOOK_CHILD") else {
            return;
        };
        match mode.as_str() {
            "loader" => assert_panics_release_capacity(PanicSite::Loader),
            "selection" => assert_panics_release_capacity(PanicSite::Selection),
            "submit" => assert_panics_release_capacity(PanicSite::Submit),
            "flush" => assert_panics_release_capacity(PanicSite::Flush),
            "unguarded" => {
                let worker = worker_with_script(DecoderScript::Echo, Arc::new(AtomicUsize::new(0)));
                stop(worker);
                panic!("freeremotedesk_unguarded_panic_sentinel");
            }
            _ => panic!("未知 decoder panic-hook child mode"),
        }
    }

    #[test]
    fn ready_retained_frame_drains_before_fatal_without_becoming_ready_again() {
        let identity = test_identity();
        let worker = worker_with_script(
            DecoderScript::TwoFramesThenFail,
            Arc::new(AtomicUsize::new(0)),
        );
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
        wait_for_latest_frame(&worker, identity);
        worker
            .sender()
            .try_send_access_unit(test_au_for(identity, 7, 3, false, 1))
            .unwrap();
        wait_for_output_terminal(&worker, identity);

        let second = recv_frame_for(&worker, identity);
        assert_eq!(second.frame().as_input().timestamp.ticks, 2);
        recv_decode_failed_for(&worker, identity, 7);
        let token = *second.token();
        let forged = super::VideoFrameToken {
            publication_id: token.publication_id + 1,
            ..token
        };
        worker.events().confirm_presented(&token).unwrap();
        assert!(!worker.events().is_ready(identity, 7));
        assert_eq!(
            worker.events().confirm_presented(&token),
            Err(VideoDecodeErrorCode::StaleStreamOrGeneration)
        );
        assert_eq!(
            worker.events().confirm_presented(&forged),
            Err(VideoDecodeErrorCode::StaleStreamOrGeneration)
        );
        wait_for_supervisor_revisions(&worker, 2);
        assert_eq!(retained_identity_counts(&worker), (0, 0));
        stop(worker);
    }

    #[derive(Clone, Copy)]
    enum PanicSite {
        Loader,
        Selection,
        Submit,
        Flush,
    }

    fn run_panic_hook_child(mode: &str) -> std::process::Output {
        Command::new(std::env::current_exe().expect("应取得当前 test binary"))
            .args([
                "--exact",
                "video_decode_worker::tests::decoder_boundary_panic_hook_child",
                "--nocapture",
            ])
            .env("FRD_VIDEO_DECODER_PANIC_HOOK_CHILD", mode)
            .env_remove("RUST_BACKTRACE")
            .output()
            .expect("应启动 decoder panic-hook child test")
    }

    fn assert_panics_release_capacity(site: PanicSite) {
        let session_id = SessionId::allocate();
        let loader_calls = Arc::new(AtomicUsize::new(0));
        let worker = VideoDecodeWorker::spawn_with_stream_registry_loader(Arc::new({
            let loader_calls = loader_calls.clone();
            move |_identity| {
                let call = loader_calls.fetch_add(1, Ordering::AcqRel);
                if matches!(site, PanicSite::Loader) && call < 4 {
                    panic!("sensitive loader panic details must not escape");
                }
                let script = if call < 4 {
                    match site {
                        PanicSite::Loader => DecoderScript::Echo,
                        PanicSite::Selection => DecoderScript::PanicSelection,
                        PanicSite::Submit => DecoderScript::PanicSubmit,
                        PanicSite::Flush => DecoderScript::PanicFlush,
                    }
                } else {
                    DecoderScript::Echo
                };
                Ok(registry(script, Arc::new(AtomicUsize::new(0))))
            }
        }))
        .unwrap();

        for stream_id in 1..=4 {
            let identity = identity_for(session_id, stream_id);
            worker
                .sender()
                .try_send_config(test_config_for(identity, 7))
                .unwrap();
            if matches!(site, PanicSite::Submit | PanicSite::Flush) {
                recv_backend_selected_for(&worker, identity, 7);
            }
            match site {
                PanicSite::Loader | PanicSite::Selection => {}
                PanicSite::Submit => worker
                    .sender()
                    .try_send_access_unit(test_au_for(identity, 7, 1, true, 1))
                    .unwrap(),
                PanicSite::Flush => worker
                    .sender()
                    .try_send_config(test_config_for(identity, 8))
                    .unwrap(),
            }
            let event = recv_decode_failed_event_for(
                &worker,
                identity,
                if matches!(site, PanicSite::Flush) {
                    8
                } else {
                    7
                },
            );
            assert_eq!(
                event,
                VideoWorkerEvent::DecodeFailed {
                    identity,
                    generation: if matches!(site, PanicSite::Flush) {
                        8
                    } else {
                        7
                    },
                    code: VideoDecodeErrorCode::BackendUnavailable,
                    after_first_frame: false,
                }
            );
            wait_for_supervisor_revisions(&worker, 2);
        }

        let fifth = identity_for(session_id, 5);
        worker
            .sender()
            .try_send_config(test_config_for(fifth, 7))
            .expect("panic terminal drain 后第五个 identity 应复用容量");
        recv_backend_selected_for(&worker, fifth, 7);
        wait_for_spawn_count(&worker, 5);
        assert_eq!(loader_calls.load(Ordering::Acquire), 5);
        assert_eq!(retained_identity_counts(&worker), (1, 1));
        stop(worker);
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
        TwoFramesThenFail,
        PanicSelection,
        PanicSubmit,
        PanicFlush,
        FlushFrame(Arc<AtomicUsize>),
        BlockingFailure {
            entered: std::sync::mpsc::Sender<()>,
            release: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
        },
        BlockingPanic {
            entered: std::sync::mpsc::Sender<()>,
            release: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
        },
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
            if matches!(&self.script, DecoderScript::PanicSelection) {
                panic!("sensitive selection panic details");
            }
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
                    vec![test_frame_for(
                        access_unit.identity(),
                        access_unit.generation(),
                        access_unit.timestamp().ticks,
                    )]
                    .into_boxed_slice(),
                )),
                DecoderScript::FrameThenFail => Err(VideoDecodeError::new(
                    VideoDecodeErrorCode::DecodeFailedAfterFirstFrame,
                )),
                DecoderScript::TwoFramesThenFail if call < 2 => Ok(DecodeOutcome::Frames(
                    vec![test_frame_for(
                        access_unit.identity(),
                        access_unit.generation(),
                        access_unit.timestamp().ticks,
                    )]
                    .into_boxed_slice(),
                )),
                DecoderScript::TwoFramesThenFail => Err(VideoDecodeError::new(
                    VideoDecodeErrorCode::DecodeFailedAfterFirstFrame,
                )),
                DecoderScript::PanicSelection => Ok(DecodeOutcome::NeedMoreData),
                DecoderScript::PanicSubmit => panic!("sensitive submit panic details"),
                DecoderScript::PanicFlush => Ok(DecodeOutcome::NeedMoreData),
                DecoderScript::BlockingFailure {
                    ref entered,
                    ref release,
                } => {
                    entered.send(()).unwrap();
                    release.lock().unwrap().recv().unwrap();
                    Err(VideoDecodeError::new(
                        VideoDecodeErrorCode::DecodeFailedBeforeFirstFrame,
                    ))
                }
                DecoderScript::BlockingPanic {
                    ref entered,
                    ref release,
                } => {
                    entered.send(()).unwrap();
                    release.lock().unwrap().recv().unwrap();
                    panic!("sensitive pending-config panic details")
                }
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
                DecoderScript::PanicFlush => panic!("sensitive flush panic details"),
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

    fn recv_decode_failed_for(
        worker: &VideoDecodeWorker,
        identity: VideoStreamIdentity,
        generation: u64,
    ) {
        let _ = recv_decode_failed_event_for(worker, identity, generation);
    }

    fn recv_decode_failed_event_for(
        worker: &VideoDecodeWorker,
        identity: VideoStreamIdentity,
        generation: u64,
    ) -> VideoWorkerEvent {
        loop {
            let event = recv_event(worker);
            if matches!(
                event,
                VideoWorkerEvent::DecodeFailed {
                    identity: actual_identity,
                    generation: actual_generation,
                    ..
                } if actual_identity == identity && actual_generation == generation
            ) {
                return event;
            }
        }
    }

    fn wait_for_latest_frame(worker: &VideoDecodeWorker, identity: VideoStreamIdentity) {
        let (lock, wake) = &*worker.events.shared;
        let mut state = lock.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !state
            .streams
            .get(&identity)
            .is_some_and(|stream| stream.latest_frame.is_some())
        {
            let now = Instant::now();
            assert!(now < deadline, "latest frame 应在 deadline 内发布");
            let (next, timeout) = wake
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .unwrap();
            state = next;
            assert!(
                !timeout.timed_out()
                    || state
                        .streams
                        .get(&identity)
                        .is_some_and(|stream| stream.latest_frame.is_some())
            );
        }
    }

    fn wait_for_output_terminal(worker: &VideoDecodeWorker, identity: VideoStreamIdentity) {
        let (lock, wake) = &*worker.events.shared;
        let mut state = lock.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !state
            .streams
            .get(&identity)
            .is_some_and(|stream| stream.terminal)
        {
            let now = Instant::now();
            assert!(now < deadline, "terminal output 应在 deadline 内发布");
            let (next, timeout) = wake
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .unwrap();
            state = next;
            assert!(
                !timeout.timed_out()
                    || state
                        .streams
                        .get(&identity)
                        .is_some_and(|stream| stream.terminal)
            );
        }
    }

    fn worker_spawn_count(worker: &VideoDecodeWorker) -> usize {
        worker.sender.router.shared.0.lock().unwrap().spawn_count
    }

    fn wait_for_spawn_count(worker: &VideoDecodeWorker, expected: usize) {
        let (lock, wake) = &*worker.sender.router.shared;
        let mut state = lock.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while state.spawn_count < expected {
            let now = Instant::now();
            if now >= deadline {
                let actual = state.spawn_count;
                drop(state);
                panic!("spawn count 应在 deadline 内达到 {expected}，实际为 {actual}");
            }
            let (next, timeout) = wake
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .unwrap();
            state = next;
            if timeout.timed_out() && state.spawn_count < expected {
                let actual = state.spawn_count;
                drop(state);
                panic!("spawn count 应在 deadline 内达到 {expected}，实际为 {actual}");
            }
        }
    }

    fn wait_for_supervisor_revisions(worker: &VideoDecodeWorker, additional: u64) {
        let (lock, wake) = &*worker.sender.router.shared;
        let mut state = lock.lock().unwrap();
        let target = state.supervisor_revision + additional;
        wake.notify_all();
        let deadline = Instant::now() + Duration::from_secs(1);
        while state.supervisor_revision < target {
            let now = Instant::now();
            assert!(now < deadline, "supervisor revision 应在 deadline 内推进");
            let (next, timeout) = wake
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .unwrap();
            state = next;
            assert!(!timeout.timed_out() || state.supervisor_revision >= target);
        }
    }

    fn retained_identity_counts(worker: &VideoDecodeWorker) -> (usize, usize) {
        let router_count = worker.sender.router.shared.0.lock().unwrap().streams.len();
        let event_count = worker.events.shared.0.lock().unwrap().streams.len();
        (router_count, event_count)
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
            colorimetry: VideoColorimetry::Bt709,
            range: VideoRange::Limited,
            planes: vec![plane(), plane(), plane()].into_boxed_slice(),
        })
        .unwrap()
    }
}
