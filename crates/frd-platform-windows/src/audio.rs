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

    #[cfg(test)]
    fn enqueue_pcm(
        &mut self,
        sample_rate_hz: u32,
        channels: u8,
        samples: &[i16],
    ) -> Result<(), AudioOutputError> {
        let frames = prepare_pcm_frames(sample_rate_hz, channels, samples, self.max_frames)?;
        self.enqueue_frames(frames);
        Ok(())
    }

    fn enqueue_frames(&mut self, frames: VecDeque<[i16; 2]>) {
        debug_assert!(frames.len() <= self.max_frames);
        let overflow = self
            .frames
            .len()
            .saturating_add(frames.len())
            .saturating_sub(self.max_frames);
        self.frames.drain(..overflow);
        self.frames.extend(frames);
    }

    fn take_frame(&mut self) -> Option<[i16; 2]> {
        self.frames.pop_front()
    }
}

fn prepare_pcm_frames(
    sample_rate_hz: u32,
    channels: u8,
    samples: &[i16],
    max_frames: usize,
) -> Result<VecDeque<[i16; 2]>, AudioOutputError> {
    if sample_rate_hz != SAMPLE_RATE_HZ
        || channels != CHANNELS
        || !samples.len().is_multiple_of(usize::from(CHANNELS))
    {
        return Err(AudioOutputError::UnsupportedFormat);
    }

    let total_frames = samples.len() / usize::from(CHANNELS);
    let retained_frames = total_frames.min(max_frames);
    let start = (total_frames - retained_frames) * usize::from(CHANNELS);
    Ok(samples[start..]
        .chunks_exact(usize::from(CHANNELS))
        .map(|frame| [frame[0], frame[1]])
        .collect())
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
                    move |output, _| try_fill_i16(output, &callback_buffer),
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
        let frames = prepare_pcm_frames(sample_rate_hz, channels, &samples, MAX_BUFFERED_FRAMES)?;
        self.buffer
            .lock()
            .map_err(|_| AudioOutputError::Closed)?
            .enqueue_frames(frames);
        Ok(())
    }
}

#[cfg(any(windows, test))]
fn try_fill_i16(output: &mut [i16], buffer: &std::sync::Arc<std::sync::Mutex<PcmPlaybackBuffer>>) {
    output.fill(0);
    let Ok(mut buffer) = buffer.try_lock() else {
        return;
    };
    for output_frame in output.chunks_mut(2) {
        let Some(frame) = buffer.take_frame() else {
            break;
        };
        output_frame[0] = frame[0];
        if output_frame.len() == 2 {
            output_frame[1] = frame[1];
        }
    }
}

#[cfg(windows)]
fn fill_f32(output: &mut [f32], buffer: &std::sync::Arc<std::sync::Mutex<PcmPlaybackBuffer>>) {
    output.fill(0.0);
    let Ok(mut buffer) = buffer.try_lock() else {
        return;
    };
    for output_frame in output.chunks_mut(2) {
        let Some(frame) = buffer.take_frame() else {
            break;
        };
        output_frame[0] = f32::from(frame[0]) / 32_768.0;
        if output_frame.len() == 2 {
            output_frame[1] = f32::from(frame[1]) / 32_768.0;
        }
    }
}

#[cfg(windows)]
fn fill_u16(output: &mut [u16], buffer: &std::sync::Arc<std::sync::Mutex<PcmPlaybackBuffer>>) {
    output.fill(32_768);
    let Ok(mut buffer) = buffer.try_lock() else {
        return;
    };
    for output_frame in output.chunks_mut(2) {
        let Some(frame) = buffer.take_frame() else {
            break;
        };
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
    use std::sync::{Arc, Mutex};

    use frd_media_api::AudioOutputError;

    use super::{try_fill_i16, PcmPlaybackBuffer, MAX_BUFFERED_FRAMES};

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

    #[test]
    fn oversized_enqueue_retains_only_newest_4800_complete_stereo_frames() {
        let mut buffer = PcmPlaybackBuffer::new(MAX_BUFFERED_FRAMES);
        let samples = (0..5_000_i16)
            .flat_map(|frame| [frame, -frame])
            .collect::<Vec<_>>();

        buffer
            .enqueue_pcm(48_000, 2, &samples)
            .expect("valid oversized PCM is accepted");

        assert_eq!(buffer.take_frame(), Some([200, -200]));
        for _ in 1..MAX_BUFFERED_FRAMES - 1 {
            assert!(buffer.take_frame().is_some());
        }
        assert_eq!(buffer.take_frame(), Some([4_999, -4_999]));
        assert_eq!(buffer.take_frame(), None);
    }

    #[test]
    fn realtime_fill_is_nonblocking_and_silent_when_queue_lock_is_busy() {
        let buffer = Arc::new(Mutex::new(PcmPlaybackBuffer::new(2)));
        let _held = buffer.lock().expect("test owns queue lock");
        let mut output = [7_i16; 4];

        try_fill_i16(&mut output, &buffer);

        assert_eq!(output, [0, 0, 0, 0]);
    }

    #[test]
    fn realtime_fill_emits_silence_for_underrun() {
        let buffer = Arc::new(Mutex::new(PcmPlaybackBuffer::new(2)));
        buffer
            .lock()
            .expect("queue lock")
            .enqueue_pcm(48_000, 2, &[11, 22])
            .expect("valid PCM");
        let mut output = [7_i16; 4];

        try_fill_i16(&mut output, &buffer);

        assert_eq!(output, [11, 22, 0, 0]);
    }
}
