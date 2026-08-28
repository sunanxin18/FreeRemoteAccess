use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use frd_media_api::MediaFrame;
use frd_protocol_api::{
    AudioState, ProtocolError, ProtocolRuntime, SessionCapabilities, SessionEvent,
};
use ironrdp::rdpsnd::client::{Rdpsnd, RdpsndClientHandler};
use ironrdp::rdpsnd::pdu::{AudioFormat, PitchPdu, VolumePdu, WaveFormat};

const PCM_SAMPLE_RATE_HZ: u32 = 48_000;
const PCM_CHANNELS: u16 = 2;
const PCM_BITS_PER_SAMPLE: u16 = 16;

fn supported_formats() -> &'static [AudioFormat] {
    static FORMATS: OnceLock<Vec<AudioFormat>> = OnceLock::new();
    FORMATS.get_or_init(|| {
        vec![AudioFormat {
            format: WaveFormat::PCM,
            n_channels: PCM_CHANNELS,
            n_samples_per_sec: PCM_SAMPLE_RATE_HZ,
            n_avg_bytes_per_sec: PCM_SAMPLE_RATE_HZ * u32::from(PCM_CHANNELS) * 2,
            n_block_align: PCM_CHANNELS * 2,
            bits_per_sample: PCM_BITS_PER_SAMPLE,
            data: None,
        }]
    })
}

pub(crate) fn new_rdpsnd(adapter: &RdpAudioAdapter) -> Rdpsnd {
    Rdpsnd::new(Box::new(adapter.clone()))
}

pub(crate) fn observe_negotiation(rdpsnd: &Rdpsnd, adapter: &RdpAudioAdapter) {
    if adapter.remote_audio() {
        return;
    }
    for format_no in 0..256u16 {
        let Ok(format) = rdpsnd.get_format(format_no) else {
            break;
        };
        if supported_formats()
            .iter()
            .any(|supported| supported == format)
        {
            adapter.negotiate_pcm(
                format.n_samples_per_sec,
                format.n_channels,
                format.bits_per_sample,
            );
            break;
        }
    }
}

pub(crate) fn capabilities(adapter: &RdpAudioAdapter) -> SessionCapabilities {
    SessionCapabilities {
        remote_audio: adapter.remote_audio(),
        ..SessionCapabilities::default()
    }
}

#[derive(Default)]
struct AudioStateInner {
    negotiated: bool,
    degraded: bool,
    playing: bool,
    pending_state: Option<AudioState>,
    frames: VecDeque<MediaFrame>,
}

#[derive(Clone, Default)]
pub(crate) struct RdpAudioAdapter {
    inner: Arc<Mutex<AudioStateInner>>,
}

impl std::fmt::Debug for RdpAudioAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RdpAudioAdapter")
            .finish_non_exhaustive()
    }
}

impl RdpAudioAdapter {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn negotiate_pcm(
        &self,
        sample_rate_hz: u32,
        channels: u16,
        bits_per_sample: u16,
    ) -> bool {
        let negotiated = sample_rate_hz == PCM_SAMPLE_RATE_HZ
            && channels == PCM_CHANNELS
            && bits_per_sample == PCM_BITS_PER_SAMPLE;
        if let Ok(mut inner) = self.inner.lock() {
            inner.negotiated = negotiated;
            if !negotiated {
                inner.frames.clear();
            }
        }
        negotiated
    }

    pub(crate) fn remote_audio(&self) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.negotiated && !inner.degraded)
            .unwrap_or(false)
    }

    pub(crate) fn accept_wave(&self, format_no: usize, data: &[u8]) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if !inner.negotiated || inner.degraded || format_no != 0 || !data.len().is_multiple_of(2) {
            return;
        }
        let samples = data
            .chunks_exact(2)
            .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        inner.frames.push_back(MediaFrame::Pcm {
            sample_rate_hz: PCM_SAMPLE_RATE_HZ,
            channels: u8::try_from(PCM_CHANNELS).expect("fixed channel count fits u8"),
            samples,
        });
    }

    pub(crate) fn take_frame(&self) -> Option<MediaFrame> {
        self.inner.lock().ok()?.frames.pop_front()
    }

    pub(crate) fn drain_to_runtime(
        &self,
        runtime: &mut ProtocolRuntime,
    ) -> Result<(), ProtocolError> {
        let pending_state = self
            .inner
            .lock()
            .ok()
            .and_then(|mut inner| inner.pending_state.take());
        if let Some(state) = pending_state {
            runtime.publish_event(SessionEvent::AudioState(state))?;
        }
        while let Some(frame) = self.take_frame() {
            if runtime.try_publish_optional_media(frame).is_err() {
                if let Ok(mut inner) = self.inner.lock() {
                    inner.degraded = true;
                    inner.frames.clear();
                }
                runtime.publish_event(SessionEvent::AudioState(AudioState::Failed))?;
                return Ok(());
            }
            let publish_playing = if let Ok(mut inner) = self.inner.lock() {
                if inner.playing {
                    false
                } else {
                    inner.playing = true;
                    true
                }
            } else {
                false
            };
            if publish_playing {
                runtime.publish_event(SessionEvent::AudioState(AudioState::Playing))?;
            }
        }
        Ok(())
    }

    pub(crate) fn stop(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            if (inner.negotiated || inner.playing) && !inner.degraded {
                inner.pending_state = Some(AudioState::Stopped);
            }
            inner.negotiated = false;
            inner.playing = false;
            inner.frames.clear();
        }
    }
}

impl RdpsndClientHandler for RdpAudioAdapter {
    fn get_formats(&self) -> &[AudioFormat] {
        supported_formats()
    }

    fn wave(&mut self, format_no: usize, _timestamp: u32, data: Cow<'_, [u8]>) {
        if !self.remote_audio() {
            let format = &supported_formats()[0];
            self.negotiate_pcm(
                format.n_samples_per_sec,
                format.n_channels,
                format.bits_per_sample,
            );
        }
        self.accept_wave(format_no, &data);
    }

    fn set_volume(&mut self, _volume: VolumePdu) {}

    fn set_pitch(&mut self, _pitch: PitchPdu) {}

    fn close(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use frd_core::SessionId;
    use frd_media_api::{MediaFrame, MediaPublishError, MediaPublisher};
    use frd_protocol_api::{
        ProtocolError, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionEvent,
        SurfacePublisher,
    };

    use super::RdpAudioAdapter;

    #[test]
    fn audio_requires_negotiation_and_converts_little_endian_pcm() {
        let adapter = RdpAudioAdapter::new();
        adapter.accept_wave(0, &[0x34, 0x12]);
        assert!(adapter.take_frame().is_none());

        assert!(!adapter.negotiate_pcm(44_100, 2, 16));
        assert!(adapter.negotiate_pcm(48_000, 2, 16));
        adapter.accept_wave(0, &[0x34, 0x12, 0xCC, 0xFF]);
        let MediaFrame::Pcm {
            sample_rate_hz,
            channels,
            samples,
        } = adapter.take_frame().expect("negotiated PCM frame")
        else {
            panic!("RDPSND must publish PCM")
        };
        assert_eq!(sample_rate_hz, 48_000);
        assert_eq!(channels, 2);
        assert_eq!(&*samples, &[0x1234, -52]);
    }

    #[test]
    fn audio_backpressure_degrades_only_audio_and_keeps_runtime_alive() {
        let adapter = RdpAudioAdapter::new();
        assert!(adapter.negotiate_pcm(48_000, 2, 16));
        adapter.accept_wave(0, &[0x01, 0x00, 0x02, 0x00]);
        let (_commands, command_rx) = mpsc::channel();
        let (events, event_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            SessionId::allocate(),
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(AcceptingFrames),
            Some(Box::new(FullMedia)),
            Box::new(NoopWake),
        );

        adapter
            .drain_to_runtime(&mut runtime)
            .expect("audio degrades locally");

        assert!(!adapter.remote_audio());
        assert!(!runtime.requires_shutdown());
        assert_eq!(event_rx.try_iter().collect::<Vec<_>>().len(), 1);
        adapter.accept_wave(0, &[0x03, 0x00]);
        assert!(
            adapter.take_frame().is_none(),
            "degraded audio does not retry"
        );
    }

    #[test]
    fn audio_close_publishes_stopped_without_stopping_the_runtime() {
        let adapter = RdpAudioAdapter::new();
        assert!(adapter.negotiate_pcm(48_000, 2, 16));
        let (_commands, command_rx) = mpsc::channel();
        let (events, event_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            SessionId::allocate(),
            command_rx,
            Box::new(RecordingEvents(events)),
            Box::new(AcceptingFrames),
            Some(Box::new(AcceptingMedia)),
            Box::new(NoopWake),
        );

        adapter.stop();
        adapter
            .drain_to_runtime(&mut runtime)
            .expect("audio stop remains local");

        assert!(!adapter.remote_audio());
        assert!(!runtime.requires_shutdown());
        assert_eq!(
            event_rx.try_iter().collect::<Vec<_>>(),
            vec![SessionEvent::AudioState(
                frd_protocol_api::AudioState::Stopped
            )]
        );
    }

    struct RecordingEvents(mpsc::Sender<SessionEvent>);

    impl RuntimeEventSink for RecordingEvents {
        fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError> {
            self.0
                .send(event)
                .map_err(|_| ProtocolError::EventPortClosed)
        }
    }

    struct AcceptingFrames;

    impl SurfacePublisher for AcceptingFrames {
        fn publish(&self, _: frd_frame::SurfaceUpdate) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct FullMedia;

    impl MediaPublisher for FullMedia {
        fn publish(&self, _: MediaFrame) -> Result<(), MediaPublishError> {
            Err(MediaPublishError::Full)
        }
    }

    struct AcceptingMedia;

    impl MediaPublisher for AcceptingMedia {
        fn publish(&self, _: MediaFrame) -> Result<(), MediaPublishError> {
            Ok(())
        }
    }

    struct NoopWake;

    impl RuntimeWake for NoopWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            Ok(())
        }
    }
}
