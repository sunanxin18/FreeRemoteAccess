use std::collections::VecDeque;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use frd_media_api::{
    ChromaFormat, DecodeOutcome, DecodedVideoFrame, EncodedVideoAccessUnit, VideoDecodeError,
    VideoDecodeErrorCode, VideoDecodeQuery, VideoDecoder, VideoDecoderRegistry,
    VideoDecoderSelection, VideoPixelFormat, VideoStreamConfig,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VideoWorkerEvent {
    BackendSelected(VideoDecoderDiagnostics),
    FrameDecoded(DecodedVideoFrame),
    DecodeFailed {
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

enum VideoWorkerCommand {
    Config(VideoStreamConfig),
    AccessUnit(EncodedVideoAccessUnit),
}

impl VideoWorkerCommand {
    #[cfg(test)]
    fn into_access_unit(self) -> Option<EncodedVideoAccessUnit> {
        match self {
            Self::AccessUnit(access_unit) => Some(access_unit),
            Self::Config(_) => None,
        }
    }
}

struct VideoInputState {
    pending_config: Option<VideoStreamConfig>,
    access_units: VecDeque<EncodedVideoAccessUnit>,
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

    fn try_push_config(&self, config: VideoStreamConfig) -> Result<(), VideoWorkerSendError> {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video input queue mutex poisoned");
        if state.closed {
            return Err(VideoWorkerSendError::Closed);
        }
        state.pending_config = Some(config);
        state.access_units.clear();
        state.access_unit_bytes = 0;
        wake.notify_one();
        Ok(())
    }

    fn try_push_access_unit(
        &self,
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
        state.access_units.push_back(access_unit);
        wake.notify_one();
        Ok(())
    }

    fn pop(&self) -> Option<VideoWorkerCommand> {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video input queue mutex poisoned");
        loop {
            if let Some(config) = state.pending_config.take() {
                return Some(VideoWorkerCommand::Config(config));
            }
            if let Some(access_unit) = state.access_units.pop_front() {
                state.access_unit_bytes -= access_unit.bytes().len();
                return Some(VideoWorkerCommand::AccessUnit(access_unit));
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
        if let Some(config) = state.pending_config.take() {
            return Some(VideoWorkerCommand::Config(config));
        }
        state.access_units.pop_front().map(|access_unit| {
            state.access_unit_bytes -= access_unit.bytes().len();
            VideoWorkerCommand::AccessUnit(access_unit)
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
}

#[derive(Clone)]
pub struct VideoDecodeSender {
    input: VideoInputQueue,
}

impl VideoDecodeSender {
    pub fn try_send_config(&self, config: VideoStreamConfig) -> Result<(), VideoWorkerSendError> {
        self.input.try_push_config(config)
    }

    pub fn try_send_access_unit(
        &self,
        access_unit: EncodedVideoAccessUnit,
    ) -> Result<(), VideoWorkerSendError> {
        self.input.try_push_access_unit(access_unit)
    }
}

struct VideoEventState {
    events: VecDeque<VideoWorkerEvent>,
    current_generation: Option<u64>,
    last_frame_generation: Option<u64>,
    ready: bool,
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
                    events: VecDeque::new(),
                    current_generation: None,
                    last_frame_generation: None,
                    ready: false,
                }),
                Condvar::new(),
            )),
            wake,
        }
    }

    fn begin_generation(&self, generation: u64) {
        let (lock, _) = &*self.shared;
        let mut state = lock.lock().expect("video event mutex poisoned");
        state.current_generation = Some(generation);
        state.last_frame_generation = None;
        state.ready = false;
        state
            .events
            .retain(|event| !matches!(event, VideoWorkerEvent::FrameDecoded(_)));
    }

    fn publish(&self, event: VideoWorkerEvent) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video event mutex poisoned");
        if let VideoWorkerEvent::FrameDecoded(frame) = &event {
            let generation = frame.as_input().generation;
            if state.current_generation != Some(generation) {
                return;
            }
            state.last_frame_generation = Some(generation);
            state
                .events
                .retain(|queued| !matches!(queued, VideoWorkerEvent::FrameDecoded(_)));
        }
        while state.events.len() >= VIDEO_EVENT_LIMIT {
            state.events.pop_front();
        }
        state.events.push_back(event);
        wake.notify_all();
        drop(state);
        if let Some(wake) = &self.wake {
            wake();
        }
    }

    pub fn try_recv(&self) -> Option<VideoWorkerEvent> {
        let (lock, _) = &*self.shared;
        lock.lock()
            .expect("video event mutex poisoned")
            .events
            .pop_front()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<VideoWorkerEvent> {
        let deadline = Instant::now() + timeout;
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().expect("video event mutex poisoned");
        loop {
            if let Some(event) = state.events.pop_front() {
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
            if wait.timed_out() && state.events.is_empty() {
                return None;
            }
        }
    }

    pub fn confirm_presented(&self, generation: u64) -> Result<(), VideoDecodeErrorCode> {
        let (lock, _) = &*self.shared;
        let mut state = lock.lock().expect("video event mutex poisoned");
        if state.current_generation != Some(generation)
            || state.last_frame_generation != Some(generation)
        {
            return Err(VideoDecodeErrorCode::StaleStreamOrGeneration);
        }
        state.ready = true;
        Ok(())
    }

    pub fn is_ready(&self) -> bool {
        self.shared
            .0
            .lock()
            .expect("video event mutex poisoned")
            .ready
    }
}

type RegistryLoader = Box<dyn FnOnce() -> Result<VideoDecoderRegistry, VideoDecodeError> + Send>;

pub struct VideoDecodeWorker {
    sender: VideoDecodeSender,
    events: VideoWorkerEvents,
    worker: Option<JoinHandle<()>>,
}

impl VideoDecodeWorker {
    pub fn spawn(wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        Self::spawn_inner(
            Box::new(|| {
                frd_video_ffmpeg::FfmpegBackend::load()
                    .map(|backend| VideoDecoderRegistry::new(vec![Box::new(backend)]))
            }),
            Some(wake),
        )
    }

    #[cfg(test)]
    pub(crate) fn spawn_with_registry_loader(loader: RegistryLoader) -> io::Result<Self> {
        Self::spawn_inner(loader, None)
    }

    fn spawn_inner(
        loader: RegistryLoader,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> io::Result<Self> {
        let input = VideoInputQueue::new();
        let events = VideoWorkerEvents::new(wake);
        let worker_input = input.clone();
        let worker_events = events.clone();
        let worker = std::thread::Builder::new()
            .name("frd-video-decoder".to_string())
            .spawn(move || {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    run_decoder_worker(worker_input, worker_events.clone(), loader)
                }));
                worker_events.publish(VideoWorkerEvent::Stopped);
            })?;
        Ok(Self {
            sender: VideoDecodeSender { input },
            events,
            worker: Some(worker),
        })
    }

    pub fn sender(&self) -> VideoDecodeSender {
        self.sender.clone()
    }

    pub fn events(&self) -> VideoWorkerEvents {
        self.events.clone()
    }

    pub fn request_stop(&self) {
        self.sender.input.close();
    }

    pub fn poll_join(&mut self) -> Result<bool, VideoWorkerShutdownError> {
        let Some(worker) = self.worker.as_ref() else {
            return Ok(true);
        };
        if !worker.is_finished() {
            return Ok(false);
        }
        let worker = self
            .worker
            .take()
            .expect("finished video worker remains owned");
        worker
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

struct ActiveDecoder {
    config: VideoStreamConfig,
    decoder: Box<dyn VideoDecoder>,
    after_first_frame: bool,
}

fn run_decoder_worker(input: VideoInputQueue, events: VideoWorkerEvents, loader: RegistryLoader) {
    let mut loader = Some(loader);
    let mut registry = None;
    let mut active: Option<ActiveDecoder> = None;

    while let Some(command) = input.pop() {
        match command {
            VideoWorkerCommand::Config(config) => {
                if let Some(mut previous) = active.take() {
                    let _ = previous.decoder.flush();
                }
                let generation = config.as_input().generation;
                events.begin_generation(generation);
                if registry.is_none() {
                    let Some(load) = loader.take() else {
                        publish_failure(
                            &events,
                            generation,
                            VideoDecodeErrorCode::BackendUnavailable,
                            false,
                        );
                        continue;
                    };
                    match load() {
                        Ok(loaded) => registry = Some(loaded),
                        Err(error) => {
                            publish_failure(&events, generation, error.code(), false);
                            continue;
                        }
                    }
                }
                let query = query_for_config(&config);
                let created = registry
                    .as_ref()
                    .expect("loaded registry remains available")
                    .select_and_create(&query, &config);
                match created {
                    Ok(created) => {
                        let (selection, decoder) = created.into_parts();
                        events.publish(VideoWorkerEvent::BackendSelected(VideoDecoderDiagnostics(
                            selection,
                        )));
                        active = Some(ActiveDecoder {
                            config,
                            decoder,
                            after_first_frame: false,
                        });
                    }
                    Err(error) => publish_failure(&events, generation, error.code(), false),
                }
            }
            VideoWorkerCommand::AccessUnit(access_unit) => {
                let Some(active) = active.as_mut() else {
                    continue;
                };
                let config = active.config.as_input();
                if access_unit.identity() != config.identity
                    || access_unit.generation() != config.generation
                {
                    continue;
                }
                match active.decoder.submit(access_unit) {
                    Ok(DecodeOutcome::NeedMoreData) => {}
                    Ok(DecodeOutcome::Frames(frames)) => {
                        publish_current_frames(&events, active, frames)
                    }
                    Err(error) => publish_failure(
                        &events,
                        config.generation,
                        error.code(),
                        active.after_first_frame,
                    ),
                }
            }
        }
    }

    if let Some(mut active) = active {
        let generation = active.config.as_input().generation;
        match active.decoder.flush() {
            Ok(frames) => publish_current_frames(&events, &mut active, frames),
            Err(error) => {
                publish_failure(&events, generation, error.code(), active.after_first_frame)
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
    let config = active.config.as_input();
    for frame in frames {
        let input = frame.as_input();
        if input.identity == config.identity && input.generation == config.generation {
            active.after_first_frame = true;
            events.publish(VideoWorkerEvent::FrameDecoded(frame));
        }
    }
}

fn publish_failure(
    events: &VideoWorkerEvents,
    generation: u64,
    code: VideoDecodeErrorCode,
    after_first_frame: bool,
) {
    events.publish(VideoWorkerEvent::DecodeFailed {
        generation,
        code,
        after_first_frame,
    });
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, OnceLock};
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
        wait_until(|| submits.load(Ordering::Acquire) == 3);

        let VideoWorkerEvent::FrameDecoded(frame) = recv_event(&worker) else {
            panic!("应收到 latest frame");
        };
        assert_eq!(frame.as_input().generation, 7);
        assert_eq!(frame.as_input().timestamp.ticks, 3);
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
        assert!(matches!(
            recv_event(&worker),
            VideoWorkerEvent::FrameDecoded(_)
        ));
        assert!(!worker.events().is_ready());
        worker.events().confirm_presented(7).unwrap();
        assert!(worker.events().is_ready());
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
            .any(|event| matches!(event, VideoWorkerEvent::FrameDecoded(frame) if frame.as_input().generation == 7)));
        assert_eq!(events.last(), Some(&VideoWorkerEvent::Stopped));
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
                    vec![test_frame(
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
                DecoderScript::FlushFrame(_) => Ok(DecodeOutcome::NeedMoreData),
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
            VideoWorkerEvent::BackendSelected(_)
        ));
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
        VideoStreamConfig::try_new(VideoStreamConfigInput {
            identity: test_identity(),
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

    fn test_au(
        generation: u64,
        ticks: u64,
        random_access: bool,
        bytes: usize,
    ) -> EncodedVideoAccessUnit {
        EncodedVideoAccessUnit::try_new(
            test_config(generation).as_input().identity,
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
        let plane = || VideoPlane::try_new(2, 2, 2, vec![0x80; 4].into_boxed_slice()).unwrap();
        DecodedVideoFrame::try_new(DecodedVideoFrameInput {
            identity: test_config(generation).as_input().identity,
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
