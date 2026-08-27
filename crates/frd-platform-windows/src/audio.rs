use std::collections::VecDeque;

use frd_media_api::{AudioOutput, AudioOutputError};

const SAMPLE_RATE_HZ: u32 = 48_000;
const CHANNELS: u8 = 2;
const MAX_BUFFERED_FRAMES: usize = 4_800;

struct PcmPlaybackBuffer {
    max_frames: usize,
    frames: VecDeque<[i16; 2]>,
}

impl PcmPlaybackBuffer {
    fn new(max_frames: usize) -> Self {
        assert!(max_frames != 0, "PCM 队列容量必须大于零");
        Self {
            max_frames,
            frames: VecDeque::with_capacity(max_frames),
        }
    }

    fn enqueue_pcm(
        &mut self,
        sample_rate_hz: u32,
        channels: u8,
        samples: &[i16],
    ) -> Result<(), AudioOutputError> {
        if sample_rate_hz != SAMPLE_RATE_HZ
            || channels != CHANNELS
            || !samples.len().is_multiple_of(usize::from(CHANNELS))
        {
            return Err(AudioOutputError::UnsupportedFormat);
        }

        for frame in samples.chunks_exact(2) {
            if self.frames.len() == self.max_frames {
                self.frames.pop_front();
            }
            self.frames.push_back([frame[0], frame[1]]);
        }
        Ok(())
    }

    fn take_frame(&mut self) -> Option<[i16; 2]> {
        self.frames.pop_front()
    }
}

#[cfg(windows)]
pub struct WindowsAudioOutput {
    _stream: cpal::Stream,
    buffer: std::sync::Arc<std::sync::Mutex<PcmPlaybackBuffer>>,
    failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(windows)]
impl WindowsAudioOutput {
    pub fn open_default() -> Result<Self, AudioOutputError> {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};

        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        use cpal::SampleFormat;

        let device = cpal::default_host()
            .default_output_device()
            .ok_or(AudioOutputError::Unavailable)?;
        let supported = device
            .supported_output_configs()
            .map_err(|_| AudioOutputError::Unavailable)?
            .find(|range| {
                range.channels() == u16::from(CHANNELS)
                    && range.min_sample_rate() <= SAMPLE_RATE_HZ
                    && range.max_sample_rate() >= SAMPLE_RATE_HZ
                    && matches!(
                        range.sample_format(),
                        SampleFormat::I16 | SampleFormat::F32 | SampleFormat::U16
                    )
            })
            .ok_or(AudioOutputError::UnsupportedFormat)?;
        let sample_format = supported.sample_format();
        let config = supported.with_sample_rate(SAMPLE_RATE_HZ).config();
        let buffer = Arc::new(Mutex::new(PcmPlaybackBuffer::new(MAX_BUFFERED_FRAMES)));
        let failed = Arc::new(AtomicBool::new(false));
        let error_state = failed.clone();
        let error_callback = move |_| error_state.store(true, Ordering::Release);

        let stream = match sample_format {
            SampleFormat::I16 => {
                let callback_buffer = buffer.clone();
                device.build_output_stream::<i16, _, _>(
                    config,
                    move |output, _| fill_i16(output, &callback_buffer),
                    error_callback,
                    None,
                )
            }
            SampleFormat::F32 => {
                let callback_buffer = buffer.clone();
                device.build_output_stream::<f32, _, _>(
                    config,
                    move |output, _| fill_f32(output, &callback_buffer),
                    error_callback,
                    None,
                )
            }
            SampleFormat::U16 => {
                let callback_buffer = buffer.clone();
                device.build_output_stream::<u16, _, _>(
                    config,
                    move |output, _| fill_u16(output, &callback_buffer),
                    error_callback,
                    None,
                )
            }
            _ => return Err(AudioOutputError::UnsupportedFormat),
        }
        .map_err(|_| AudioOutputError::Unavailable)?;
        stream.play().map_err(|_| AudioOutputError::Unavailable)?;

        Ok(Self {
            _stream: stream,
            buffer,
            failed,
        })
    }
}

#[cfg(windows)]
impl AudioOutput for WindowsAudioOutput {
    fn enqueue_pcm(
        &mut self,
        sample_rate_hz: u32,
        channels: u8,
        samples: Box<[i16]>,
    ) -> Result<(), AudioOutputError> {
        use std::sync::atomic::Ordering;

        if self.failed.load(Ordering::Acquire) {
            return Err(AudioOutputError::Closed);
        }
        self.buffer
            .lock()
            .map_err(|_| AudioOutputError::Closed)?
            .enqueue_pcm(sample_rate_hz, channels, &samples)
    }
}

#[cfg(windows)]
fn fill_i16(output: &mut [i16], buffer: &std::sync::Arc<std::sync::Mutex<PcmPlaybackBuffer>>) {
    let Ok(mut buffer) = buffer.lock() else {
        output.fill(0);
        return;
    };
    for output_frame in output.chunks_mut(2) {
        let frame = buffer.take_frame().unwrap_or([0, 0]);
        output_frame[0] = frame[0];
        if output_frame.len() == 2 {
            output_frame[1] = frame[1];
        }
    }
}

#[cfg(windows)]
fn fill_f32(output: &mut [f32], buffer: &std::sync::Arc<std::sync::Mutex<PcmPlaybackBuffer>>) {
    let Ok(mut buffer) = buffer.lock() else {
        output.fill(0.0);
        return;
    };
    for output_frame in output.chunks_mut(2) {
        let frame = buffer.take_frame().unwrap_or([0, 0]);
        output_frame[0] = f32::from(frame[0]) / 32_768.0;
        if output_frame.len() == 2 {
            output_frame[1] = f32::from(frame[1]) / 32_768.0;
        }
    }
}

#[cfg(windows)]
fn fill_u16(output: &mut [u16], buffer: &std::sync::Arc<std::sync::Mutex<PcmPlaybackBuffer>>) {
    let Ok(mut buffer) = buffer.lock() else {
        output.fill(32_768);
        return;
    };
    for output_frame in output.chunks_mut(2) {
        let frame = buffer.take_frame().unwrap_or([0, 0]);
        output_frame[0] = (i32::from(frame[0]) + 32_768) as u16;
        if output_frame.len() == 2 {
            output_frame[1] = (i32::from(frame[1]) + 32_768) as u16;
        }
    }
}

#[cfg(not(windows))]
pub struct WindowsAudioOutput;

#[cfg(not(windows))]
impl WindowsAudioOutput {
    pub fn open_default() -> Result<Self, AudioOutputError> {
        Err(AudioOutputError::Unavailable)
    }
}

#[cfg(not(windows))]
impl AudioOutput for WindowsAudioOutput {
    fn enqueue_pcm(&mut self, _: u32, _: u8, _: Box<[i16]>) -> Result<(), AudioOutputError> {
        Err(AudioOutputError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use frd_media_api::AudioOutputError;

    use super::PcmPlaybackBuffer;

    #[test]
    fn pcm_output_queue_drops_the_oldest_whole_frame_at_its_latency_bound() {
        let mut buffer = PcmPlaybackBuffer::new(2);

        buffer
            .enqueue_pcm(48_000, 2, &[1, 2, 3, 4, 5, 6])
            .expect("valid stereo PCM is accepted");

        assert_eq!(buffer.take_frame(), Some([3, 4]));
        assert_eq!(buffer.take_frame(), Some([5, 6]));
        assert_eq!(buffer.take_frame(), None);
    }

    #[test]
    fn pcm_output_rejects_any_format_except_48khz_stereo() {
        let mut buffer = PcmPlaybackBuffer::new(2);

        assert_eq!(
            buffer.enqueue_pcm(44_100, 2, &[1, 2]),
            Err(AudioOutputError::UnsupportedFormat)
        );
        assert_eq!(
            buffer.enqueue_pcm(48_000, 1, &[1, 2]),
            Err(AudioOutputError::UnsupportedFormat)
        );
        assert_eq!(
            buffer.enqueue_pcm(48_000, 2, &[1]),
            Err(AudioOutputError::UnsupportedFormat)
        );
        assert_eq!(buffer.take_frame(), None);
    }
}
