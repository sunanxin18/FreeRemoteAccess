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
use std::sync::{Arc, Mutex};

#[cfg(feature = "media")]
const PLAYBACK_QUEUE_CAPACITY_MILLISECONDS: usize = 500;
#[cfg(feature = "media")]
const MILLISECONDS_PER_SECOND: usize = 1_000;
#[cfg(feature = "media")]
const PLAYBACK_QUEUE_CAPACITY_FRAMES: usize = AudioOutputSpec::NORMALIZED_SAMPLE_RATE_HZ as usize
    * PLAYBACK_QUEUE_CAPACITY_MILLISECONDS
    / MILLISECONDS_PER_SECOND;

pub(super) fn open_cpal_audio_output(
    spec: AudioOutputSpec,
) -> Result<AudioOutputSink, PlatformError> {
    #[cfg(feature = "media")]
    {
        CpalAudioOutput::open(spec).map(|backend| AudioOutputSink::new(spec, Box::new(backend)))
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
struct CpalAudioOutput {
    _stream: Stream,
    buffer: Arc<Mutex<PcmPlaybackBuffer>>,
    device_description: String,
}

#[cfg(feature = "media")]
impl CpalAudioOutput {
    fn open(spec: AudioOutputSpec) -> Result<Self, PlatformError> {
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
        let buffer = Arc::new(Mutex::new(PcmPlaybackBuffer::default()));
        let stream = match sample_format {
            SampleFormat::I8 => {
                build_output_stream::<i8>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::I16 => {
                build_output_stream::<i16>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::I24 => {
                build_output_stream::<I24>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::I32 => {
                build_output_stream::<i32>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::I64 => {
                build_output_stream::<i64>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::U8 => {
                build_output_stream::<u8>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::U16 => {
                build_output_stream::<u16>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::U24 => {
                build_output_stream::<U24>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::U32 => {
                build_output_stream::<u32>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::U64 => {
                build_output_stream::<u64>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::F32 => {
                build_output_stream::<f32>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::F64 => {
                build_output_stream::<f64>(&device, stream_config, channels, &buffer)
            }
            _ => Err(PlatformError::new("audio_output_format_unsupported")),
        }?;
        stream
            .play()
            .map_err(|_| PlatformError::new("audio_output_stream_start_failed"))?;
        Ok(Self {
            _stream: stream,
            buffer,
            device_description,
        })
    }
}

#[cfg(feature = "media")]
impl AudioOutputBackend for CpalAudioOutput {
    fn enqueue_interleaved_i16(&mut self, samples: &[i16]) -> Result<(), PlatformError> {
        self.buffer
            .lock()
            .map_err(|_| PlatformError::new("audio_output_queue_unavailable"))?
            .enqueue_interleaved_stereo(samples);
        Ok(())
    }

    fn device_description(&self) -> &str {
        &self.device_description
    }
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
) -> Result<Stream, PlatformError>
where
    T: SizedSample + Sample + FromSample<f32>,
{
    let buffer = Arc::clone(buffer);
    device
        .build_output_stream(
            config,
            move |output: &mut [T], _| match buffer.lock() {
                Ok(mut buffer) => buffer.render(output, channels),
                Err(_) => output.fill(T::from_sample(0.0)),
            },
            |_| eprintln!("[audio-out] 本地音频输出流发生错误"),
            None,
        )
        .map_err(|_| PlatformError::new("audio_output_stream_create_failed"))
}

#[cfg(all(test, feature = "media"))]
mod tests {
    use super::{PcmPlaybackBuffer, PLAYBACK_QUEUE_CAPACITY_FRAMES};

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
}
