use crate::{
    VideoBackendAvailability, VideoBackendDiagnostic, VideoBackendId, VideoBackendKind,
    VideoDecodeError, VideoDecodeErrorCode, VideoDecodeQuery, VideoDecodeSupport, VideoDecoder,
    VideoDecoderFactory, VideoStreamConfig,
};

pub struct VideoDecoderRegistry {
    factories: Vec<Box<dyn VideoDecoderFactory>>,
}

impl VideoDecoderRegistry {
    pub fn new(factories: Vec<Box<dyn VideoDecoderFactory>>) -> Self {
        Self { factories }
    }

    pub fn select(
        &self,
        query: &VideoDecodeQuery,
    ) -> Result<VideoDecoderSelection, VideoDecodeError> {
        let (diagnostics, mut candidates) = self.evaluate(query);
        candidates.sort_by_key(|candidate| (candidate.tier, candidate.registration_index));
        let Some(candidate) = candidates.into_iter().next() else {
            return Err(VideoDecodeError::with_diagnostics(
                VideoDecodeErrorCode::BackendUnavailable,
                diagnostics,
            ));
        };

        Ok(VideoDecoderSelection {
            backend_id: candidate.backend_id,
            support: candidate.support,
            diagnostics,
        })
    }

    /// 按精确 tier 选择并创建同一个工厂的 decoder；创建成功后不再尝试其他候选。
    pub fn select_and_create(
        &self,
        query: &VideoDecodeQuery,
        config: &VideoStreamConfig,
    ) -> Result<CreatedVideoDecoder, VideoDecodeError> {
        let (diagnostics, mut candidates) = self.evaluate(query);
        candidates.sort_by_key(|candidate| (candidate.tier, candidate.registration_index));
        if candidates.is_empty() {
            return Err(VideoDecodeError::with_diagnostics(
                VideoDecodeErrorCode::BackendUnavailable,
                diagnostics,
            ));
        }

        for candidate in candidates {
            let factory = &self.factories[candidate.registration_index];
            if let Ok(decoder) = factory.create(config) {
                return Ok(CreatedVideoDecoder {
                    selection: VideoDecoderSelection {
                        backend_id: candidate.backend_id,
                        support: candidate.support,
                        diagnostics,
                    },
                    decoder,
                });
            }
        }

        Err(VideoDecodeError::with_diagnostics(
            VideoDecodeErrorCode::DecoderCreationFailed,
            diagnostics,
        ))
    }

    fn evaluate(
        &self,
        query: &VideoDecodeQuery,
    ) -> (Box<[VideoBackendDiagnostic]>, Vec<SelectionCandidate>) {
        let mut diagnostics = Vec::with_capacity(self.factories.len());
        let mut candidates = Vec::new();

        for (registration_index, factory) in self.factories.iter().enumerate() {
            let backend_id = factory.backend_id();
            let kind = factory.backend_kind();
            let availability = factory.availability();
            let support = factory.query(query);
            diagnostics.push(VideoBackendDiagnostic {
                backend_id: backend_id.clone(),
                kind,
                availability,
                support: support.clone(),
            });

            if availability == VideoBackendAvailability::DecoderReady {
                if let Some(tier) = selection_tier(kind, &support, query) {
                    candidates.push(SelectionCandidate {
                        tier,
                        registration_index,
                        backend_id,
                        support,
                    });
                }
            }
        }

        (diagnostics.into_boxed_slice(), candidates)
    }
}

struct SelectionCandidate {
    tier: u8,
    registration_index: usize,
    backend_id: VideoBackendId,
    support: VideoDecodeSupport,
}

/// 注册顺序只用于相同 tier 的稳定决胜；不得依赖枚举或发现顺序。
fn selection_tier(
    kind: VideoBackendKind,
    support: &VideoDecodeSupport,
    query: &VideoDecodeQuery,
) -> Option<u8> {
    match (kind, support) {
        (VideoBackendKind::Native, VideoDecodeSupport::HardwareExact(capability))
            if capability.matches_exactly(query) =>
        {
            Some(0)
        }
        (VideoBackendKind::Ffmpeg, VideoDecodeSupport::HardwareExact(capability))
            if capability.matches_exactly(query) =>
        {
            Some(1)
        }
        (VideoBackendKind::Ffmpeg, VideoDecodeSupport::SoftwareExact(capability))
            if capability.matches_exactly(query) =>
        {
            Some(2)
        }
        (_, VideoDecodeSupport::Unsupported(_)) => None,
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoDecoderSelection {
    pub backend_id: VideoBackendId,
    pub support: VideoDecodeSupport,
    pub diagnostics: Box<[VideoBackendDiagnostic]>,
}

pub struct CreatedVideoDecoder {
    selection: VideoDecoderSelection,
    decoder: Box<dyn VideoDecoder>,
}

impl CreatedVideoDecoder {
    pub fn selection(&self) -> &VideoDecoderSelection {
        &self.selection
    }

    pub fn into_parts(self) -> (VideoDecoderSelection, Box<dyn VideoDecoder>) {
        (self.selection, self.decoder)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use frd_core::PixelSize;

    use crate::{
        ChromaFormat, ChromaLocation, DecodeOutcome, EncodedVideoAccessUnit,
        VideoBackendAvailability, VideoBackendId, VideoBackendKind, VideoBitstreamFormat,
        VideoCodec, VideoColorimetry, VideoDecodeCapability, VideoDecodeError,
        VideoDecodeErrorCode, VideoDecodeQuery, VideoDecodeSupport, VideoDecoder,
        VideoDecoderFactory, VideoDecoderRegistry, VideoParameterSets, VideoPixelFormat,
        VideoProfile, VideoRange, VideoStreamConfig, VideoStreamConfigInput, VideoStreamIdentity,
        VideoTimeBase, VideoUnsupportedReason,
    };

    #[test]
    fn hevc_main10_never_matches_main444_query() {
        let query = main444_query();
        let capability = hevc_main10_capability("windows-native");

        assert!(!capability.matches_exactly(&query));
    }

    #[test]
    fn capability_rejects_a_query_larger_than_its_exact_dimension_limit() {
        let mut query = main444_query();
        query.coded_size = PixelSize::new(3841, 2160).expect("测试尺寸有效");

        assert!(!main444_capability("ffmpeg-software").matches_exactly(&query));
    }

    #[test]
    fn capability_requires_at_least_one_preferred_output_match() {
        let mut query = main444_query();
        query.preferred_outputs = vec![VideoPixelFormat::P010].into_boxed_slice();

        assert!(!main444_capability("ffmpeg-software").matches_exactly(&query));
    }

    #[test]
    fn registry_uses_stable_backend_tier_order() {
        let registry = VideoDecoderRegistry::new(vec![
            fake_factory(
                "ffmpeg-software",
                VideoBackendKind::Ffmpeg,
                VideoBackendAvailability::DecoderReady,
                VideoDecodeSupport::SoftwareExact(main444_capability("ffmpeg-software")),
            ),
            fake_factory(
                "windows-native",
                VideoBackendKind::Native,
                VideoBackendAvailability::DecoderReady,
                VideoDecodeSupport::HardwareExact(main444_capability("windows-native")),
            ),
        ]);

        let selection = registry.select(&main444_query()).expect("应选择精确后端");

        assert_eq!(selection.backend_id.as_str(), "windows-native");
    }

    #[test]
    fn registry_rejects_a_factory_claiming_exact_for_a_mismatched_capability() {
        let registry = VideoDecoderRegistry::new(vec![fake_factory(
            "windows-native",
            VideoBackendKind::Native,
            VideoBackendAvailability::DecoderReady,
            VideoDecodeSupport::HardwareExact(hevc_main10_capability("windows-native")),
        )]);

        let error = registry
            .select(&main444_query())
            .expect_err("不匹配的能力不得被当作精确支持");

        assert_eq!(error.code(), VideoDecodeErrorCode::BackendUnavailable);
    }

    #[test]
    fn registry_breaks_equal_tiers_by_registration_index() {
        let registry = VideoDecoderRegistry::new(vec![
            fake_factory(
                "ffmpeg-software-first",
                VideoBackendKind::Ffmpeg,
                VideoBackendAvailability::DecoderReady,
                VideoDecodeSupport::SoftwareExact(main444_capability("ffmpeg-software-first")),
            ),
            fake_factory(
                "ffmpeg-software-second",
                VideoBackendKind::Ffmpeg,
                VideoBackendAvailability::DecoderReady,
                VideoDecodeSupport::SoftwareExact(main444_capability("ffmpeg-software-second")),
            ),
        ]);

        let selection = registry.select(&main444_query()).expect("应选择精确后端");

        assert_eq!(selection.backend_id.as_str(), "ffmpeg-software-first");
    }

    #[test]
    fn registry_uses_ffmpeg_software_when_native_is_unsupported() {
        let registry = VideoDecoderRegistry::new(vec![
            fake_factory(
                "windows-native",
                VideoBackendKind::Native,
                VideoBackendAvailability::DecoderReady,
                VideoDecodeSupport::Unsupported(VideoUnsupportedReason::ProfileUnavailable),
            ),
            fake_factory(
                "ffmpeg-software",
                VideoBackendKind::Ffmpeg,
                VideoBackendAvailability::DecoderReady,
                VideoDecodeSupport::SoftwareExact(main444_capability("ffmpeg-software")),
            ),
        ]);

        let selection = registry
            .select(&main444_query())
            .expect("应选择 FFmpeg software");

        assert_eq!(selection.backend_id.as_str(), "ffmpeg-software");
    }

    #[test]
    fn registry_does_not_select_native_software_exact_support() {
        let registry = VideoDecoderRegistry::new(vec![fake_factory(
            "native-software",
            VideoBackendKind::Native,
            VideoBackendAvailability::DecoderReady,
            VideoDecodeSupport::SoftwareExact(main444_capability("native-software")),
        )]);

        let error = registry
            .select(&main444_query())
            .expect_err("native software 不在批准的选择 tier 中");

        assert_eq!(error.code(), VideoDecodeErrorCode::BackendUnavailable);
        assert_eq!(error.diagnostics().len(), 1);
        assert_eq!(
            error.diagnostics()[0].support,
            VideoDecodeSupport::SoftwareExact(main444_capability("native-software"))
        );
    }

    #[test]
    fn registry_never_selects_probe_only_provider() {
        let registry = VideoDecoderRegistry::new(vec![
            fake_factory(
                "windows-probe",
                VideoBackendKind::Native,
                VideoBackendAvailability::ProbeOnly,
                VideoDecodeSupport::HardwareExact(main444_capability("windows-probe")),
            ),
            fake_factory(
                "ffmpeg-software",
                VideoBackendKind::Ffmpeg,
                VideoBackendAvailability::DecoderReady,
                VideoDecodeSupport::SoftwareExact(main444_capability("ffmpeg-software")),
            ),
        ]);

        let selection = registry
            .select(&main444_query())
            .expect("ProbeOnly 不得被选中");

        assert_eq!(selection.backend_id.as_str(), "ffmpeg-software");
        assert_eq!(selection.diagnostics.len(), 2);
        assert_eq!(
            selection.diagnostics[0].availability,
            VideoBackendAvailability::ProbeOnly
        );
    }

    #[test]
    fn registry_preserves_all_unsupported_candidate_diagnostics() {
        let registry = VideoDecoderRegistry::new(vec![
            fake_factory(
                "windows-native",
                VideoBackendKind::Native,
                VideoBackendAvailability::DecoderReady,
                VideoDecodeSupport::Unsupported(VideoUnsupportedReason::ProfileUnavailable),
            ),
            fake_factory(
                "ffmpeg-software",
                VideoBackendKind::Ffmpeg,
                VideoBackendAvailability::DecoderReady,
                VideoDecodeSupport::Unsupported(VideoUnsupportedReason::OutputFormatUnavailable),
            ),
        ]);

        let error = registry
            .select(&main444_query())
            .expect_err("无精确后端时应返回稳定错误");

        assert_eq!(error.code(), VideoDecodeErrorCode::BackendUnavailable);
        assert_eq!(error.diagnostics().len(), 2);
        assert_eq!(
            error.diagnostics()[0].support,
            VideoDecodeSupport::Unsupported(VideoUnsupportedReason::ProfileUnavailable)
        );
        assert_eq!(
            error.diagnostics()[1].support,
            VideoDecodeSupport::Unsupported(VideoUnsupportedReason::OutputFormatUnavailable)
        );
    }

    #[test]
    fn registry_create_falls_through_only_after_an_exact_candidate_fails_creation() {
        let first_creates = Arc::new(AtomicUsize::new(0));
        let second_creates = Arc::new(AtomicUsize::new(0));
        let second_submits = Arc::new(AtomicUsize::new(0));
        let registry = VideoDecoderRegistry::new(vec![
            creating_factory(
                "duplicate-id",
                first_creates.clone(),
                Err(VideoDecodeErrorCode::DecoderCreationFailed),
            ),
            creating_factory(
                "duplicate-id",
                second_creates.clone(),
                Ok(second_submits.clone()),
            ),
        ]);

        let config = main444_config();
        let created = registry
            .select_and_create(&main444_query(), &config)
            .expect("首个精确候选创建失败后应尝试同 tier 的下一个工厂");
        let (selection, mut decoder) = created.into_parts();
        let error = decoder
            .submit(test_access_unit(&config))
            .expect_err("第二个同 id 工厂创建的 decoder 应被原子返回并被调用");

        assert_eq!(selection.backend_id.as_str(), "duplicate-id");
        assert_eq!(
            error.code(),
            VideoDecodeErrorCode::DecodeFailedBeforeFirstFrame
        );
        assert_eq!(selection.diagnostics.len(), 2);
        assert_eq!(first_creates.load(Ordering::Acquire), 1);
        assert_eq!(second_creates.load(Ordering::Acquire), 1);
        assert_eq!(second_submits.load(Ordering::Acquire), 1);
    }

    #[test]
    fn registry_does_not_create_lower_candidates_after_the_selected_decoder_exists() {
        let first_creates = Arc::new(AtomicUsize::new(0));
        let second_creates = Arc::new(AtomicUsize::new(0));
        let first_submits = Arc::new(AtomicUsize::new(0));
        let registry = VideoDecoderRegistry::new(vec![
            creating_factory("first", first_creates.clone(), Ok(first_submits.clone())),
            creating_factory(
                "second",
                second_creates.clone(),
                Ok(Arc::new(AtomicUsize::new(0))),
            ),
        ]);

        let config = main444_config();
        let created = registry
            .select_and_create(&main444_query(), &config)
            .expect("首个精确候选应创建成功");
        let (_, mut decoder) = created.into_parts();
        let error = decoder
            .submit(test_access_unit(&config))
            .expect_err("测试 decoder 在 AU 开始后失败");

        assert_eq!(
            error.code(),
            VideoDecodeErrorCode::DecodeFailedBeforeFirstFrame
        );
        assert_eq!(first_creates.load(Ordering::Acquire), 1);
        assert_eq!(first_submits.load(Ordering::Acquire), 1);
        assert_eq!(second_creates.load(Ordering::Acquire), 0);
    }

    fn main444_query() -> VideoDecodeQuery {
        VideoDecodeQuery {
            codec: crate::VideoCodec::Hevc,
            profile: VideoProfile::HevcMain4448,
            chroma: ChromaFormat::Yuv444,
            bit_depth: 8,
            coded_size: PixelSize::new(1920, 1080).expect("测试尺寸有效"),
            frame_rate: None,
            preferred_outputs: vec![VideoPixelFormat::Yuv444P8].into_boxed_slice(),
        }
    }

    fn main444_config() -> VideoStreamConfig {
        VideoStreamConfig::try_new(VideoStreamConfigInput {
            identity: VideoStreamIdentity {
                session_id: frd_core::SessionId::allocate(),
                stream_id: 7,
            },
            generation: 3,
            codec: VideoCodec::Hevc,
            profile: VideoProfile::HevcMain4448,
            chroma: ChromaFormat::Yuv444,
            bit_depth: 8,
            coded_size: PixelSize::new(1920, 1080).expect("测试尺寸有效"),
            visible_rect: frd_core::PixelRect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            time_base: VideoTimeBase::try_new(90_000).expect("测试 timebase 有效"),
            bitstream_format: VideoBitstreamFormat::AnnexB,
            colorimetry: VideoColorimetry::Bt709,
            range: VideoRange::Limited,
            chroma_location: ChromaLocation::Left,
            parameter_sets: VideoParameterSets::try_new(
                Some(vec![0x40].into_boxed_slice()),
                vec![0x42].into_boxed_slice(),
                vec![0x44].into_boxed_slice(),
            )
            .expect("测试参数集有效"),
        })
        .expect("测试配置有效")
    }

    fn test_access_unit(config: &VideoStreamConfig) -> EncodedVideoAccessUnit {
        let input = config.as_input();
        EncodedVideoAccessUnit::try_new(
            input.identity,
            input.generation,
            crate::VideoTimestamp {
                ticks: 1,
                timescale: NonZeroU32::new(90_000).expect("测试 timebase 非零"),
            },
            true,
            vec![0, 0, 0, 1, 0x26].into_boxed_slice(),
        )
        .expect("测试 AU 有效")
    }

    fn hevc_main10_capability(backend_id: &str) -> VideoDecodeCapability {
        VideoDecodeCapability {
            backend_id: VideoBackendId::new(backend_id),
            codec: crate::VideoCodec::Hevc,
            profile: VideoProfile::HevcMain10,
            chroma: ChromaFormat::Yuv420,
            bit_depth: 10,
            max_coded_size: PixelSize::new(3840, 2160).expect("测试尺寸有效"),
            output_formats: vec![VideoPixelFormat::P010].into_boxed_slice(),
            requires_bitstream_conversion: false,
        }
    }

    fn main444_capability(backend_id: &str) -> VideoDecodeCapability {
        VideoDecodeCapability {
            backend_id: VideoBackendId::new(backend_id),
            codec: crate::VideoCodec::Hevc,
            profile: VideoProfile::HevcMain4448,
            chroma: ChromaFormat::Yuv444,
            bit_depth: 8,
            max_coded_size: PixelSize::new(3840, 2160).expect("测试尺寸有效"),
            output_formats: vec![VideoPixelFormat::Yuv444P8].into_boxed_slice(),
            requires_bitstream_conversion: false,
        }
    }

    fn fake_factory(
        backend_id: &str,
        kind: VideoBackendKind,
        availability: VideoBackendAvailability,
        support: VideoDecodeSupport,
    ) -> Box<dyn VideoDecoderFactory> {
        Box::new(FakeFactory {
            backend_id: VideoBackendId::new(backend_id),
            kind,
            availability,
            support,
        })
    }

    struct FakeFactory {
        backend_id: VideoBackendId,
        kind: VideoBackendKind,
        availability: VideoBackendAvailability,
        support: VideoDecodeSupport,
    }

    impl crate::VideoCapabilityProvider for FakeFactory {
        fn backend_id(&self) -> VideoBackendId {
            self.backend_id.clone()
        }

        fn backend_kind(&self) -> VideoBackendKind {
            self.kind
        }

        fn availability(&self) -> VideoBackendAvailability {
            self.availability
        }

        fn query(&self, _query: &VideoDecodeQuery) -> VideoDecodeSupport {
            self.support.clone()
        }
    }

    impl VideoDecoderFactory for FakeFactory {
        fn create(
            &self,
            _config: &crate::VideoStreamConfig,
        ) -> Result<Box<dyn VideoDecoder>, crate::VideoDecodeError> {
            unreachable!("registry 测试只查询能力，不创建 decoder")
        }
    }

    fn creating_factory(
        backend_id: &str,
        create_count: Arc<AtomicUsize>,
        outcome: Result<Arc<AtomicUsize>, VideoDecodeErrorCode>,
    ) -> Box<dyn VideoDecoderFactory> {
        Box::new(CreatingFactory {
            backend_id: VideoBackendId::new(backend_id),
            create_count,
            outcome,
        })
    }

    struct CreatingFactory {
        backend_id: VideoBackendId,
        create_count: Arc<AtomicUsize>,
        outcome: Result<Arc<AtomicUsize>, VideoDecodeErrorCode>,
    }

    impl crate::VideoCapabilityProvider for CreatingFactory {
        fn backend_id(&self) -> VideoBackendId {
            self.backend_id.clone()
        }

        fn backend_kind(&self) -> VideoBackendKind {
            VideoBackendKind::Ffmpeg
        }

        fn availability(&self) -> VideoBackendAvailability {
            VideoBackendAvailability::DecoderReady
        }

        fn query(&self, _query: &VideoDecodeQuery) -> VideoDecodeSupport {
            VideoDecodeSupport::SoftwareExact(main444_capability(self.backend_id.as_str()))
        }
    }

    impl VideoDecoderFactory for CreatingFactory {
        fn create(
            &self,
            _config: &VideoStreamConfig,
        ) -> Result<Box<dyn VideoDecoder>, VideoDecodeError> {
            self.create_count.fetch_add(1, Ordering::AcqRel);
            match &self.outcome {
                Ok(submit_count) => Ok(Box::new(SubmitFailureDecoder(submit_count.clone()))),
                Err(code) => Err(VideoDecodeError::new(*code)),
            }
        }
    }

    struct SubmitFailureDecoder(Arc<AtomicUsize>);

    impl VideoDecoder for SubmitFailureDecoder {
        fn submit(
            &mut self,
            _access_unit: EncodedVideoAccessUnit,
        ) -> Result<DecodeOutcome, VideoDecodeError> {
            self.0.fetch_add(1, Ordering::AcqRel);
            Err(VideoDecodeError::new(
                VideoDecodeErrorCode::DecodeFailedBeforeFirstFrame,
            ))
        }

        fn flush(&mut self) -> Result<Box<[crate::DecodedVideoFrame]>, VideoDecodeError> {
            Ok(Box::default())
        }

        fn reset(&mut self, _generation: u64) -> Result<(), VideoDecodeError> {
            Ok(())
        }
    }
}
