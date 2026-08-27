use std::net::IpAddr;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use frd_media_api::MediaFrame;
use frd_protocol_api::{ProtocolError, ProtocolRuntime};

use crate::audio_codec::{
    ArdAudioReceiver, AudioReceiveOutcome, DecodedAudioPacket, ARD_AUDIO_CHANNEL_COUNT,
    ARD_AUDIO_SAMPLE_RATE_HZ,
};
use crate::connection::AppleWriterHandle;
use crate::media_negotiation::{AudioMediaFlow, MediaStreamAnswer};
use crate::media_protocol::MediaStreamPortAnnouncement;
use crate::media_transport::{MediaDatagram, MediaRole, MediaTransport, MediaTransportPhase};

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
    runtime.publish_media(MediaFrame::Pcm {
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
}

impl ViewerMediaState {
    pub(crate) fn new(
        audio_flow: AudioMediaFlow,
        generation: u64,
        server_address: IpAddr,
    ) -> Result<Self> {
        Self::new_with_transport_factory(audio_flow, || {
            MediaTransport::new(generation, server_address)
        })
    }

    fn new_with_transport_factory<F>(audio_flow: AudioMediaFlow, factory: F) -> Result<Self>
    where
        F: FnOnce() -> MediaTransport,
    {
        validate_hpss_audio_flow(audio_flow)?;
        let mut transport = factory();
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
        })
    }

    pub(crate) fn reset_generation(&mut self, generation: u64) -> Result<()> {
        self.transport.reset_generation(generation)?;
        self.audio_receiver = ArdAudioReceiver::new().context("重建 Mac→PC AAC-ELD 接收器失败")?;
        Ok(())
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
        Ok(())
    }

    pub(crate) fn handle_answer(
        &mut self,
        generation: u64,
        answer: MediaStreamAnswer,
    ) -> Result<()> {
        self.transport.accept_answer(generation, answer)?;
        self.transport.activate(generation)?;
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
                runtime,
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
    runtime: &mut ProtocolRuntime,
    role: MediaRole,
    datagram: MediaDatagram,
) -> Result<()> {
    match (role, datagram) {
        (MediaRole::Audio, MediaDatagram::Rtp(packet)) => {
            let decoded = match audio_receiver.decode_rtp_packet(&packet)? {
                AudioReceiveOutcome::DiscardedLate { .. } => {
                    *late_audio_packets = late_audio_packets.saturating_add(1);
                    return Ok(());
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
            publish_decoded_audio(runtime, decoded)
                .map_err(|error| anyhow::anyhow!(error.code()))?;
            *authenticated_audio_packets = authenticated_audio_packets.saturating_add(1);
        }
        (MediaRole::Audio, MediaDatagram::Rtcp(_)) => {}
        (MediaRole::VideoStream1 | MediaRole::VideoStream2, MediaDatagram::Rtp(_)) => {
            *authenticated_video_packets = authenticated_video_packets.saturating_add(1);
        }
        (MediaRole::VideoStream1 | MediaRole::VideoStream2, MediaDatagram::Rtcp(_)) => {}
    }
    Ok(())
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

    use frd_core::SessionId;
    use frd_media_api::{MediaFrame, MediaPublishError, MediaPublisher};
    use frd_protocol_api::{
        ProtocolError, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionEvent,
        SurfacePublisher,
    };

    use crate::audio_codec::DecodedAudioPacket;
    use crate::media_negotiation::AudioMediaFlow;

    use super::{publish_decoded_audio, ViewerMediaState};

    struct NoopEvents;

    impl RuntimeEventSink for NoopEvents {
        fn publish(&self, _event: SessionEvent) -> Result<(), ProtocolError> {
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
}
