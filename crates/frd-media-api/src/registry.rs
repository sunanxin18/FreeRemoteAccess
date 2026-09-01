use crate::{
    VideoBackendAvailability, VideoBackendDiagnostic, VideoBackendId, VideoBackendKind,
    VideoDecodeError, VideoDecodeErrorCode, VideoDecodeQuery, VideoDecodeSupport,
    VideoDecoderFactory,
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
                    candidates.push((tier, registration_index, backend_id, support));
                }
            }
        }

        let diagnostics = diagnostics.into_boxed_slice();
        let Some((_, _, backend_id, support)) = candidates
            .into_iter()
            .min_by_key(|(tier, registration_index, _, _)| (*tier, *registration_index))
        else {
            return Err(VideoDecodeError::with_diagnostics(
                VideoDecodeErrorCode::BackendUnavailable,
                diagnostics,
            ));
        };

        Ok(VideoDecoderSelection {
            backend_id,
            support,
            diagnostics,
        })
    }
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

#[cfg(test)]
mod tests {
    use frd_core::PixelSize;

    use crate::{
        ChromaFormat, VideoBackendAvailability, VideoBackendId, VideoBackendKind,
        VideoDecodeCapability, VideoDecodeErrorCode, VideoDecodeQuery, VideoDecodeSupport,
        VideoDecoder, VideoDecoderFactory, VideoDecoderRegistry, VideoPixelFormat, VideoProfile,
        VideoUnsupportedReason,
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
}
