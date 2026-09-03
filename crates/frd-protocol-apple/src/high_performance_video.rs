use std::error::Error;
use std::fmt;
use std::num::NonZeroU32;

use frd_core::{PixelRect, PixelSize};
use frd_media_api::{
    ChromaFormat, ChromaLocation, EncodedVideoAccessUnit, MediaFrame, MediaPublishError,
    MediaStageDiagnostic, MediaStageTrace, VideoBitstreamFormat, VideoCodec, VideoColorimetry,
    VideoParameterSets, VideoProfile, VideoRange, VideoStreamConfig, VideoStreamConfigInput,
    VideoStreamIdentity, VideoTimeBase, VideoTimestamp,
};
use frd_protocol_api::{ProtocolRuntime, RecoverableMediaPublishOutcome};

use crate::hevc_access_unit::HevcAccessUnit;
use crate::hevc_sps::{parse_hevc_sps, HevcSps};

pub(crate) const APPLE_HEVC_RTP_CLOCK_HZ: u32 = 90_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AppleHighPerformanceVideoError {
    StaleGeneration,
    MalformedAccessUnit,
    UnsupportedStreamConfig,
    InvalidStreamConfig,
    StreamConfigChangedWithinGeneration,
    MediaPublicationFailed(MediaPublishError),
}

impl fmt::Display for AppleHighPerformanceVideoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::StaleGeneration => "Apple HP 视频 generation 已过期",
            Self::MalformedAccessUnit => "Apple HP 视频访问单元非法",
            Self::UnsupportedStreamConfig => "Apple HP 视频配置不受支持",
            Self::InvalidStreamConfig => "Apple HP 视频配置非法",
            Self::StreamConfigChangedWithinGeneration => {
                "Apple HP 视频参数集在同一 generation 内变化"
            }
            Self::MediaPublicationFailed(MediaPublishError::Full) => "Apple HP 视频发布队列已满",
            Self::MediaPublicationFailed(MediaPublishError::Closed) => "Apple HP 视频发布端已关闭",
        })
    }
}

impl Error for AppleHighPerformanceVideoError {}

#[cfg(any(debug_assertions, test))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AppleHighPerformanceVideoDiagnostics {
    pub(crate) video_config_publications: u64,
    pub(crate) encoded_video_publications: u64,
}

#[derive(Debug)]
pub(crate) struct AppleHighPerformanceVideoAdapter {
    identity: VideoStreamIdentity,
    generation: u64,
    parameter_sets: [Option<Vec<u8>>; 3],
    configured_parameter_sets: Option<[Option<Vec<u8>>; 3]>,
    stage_trace: MediaStageTrace,
    #[cfg(any(debug_assertions, test))]
    diagnostics: AppleHighPerformanceVideoDiagnostics,
}

impl AppleHighPerformanceVideoAdapter {
    pub(crate) fn new(identity: VideoStreamIdentity, generation: u64) -> Self {
        Self {
            identity,
            generation,
            parameter_sets: [None, None, None],
            configured_parameter_sets: None,
            stage_trace: MediaStageTrace::default(),
            #[cfg(any(debug_assertions, test))]
            diagnostics: AppleHighPerformanceVideoDiagnostics::default(),
        }
    }

    pub(crate) fn reset(&mut self, generation: u64) {
        self.generation = generation;
        self.parameter_sets = [None, None, None];
        self.configured_parameter_sets = None;
        self.stage_trace = MediaStageTrace::default();
        #[cfg(any(debug_assertions, test))]
        {
            self.diagnostics = AppleHighPerformanceVideoDiagnostics::default();
        }
    }

    #[cfg(any(debug_assertions, test))]
    pub(crate) const fn diagnostics(&self) -> AppleHighPerformanceVideoDiagnostics {
        self.diagnostics
    }

    pub(crate) const fn stream_id(&self) -> u32 {
        self.identity.stream_id
    }

    pub(crate) fn publish_access_unit(
        &mut self,
        runtime: &mut ProtocolRuntime,
        access_unit: HevcAccessUnit,
    ) -> Result<(), AppleHighPerformanceVideoError> {
        if access_unit.generation != self.generation {
            return Err(AppleHighPerformanceVideoError::StaleGeneration);
        }
        let nals = access_unit
            .nal_units()
            .map_err(|_| AppleHighPerformanceVideoError::MalformedAccessUnit)?;
        let mut next_parameter_sets = self.parameter_sets.clone();
        let mut parsed_sps = None;
        for nal in &nals {
            match nal_type(nal) {
                Some(32) => next_parameter_sets[0] = Some((*nal).to_vec()),
                Some(33) => {
                    parsed_sps =
                        Some(parse_hevc_sps(nal).map_err(|_| {
                            AppleHighPerformanceVideoError::UnsupportedStreamConfig
                        })?);
                    next_parameter_sets[1] = Some((*nal).to_vec());
                }
                Some(34) => next_parameter_sets[2] = Some((*nal).to_vec()),
                _ => {}
            }
        }

        if let Some(configured) = &self.configured_parameter_sets {
            if configured != &next_parameter_sets {
                return Err(AppleHighPerformanceVideoError::StreamConfigChangedWithinGeneration);
            }
        }

        let mut published_config_size = None;
        if self.configured_parameter_sets.is_none() {
            let Some(sps_bytes) = next_parameter_sets[1].as_deref() else {
                self.parameter_sets = next_parameter_sets;
                return Ok(());
            };
            let Some(pps) = next_parameter_sets[2].clone() else {
                self.parameter_sets = next_parameter_sets;
                return Ok(());
            };
            let sps = match parsed_sps {
                Some(sps) => sps,
                None => parse_hevc_sps(sps_bytes)
                    .map_err(|_| AppleHighPerformanceVideoError::UnsupportedStreamConfig)?,
            };
            let parameter_sets = VideoParameterSets::try_new(
                next_parameter_sets[0].clone().map(Vec::into_boxed_slice),
                sps_bytes.to_vec().into_boxed_slice(),
                pps.into_boxed_slice(),
            )
            .map_err(|_| AppleHighPerformanceVideoError::InvalidStreamConfig)?;
            let config =
                build_video_config(self.identity, self.generation, sps, Some(parameter_sets))?;
            published_config_size = Some(config.as_input().coded_size);
            match runtime
                .publish_media_with_recoverable_backpressure(MediaFrame::VideoConfig(config))
            {
                Ok(RecoverableMediaPublishOutcome::Published) => {}
                Ok(RecoverableMediaPublishOutcome::Backpressured) => {
                    return Err(AppleHighPerformanceVideoError::MediaPublicationFailed(
                        MediaPublishError::Full,
                    ));
                }
                Err(error) => {
                    return Err(AppleHighPerformanceVideoError::MediaPublicationFailed(
                        error,
                    ));
                }
            }
            #[cfg(any(debug_assertions, test))]
            {
                self.diagnostics.video_config_publications =
                    self.diagnostics.video_config_publications.saturating_add(1);
            }
            self.configured_parameter_sets = Some(next_parameter_sets.clone());
        }

        self.parameter_sets = next_parameter_sets;
        let annex_b = access_unit
            .annex_b_bytes()
            .map_err(|_| AppleHighPerformanceVideoError::MalformedAccessUnit)?;
        let local_ingress_at = access_unit.local_ingress_at;
        let access_unit = EncodedVideoAccessUnit::try_new(
            self.identity,
            self.generation,
            VideoTimestamp {
                ticks: u64::from(access_unit.timestamp),
                timescale: NonZeroU32::new(APPLE_HEVC_RTP_CLOCK_HZ).unwrap(),
            },
            access_unit.keyframe,
            annex_b.into_boxed_slice(),
        )
        .map_err(|_| AppleHighPerformanceVideoError::MalformedAccessUnit)?
        .with_local_ingress_at(local_ingress_at);
        match runtime
            .publish_media_with_recoverable_backpressure(MediaFrame::EncodedVideo(access_unit))
        {
            Ok(RecoverableMediaPublishOutcome::Published) => {}
            Ok(RecoverableMediaPublishOutcome::Backpressured) => {
                return Err(AppleHighPerformanceVideoError::MediaPublicationFailed(
                    MediaPublishError::Full,
                ));
            }
            Err(error) => {
                return Err(AppleHighPerformanceVideoError::MediaPublicationFailed(
                    error,
                ));
            }
        }
        #[cfg(any(debug_assertions, test))]
        {
            self.diagnostics.encoded_video_publications = self
                .diagnostics
                .encoded_video_publications
                .saturating_add(1);
        }
        if let Some(size) = published_config_size {
            self.stage_trace
                .observe(MediaStageDiagnostic::HevcAccessUnitPublished {
                    generation: self.generation,
                    stream_id: self.identity.stream_id,
                    width: size.width,
                    height: size.height,
                });
        }
        Ok(())
    }
}

pub(crate) fn build_video_config(
    identity: VideoStreamIdentity,
    generation: u64,
    sps: HevcSps,
    parameter_sets: Option<VideoParameterSets>,
) -> Result<VideoStreamConfig, AppleHighPerformanceVideoError> {
    if !sps.is_main444_8bit() {
        return Err(AppleHighPerformanceVideoError::UnsupportedStreamConfig);
    }
    let parameter_sets =
        parameter_sets.ok_or(AppleHighPerformanceVideoError::InvalidStreamConfig)?;
    let coded_size = PixelSize::new(sps.coded_width, sps.coded_height)
        .ok_or(AppleHighPerformanceVideoError::InvalidStreamConfig)?;
    let visible_rect = PixelRect {
        x: sps.crop_left,
        y: sps.crop_top,
        width: sps.visible_width,
        height: sps.visible_height,
    };
    VideoStreamConfig::try_new(VideoStreamConfigInput {
        identity,
        generation,
        codec: VideoCodec::Hevc,
        profile: VideoProfile::HevcMain4448,
        chroma: ChromaFormat::Yuv444,
        bit_depth: 8,
        coded_size,
        visible_rect,
        time_base: VideoTimeBase::try_new(APPLE_HEVC_RTP_CLOCK_HZ)
            .map_err(|_| AppleHighPerformanceVideoError::InvalidStreamConfig)?,
        bitstream_format: VideoBitstreamFormat::AnnexB,
        // Captured stream has no complete normalized VUI contract yet; use the
        // explicit, tested product defaults instead of inheriting prior state.
        colorimetry: VideoColorimetry::Bt709,
        range: VideoRange::Limited,
        chroma_location: ChromaLocation::Unspecified,
        parameter_sets,
    })
    .map_err(|_| AppleHighPerformanceVideoError::InvalidStreamConfig)
}

fn nal_type(nal: &[u8]) -> Option<u8> {
    (nal.len() >= 2).then_some((nal[0] >> 1) & 0x3f)
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc, Mutex};

    use frd_core::{PixelSize, SessionId};
    use frd_frame::SurfaceUpdate;
    use frd_media_api::{
        ChromaFormat, MediaFrame, MediaPublishError, MediaPublisher, VideoBitstreamFormat,
        VideoParameterSets, VideoProfile, VideoStreamIdentity,
    };
    use frd_protocol_api::{
        ProtocolError, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionEvent,
        SurfacePublisher,
    };

    use crate::hevc_access_unit::HevcAccessUnit;
    use crate::hevc_sps::{parse_hevc_sps, CAPTURED_MAIN444_8BIT_SPS};

    use super::{
        build_video_config, AppleHighPerformanceVideoAdapter, AppleHighPerformanceVideoError,
    };

    const GENERATION: u64 = 7;

    struct NoopEvents;

    impl RuntimeEventSink for NoopEvents {
        fn publish(&self, _event: SessionEvent) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct NoopSurface;

    impl SurfacePublisher for NoopSurface {
        fn publish(&self, _update: SurfaceUpdate) -> Result<(), ProtocolError> {
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

    fn runtime(session_id: SessionId) -> (ProtocolRuntime, Arc<Mutex<Vec<MediaFrame>>>) {
        let (_commands, command_rx) = mpsc::channel();
        let frames = Arc::new(Mutex::new(Vec::new()));
        (
            ProtocolRuntime::new(
                session_id,
                command_rx,
                Box::new(NoopEvents),
                Box::new(NoopSurface),
                Some(Box::new(RecordingMedia(frames.clone()))),
                Box::new(NoopWake),
            ),
            frames,
        )
    }

    fn length_prefixed_access_unit(sps: &[u8]) -> HevcAccessUnit {
        let nals: [&[u8]; 4] = [
            &[0x40, 0x01, 0xaa],
            sps,
            &[0x44, 0x01, 0xbb],
            &[0x26, 0x01, 0xcc],
        ];
        let mut data = Vec::new();
        for nal in nals {
            data.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            data.extend_from_slice(nal);
        }
        HevcAccessUnit {
            generation: GENERATION,
            ssrc: 0x1020_3040,
            timestamp: 90_000,
            keyframe: true,
            parameter_sets_prepended: false,
            local_ingress_at: std::time::Instant::now(),
            data,
        }
    }

    #[test]
    fn high_performance_video_publishes_strict_main444_config_before_matching_annex_b_au() {
        let session_id = SessionId::allocate();
        let identity = VideoStreamIdentity {
            session_id,
            stream_id: 1,
        };
        let (mut runtime, published) = runtime(session_id);
        let mut adapter = AppleHighPerformanceVideoAdapter::new(identity, GENERATION);

        adapter
            .publish_access_unit(
                &mut runtime,
                length_prefixed_access_unit(CAPTURED_MAIN444_8BIT_SPS),
            )
            .unwrap();

        let published = published.lock().unwrap();
        assert_eq!(published.len(), 2);
        let MediaFrame::VideoConfig(config) = &published[0] else {
            panic!("strict SPS must publish config before the first AU");
        };
        let config = config.as_input();
        assert_eq!(config.identity, identity);
        assert_eq!(config.generation, GENERATION);
        assert_eq!(config.profile, VideoProfile::HevcMain4448);
        assert_eq!(config.chroma, ChromaFormat::Yuv444);
        assert_eq!(config.bit_depth, 8);
        assert_eq!(config.coded_size, PixelSize::new(1920, 1088).unwrap());
        assert_eq!(
            (config.visible_rect.width, config.visible_rect.height),
            (1920, 1080)
        );
        assert_eq!(config.bitstream_format, VideoBitstreamFormat::AnnexB);
        let MediaFrame::EncodedVideo(access_unit) = &published[1] else {
            panic!("config must be followed by the encoded AU");
        };
        assert_eq!(access_unit.identity(), identity);
        assert_eq!(access_unit.generation(), GENERATION);
        assert_eq!(access_unit.timestamp().ticks, 90_000);
        assert!(access_unit.random_access());
        assert!(access_unit.bytes().starts_with(&[0, 0, 0, 1, 0x40, 0x01]));
        assert_eq!(adapter.diagnostics().video_config_publications, 1);
        assert_eq!(adapter.diagnostics().encoded_video_publications, 1);

        adapter.reset(GENERATION + 1);
        assert_eq!(
            adapter.diagnostics(),
            super::AppleHighPerformanceVideoDiagnostics::default()
        );
    }

    #[test]
    fn main_and_main10_capabilities_cannot_masquerade_as_main444_8bit() {
        let identity = VideoStreamIdentity {
            session_id: SessionId::allocate(),
            stream_id: 1,
        };
        let captured = parse_hevc_sps(CAPTURED_MAIN444_8BIT_SPS).unwrap();
        for incompatible in [
            crate::hevc_sps::HevcSps {
                general_profile_idc: 1,
                chroma_format_idc: 1,
                general_constraint_indicator_flags: 0,
                ..captured
            },
            crate::hevc_sps::HevcSps {
                general_profile_idc: 2,
                chroma_format_idc: 1,
                bit_depth_luma: 10,
                bit_depth_chroma: 10,
                general_constraint_indicator_flags: 0,
                ..captured
            },
        ] {
            let parameter_sets = VideoParameterSets::try_new(
                Some(vec![0x40, 0x01, 0xaa].into_boxed_slice()),
                CAPTURED_MAIN444_8BIT_SPS.to_vec().into_boxed_slice(),
                vec![0x44, 0x01, 0xbb].into_boxed_slice(),
            )
            .unwrap();
            assert_eq!(
                build_video_config(identity, GENERATION, incompatible, Some(parameter_sets))
                    .unwrap_err(),
                AppleHighPerformanceVideoError::UnsupportedStreamConfig
            );
        }
    }
}
