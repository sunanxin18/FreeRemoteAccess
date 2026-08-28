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
const PCM_BLOCK_ALIGN_BYTES: usize = 4;
const MAX_QUEUED_PCM_FRAMES: usize = 8;
const MAX_QUEUED_PCM_SAMPLES: usize = PCM_SAMPLE_RATE_HZ as usize * PCM_CHANNELS as usize;
const MAX_QUEUED_PCM_BYTES: usize = MAX_QUEUED_PCM_SAMPLES * 2;

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
    let Some(offer_epoch) = adapter.pending_server_offer() else {
        return;
    };
    let mut exact_format = None;
    for format_no in 0..256u16 {
        let Ok(format) = rdpsnd.get_format(format_no) else {
            break;
        };
        if supported_formats()
            .iter()
            .any(|supported| supported == format)
        {
            exact_format = Some(format);
            break;
        }
    }
    let (sample_rate_hz, channels, bits_per_sample) = exact_format
        .map(|format| {
            (
                format.n_samples_per_sec,
                format.n_channels,
                format.bits_per_sample,
            )
        })
        .unwrap_or_default();
    adapter.observe_server_offer(offer_epoch, sample_rate_hz, channels, bits_per_sample);
}

pub(crate) fn capabilities(adapter: &RdpAudioAdapter) -> SessionCapabilities {
    SessionCapabilities {
        remote_audio: adapter.remote_audio(),
        ..SessionCapabilities::default()
    }
}

#[derive(Default)]
struct AudioStateInner {
    next_offer_epoch: u64,
    pending_offer_epoch: Option<u64>,
    current_offer_epoch: Option<u64>,
    degraded: bool,
    playing: bool,
    pending_state: Option<AudioState>,
    frames: VecDeque<MediaFrame>,
    queued_samples: usize,
}

impl AudioStateInner {
    fn clear_frames(&mut self) {
        self.frames.clear();
        self.queued_samples = 0;
    }

    fn degrade(&mut self) {
        self.pending_offer_epoch = None;
        self.current_offer_epoch = None;
        self.degraded = true;
        self.playing = false;
        self.clear_frames();
        self.pending_state = Some(AudioState::Failed);
    }
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

    pub(crate) fn begin_server_offer(&self) -> Option<u64> {
        let mut inner = self.inner.lock().ok()?;
        let epoch = inner.next_offer_epoch.checked_add(1).unwrap_or(1);
        inner.next_offer_epoch = epoch;
        inner.pending_offer_epoch = Some(epoch);
        inner.current_offer_epoch = None;
        inner.degraded = false;
        inner.playing = false;
        inner.clear_frames();
        inner.pending_state = None;
        Some(epoch)
    }

    fn pending_server_offer(&self) -> Option<u64> {
        self.inner.lock().ok()?.pending_offer_epoch
    }

    pub(crate) fn observe_server_offer(
        &self,
        offer_epoch: u64,
        sample_rate_hz: u32,
        channels: u16,
        bits_per_sample: u16,
    ) -> bool {
        let compatible = sample_rate_hz == PCM_SAMPLE_RATE_HZ
            && channels == PCM_CHANNELS
            && bits_per_sample == PCM_BITS_PER_SAMPLE;
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        if inner.pending_offer_epoch != Some(offer_epoch) {
            return false;
        }
        inner.pending_offer_epoch = None;
        if compatible {
            inner.current_offer_epoch = Some(offer_epoch);
            true
        } else {
            false
        }
    }

    pub(crate) fn remote_audio(&self) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.current_offer_epoch.is_some() && !inner.degraded)
            .unwrap_or(false)
    }

    pub(crate) fn accept_wave(&self, format_no: usize, data: &[u8]) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.current_offer_epoch.is_none() || inner.degraded {
            return;
        }
        let sample_count = data.len() / 2;
        let queue_would_overflow = inner.frames.len() >= MAX_QUEUED_PCM_FRAMES
            || inner
                .queued_samples
                .checked_add(sample_count)
                .is_none_or(|queued| queued > MAX_QUEUED_PCM_SAMPLES);
        if format_no != 0
            || data.is_empty()
            || !data.len().is_multiple_of(PCM_BLOCK_ALIGN_BYTES)
            || data.len() > MAX_QUEUED_PCM_BYTES
            || queue_would_overflow
        {
            inner.degrade();
            return;
        }
        let mut samples = Vec::new();
        if samples.try_reserve_exact(sample_count).is_err() {
            inner.degrade();
            return;
        }
        samples.extend(
            data.chunks_exact(2)
                .map(|sample| i16::from_le_bytes([sample[0], sample[1]])),
        );
        inner.queued_samples += samples.len();
        inner.frames.push_back(MediaFrame::Pcm {
            sample_rate_hz: PCM_SAMPLE_RATE_HZ,
            channels: u8::try_from(PCM_CHANNELS).expect("fixed channel count fits u8"),
            samples: samples.into_boxed_slice(),
        });
    }

    pub(crate) fn take_frame(&self) -> Option<MediaFrame> {
        let mut inner = self.inner.lock().ok()?;
        let frame = inner.frames.pop_front()?;
        let MediaFrame::Pcm { samples, .. } = &frame else {
            return None;
        };
        inner.queued_samples = inner.queued_samples.saturating_sub(samples.len());
        Some(frame)
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
                    inner.degrade();
                    inner.pending_state = None;
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
            if (inner.current_offer_epoch.is_some() || inner.playing) && !inner.degraded {
                inner.pending_state = Some(AudioState::Stopped);
            }
            inner.pending_offer_epoch = None;
            inner.current_offer_epoch = None;
            inner.playing = false;
            inner.clear_frames();
        }
    }
}

impl RdpsndClientHandler for RdpAudioAdapter {
    fn get_formats(&self) -> &[AudioFormat] {
        // Pinned IronRDP 0.17 calls this while building the reply to each
        // successfully installed server AudioFormat PDU. That callback is the
        // negotiation boundary; merely rereading IronRDP's cached server list
        // after Close does not create an epoch.
        let _ = self.begin_server_offer();
        supported_formats()
    }

    fn wave(&mut self, format_no: usize, _timestamp: u32, data: Cow<'_, [u8]>) {
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
    use std::borrow::Cow;
    use std::sync::mpsc;

    use frd_core::SessionId;
    use frd_media_api::{MediaFrame, MediaPublishError, MediaPublisher};
    use frd_protocol_api::{
        ProtocolError, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionEvent,
        SurfacePublisher,
    };
    use ironrdp::core::encode_vec;
    use ironrdp::rdpsnd::client::RdpsndClientHandler;
    use ironrdp::rdpsnd::pdu::{
        ServerAudioFormatPdu, ServerAudioOutputPdu, TrainingPdu, Version, Wave2Pdu,
    };
    use ironrdp::svc::SvcProcessor;

    use super::{new_rdpsnd, observe_negotiation, supported_formats, RdpAudioAdapter};

    fn process_server_audio_pdu(
        rdpsnd: &mut ironrdp::rdpsnd::client::Rdpsnd,
        pdu: ServerAudioOutputPdu<'_>,
    ) {
        let bytes = encode_vec(&pdu).expect("server RDPSND PDU encodes");
        rdpsnd.process(&bytes).expect("server RDPSND PDU processes");
    }

    fn begin_offer(adapter: &RdpAudioAdapter) -> u64 {
        adapter
            .begin_server_offer()
            .expect("server offer begins an epoch")
    }

    #[test]
    fn audio_requires_negotiation_and_converts_little_endian_pcm() {
        let adapter = RdpAudioAdapter::new();
        adapter.accept_wave(0, &[0x34, 0x12]);
        assert!(adapter.take_frame().is_none());

        let incompatible_epoch = begin_offer(&adapter);
        assert!(!adapter.observe_server_offer(incompatible_epoch, 44_100, 2, 16));
        let exact_epoch = begin_offer(&adapter);
        assert!(adapter.observe_server_offer(exact_epoch, 48_000, 2, 16));
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
        let epoch = begin_offer(&adapter);
        assert!(adapter.observe_server_offer(epoch, 48_000, 2, 16));
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
        let epoch = begin_offer(&adapter);
        assert!(adapter.observe_server_offer(epoch, 48_000, 2, 16));
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

    #[test]
    fn audio_wave_requires_a_current_exact_server_offer_epoch() {
        let adapter = RdpAudioAdapter::new();
        let mut handler = adapter.clone();

        handler.wave(0, 0, Cow::Borrowed(&[0x01, 0x00, 0x02, 0x00]));
        assert!(!adapter.remote_audio());
        assert!(adapter.take_frame().is_none());

        let first_epoch = begin_offer(&adapter);
        assert!(adapter.observe_server_offer(first_epoch, 48_000, 2, 16));
        handler.close();
        assert!(!adapter.observe_server_offer(first_epoch, 48_000, 2, 16));
        handler.wave(0, 0, Cow::Borrowed(&[0x03, 0x00, 0x04, 0x00]));
        assert!(!adapter.remote_audio());
        assert!(adapter.take_frame().is_none());

        let second_epoch = begin_offer(&adapter);
        assert!(adapter.observe_server_offer(second_epoch, 48_000, 2, 16));
        handler.wave(0, 0, Cow::Borrowed(&[0x05, 0x00, 0x06, 0x00]));
        assert!(adapter.take_frame().is_some());
    }

    #[test]
    fn audio_real_processor_close_rejects_cached_offer_until_new_formats_pdu() {
        let adapter = RdpAudioAdapter::new();
        let mut inexact_rdpsnd = new_rdpsnd(&adapter);
        let mut inexact_format = supported_formats()[0].clone();
        inexact_format.n_block_align = 2;
        process_server_audio_pdu(
            &mut inexact_rdpsnd,
            ServerAudioOutputPdu::AudioFormat(ServerAudioFormatPdu {
                version: Version::V8,
                formats: vec![inexact_format],
            }),
        );
        observe_negotiation(&inexact_rdpsnd, &adapter);
        assert!(
            !adapter.remote_audio(),
            "matching headline PCM fields do not accept an inexact server format"
        );

        let adapter = RdpAudioAdapter::new();
        let mut rdpsnd = new_rdpsnd(&adapter);
        let formats = || {
            ServerAudioOutputPdu::AudioFormat(ServerAudioFormatPdu {
                version: Version::V8,
                formats: supported_formats().to_vec(),
            })
        };
        process_server_audio_pdu(&mut rdpsnd, formats());
        observe_negotiation(&rdpsnd, &adapter);
        assert!(adapter.remote_audio());
        process_server_audio_pdu(
            &mut rdpsnd,
            ServerAudioOutputPdu::Training(TrainingPdu {
                timestamp: 1,
                data: Vec::new(),
            }),
        );
        process_server_audio_pdu(
            &mut rdpsnd,
            ServerAudioOutputPdu::Wave2(Wave2Pdu {
                timestamp: 2,
                format_no: 0,
                block_no: 1,
                audio_timestamp: 2,
                data: Cow::Borrowed(&[0x01, 0x00, 0x02, 0x00]),
            }),
        );
        assert!(adapter.take_frame().is_some());

        process_server_audio_pdu(&mut rdpsnd, ServerAudioOutputPdu::Close);
        observe_negotiation(&rdpsnd, &adapter);
        assert!(!adapter.remote_audio());

        process_server_audio_pdu(&mut rdpsnd, formats());
        observe_negotiation(&rdpsnd, &adapter);
        assert!(adapter.remote_audio());
    }

    #[test]
    fn audio_misaligned_pcm_degrades_without_stopping_graphics() {
        let adapter = RdpAudioAdapter::new();
        let epoch = begin_offer(&adapter);
        assert!(adapter.observe_server_offer(epoch, 48_000, 2, 16));
        adapter.accept_wave(0, &[0x01, 0x00]);
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

        adapter
            .drain_to_runtime(&mut runtime)
            .expect("malformed audio degrades locally");

        assert!(!adapter.remote_audio());
        assert!(adapter.take_frame().is_none());
        assert!(!runtime.requires_shutdown());
        assert_eq!(
            event_rx.try_iter().collect::<Vec<_>>(),
            vec![SessionEvent::AudioState(
                frd_protocol_api::AudioState::Failed
            )]
        );
        let next_epoch = begin_offer(&adapter);
        assert!(
            adapter.observe_server_offer(next_epoch, 48_000, 2, 16),
            "a new exact server offer starts a fresh audio epoch"
        );
    }

    #[test]
    fn audio_oversize_or_ninth_queued_frame_degrades_before_publication() {
        let oversized = RdpAudioAdapter::new();
        let epoch = begin_offer(&oversized);
        assert!(oversized.observe_server_offer(epoch, 48_000, 2, 16));
        oversized.accept_wave(0, &vec![0; 192_004]);
        assert!(!oversized.remote_audio());
        assert!(oversized.take_frame().is_none());

        let queued = RdpAudioAdapter::new();
        let epoch = begin_offer(&queued);
        assert!(queued.observe_server_offer(epoch, 48_000, 2, 16));
        for _ in 0..9 {
            queued.accept_wave(0, &[0x01, 0x00, 0x02, 0x00]);
        }
        assert!(!queued.remote_audio());
        assert!(queued.take_frame().is_none());
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
