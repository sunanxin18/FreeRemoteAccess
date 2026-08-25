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
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(feature = "media")]
use std::thread;
#[cfg(feature = "media")]
use std::time::Duration;

#[cfg(feature = "media")]
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};

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
const AUDIO_OUTPUT_WORKER_POLL_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(feature = "media")]
const AUDIO_OUTPUT_PCM_QUEUE_CAPACITY_CHUNKS: usize = 4;

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
    fn enqueue_interleaved_stereo(&mut self, pcm: &[i16]) {
        for frame in pcm.chunks_exact(2) {
            if self.frames.len() == PLAYBACK_QUEUE_CAPACITY_FRAMES {
                self.frames.pop_front();
            }
            self.frames.push_back([frame[0], frame[1]]);
        }
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
#[derive(Default)]
struct AudioWorkerSlot {
    occupied: AtomicBool,
    #[cfg(test)]
    spawned_workers: std::sync::atomic::AtomicUsize,
}

#[cfg(feature = "media")]
impl AudioWorkerSlot {
    fn try_acquire(self: &Arc<Self>) -> Result<AudioWorkerLease, PlatformError> {
        self.occupied
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| PlatformError::new("audio_output_worker_busy"))?;
        Ok(AudioWorkerLease {
            slot: Arc::clone(self),
        })
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
        self.slot.occupied.store(false, Ordering::Release);
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
    pcm_sender: Sender<Vec<i16>>,
    failure: Arc<AudioOutputFailure>,
    closed: Arc<AtomicBool>,
    device_description: String,
}

#[cfg(feature = "media")]
impl WorkerAudioOutput {
    fn new(
        pcm_sender: Sender<Vec<i16>>,
        failure: Arc<AudioOutputFailure>,
        closed: Arc<AtomicBool>,
        device_description: String,
    ) -> Self {
        Self {
            pcm_sender,
            failure,
            closed,
            device_description,
        }
    }
}

#[cfg(feature = "media")]
impl AudioOutputBackend for WorkerAudioOutput {
    fn enqueue_interleaved_i16(&mut self, samples: &[i16]) -> Result<(), PlatformError> {
        self.failure.check()?;
        match self.pcm_sender.try_send(samples.to_vec()) {
            Ok(()) => self.failure.check(),
            Err(TrySendError::Full(_)) => Err(PlatformError::new("audio_output_queue_full")),
            Err(TrySendError::Disconnected(_)) => {
                self.failure.check()?;
                Err(PlatformError::new("audio_output_worker_unavailable"))
            }
        }
    }

    fn device_description(&self) -> &str {
        &self.device_description
    }
}

#[cfg(feature = "media")]
impl Drop for WorkerAudioOutput {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}

#[cfg(feature = "media")]
fn checked_audio_queue_capacity_bytes() -> Result<usize, PlatformError> {
    AudioOutputSink::MAX_INTERLEAVED_I16_SAMPLES_PER_ENQUEUE
        .checked_mul(std::mem::size_of::<i16>())
        .and_then(|bytes| bytes.checked_mul(AUDIO_OUTPUT_PCM_QUEUE_CAPACITY_CHUNKS))
        .ok_or_else(|| PlatformError::new("audio_output_queue_capacity_overflow"))
}

#[cfg(feature = "media")]
fn open_audio_output_with_worker(
    spec: AudioOutputSpec,
    slot: Arc<AudioWorkerSlot>,
    opener: Arc<dyn AudioDeviceOpener>,
    startup_timeout: Duration,
) -> Result<AudioOutputSink, PlatformError> {
    let _queue_capacity_bytes = checked_audio_queue_capacity_bytes()?;
    if startup_timeout.is_zero() {
        return Err(PlatformError::new("audio_output_open_timeout"));
    }
    let lease = slot.try_acquire()?;
    let (pcm_sender, pcm_receiver) = bounded(AUDIO_OUTPUT_PCM_QUEUE_CAPACITY_CHUNKS);
    let (startup_sender, startup_receiver) = bounded(1);
    let failure = Arc::new(AudioOutputFailure::default());
    let closed = Arc::new(AtomicBool::new(false));
    let worker_failure = Arc::clone(&failure);
    let worker_closed = Arc::clone(&closed);
    let worker = thread::Builder::new()
        .name("frd-audio-output".to_owned())
        .spawn(move || {
            run_audio_worker(
                spec,
                opener,
                lease,
                pcm_receiver,
                startup_sender,
                worker_failure,
                worker_closed,
            );
        })
        .map_err(|_| PlatformError::new("audio_output_worker_spawn_failed"))?;
    drop(worker);
    #[cfg(test)]
    slot.spawned_workers.fetch_add(1, Ordering::AcqRel);

    match startup_receiver.recv_timeout(startup_timeout) {
        Ok(Ok(device_description)) => Ok(AudioOutputSink::new(
            spec,
            Box::new(WorkerAudioOutput::new(
                pcm_sender,
                failure,
                closed,
                device_description,
            )),
        )),
        Ok(Err(error)) => Err(error),
        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
            closed.store(true, Ordering::Release);
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
    pcm_receiver: Receiver<Vec<i16>>,
    startup_sender: Sender<Result<String, PlatformError>>,
    failure: Arc<AudioOutputFailure>,
    closed: Arc<AtomicBool>,
) {
    let buffer = Arc::new(Mutex::new(PcmPlaybackBuffer::default()));
    let opened = match opener.open(spec, Arc::clone(&buffer), Arc::clone(&failure)) {
        Ok(opened) => opened,
        Err(error) => {
            let _ = startup_sender.try_send(Err(error));
            return;
        }
    };
    if closed.load(Ordering::Acquire) {
        return;
    }
    let description = opened.device_description().to_owned();
    if startup_sender.try_send(Ok(description)).is_err() {
        return;
    }

    while !closed.load(Ordering::Acquire) && failure.check().is_ok() {
        match pcm_receiver.recv_timeout(AUDIO_OUTPUT_WORKER_POLL_INTERVAL) {
            Ok(pcm) => match buffer.lock() {
                Ok(mut buffer) => buffer.enqueue_interleaved_stereo(&pcm),
                Err(_) => failure.mark_stream_failed(),
            },
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
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

    use crossbeam_channel::{bounded, Sender};

    use super::{
        open_audio_output_with_worker, AudioDeviceOpener, AudioOutputFailure, AudioWorkerSlot,
        OpenedAudioDevice, PcmPlaybackBuffer, WorkerAudioOutput,
        AUDIO_OUTPUT_PCM_QUEUE_CAPACITY_CHUNKS, PLAYBACK_QUEUE_CAPACITY_FRAMES,
    };
    use crate::platform::{AudioOutputBackend, AudioOutputSink, AudioOutputSpec, PlatformError};

    struct PermanentlyBlockingOpener {
        opens: Arc<AtomicUsize>,
        entered: Sender<()>,
    }

    struct FakeOpenedAudioDevice;

    impl OpenedAudioDevice for FakeOpenedAudioDevice {
        fn device_description(&self) -> &str {
            "测试worker输出"
        }
    }

    struct ReadyOpener {
        failure_sender: Sender<Arc<AudioOutputFailure>>,
    }

    impl AudioDeviceOpener for ReadyOpener {
        fn open(
            &self,
            _spec: AudioOutputSpec,
            _buffer: Arc<std::sync::Mutex<PcmPlaybackBuffer>>,
            failure: Arc<AudioOutputFailure>,
        ) -> Result<Box<dyn OpenedAudioDevice>, PlatformError> {
            self.failure_sender.try_send(failure).unwrap();
            Ok(Box::new(FakeOpenedAudioDevice))
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

    fn worker_output_for_test() -> (
        WorkerAudioOutput,
        crossbeam_channel::Receiver<Vec<i16>>,
        Arc<AudioOutputFailure>,
    ) {
        let (sender, receiver) = bounded(AUDIO_OUTPUT_PCM_QUEUE_CAPACITY_CHUNKS);
        let failure = Arc::new(AudioOutputFailure::default());
        let output = WorkerAudioOutput::new(
            sender,
            Arc::clone(&failure),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            "测试worker输出".to_owned(),
        );
        (output, receiver, failure)
    }

    #[test]
    fn playback_buffer_preserves_interleaved_stereo_order() {
        let mut buffer = PcmPlaybackBuffer::default();
        buffer.enqueue_interleaved_stereo(&[i16::MIN, i16::MAX, 0, 16_384]);
        let mut output = [0.0f32; 4];

        buffer.render(&mut output, 2);

        assert_eq!(output[0], -1.0);
        assert!(output[1] > 0.999);
        assert_eq!(output[2], 0.0);
        assert!(output[3] > 0.49 && output[3] < 0.51);
    }

    #[test]
    fn playback_buffer_drops_the_oldest_whole_frame_at_the_latency_bound() {
        let mut buffer = PcmPlaybackBuffer::default();
        let mut pcm = vec![0; PLAYBACK_QUEUE_CAPACITY_FRAMES * 2];
        pcm.extend_from_slice(&[i16::MAX, i16::MIN]);

        buffer.enqueue_interleaved_stereo(&pcm);

        assert_eq!(buffer.frames.len(), PLAYBACK_QUEUE_CAPACITY_FRAMES);
        assert_eq!(buffer.frames.back(), Some(&[i16::MAX, i16::MIN]));
    }

    #[test]
    fn stream_callback_failure_is_returned_by_the_next_enqueue() {
        let slot = Arc::new(AudioWorkerSlot::default());
        let (failure_sender, failure_receiver) = bounded(1);
        let mut output = open_audio_output_with_worker(
            AudioOutputSpec::normalized(),
            slot,
            Arc::new(ReadyOpener { failure_sender }),
            Duration::from_millis(100),
        )
        .unwrap();
        let failure = failure_receiver.recv().unwrap();
        failure.mark_stream_failed();

        assert_eq!(
            output.enqueue_interleaved_i16(&[1, -1]).unwrap_err().code(),
            "audio_output_stream_failed"
        );
    }

    #[test]
    fn pcm_worker_queue_has_a_checked_total_byte_bound_and_never_silently_drops() {
        let (mut output, receiver, _failure) = worker_output_for_test();
        let chunk = vec![0; AudioOutputSink::MAX_INTERLEAVED_I16_SAMPLES_PER_ENQUEUE];

        for _ in 0..AUDIO_OUTPUT_PCM_QUEUE_CAPACITY_CHUNKS {
            output.enqueue_interleaved_i16(&chunk).unwrap();
        }
        assert_eq!(
            output.enqueue_interleaved_i16(&[1, -1]).unwrap_err().code(),
            "audio_output_queue_full"
        );
        assert_eq!(receiver.len(), 4);
        assert_eq!(
            (0..AUDIO_OUTPUT_PCM_QUEUE_CAPACITY_CHUNKS)
                .map(|_| receiver.try_recv().unwrap().len() * 2)
                .sum::<usize>(),
            1_048_576
        );
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
