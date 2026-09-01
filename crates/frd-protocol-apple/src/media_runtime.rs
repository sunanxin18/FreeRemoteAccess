use std::net::IpAddr;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use frd_core::SessionId;
use frd_media_api::{MediaFrame, MediaStageDiagnostic, MediaStageTrace, VideoStreamIdentity};
use frd_protocol_api::{AudioState, ProtocolError, ProtocolRuntime, SessionEvent};

use crate::audio_codec::{
    ArdAudioReceiver, AudioReceiveOutcome, DecodedAudioPacket, ARD_AUDIO_CHANNEL_COUNT,
    ARD_AUDIO_SAMPLE_RATE_HZ,
};
use crate::connection::AppleWriterHandle;
use crate::hevc_access_unit::{HevcAccessUnitAssembler, HevcAccessUnitLimits, HevcRtpPacket};
use crate::high_performance_video::AppleHighPerformanceVideoAdapter;
use crate::media_negotiation::{AudioMediaFlow, MediaStreamAnswer};
use crate::media_protocol::MediaStreamPortAnnouncement;
use crate::media_transport::{MediaDatagram, MediaRole, MediaTransport, MediaTransportPhase};
use crate::srtp::parse_rtp_packet;

const MAX_PCM_PUBLICATION_SAMPLES: usize =
    ARD_AUDIO_SAMPLE_RATE_HZ as usize * ARD_AUDIO_CHANNEL_COUNT * 2;

pub(crate) fn publish_decoded_audio(
    runtime: &mut ProtocolRuntime,
    decoded: DecodedAudioPacket,
) -> Result<(), ProtocolError> {
    if decoded.pcm.len() > MAX_PCM_PUBLICATION_SAMPLES {
        return Err(ProtocolError::adapter(
            frd_core::ProtocolId::apple_hpss_mvs(),
            "apple_pcm_publication_too_large",
        ));
    }
    runtime.try_publish_optional_media(MediaFrame::Pcm {
        sample_rate_hz: ARD_AUDIO_SAMPLE_RATE_HZ,
        channels: ARD_AUDIO_CHANNEL_COUNT as u8,
        samples: decoded.pcm.into_boxed_slice(),
    })
}

pub(crate) struct ViewerMediaState {
    pub(crate) transport: MediaTransport,
    audio_receiver: ArdAudioReceiver,
    pub(crate) authenticated_audio_packets: u64,
    pub(crate) late_audio_packets: u64,
    pub(crate) audio_resynchronizations: u64,
    pub(crate) non_silent_audio_access_units: u64,
    pub(crate) concealed_audio_access_units: u64,
    pub(crate) authenticated_video_packets: u64,
    video_stream_1: VideoReceiveState,
    video_stream_2: VideoReceiveState,
    audio_degraded: bool,
    control_stage_trace: MediaStageTrace,
}

struct VideoReceiveState {
    assembler: HevcAccessUnitAssembler,
    adapter: AppleHighPerformanceVideoAdapter,
    stage_trace: MediaStageTrace,
}

impl VideoReceiveState {
    fn new(identity: VideoStreamIdentity, generation: u64) -> Result<Self> {
        Ok(Self {
            assembler: HevcAccessUnitAssembler::new(generation, HevcAccessUnitLimits::default())
                .context("创建 Apple HP HEVC AU 组装器失败")?,
            adapter: AppleHighPerformanceVideoAdapter::new(identity, generation),
            stage_trace: MediaStageTrace::default(),
        })
    }

    fn reset(&mut self, generation: u64) {
        self.assembler.reset(generation);
        self.adapter.reset(generation);
        self.stage_trace = MediaStageTrace::default();
    }

    fn accept_rtp(
        &mut self,
        runtime: &mut ProtocolRuntime,
        generation: u64,
        datagram: &[u8],
    ) -> Result<()> {
        let packet = parse_rtp_packet(datagram).context("解析 Apple HP 视频 RTP 数据报失败")?;
        let access_units = self
            .assembler
            .push(HevcRtpPacket {
                generation,
                ssrc: packet.header.ssrc,
                sequence: packet.header.sequence,
                timestamp: packet.header.timestamp,
                marker: packet.header.marker,
                payload: packet.payload,
            })
            .context("组装 Apple HP HEVC 访问单元失败")?;
        for access_unit in access_units {
            self.adapter
                .publish_access_unit(runtime, access_unit)
                .context("发布 Apple HP HEVC 访问单元失败")?;
        }
        self.stage_trace
            .observe(MediaStageDiagnostic::AuthenticatedVideoRtp {
                generation,
                stream_id: self.adapter.stream_id(),
            });
        Ok(())
    }
}

impl ViewerMediaState {
    #[cfg(test)]
    pub(crate) fn new(
        audio_flow: AudioMediaFlow,
        generation: u64,
        server_address: IpAddr,
    ) -> Result<Self> {
        Self::new_for_session(
            SessionId::allocate(),
            audio_flow,
            generation,
            server_address,
        )
    }

    pub(crate) fn new_for_session(
        session_id: SessionId,
        audio_flow: AudioMediaFlow,
        generation: u64,
        server_address: IpAddr,
    ) -> Result<Self> {
        Self::new_for_session_with_transport_factory(session_id, audio_flow, || {
            MediaTransport::new(generation, server_address)
        })
    }

    #[cfg(test)]
    fn new_with_transport_factory<F>(audio_flow: AudioMediaFlow, factory: F) -> Result<Self>
    where
        F: FnOnce() -> MediaTransport,
    {
        Self::new_for_session_with_transport_factory(SessionId::allocate(), audio_flow, factory)
    }

    fn new_for_session_with_transport_factory<F>(
        session_id: SessionId,
        audio_flow: AudioMediaFlow,
        factory: F,
    ) -> Result<Self>
    where
        F: FnOnce() -> MediaTransport,
    {
        validate_hpss_audio_flow(audio_flow)?;
        let mut transport = factory();
        let generation = transport.generation();
        transport.set_audio_flow(audio_flow)?;
        Ok(Self {
            transport,
            audio_receiver: ArdAudioReceiver::new().context("创建 Mac→PC AAC-ELD 接收器失败")?,
            authenticated_audio_packets: 0,
            late_audio_packets: 0,
            audio_resynchronizations: 0,
            non_silent_audio_access_units: 0,
            concealed_audio_access_units: 0,
            authenticated_video_packets: 0,
            video_stream_1: VideoReceiveState::new(
                VideoStreamIdentity {
                    session_id,
                    stream_id: 1,
                },
                generation,
            )?,
            video_stream_2: VideoReceiveState::new(
                VideoStreamIdentity {
                    session_id,
                    stream_id: 2,
                },
                generation,
            )?,
            audio_degraded: false,
            control_stage_trace: MediaStageTrace::default(),
        })
    }

    pub(crate) fn reset_generation(&mut self, generation: u64) -> Result<()> {
        self.transport.reset_generation(generation)?;
        self.audio_receiver = ArdAudioReceiver::new().context("重建 Mac→PC AAC-ELD 接收器失败")?;
        self.video_stream_1.reset(generation);
        self.video_stream_2.reset(generation);
        self.audio_degraded = false;
        self.control_stage_trace = MediaStageTrace::default();
        Ok(())
    }

    pub(crate) fn generation(&self) -> u64 {
        self.transport.generation()
    }

    pub(crate) fn handle_port_announcement(
        &mut self,
        generation: u64,
        announcement: MediaStreamPortAnnouncement,
        bind_address: IpAddr,
        writer: &AppleWriterHandle,
    ) -> Result<()> {
        if self.transport.phase() != MediaTransportPhase::Idle {
            return Ok(());
        }
        self.transport
            .accept_port_announcement(generation, announcement)?;
        self.transport
            .bind_local_sockets(generation, bind_address)?;
        let configuration = self.transport.prepare_configuration(generation)?;
        writer.send_private_message(&configuration)?;
        self.transport.mark_configuration_sent(generation)?;
        self.control_stage_trace
            .observe(MediaStageDiagnostic::Message1ConfigurationWritten { generation });
        Ok(())
    }

    pub(crate) fn handle_answer(
        &mut self,
        generation: u64,
        answer: MediaStreamAnswer,
    ) -> Result<()> {
        self.transport.accept_answer(generation, answer)?;
        self.transport.activate(generation)?;
        self.control_stage_trace
            .observe(MediaStageDiagnostic::Message2TransportActive { generation });
        Ok(())
    }

    pub(crate) fn service_active(
        &mut self,
        runtime: &mut ProtocolRuntime,
        generation: u64,
        now: Instant,
    ) -> Result<usize> {
        if self.transport.phase() != MediaTransportPhase::Active {
            return Ok(0);
        }
        self.transport.service_control_reports_at(generation, now)?;
        let Self {
            transport,
            audio_receiver,
            authenticated_audio_packets,
            late_audio_packets,
            audio_resynchronizations,
            non_silent_audio_access_units,
            concealed_audio_access_units,
            authenticated_video_packets,
            video_stream_1,
            video_stream_2,
            audio_degraded,
            control_stage_trace: _,
        } = self;
        let summary = transport.drain_receive_round(generation, |role, datagram| {
            accept_datagram(
                audio_receiver,
                authenticated_audio_packets,
                late_audio_packets,
                audio_resynchronizations,
                non_silent_audio_access_units,
                concealed_audio_access_units,
                authenticated_video_packets,
                video_stream_1,
                video_stream_2,
                audio_degraded,
                runtime,
                generation,
                role,
                datagram,
            )
        })?;
        Ok(summary.accepted_total)
    }

    pub(crate) fn close(&mut self, generation: u64) -> Result<()> {
        self.transport.close(generation)
    }
}

#[allow(clippy::too_many_arguments)]
fn accept_datagram(
    audio_receiver: &mut ArdAudioReceiver,
    authenticated_audio_packets: &mut u64,
    late_audio_packets: &mut u64,
    audio_resynchronizations: &mut u64,
    non_silent_audio_access_units: &mut u64,
    concealed_audio_access_units: &mut u64,
    authenticated_video_packets: &mut u64,
    video_stream_1: &mut VideoReceiveState,
    video_stream_2: &mut VideoReceiveState,
    audio_degraded: &mut bool,
    runtime: &mut ProtocolRuntime,
    generation: u64,
    role: MediaRole,
    datagram: MediaDatagram,
) -> Result<()> {
    if role == MediaRole::Audio && *audio_degraded {
        return Ok(());
    }
    match (role, datagram) {
        (MediaRole::Audio, MediaDatagram::Rtp(packet)) => {
            let outcome = match audio_receiver.decode_rtp_packet(&packet) {
                Err(_) => {
                    degrade_audio(audio_degraded, runtime);
                    return Ok(());
                }
                Ok(outcome) => outcome,
            };
            accept_audio_outcome(
                authenticated_audio_packets,
                late_audio_packets,
                audio_resynchronizations,
                non_silent_audio_access_units,
                concealed_audio_access_units,
                audio_degraded,
                runtime,
                outcome,
            );
        }
        (MediaRole::Audio, MediaDatagram::Rtcp(_)) => {}
        (MediaRole::VideoStream1, MediaDatagram::Rtp(packet)) => {
            video_stream_1.accept_rtp(runtime, generation, &packet)?;
            *authenticated_video_packets = authenticated_video_packets.saturating_add(1);
        }
        (MediaRole::VideoStream2, MediaDatagram::Rtp(packet)) => {
            video_stream_2.accept_rtp(runtime, generation, &packet)?;
            *authenticated_video_packets = authenticated_video_packets.saturating_add(1);
        }
        (MediaRole::VideoStream1 | MediaRole::VideoStream2, MediaDatagram::Rtcp(_)) => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn accept_audio_outcome(
    authenticated_audio_packets: &mut u64,
    late_audio_packets: &mut u64,
    audio_resynchronizations: &mut u64,
    non_silent_audio_access_units: &mut u64,
    concealed_audio_access_units: &mut u64,
    audio_degraded: &mut bool,
    runtime: &mut ProtocolRuntime,
    outcome: AudioReceiveOutcome,
) {
    if *audio_degraded {
        return;
    }
    let decoded = match outcome {
        AudioReceiveOutcome::DiscardedLate { .. } => {
            *late_audio_packets = late_audio_packets.saturating_add(1);
            return;
        }
        AudioReceiveOutcome::Decoded(decoded) => decoded,
        AudioReceiveOutcome::Resynchronized {
            decoded,
            skipped_access_units: _,
        } => {
            *audio_resynchronizations = audio_resynchronizations.saturating_add(1);
            decoded
        }
    };
    if decoded.pcm.iter().any(|sample| *sample != 0) {
        *non_silent_audio_access_units = non_silent_audio_access_units.saturating_add(1);
    }
    *concealed_audio_access_units =
        concealed_audio_access_units.saturating_add(decoded.concealed_access_units as u64);
    if publish_decoded_audio(runtime, decoded).is_err() {
        degrade_audio(audio_degraded, runtime);
        return;
    }
    *authenticated_audio_packets = authenticated_audio_packets.saturating_add(1);
}

fn degrade_audio(audio_degraded: &mut bool, runtime: &mut ProtocolRuntime) {
    if *audio_degraded {
        return;
    }
    *audio_degraded = true;
    let _ = runtime.publish_event(SessionEvent::AudioState(AudioState::Failed));
}

fn validate_hpss_audio_flow(audio_flow: AudioMediaFlow) -> Result<()> {
    if audio_flow == AudioMediaFlow::PcToMac {
        bail!(
            "用户名/密码 HPSS 不支持 PC→Mac Audio Chat；stock Apple 实现需要 IDS/Apple ID 邀请状态"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc, Mutex};

    use frd_core::{PixelRect, PixelSize, SessionId};
    use frd_frame::{PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate};
    use frd_media_api::{MediaFrame, MediaPublishError, MediaPublisher};
    use frd_protocol_api::{
        ProtocolError, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionEvent,
        SurfacePublisher,
    };

    use crate::audio_codec::DecodedAudioPacket;
    use crate::media_negotiation::AudioMediaFlow;

    use super::{
        accept_audio_outcome, accept_datagram, publish_decoded_audio, AudioReceiveOutcome,
        MediaDatagram, MediaRole, ViewerMediaState,
    };

    fn accept_state_datagram(
        state: &mut ViewerMediaState,
        runtime: &mut ProtocolRuntime,
        role: MediaRole,
        datagram: MediaDatagram,
    ) -> anyhow::Result<()> {
        accept_datagram(
            &mut state.audio_receiver,
            &mut state.authenticated_audio_packets,
            &mut state.late_audio_packets,
            &mut state.audio_resynchronizations,
            &mut state.non_silent_audio_access_units,
            &mut state.concealed_audio_access_units,
            &mut state.authenticated_video_packets,
            &mut state.video_stream_1,
            &mut state.video_stream_2,
            &mut state.audio_degraded,
            runtime,
            state.transport.generation(),
            role,
            datagram,
        )
    }

    fn establish_desktop_generation(runtime: &mut ProtocolRuntime, session_id: SessionId) {
        runtime
            .begin_generation(
                session_id,
                1,
                PixelSize::new(1, 1).unwrap(),
                PixelFormat::Bgrx8UnormSrgb,
            )
            .unwrap();
    }

    fn assert_desktop_surface_continues(runtime: &mut ProtocolRuntime, session_id: SessionId) {
        runtime
            .publish_surface(SurfaceUpdate::Damage {
                session_id,
                generation: 1,
                revision: 1,
                patches: vec![PixelPatch {
                    rect: PixelRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    stride_bytes: 4,
                    pixels: PixelBuffer::new(vec![1, 2, 3, 0]),
                }],
            })
            .unwrap();
    }

    struct NoopEvents;

    impl RuntimeEventSink for NoopEvents {
        fn publish(&self, _event: SessionEvent) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct RecordingEvents(Arc<Mutex<Vec<SessionEvent>>>);

    impl RuntimeEventSink for RecordingEvents {
        fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
    }

    struct NoopFrames;

    impl SurfacePublisher for NoopFrames {
        fn publish(&self, _update: frd_frame::SurfaceUpdate) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct NoopWake;

    impl RuntimeWake for NoopWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct RecordingMedia(Arc<Mutex<Vec<MediaFrame>>>);

    impl MediaPublisher for RecordingMedia {
        fn publish(&self, frame: MediaFrame) -> Result<(), MediaPublishError> {
            self.0.lock().unwrap().push(frame);
            Ok(())
        }
    }

    struct RejectingMedia {
        failure: MediaPublishError,
        attempts: Arc<Mutex<usize>>,
    }

    impl MediaPublisher for RejectingMedia {
        fn publish(&self, _frame: MediaFrame) -> Result<(), MediaPublishError> {
            *self.attempts.lock().unwrap() += 1;
            Err(self.failure.clone())
        }
    }

    #[test]
    fn decoded_aac_eld_publishes_bounded_48khz_stereo_pcm_without_platform_device() {
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let published = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(NoopFrames),
            Some(Box::new(RecordingMedia(published.clone()))),
            Box::new(NoopWake),
        );
        establish_desktop_generation(&mut runtime, session_id);
        publish_decoded_audio(
            &mut runtime,
            DecodedAudioPacket {
                pcm: vec![1, -2, 3, -4],
                concealed_access_units: 0,
                sequence: 7,
                timestamp: 480,
                ssrc: 0x1234_5678,
            },
        )
        .unwrap();

        let mut published = published.lock().unwrap();
        let MediaFrame::Pcm {
            sample_rate_hz,
            channels,
            samples,
        } = published.pop().expect("one PCM publication")
        else {
            panic!("AAC-ELD decode must publish PCM");
        };
        assert_eq!(sample_rate_hz, 48_000);
        assert_eq!(channels, 2);
        assert_eq!(&*samples, &[1, -2, 3, -4]);
    }

    #[test]
    fn password_hpss_pc_to_mac_fails_before_transport_selection() {
        let selected_transport = Arc::new(Mutex::new(false));
        let observed = selected_transport.clone();
        let result =
            ViewerMediaState::new_with_transport_factory(AudioMediaFlow::PcToMac, move || {
                *observed.lock().unwrap() = true;
                unreachable!("P5 must fail before selecting a network transport")
            });

        assert!(result.is_err());
        assert!(!*selected_transport.lock().unwrap());
    }

    #[test]
    fn authenticated_audio_codec_failure_degrades_once_while_video_and_runtime_continue() {
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let events = Arc::new(Mutex::new(Vec::new()));
        let published = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(RecordingEvents(events.clone())),
            Box::new(NoopFrames),
            Some(Box::new(RecordingMedia(published))),
            Box::new(NoopWake),
        );
        establish_desktop_generation(&mut runtime, session_id);
        let mut state =
            ViewerMediaState::new(AudioMediaFlow::MacToPc, 1, "127.0.0.1".parse().unwrap())
                .unwrap();

        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::Audio,
            MediaDatagram::Rtp(vec![0; 3]),
        )
        .unwrap();
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::Audio,
            MediaDatagram::Rtp(vec![0; 3]),
        )
        .unwrap();
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp(1, 1, true, &[0x02, 0x01, 0xaa])),
        )
        .unwrap();

        assert!(state.audio_degraded);
        assert!(!runtime.requires_shutdown());
        assert_eq!(state.authenticated_audio_packets, 0);
        assert_eq!(state.authenticated_video_packets, 1);
        assert_desktop_surface_continues(&mut runtime, session_id);
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| matches!(
                    event,
                    SessionEvent::AudioState(frd_protocol_api::AudioState::Failed)
                ))
                .count(),
            1,
            "degraded audio must not retry the codec or republish failure"
        );
    }

    #[test]
    fn authenticated_udp_video_routes_completed_au_through_the_unified_adapter() {
        let session_id = SessionId::allocate();
        let (_commands, command_rx) = mpsc::channel();
        let published = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(NoopFrames),
            Some(Box::new(RecordingMedia(published.clone()))),
            Box::new(NoopWake),
        );
        establish_desktop_generation(&mut runtime, session_id);
        let mut state = ViewerMediaState::new_for_session(
            session_id,
            AudioMediaFlow::MacToPc,
            1,
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();

        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp(1, 1, true, &[0x02, 0x01, 0xaa])),
        )
        .unwrap();
        assert!(published.lock().unwrap().is_empty());

        let mut aggregation = vec![0x60, 0x01];
        for nal in [
            &[0x40, 0x01, 0xaa][..],
            crate::hevc_sps::CAPTURED_MAIN444_8BIT_SPS,
            &[0x44, 0x01, 0xbb],
            &[0x26, 0x01, 0xcc],
        ] {
            aggregation.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            aggregation.extend_from_slice(nal);
        }
        accept_state_datagram(
            &mut state,
            &mut runtime,
            MediaRole::VideoStream1,
            MediaDatagram::Rtp(video_rtp(2, 90_000, true, &aggregation)),
        )
        .unwrap();

        let published = published.lock().unwrap();
        assert!(matches!(&published[0], MediaFrame::VideoConfig(config)
            if config.as_input().identity.session_id == session_id
                && config.as_input().identity.stream_id == 1));
        assert!(
            matches!(&published[1], MediaFrame::EncodedVideo(access_unit)
            if access_unit.identity().session_id == session_id
                && access_unit.identity().stream_id == 1)
        );
    }

    fn video_rtp(sequence: u16, timestamp: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x80, 96 | if marker { 0x80 } else { 0 }];
        packet.extend_from_slice(&sequence.to_be_bytes());
        packet.extend_from_slice(&timestamp.to_be_bytes());
        packet.extend_from_slice(&0x1020_3040_u32.to_be_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    #[test]
    fn pcm_full_or_closed_degrades_audio_once_without_poisoning_desktop_runtime() {
        for failure in [MediaPublishError::Full, MediaPublishError::Closed] {
            let session_id = SessionId::allocate();
            let (_commands, command_rx) = mpsc::channel();
            let events = Arc::new(Mutex::new(Vec::new()));
            let attempts = Arc::new(Mutex::new(0usize));
            let mut runtime = ProtocolRuntime::new(
                session_id,
                command_rx,
                Box::new(RecordingEvents(events.clone())),
                Box::new(NoopFrames),
                Some(Box::new(RejectingMedia {
                    failure,
                    attempts: attempts.clone(),
                })),
                Box::new(NoopWake),
            );
            establish_desktop_generation(&mut runtime, session_id);
            let mut state =
                ViewerMediaState::new(AudioMediaFlow::MacToPc, 1, "127.0.0.1".parse().unwrap())
                    .unwrap();
            let decoded = || crate::audio_codec::DecodedAudioPacket {
                pcm: vec![1, -2, 3, -4],
                concealed_access_units: 0,
                sequence: 7,
                timestamp: 480,
                ssrc: 0x1234_5678,
            };

            accept_audio_outcome(
                &mut state.authenticated_audio_packets,
                &mut state.late_audio_packets,
                &mut state.audio_resynchronizations,
                &mut state.non_silent_audio_access_units,
                &mut state.concealed_audio_access_units,
                &mut state.audio_degraded,
                &mut runtime,
                AudioReceiveOutcome::Decoded(decoded()),
            );
            accept_audio_outcome(
                &mut state.authenticated_audio_packets,
                &mut state.late_audio_packets,
                &mut state.audio_resynchronizations,
                &mut state.non_silent_audio_access_units,
                &mut state.concealed_audio_access_units,
                &mut state.audio_degraded,
                &mut runtime,
                AudioReceiveOutcome::Decoded(decoded()),
            );
            accept_state_datagram(
                &mut state,
                &mut runtime,
                MediaRole::VideoStream2,
                MediaDatagram::Rtp(video_rtp(1, 1, true, &[0x02, 0x01, 0xaa])),
            )
            .unwrap();

            assert!(state.audio_degraded);
            assert!(!runtime.requires_shutdown());
            assert_eq!(
                *attempts.lock().unwrap(),
                1,
                "degraded audio must not retry"
            );
            assert_eq!(state.authenticated_video_packets, 1);
            assert_desktop_surface_continues(&mut runtime, session_id);
            assert_eq!(
                events
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|event| matches!(
                        event,
                        SessionEvent::AudioState(frd_protocol_api::AudioState::Failed)
                    ))
                    .count(),
                1
            );
        }
    }
}
