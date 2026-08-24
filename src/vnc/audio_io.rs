//! Windows-first real-time PCM playback and test-only capture adapters for ARD audio.

use anyhow::{bail, ensure, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, Stream, SupportedStreamConfig, I24, U24,
};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::vnc::audio_codec::ARD_AUDIO_SAMPLE_RATE_HZ;
#[cfg(test)]
use crate::vnc::audio_codec::{
    ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT, ARD_AUDIO_PC_TO_MAC_PCM_SAMPLES_PER_ACCESS_UNIT,
    ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT,
};

const PLAYBACK_QUEUE_CAPACITY_MILLISECONDS: usize = 500;
const MILLISECONDS_PER_SECOND: usize = 1_000;
const PLAYBACK_QUEUE_CAPACITY_FRAMES: usize = ARD_AUDIO_SAMPLE_RATE_HZ as usize
    * PLAYBACK_QUEUE_CAPACITY_MILLISECONDS
    / MILLISECONDS_PER_SECOND;
#[cfg(test)]
const CAPTURE_QUEUE_CAPACITY_FRAMES: usize = PLAYBACK_QUEUE_CAPACITY_FRAMES;

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

/// Owns the active system output stream and a bounded 48 kHz stereo queue.
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
        let config = select_protocol_output_config(&device)?;
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

#[derive(Default)]
#[cfg(test)]
struct PcmCaptureBuffer {
    frames: VecDeque<[i16; 2]>,
    dropped_frames: u64,
}

#[cfg(test)]
impl PcmCaptureBuffer {
    fn enqueue<T>(&mut self, input: &[T], channels: usize)
    where
        T: Sample + Copy,
        f32: FromSample<T>,
    {
        if channels == 0 {
            return;
        }
        for input_frame in input.chunks_exact(channels) {
            let left = f32::from_sample(input_frame[0]);
            let right = if channels == 1 {
                left
            } else {
                f32::from_sample(input_frame[1])
            };
            if self.frames.len() == CAPTURE_QUEUE_CAPACITY_FRAMES {
                self.frames.pop_front();
                self.dropped_frames = self.dropped_frames.saturating_add(1);
            }
            self.frames.push_back([
                i16::from_sample(left.clamp(-1.0, 1.0)),
                i16::from_sample(right.clamp(-1.0, 1.0)),
            ]);
        }
    }

    #[cfg(test)]
    fn enqueue_interleaved_f32(&mut self, input: &[f32], channels: usize) {
        self.enqueue(input, channels);
    }

    #[cfg(test)]
    fn take_protocol_frame(&mut self) -> Option<Vec<i16>> {
        if self.frames.len() < ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT {
            return None;
        }
        let mut pcm = Vec::with_capacity(ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT);
        for _ in 0..ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT {
            let frame = self
                .frames
                .pop_front()
                .expect("已确认采集队列包含完整协议帧");
            pcm.extend_from_slice(&frame);
        }
        Some(pcm)
    }

    fn take_pc_to_mac_protocol_frame(&mut self) -> Option<Vec<i16>> {
        if self.frames.len() < ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT {
            return None;
        }
        let mut pcm = Vec::with_capacity(ARD_AUDIO_PC_TO_MAC_PCM_SAMPLES_PER_ACCESS_UNIT);
        for _ in 0..ARD_AUDIO_SAMPLES_PER_ACCESS_UNIT {
            let [left, right] = self
                .frames
                .pop_front()
                .expect("已确认采集队列包含完整协议帧");
            let mono = (i32::from(left) + i32::from(right)) / 2;
            pcm.push(mono as i16);
        }
        Some(pcm)
    }
}

/// Owns the active system input stream and yields exact 480-frame stereo PCM units.
#[cfg(test)]
pub struct AudioCapture {
    _stream: Stream,
    buffer: Arc<Mutex<PcmCaptureBuffer>>,
    device_description: String,
}

#[cfg(test)]
impl AudioCapture {
    pub fn open_default() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("没有可用的默认音频输入设备")?;
        let device_description = device
            .description()
            .map(|description| description.to_string())
            .unwrap_or_else(|_| "默认音频输入设备".to_string());
        let config = select_protocol_input_config(&device)?;
        let channels = usize::from(config.channels());
        let sample_format = config.sample_format();
        let stream_config = config.into();
        let buffer = Arc::new(Mutex::new(PcmCaptureBuffer::default()));
        let stream = match sample_format {
            SampleFormat::I8 => build_input_stream::<i8>(&device, stream_config, channels, &buffer),
            SampleFormat::I16 => {
                build_input_stream::<i16>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::I24 => {
                build_input_stream::<I24>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::I32 => {
                build_input_stream::<i32>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::I64 => {
                build_input_stream::<i64>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::U8 => build_input_stream::<u8>(&device, stream_config, channels, &buffer),
            SampleFormat::U16 => {
                build_input_stream::<u16>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::U24 => {
                build_input_stream::<U24>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::U32 => {
                build_input_stream::<u32>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::U64 => {
                build_input_stream::<u64>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::F32 => {
                build_input_stream::<f32>(&device, stream_config, channels, &buffer)
            }
            SampleFormat::F64 => {
                build_input_stream::<f64>(&device, stream_config, channels, &buffer)
            }
            unsupported => bail!("默认音频输入样本格式不受支持: {unsupported}"),
        }?;
        stream.play().context("启动默认音频输入流失败")?;
        Ok(Self {
            _stream: stream,
            buffer,
            device_description,
        })
    }

    pub fn device_description(&self) -> &str {
        &self.device_description
    }

    pub fn try_take_pc_to_mac_protocol_frame(&self) -> Result<Option<Vec<i16>>> {
        Ok(self
            .buffer
            .lock()
            .map_err(|_| anyhow::anyhow!("音频输入队列锁已损坏"))?
            .take_pc_to_mac_protocol_frame())
    }
}

fn select_protocol_output_config(device: &cpal::Device) -> Result<SupportedStreamConfig> {
    let mut candidates = device
        .supported_output_configs()
        .context("枚举音频输出格式失败")?
        .filter(|config| config.channels() >= 2)
        .filter_map(|config| config.try_with_sample_rate(ARD_AUDIO_SAMPLE_RATE_HZ))
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
        .context("默认输出设备不支持 ARD 所需的 48 kHz 双声道格式")
}

#[cfg(test)]
fn select_protocol_input_config(device: &cpal::Device) -> Result<SupportedStreamConfig> {
    let mut candidates = device
        .supported_input_configs()
        .context("枚举音频输入格式失败")?
        .filter(|config| config.channels() >= 1)
        .filter_map(|config| config.try_with_sample_rate(ARD_AUDIO_SAMPLE_RATE_HZ))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|config| {
        let channel_rank = if config.channels() == 2 {
            0
        } else if config.channels() == 1 {
            1
        } else {
            2
        };
        let format_rank = match config.sample_format() {
            SampleFormat::F32 => 0,
            SampleFormat::I16 => 1,
            _ => 2,
        };
        (channel_rank, format_rank)
    });
    candidates
        .into_iter()
        .next()
        .context("默认输入设备不支持 ARD 所需的 48 kHz 格式")
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
            |error| eprintln!("[audio-out] 输出流错误: {error}"),
            None,
        )
        .context("创建默认音频输出流失败")
}

#[cfg(test)]
fn build_input_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    buffer: &Arc<Mutex<PcmCaptureBuffer>>,
) -> Result<Stream>
where
    T: SizedSample + Sample + Copy,
    f32: FromSample<T>,
{
    let buffer = buffer.clone();
    device
        .build_input_stream(
            config,
            move |input: &[T], _| {
                if let Ok(mut buffer) = buffer.lock() {
                    buffer.enqueue(input, channels);
                }
            },
            |error| eprintln!("[audio-in] 输入流错误: {error}"),
            None,
        )
        .context("创建默认音频输入流失败")
}

#[cfg(test)]
mod tests {
    use super::{
        AudioCapture, AudioPlayback, PcmCaptureBuffer, PcmPlaybackBuffer,
        PLAYBACK_QUEUE_CAPACITY_FRAMES,
    };
    use crate::vnc::audio_codec::ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT;

    #[test]
    fn playback_buffer_preserves_stereo_order_at_protocol_sample_rate() {
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
    fn playback_buffer_drops_oldest_whole_frames_when_latency_bound_is_reached() {
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

    #[test]
    fn capture_buffer_converts_mono_to_exact_protocol_stereo_access_units() {
        let mut buffer = PcmCaptureBuffer::default();
        let mono = (0..ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT / 2)
            .map(|sample| sample as f32 / 1_000.0)
            .collect::<Vec<_>>();

        buffer.enqueue_interleaved_f32(&mono, 1);
        let frame = buffer
            .take_protocol_frame()
            .expect("完整的 480 帧输入应产生一个协议帧");

        assert_eq!(frame.len(), ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT);
        assert_eq!(frame[0], frame[1]);
        assert_eq!(frame[958], frame[959]);
        assert!(buffer.take_protocol_frame().is_none());
    }

    #[test]
    fn capture_buffer_waits_for_a_complete_access_unit() {
        let mut buffer = PcmCaptureBuffer::default();
        buffer.enqueue_interleaved_f32(&[0.25; 478 * 2], 2);

        assert!(buffer.take_protocol_frame().is_none());

        buffer.enqueue_interleaved_f32(&[0.5; 2 * 2], 2);
        assert_eq!(
            buffer.take_protocol_frame().unwrap().len(),
            ARD_AUDIO_PCM_SAMPLES_PER_ACCESS_UNIT
        );
    }

    #[test]
    fn capture_buffer_downmixes_stereo_for_pc_to_mac_mono_contract() {
        let mut buffer = PcmCaptureBuffer::default();
        buffer.enqueue_interleaved_f32(&[0.5, -0.5].repeat(480), 2);

        let frame = buffer.take_pc_to_mac_protocol_frame().unwrap();

        assert_eq!(frame.len(), 480);
        assert!(frame.iter().all(|sample| *sample == 0));
    }

    #[test]
    #[ignore = "需要当前桌面会话存在真实音频输出设备"]
    fn default_audio_playback_device_opens_at_protocol_format() {
        let playback = AudioPlayback::open_default().unwrap();
        playback.enqueue_interleaved_stereo(&[0; 960]).unwrap();
    }

    #[test]
    #[ignore = "需要当前桌面会话存在真实音频输入设备"]
    fn default_audio_capture_device_opens_at_protocol_format() {
        let capture = AudioCapture::open_default().unwrap();
        assert!(!capture.device_description().is_empty());
    }
}
