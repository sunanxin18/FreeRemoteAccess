use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::{bail, ensure, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, Stream, SupportedStreamConfig, I24, U24,
};

const PCM_SAMPLE_RATE_HZ: u32 = 48_000;
const PLAYBACK_QUEUE_CAPACITY_MILLISECONDS: usize = 500;
const MILLISECONDS_PER_SECOND: usize = 1_000;
const PLAYBACK_QUEUE_CAPACITY_FRAMES: usize =
    PCM_SAMPLE_RATE_HZ as usize * PLAYBACK_QUEUE_CAPACITY_MILLISECONDS / MILLISECONDS_PER_SECOND;

#[derive(Default)]
struct PcmPlaybackBuffer {
    frames: VecDeque<[i16; 2]>,
    dropped_frames: u64,
}

impl PcmPlaybackBuffer {
    fn enqueue_interleaved_stereo(&mut self, pcm: &[i16]) -> Result<()> {
        ensure!(pcm.len().is_multiple_of(2), "双声道 PCM 样本数必须为偶数");
        for frame in pcm.chunks_exact(2) {
            if self.frames.len() == PLAYBACK_QUEUE_CAPACITY_FRAMES {
                self.frames.pop_front();
                self.dropped_frames = self.dropped_frames.saturating_add(1);
            }
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

    #[cfg(test)]
    fn render_interleaved_f32(&mut self, output: &mut [f32], channels: usize) {
        self.render(output, channels);
    }
}

pub struct AudioPlayback {
    _stream: Stream,
    buffer: Arc<Mutex<PcmPlaybackBuffer>>,
    device_description: String,
}

impl AudioPlayback {
    pub fn open_default() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("没有可用的默认音频输出设备")?;
        let device_description = device
            .description()
            .map(|description| description.to_string())
            .unwrap_or_else(|_| "默认音频输出设备".to_string());
        let config = select_output_config(&device)?;
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
            unsupported => bail!("默认音频输出样本格式不受支持: {unsupported}"),
        }?;
        stream.play().context("启动默认音频输出流失败")?;
        Ok(Self {
            _stream: stream,
            buffer,
            device_description,
        })
    }

    pub fn device_description(&self) -> &str {
        &self.device_description
    }

    pub fn enqueue_interleaved_stereo(&self, pcm: &[i16]) -> Result<()> {
        self.buffer
            .lock()
            .map_err(|_| anyhow::anyhow!("音频输出队列锁已损坏"))?
            .enqueue_interleaved_stereo(pcm)
    }
}

fn select_output_config(device: &cpal::Device) -> Result<SupportedStreamConfig> {
    let mut candidates = device
        .supported_output_configs()
        .context("枚举音频输出格式失败")?
        .filter(|config| config.channels() >= 2)
        .filter_map(|config| config.try_with_sample_rate(PCM_SAMPLE_RATE_HZ))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|config| {
        let stereo_penalty = config.channels().saturating_sub(2);
        let format_rank = match config.sample_format() {
            SampleFormat::F32 => 0,
            SampleFormat::I16 => 1,
            _ => 2,
        };
        (stereo_penalty, format_rank)
    });
    candidates
        .into_iter()
        .next()
        .context("默认输出设备不支持 48 kHz 双声道格式")
}

fn build_output_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    buffer: &Arc<Mutex<PcmPlaybackBuffer>>,
) -> Result<Stream>
where
    T: SizedSample + Sample + FromSample<f32>,
{
    let buffer = buffer.clone();
    device
        .build_output_stream(
            config,
            move |output: &mut [T], _| match buffer.lock() {
                Ok(mut buffer) => buffer.render(output, channels),
                Err(_) => output.fill(T::from_sample(0.0)),
            },
            |error| eprintln!("[legacy-audio-out] 输出流错误: {error}"),
            None,
        )
        .context("创建默认音频输出流失败")
}

#[cfg(test)]
mod tests {
    use super::{PcmPlaybackBuffer, PLAYBACK_QUEUE_CAPACITY_FRAMES};

    #[test]
    fn playback_buffer_preserves_stereo_order() {
        let mut buffer = PcmPlaybackBuffer::default();
        buffer
            .enqueue_interleaved_stereo(&[i16::MIN, i16::MAX, 0, 16_384])
            .unwrap();
        let mut output = [0.0f32; 4];

        buffer.render_interleaved_f32(&mut output, 2);

        assert_eq!(output[0], -1.0);
        assert!(output[1] > 0.999);
        assert_eq!(output[2], 0.0);
        assert!(output[3] > 0.49 && output[3] < 0.51);
    }

    #[test]
    fn playback_buffer_drops_the_oldest_whole_frame_at_the_latency_bound() {
        let mut buffer = PcmPlaybackBuffer::default();
        let mut pcm = Vec::with_capacity((PLAYBACK_QUEUE_CAPACITY_FRAMES + 1) * 2);
        for frame in 0..=PLAYBACK_QUEUE_CAPACITY_FRAMES {
            let sample = i16::try_from(frame.min(i16::MAX as usize)).unwrap();
            pcm.extend_from_slice(&[sample, sample]);
        }

        buffer.enqueue_interleaved_stereo(&pcm).unwrap();
        let mut first = [0.0f32; 2];
        buffer.render_interleaved_f32(&mut first, 2);

        assert!(first[0] > 0.0);
        assert_eq!(first[0], first[1]);
    }
}
