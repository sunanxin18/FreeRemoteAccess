use super::{AudioOutputSink, AudioOutputSpec, PlatformError};

#[cfg(feature = "media")]
use super::AudioOutputBackend;

#[cfg(feature = "media")]
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
#[cfg(feature = "media")]
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, Stream, SupportedStreamConfig, I24, U24,
};
#[cfg(feature = "media")]
use std::collections::VecDeque;
#[cfg(feature = "media")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "media")]
use std::sync::{Arc, Condvar, Mutex, OnceLock};
#[cfg(feature = "media")]
use std::thread;
#[cfg(feature = "media")]
use std::time::{Duration, Instant};

#[cfg(feature = "media")]
use crossbeam_channel::{bounded, Sender};

#[cfg(feature = "media")]
const PLAYBACK_QUEUE_CAPACITY_MILLISECONDS: usize = 500;
#[cfg(feature = "media")]
const MILLISECONDS_PER_SECOND: usize = 1_000;
#[cfg(feature = "media")]
const PLAYBACK_QUEUE_CAPACITY_FRAMES: usize = AudioOutputSpec::NORMALIZED_SAMPLE_RATE_HZ as usize
    * PLAYBACK_QUEUE_CAPACITY_MILLISECONDS
    / MILLISECONDS_PER_SECOND;
#[cfg(feature = "media")]
const AUDIO_OUTPUT_STARTUP_TIMEOUT: Duration = Duration::from_millis(350);
#[cfg(feature = "media")]
static AUDIO_WORKER_SLOT: OnceLock<Arc<AudioWorkerSlot>> = OnceLock::new();

pub(super) fn open_cpal_audio_output(
    spec: AudioOutputSpec,
) -> Result<AudioOutputSink, PlatformError> {
    #[cfg(feature = "media")]
    {
        open_audio_output_with_worker(
            spec,
            Arc::clone(AUDIO_WORKER_SLOT.get_or_init(|| Arc::new(AudioWorkerSlot::default()))),
            Arc::new(CpalDeviceOpener),
            AUDIO_OUTPUT_STARTUP_TIMEOUT,
        )
    }
    #[cfg(not(feature = "media"))]
    {
        let _ = spec;
        Err(PlatformError::new("audio_output_unavailable"))
    }
}

#[cfg(feature = "media")]
#[derive(Default)]
struct PcmPlaybackBuffer {
    frames: VecDeque<[i16; 2]>,
}

#[cfg(feature = "media")]
impl PcmPlaybackBuffer {
    fn try_enqueue_interleaved_stereo(&mut self, pcm: &[i16]) -> Result<(), PlatformError> {
        let incoming_frames = pcm.len() / usize::from(AudioOutputSpec::NORMALIZED_CHANNELS);
        let queued_after = self
            .frames
            .len()
            .checked_add(incoming_frames)
            .ok_or_else(|| PlatformError::new("audio_output_queue_capacity_overflow"))?;
        if queued_after > PLAYBACK_QUEUE_CAPACITY_FRAMES {
            return Err(PlatformError::new("audio_output_queue_full"));
        }
        for frame in pcm.chunks_exact(2) {
            self.frames.push_back([frame[0], frame[1]]);
        }
        Ok(())
    }

    fn render<T>(&mut self, output: &mut [T], channels: usize)
    where
        T: Sample + FromSample<f32>,
    {
        if channels == 0 {
            return;
        }
        for output_frame in output.chunks_mut(channels) {
            let source = self.frames.pop_front().unwrap_or([0, 0]);
            let left = f32::from(source[0]) / 32_768.0;
            let right = f32::from(source[1]) / 32_768.0;
            if channels == 1 {
                output_frame[0] = T::from_sample((left + right) * 0.5);
                continue;
            }
            output_frame[0] = T::from_sample(left);
            output_frame[1] = T::from_sample(right);
            for sample in &mut output_frame[2..] {
                *sample = T::from_sample(0.0);
            }
        }
    }
}

#[cfg(feature = "media")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AudioWorkerSlotState {
    Available,
    Opening,
    Active,
    Closing,
    Stuck,
}

#[cfg(feature = "media")]
struct AudioWorkerSlot {
    state: Mutex<AudioWorkerSlotState>,
    state_changed: Condvar,
    #[cfg(test)]
    spawned_workers: std::sync::atomic::AtomicUsize,
}

#[cfg(feature = "media")]
impl Default for AudioWorkerSlot {
    fn default() -> Self {
        Self {
            state: Mutex::new(AudioWorkerSlotState::Available),
            state_changed: Condvar::new(),
            #[cfg(test)]
            spawned_workers: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[cfg(feature = "media")]
impl AudioWorkerSlot {
    fn acquire_until(
        self: &Arc<Self>,
        deadline: Instant,
    ) -> Result<AudioWorkerLease, PlatformError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PlatformError::new("audio_output_worker_state_unavailable"))?;
        loop {
            match *state {
                AudioWorkerSlotState::Available => {
                    *state = AudioWorkerSlotState::Opening;
                    return Ok(AudioWorkerLease {
                        slot: Arc::clone(self),
                    });
                }
                AudioWorkerSlotState::Closing => {
                    let now = Instant::now();
                    let Some(remaining) = deadline.checked_duration_since(now) else {
                        return Err(PlatformError::new("audio_output_worker_busy"));
                    };
                    if remaining.is_zero() {
                        return Err(PlatformError::new("audio_output_worker_busy"));
                    }
                    let (next_state, wait) = self
                        .state_changed
                        .wait_timeout(state, remaining)
                        .map_err(|_| PlatformError::new("audio_output_worker_state_unavailable"))?;
                    state = next_state;
                    if wait.timed_out() && *state == AudioWorkerSlotState::Closing {
                        return Err(PlatformError::new("audio_output_worker_busy"));
                    }
                }
                AudioWorkerSlotState::Opening
                | AudioWorkerSlotState::Active
                | AudioWorkerSlotState::Stuck => {
                    return Err(PlatformError::new("audio_output_worker_busy"));
                }
            }
        }
    }

    fn activate_if_opening(&self, closed: &AtomicBool) -> Result<bool, PlatformError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PlatformError::new("audio_output_worker_state_unavailable"))?;
        if *state == AudioWorkerSlotState::Opening && !closed.load(Ordering::Acquire) {
            *state = AudioWorkerSlotState::Active;
            return Ok(true);
        }
        Ok(false)
    }

    fn mark_stuck(&self) {
        if let Ok(mut state) = self.state.lock() {
            if matches!(
                *state,
                AudioWorkerSlotState::Opening | AudioWorkerSlotState::Active
            ) {
                *state = AudioWorkerSlotState::Stuck;
                self.state_changed.notify_all();
            }
        }
    }

    fn mark_closing(&self) {
        if let Ok(mut state) = self.state.lock() {
            if matches!(
                *state,
                AudioWorkerSlotState::Opening | AudioWorkerSlotState::Active
            ) {
                *state = AudioWorkerSlotState::Closing;
                self.state_changed.notify_all();
            }
        }
    }

    fn wait_until_not_active(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        while *state == AudioWorkerSlotState::Active {
            let Ok(next_state) = self.state_changed.wait(state) else {
                return;
            };
            state = next_state;
        }
    }

    #[cfg(test)]
    fn spawned_workers_for_test(&self) -> usize {
        self.spawned_workers.load(Ordering::Acquire)
    }
}

#[cfg(feature = "media")]
struct AudioWorkerLease {
    slot: Arc<AudioWorkerSlot>,
}

#[cfg(feature = "media")]
impl Drop for AudioWorkerLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.slot.state.lock() {
            *state = AudioWorkerSlotState::Available;
            self.slot.state_changed.notify_all();
        }
    }
}

#[cfg(feature = "media")]
#[derive(Default)]
struct AudioOutputFailure {
    stream_failed: AtomicBool,
}

#[cfg(feature = "media")]
impl AudioOutputFailure {
    fn mark_stream_failed(&self) {
        self.stream_failed.store(true, Ordering::Release);
    }

    fn check(&self) -> Result<(), PlatformError> {
        if self.stream_failed.load(Ordering::Acquire) {
            return Err(PlatformError::new("audio_output_stream_failed"));
        }
        Ok(())
    }
}

#[cfg(feature = "media")]
trait OpenedAudioDevice {
    fn device_description(&self) -> &str;
}

#[cfg(feature = "media")]
trait AudioDeviceOpener: Send + Sync + 'static {
    fn open(
        &self,
        spec: AudioOutputSpec,
        buffer: Arc<Mutex<PcmPlaybackBuffer>>,
        failure: Arc<AudioOutputFailure>,
    ) -> Result<Box<dyn OpenedAudioDevice>, PlatformError>;
}

#[cfg(feature = "media")]
struct CpalDeviceOpener;

#[cfg(feature = "media")]
struct CpalOpenedAudioDevice {
    _stream: Stream,
    device_description: String,
}

#[cfg(feature = "media")]
impl OpenedAudioDevice for CpalOpenedAudioDevice {
    fn device_description(&self) -> &str {
        &self.device_description
    }
}

#[cfg(feature = "media")]
impl AudioDeviceOpener for CpalDeviceOpener {
    fn open(
        &self,
        spec: AudioOutputSpec,
        buffer: Arc<Mutex<PcmPlaybackBuffer>>,
        failure: Arc<AudioOutputFailure>,
    ) -> Result<Box<dyn OpenedAudioDevice>, PlatformError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| PlatformError::new("audio_output_device_unavailable"))?;
        let device_description = device
            .description()
            .map(|description| description.to_string())
            .unwrap_or_else(|_| "默认音频输出设备".to_owned());
        let config = select_output_config(&device, spec)?;
        let channels = usize::from(config.channels());
        let sample_format = config.sample_format();
        let stream_config = config.into();
        let stream = match sample_format {
            SampleFormat::I8 => {
                build_output_stream::<i8>(&device, stream_config, channels, &buffer, &failure)
            }
            SampleFormat::I16 => {
                build_output_stream::<i16>(&device, stream_config, channels, &buffer, &failure)
            }
            SampleFormat::I24 => {
                build_output_stream::<I24>(&device, stream_config, channels, &buffer, &failure)
            }
            SampleFormat::I32 => {
                build_output_stream::<i32>(&device, stream_config, channels, &buffer, &failure)
            }
            SampleFormat::I64 => {
                build_output_stream::<i64>(&device, stream_config, channels, &buffer, &failure)
            }
            SampleFormat::U8 => {
                build_output_stream::<u8>(&device, stream_config, channels, &buffer, &failure)
            }
            SampleFormat::U16 => {
                build_output_stream::<u16>(&device, stream_config, channels, &buffer, &failure)
            }
            SampleFormat::U24 => {
                build_output_stream::<U24>(&device, stream_config, channels, &buffer, &failure)
            }
            SampleFormat::U32 => {
                build_output_stream::<u32>(&device, stream_config, channels, &buffer, &failure)
            }
            SampleFormat::U64 => {
                build_output_stream::<u64>(&device, stream_config, channels, &buffer, &failure)
            }
            SampleFormat::F32 => {
                build_output_stream::<f32>(&device, stream_config, channels, &buffer, &failure)
            }
            SampleFormat::F64 => {
                build_output_stream::<f64>(&device, stream_config, channels, &buffer, &failure)
            }
            _ => Err(PlatformError::new("audio_output_format_unsupported")),
        }?;
        stream
            .play()
            .map_err(|_| PlatformError::new("audio_output_stream_start_failed"))?;
        Ok(Box::new(CpalOpenedAudioDevice {
            _stream: stream,
            device_description,
        }))
    }
}

#[cfg(feature = "media")]
struct WorkerAudioOutput {
    buffer: Arc<Mutex<PcmPlaybackBuffer>>,
    failure: Arc<AudioOutputFailure>,
    closed: Arc<AtomicBool>,
    slot: Arc<AudioWorkerSlot>,
    device_description: String,
}

#[cfg(feature = "media")]
impl WorkerAudioOutput {
    fn new(
        buffer: Arc<Mutex<PcmPlaybackBuffer>>,
        failure: Arc<AudioOutputFailure>,
        closed: Arc<AtomicBool>,
        slot: Arc<AudioWorkerSlot>,
        device_description: String,
    ) -> Self {
        Self {
            buffer,
            failure,
            closed,
            slot,
            device_description,
        }
    }
}

#[cfg(feature = "media")]
impl AudioOutputBackend for WorkerAudioOutput {
    fn enqueue_interleaved_i16(&mut self, samples: &[i16]) -> Result<(), PlatformError> {
        self.failure.check()?;
        self.buffer
            .lock()
            .map_err(|_| PlatformError::new("audio_output_queue_unavailable"))?
            .try_enqueue_interleaved_stereo(samples)?;
        self.failure.check()
    }

    fn device_description(&self) -> &str {
        &self.device_description
    }
}

#[cfg(feature = "media")]
impl Drop for WorkerAudioOutput {
    fn drop(&mut self) {
        self.slot.mark_closing();
        self.closed.store(true, Ordering::Release);
    }
}

#[cfg(feature = "media")]
fn open_audio_output_with_worker(
    spec: AudioOutputSpec,
    slot: Arc<AudioWorkerSlot>,
    opener: Arc<dyn AudioDeviceOpener>,
    startup_timeout: Duration,
) -> Result<AudioOutputSink, PlatformError> {
    if startup_timeout.is_zero() {
        return Err(PlatformError::new("audio_output_open_timeout"));
    }
    let deadline = Instant::now()
        .checked_add(startup_timeout)
        .ok_or_else(|| PlatformError::new("audio_output_open_timeout"))?;
    let lease = slot.acquire_until(deadline)?;
    let (startup_sender, startup_receiver) = bounded(1);
    let buffer = Arc::new(Mutex::new(PcmPlaybackBuffer::default()));
    let failure = Arc::new(AudioOutputFailure::default());
    let closed = Arc::new(AtomicBool::new(false));
    let worker_buffer = Arc::clone(&buffer);
    let worker_failure = Arc::clone(&failure);
    let worker_closed = Arc::clone(&closed);
    let worker_slot = Arc::clone(&slot);
    let worker = thread::Builder::new()
        .name("frd-audio-output".to_owned())
        .spawn(move || {
            run_audio_worker(
                spec,
                opener,
                lease,
                startup_sender,
                worker_buffer,
                worker_failure,
                worker_closed,
                worker_slot,
            );
        })
        .map_err(|_| PlatformError::new("audio_output_worker_spawn_failed"))?;
    drop(worker);
    #[cfg(test)]
    slot.spawned_workers.fetch_add(1, Ordering::AcqRel);

    let Some(startup_remaining) = deadline.checked_duration_since(Instant::now()) else {
        closed.store(true, Ordering::Release);
        slot.mark_stuck();
        return Err(PlatformError::new("audio_output_open_timeout"));
    };
    match startup_receiver.recv_timeout(startup_remaining) {
        Ok(Ok(device_description)) => Ok(AudioOutputSink::new(
            spec,
            Box::new(WorkerAudioOutput::new(
                buffer,
                failure,
                closed,
                slot,
                device_description,
            )),
        )),
        Ok(Err(error)) => Err(error),
        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
            closed.store(true, Ordering::Release);
            slot.mark_stuck();
            Err(PlatformError::new("audio_output_open_timeout"))
        }
        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
            Err(PlatformError::new("audio_output_worker_start_failed"))
        }
    }
}

#[cfg(feature = "media")]
fn run_audio_worker(
    spec: AudioOutputSpec,
    opener: Arc<dyn AudioDeviceOpener>,
    _lease: AudioWorkerLease,
    startup_sender: Sender<Result<String, PlatformError>>,
    buffer: Arc<Mutex<PcmPlaybackBuffer>>,
    failure: Arc<AudioOutputFailure>,
    closed: Arc<AtomicBool>,
    slot: Arc<AudioWorkerSlot>,
) {
    let opened = match opener.open(spec, Arc::clone(&buffer), Arc::clone(&failure)) {
        Ok(opened) => opened,
        Err(error) => {
            slot.mark_closing();
            let _ = startup_sender.try_send(Err(error));
            return;
        }
    };
    let activated = slot.activate_if_opening(&closed).unwrap_or(false);
    if !activated {
        return;
    }
    let description = opened.device_description().to_owned();
    if startup_sender.try_send(Ok(description)).is_err() {
        slot.mark_closing();
        return;
    }

    slot.wait_until_not_active();
    drop(opened);
}

#[cfg(feature = "media")]
fn select_output_config(
    device: &cpal::Device,
    spec: AudioOutputSpec,
) -> Result<SupportedStreamConfig, PlatformError> {
    let mut candidates = device
        .supported_output_configs()
        .map_err(|_| PlatformError::new("audio_output_config_unavailable"))?
        .filter(|config| config.channels() >= spec.channels())
        .filter_map(|config| config.try_with_sample_rate(spec.sample_rate_hz()))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|config| {
        let channel_penalty = config.channels().saturating_sub(spec.channels());
        let format_rank = match config.sample_format() {
            SampleFormat::F32 => 0,
            SampleFormat::I16 => 1,
            _ => 2,
        };
        (channel_penalty, format_rank)
    });
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| PlatformError::new("audio_output_config_unsupported"))
}

#[cfg(feature = "media")]
fn build_output_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    buffer: &Arc<Mutex<PcmPlaybackBuffer>>,
    failure: &Arc<AudioOutputFailure>,
) -> Result<Stream, PlatformError>
where
    T: SizedSample + Sample + FromSample<f32>,
{
    let buffer = Arc::clone(buffer);
    let failure = Arc::clone(failure);
    device
        .build_output_stream(
            config,
            move |output: &mut [T], _| match buffer.lock() {
                Ok(mut buffer) => buffer.render(output, channels),
                Err(_) => output.fill(T::from_sample(0.0)),
            },
            move |_| {
                failure.mark_stream_failed();
                eprintln!("[audio-out] 本地音频输出流发生错误");
            },
            None,
        )
        .map_err(|_| PlatformError::new("audio_output_stream_create_failed"))
}

#[cfg(all(test, feature = "media"))]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use crossbeam_channel::{bounded, unbounded, Sender};

    use super::{
        open_audio_output_with_worker, AudioDeviceOpener, AudioOutputFailure, AudioWorkerSlot,
        OpenedAudioDevice, PcmPlaybackBuffer, PLAYBACK_QUEUE_CAPACITY_FRAMES,
    };
    use crate::platform::{AudioOutputSink, AudioOutputSpec, PlatformError};

    struct PermanentlyBlockingOpener {
        opens: Arc<AtomicUsize>,
        entered: Sender<()>,
    }

    struct FakeOpenedAudioDevice {
        drop_delay: Duration,
    }

    impl OpenedAudioDevice for FakeOpenedAudioDevice {
        fn device_description(&self) -> &str {
            "测试worker输出"
        }
    }

    impl Drop for FakeOpenedAudioDevice {
        fn drop(&mut self) {
            if !self.drop_delay.is_zero() {
                thread::sleep(self.drop_delay);
            }
        }
    }

    struct ReadyOpener {
        ready_sender: Sender<(
            Arc<AudioOutputFailure>,
            Arc<std::sync::Mutex<PcmPlaybackBuffer>>,
        )>,
        drop_delay: Duration,
    }

    impl AudioDeviceOpener for ReadyOpener {
        fn open(
            &self,
            _spec: AudioOutputSpec,
            buffer: Arc<std::sync::Mutex<PcmPlaybackBuffer>>,
            failure: Arc<AudioOutputFailure>,
        ) -> Result<Box<dyn OpenedAudioDevice>, PlatformError> {
            self.ready_sender.try_send((failure, buffer)).unwrap();
            Ok(Box::new(FakeOpenedAudioDevice {
                drop_delay: self.drop_delay,
            }))
        }
    }

    struct SequencedOpener {
        opens: AtomicUsize,
    }

    impl AudioDeviceOpener for SequencedOpener {
        fn open(
            &self,
            _spec: AudioOutputSpec,
            _buffer: Arc<std::sync::Mutex<PcmPlaybackBuffer>>,
            _failure: Arc<AudioOutputFailure>,
        ) -> Result<Box<dyn OpenedAudioDevice>, PlatformError> {
            let open = self.opens.fetch_add(1, Ordering::SeqCst);
            if open == 1 {
                thread::sleep(Duration::from_millis(90));
            }
            Ok(Box::new(FakeOpenedAudioDevice {
                drop_delay: if open == 0 {
                    Duration::from_millis(90)
                } else {
                    Duration::ZERO
                },
            }))
        }
    }

    impl AudioDeviceOpener for PermanentlyBlockingOpener {
        fn open(
            &self,
            _spec: AudioOutputSpec,
            _buffer: Arc<std::sync::Mutex<PcmPlaybackBuffer>>,
            _failure: Arc<AudioOutputFailure>,
        ) -> Result<Box<dyn OpenedAudioDevice>, PlatformError> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            let _ = self.entered.try_send(());
            loop {
                thread::park();
            }
        }
    }

    #[test]
    fn playback_buffer_preserves_interleaved_stereo_order() {
        let mut buffer = PcmPlaybackBuffer::default();
        buffer
            .try_enqueue_interleaved_stereo(&[i16::MIN, i16::MAX, 0, 16_384])
            .unwrap();
        let mut output = [0.0f32; 4];

        buffer.render(&mut output, 2);

        assert_eq!(output[0], -1.0);
        assert!(output[1] > 0.999);
        assert_eq!(output[2], 0.0);
        assert!(output[3] > 0.49 && output[3] < 0.51);
    }

    #[test]
    fn playback_buffer_rejects_a_whole_chunk_instead_of_evicting_old_frames() {
        let mut buffer = PcmPlaybackBuffer::default();
        let pcm = vec![0; PLAYBACK_QUEUE_CAPACITY_FRAMES * 2];

        buffer.try_enqueue_interleaved_stereo(&pcm).unwrap();
        assert_eq!(
            buffer
                .try_enqueue_interleaved_stereo(&[i16::MAX, i16::MIN])
                .unwrap_err()
                .code(),
            "audio_output_queue_full"
        );

        assert_eq!(buffer.frames.len(), PLAYBACK_QUEUE_CAPACITY_FRAMES);
        assert_eq!(buffer.frames.front(), Some(&[0, 0]));
        assert_eq!(buffer.frames.back(), Some(&[0, 0]));
    }

    #[test]
    fn stream_callback_failure_is_returned_by_the_next_enqueue() {
        let slot = Arc::new(AudioWorkerSlot::default());
        let (ready_sender, ready_receiver) = bounded(1);
        let mut output = open_audio_output_with_worker(
            AudioOutputSpec::normalized(),
            slot,
            Arc::new(ReadyOpener {
                ready_sender,
                drop_delay: Duration::ZERO,
            }),
            Duration::from_millis(100),
        )
        .unwrap();
        let (failure, _) = ready_receiver.recv().unwrap();
        failure.mark_stream_failed();

        assert_eq!(
            output.enqueue_interleaved_i16(&[1, -1]).unwrap_err().code(),
            "audio_output_stream_failed"
        );
    }

    #[test]
    fn shared_playback_buffer_accepts_one_max_chunk_and_rejects_the_next_without_eviction() {
        let slot = Arc::new(AudioWorkerSlot::default());
        let (ready_sender, ready_receiver) = bounded(1);
        let mut output = open_audio_output_with_worker(
            AudioOutputSpec::normalized(),
            slot,
            Arc::new(ReadyOpener {
                ready_sender,
                drop_delay: Duration::ZERO,
            }),
            Duration::from_millis(100),
        )
        .unwrap();
        let (_, buffer) = ready_receiver.recv().unwrap();
        let chunk = vec![0; AudioOutputSink::MAX_INTERLEAVED_I16_SAMPLES_PER_ENQUEUE];
        assert_eq!(chunk.len() * std::mem::size_of::<i16>(), 96_000);

        output.enqueue_interleaved_i16(&chunk).unwrap();
        assert_eq!(
            output.enqueue_interleaved_i16(&[1, -1]).unwrap_err().code(),
            "audio_output_queue_full"
        );
        let buffer = buffer.lock().unwrap();
        assert_eq!(buffer.frames.len(), PLAYBACK_QUEUE_CAPACITY_FRAMES);
        assert_eq!(buffer.frames.front(), Some(&[0, 0]));
        assert_eq!(buffer.frames.back(), Some(&[0, 0]));
    }

    #[test]
    fn immediate_reopen_waits_for_known_closing_worker_and_succeeds() {
        let slot = Arc::new(AudioWorkerSlot::default());
        let (ready_sender, _ready_receiver) = unbounded();
        let opener: Arc<dyn AudioDeviceOpener> = Arc::new(ReadyOpener {
            ready_sender,
            drop_delay: Duration::from_millis(40),
        });
        let first = open_audio_output_with_worker(
            AudioOutputSpec::normalized(),
            Arc::clone(&slot),
            Arc::clone(&opener),
            Duration::from_millis(150),
        )
        .unwrap();

        drop(first);
        let started = Instant::now();
        let second = open_audio_output_with_worker(
            AudioOutputSpec::normalized(),
            slot,
            opener,
            Duration::from_millis(150),
        )
        .expect("已知Closing状态必须在同一deadline内等待并重开");

        assert!(started.elapsed() >= Duration::from_millis(30));
        assert!(started.elapsed() < Duration::from_millis(150));
        drop(second);
    }

    #[test]
    fn closing_wait_and_next_startup_share_one_absolute_deadline() {
        let slot = Arc::new(AudioWorkerSlot::default());
        let opener: Arc<dyn AudioDeviceOpener> = Arc::new(SequencedOpener {
            opens: AtomicUsize::new(0),
        });
        let first = open_audio_output_with_worker(
            AudioOutputSpec::normalized(),
            Arc::clone(&slot),
            Arc::clone(&opener),
            Duration::from_millis(200),
        )
        .unwrap();

        drop(first);
        let started = Instant::now();
        let error = open_audio_output_with_worker(
            AudioOutputSpec::normalized(),
            slot,
            opener,
            Duration::from_millis(120),
        )
        .err()
        .expect("Closing等待与第二次startup不能各自获得完整deadline");

        assert_eq!(error.code(), "audio_output_open_timeout");
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_millis(165));
    }

    #[test]
    fn permanently_blocking_opener_uses_one_worker_and_all_requests_are_bounded() {
        let slot = Arc::new(AudioWorkerSlot::default());
        let opens = Arc::new(AtomicUsize::new(0));
        let (entered_sender, entered_receiver) = bounded(1);
        let opener: Arc<dyn AudioDeviceOpener> = Arc::new(PermanentlyBlockingOpener {
            opens: Arc::clone(&opens),
            entered: entered_sender,
        });
        let startup_timeout = Duration::from_millis(40);
        let started = Instant::now();

        let first = open_audio_output_with_worker(
            AudioOutputSpec::normalized(),
            Arc::clone(&slot),
            Arc::clone(&opener),
            startup_timeout,
        )
        .err()
        .expect("永久阻塞的opener必须在固定deadline返回失败");
        assert_eq!(first.code(), "audio_output_open_timeout");
        entered_receiver
            .recv_timeout(Duration::from_millis(250))
            .expect("测试worker必须进入注入opener");

        let mut callers = Vec::new();
        for _ in 0..16 {
            let slot = Arc::clone(&slot);
            let opener = Arc::clone(&opener);
            callers.push(thread::spawn(move || {
                open_audio_output_with_worker(
                    AudioOutputSpec::normalized(),
                    slot,
                    opener,
                    startup_timeout,
                )
                .err()
                .expect("占用的全局slot必须fail closed")
                .code()
            }));
        }
        for caller in callers {
            assert_eq!(caller.join().unwrap(), "audio_output_worker_busy");
        }
        for _ in 0..15 {
            assert_eq!(
                open_audio_output_with_worker(
                    AudioOutputSpec::normalized(),
                    Arc::clone(&slot),
                    Arc::clone(&opener),
                    startup_timeout,
                )
                .err()
                .expect("串行重试也不得spawn新worker")
                .code(),
                "audio_output_worker_busy"
            );
        }

        assert_eq!(slot.spawned_workers_for_test(), 1);
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
