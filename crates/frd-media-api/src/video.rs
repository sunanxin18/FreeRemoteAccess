use frd_core::{PixelRect, PixelSize, SessionId};
use std::num::NonZeroU32;
use std::time::Instant;
use std::{hash::Hash, hash::Hasher};

/// 单个编码参数集允许的最大字节数。
pub const MAX_VIDEO_PARAMETER_SET_BYTES: usize = 1024 * 1024;
/// 单个编码访问单元允许的最大字节数。
pub const MAX_ENCODED_VIDEO_ACCESS_UNIT_BYTES: usize = 16 * 1024 * 1024;
/// 一个已解码视频帧所有 CPU plane 的最大总字节数。
pub const MAX_DECODED_VIDEO_FRAME_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VideoContractError {
    ZeroTimebase,
    VisibleRectOutOfBounds,
    InvalidBitDepth,
    EmptyParameterSet,
    ParameterSetBudgetExceeded,
    EmptyAccessUnit,
    AccessUnitBudgetExceeded,
    ZeroPlaneExtent,
    PlaneStrideTooShort,
    PlaneBufferLengthOverflow,
    PlaneBufferTooShort,
    PlaneBudgetExceeded,
    EmptyPlaneSet,
    DecodedFrameLayoutMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoStreamIdentity {
    pub session_id: SessionId,
    pub stream_id: u32,
}

impl Hash for VideoStreamIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.session_id.get().hash(state);
        self.stream_id.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoTimeBase(NonZeroU32);

impl VideoTimeBase {
    pub fn try_new(ticks_per_second: u32) -> Result<Self, VideoContractError> {
        NonZeroU32::new(ticks_per_second)
            .map(Self)
            .ok_or(VideoContractError::ZeroTimebase)
    }

    pub const fn ticks_per_second(self) -> NonZeroU32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoTimestamp {
    pub ticks: u64,
    pub timescale: NonZeroU32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoCodec {
    H264,
    Hevc,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoProfile {
    H264Baseline,
    H264Main,
    H264High,
    HevcMain,
    HevcMain10,
    HevcMain4448,
    CodecSpecific { codec: VideoCodec, profile_idc: u8 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChromaFormat {
    Monochrome,
    Yuv420,
    Yuv422,
    Yuv444,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoBitstreamFormat {
    AnnexB,
    LengthPrefixed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoColorimetry {
    Unspecified,
    Bt601,
    Bt709,
    Bt2020,
    CodecSpecific {
        primaries: u8,
        transfer: u8,
        matrix: u8,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoRange {
    Limited,
    Full,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChromaLocation {
    Unspecified,
    Left,
    Center,
    TopLeft,
    Top,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoParameterSets {
    vps: Option<Box<[u8]>>,
    sps: Box<[u8]>,
    pps: Box<[u8]>,
}

impl VideoParameterSets {
    pub fn try_new(
        vps: Option<Box<[u8]>>,
        sps: Box<[u8]>,
        pps: Box<[u8]>,
    ) -> Result<Self, VideoContractError> {
        let mut total_bytes = 0usize;
        for parameter_set in vps
            .iter()
            .map(Box::as_ref)
            .chain([sps.as_ref(), pps.as_ref()])
        {
            if parameter_set.is_empty() {
                return Err(VideoContractError::EmptyParameterSet);
            }
            total_bytes = total_bytes
                .checked_add(parameter_set.len())
                .ok_or(VideoContractError::ParameterSetBudgetExceeded)?;
            if total_bytes > MAX_VIDEO_PARAMETER_SET_BYTES {
                return Err(VideoContractError::ParameterSetBudgetExceeded);
            }
        }
        Ok(Self { vps, sps, pps })
    }

    pub fn vps(&self) -> Option<&[u8]> {
        self.vps.as_deref()
    }

    pub fn sps(&self) -> &[u8] {
        &self.sps
    }

    pub fn pps(&self) -> &[u8] {
        &self.pps
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoStreamConfigInput {
    pub identity: VideoStreamIdentity,
    pub generation: u64,
    pub codec: VideoCodec,
    pub profile: VideoProfile,
    pub chroma: ChromaFormat,
    pub bit_depth: u8,
    pub coded_size: PixelSize,
    pub visible_rect: PixelRect,
    pub time_base: VideoTimeBase,
    pub bitstream_format: VideoBitstreamFormat,
    pub colorimetry: VideoColorimetry,
    pub range: VideoRange,
    pub chroma_location: ChromaLocation,
    pub parameter_sets: VideoParameterSets,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoStreamConfig(VideoStreamConfigInput);

impl VideoStreamConfig {
    pub fn try_new(config: VideoStreamConfigInput) -> Result<Self, VideoContractError> {
        if config.bit_depth == 0 || config.bit_depth > 16 {
            return Err(VideoContractError::InvalidBitDepth);
        }
        if !rect_is_within(config.visible_rect, config.coded_size) {
            return Err(VideoContractError::VisibleRectOutOfBounds);
        }
        Ok(Self(config))
    }

    pub fn as_input(&self) -> &VideoStreamConfigInput {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedVideoAccessUnit {
    identity: VideoStreamIdentity,
    generation: u64,
    timestamp: VideoTimestamp,
    random_access: bool,
    local_ingress_at: Option<Instant>,
    bytes: Box<[u8]>,
}

impl EncodedVideoAccessUnit {
    pub fn try_new(
        identity: VideoStreamIdentity,
        generation: u64,
        timestamp: VideoTimestamp,
        random_access: bool,
        bytes: Box<[u8]>,
    ) -> Result<Self, VideoContractError> {
        if bytes.is_empty() {
            return Err(VideoContractError::EmptyAccessUnit);
        }
        if bytes.len() > MAX_ENCODED_VIDEO_ACCESS_UNIT_BYTES {
            return Err(VideoContractError::AccessUnitBudgetExceeded);
        }
        Ok(Self {
            identity,
            generation,
            timestamp,
            random_access,
            local_ingress_at: None,
            bytes,
        })
    }

    pub const fn identity(&self) -> VideoStreamIdentity {
        self.identity
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn timestamp(&self) -> VideoTimestamp {
        self.timestamp
    }

    pub const fn random_access(&self) -> bool {
        self.random_access
    }

    /// 附加协议已完成认证后的本机入站时刻。该元数据不属于媒体 wire timestamp。
    pub fn with_local_ingress_at(mut self, received_at: Instant) -> Self {
        self.local_ingress_at = Some(received_at);
        self
    }

    pub const fn local_ingress_at(&self) -> Option<Instant> {
        self.local_ingress_at
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
mod access_unit_timing_tests {
    use std::num::NonZeroU32;
    use std::time::Instant;

    use frd_core::SessionId;

    use super::{EncodedVideoAccessUnit, VideoStreamIdentity, VideoTimestamp};

    #[test]
    fn local_ingress_metadata_is_optional_and_can_be_attached_without_changing_wire_timestamp() {
        let identity = VideoStreamIdentity {
            session_id: SessionId::allocate(),
            stream_id: 1,
        };
        let timestamp = VideoTimestamp {
            ticks: 90_000,
            timescale: NonZeroU32::new(90_000).unwrap(),
        };
        let ingress = Instant::now();
        let access_unit = EncodedVideoAccessUnit::try_new(
            identity,
            7,
            timestamp,
            true,
            vec![1].into_boxed_slice(),
        )
        .unwrap()
        .with_local_ingress_at(ingress);

        assert_eq!(access_unit.timestamp(), timestamp);
        assert_eq!(access_unit.local_ingress_at(), Some(ingress));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoPixelFormat {
    Yuv420P8,
    Yuv444P8,
    Nv12,
    P010,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoPlane {
    width: u32,
    height: u32,
    stride_bytes: u32,
    bytes: Box<[u8]>,
}

impl VideoPlane {
    pub fn try_new(
        width: u32,
        height: u32,
        stride_bytes: u32,
        bytes: Box<[u8]>,
    ) -> Result<Self, VideoContractError> {
        if width == 0 || height == 0 {
            return Err(VideoContractError::ZeroPlaneExtent);
        }
        if stride_bytes < width {
            return Err(VideoContractError::PlaneStrideTooShort);
        }
        let required = usize::try_from(stride_bytes)
            .ok()
            .and_then(|stride| stride.checked_mul(height as usize))
            .ok_or(VideoContractError::PlaneBufferLengthOverflow)?;
        validate_plane_buffer_length(required, bytes.len())?;
        Ok(Self {
            width,
            height,
            stride_bytes,
            bytes,
        })
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub const fn stride_bytes(&self) -> u32 {
        self.stride_bytes
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedVideoFrameInput {
    pub identity: VideoStreamIdentity,
    pub generation: u64,
    pub timestamp: VideoTimestamp,
    pub coded_size: PixelSize,
    pub visible_rect: PixelRect,
    pub format: VideoPixelFormat,
    /// 协议已规范化的颜色元数据；`Unspecified` 是显式产品默认门禁，不得继承上一流。
    pub colorimetry: VideoColorimetry,
    pub range: VideoRange,
    pub planes: Box<[VideoPlane]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedVideoFrame(DecodedVideoFrameInput);

impl DecodedVideoFrame {
    pub fn try_new(frame: DecodedVideoFrameInput) -> Result<Self, VideoContractError> {
        if !rect_is_within(frame.visible_rect, frame.coded_size) {
            return Err(VideoContractError::VisibleRectOutOfBounds);
        }
        if frame.planes.is_empty() {
            return Err(VideoContractError::EmptyPlaneSet);
        }
        validate_decoded_frame_layout(frame.format, frame.coded_size, &frame.planes)?;
        let total = frame.planes.iter().try_fold(0usize, |total, plane| {
            total
                .checked_add(plane.bytes.len())
                .ok_or(VideoContractError::PlaneBudgetExceeded)
        })?;
        if total > MAX_DECODED_VIDEO_FRAME_BYTES {
            return Err(VideoContractError::PlaneBudgetExceeded);
        }
        Ok(Self(frame))
    }

    pub fn as_input(&self) -> &DecodedVideoFrameInput {
        &self.0
    }
}

fn rect_is_within(rect: PixelRect, size: PixelSize) -> bool {
    let Some((_, end)) = rect.checked_bounds() else {
        return false;
    };
    end.x <= size.width && end.y <= size.height
}

fn validate_decoded_frame_layout(
    format: VideoPixelFormat,
    coded_size: PixelSize,
    planes: &[VideoPlane],
) -> Result<(), VideoContractError> {
    match format {
        VideoPixelFormat::Yuv420P8 => {
            let chroma_width = ceil_half(coded_size.width)?;
            let chroma_height = ceil_half(coded_size.height)?;
            validate_plane_layout(
                planes,
                &[
                    (coded_size.width, coded_size.height, coded_size.width),
                    (chroma_width, chroma_height, chroma_width),
                    (chroma_width, chroma_height, chroma_width),
                ],
            )
        }
        VideoPixelFormat::Yuv444P8 => validate_plane_layout(
            planes,
            &[
                (coded_size.width, coded_size.height, coded_size.width),
                (coded_size.width, coded_size.height, coded_size.width),
                (coded_size.width, coded_size.height, coded_size.width),
            ],
        ),
        VideoPixelFormat::Nv12 => {
            let chroma_height = ceil_half(coded_size.height)?;
            validate_plane_layout(
                planes,
                &[
                    (coded_size.width, coded_size.height, coded_size.width),
                    (coded_size.width, chroma_height, coded_size.width),
                ],
            )
        }
        VideoPixelFormat::P010 => {
            let chroma_height = ceil_half(coded_size.height)?;
            let stride = coded_size
                .width
                .checked_mul(2)
                .ok_or(VideoContractError::DecodedFrameLayoutMismatch)?;
            validate_plane_layout(
                planes,
                &[
                    (coded_size.width, coded_size.height, stride),
                    (coded_size.width, chroma_height, stride),
                ],
            )
        }
    }
}

fn ceil_half(value: u32) -> Result<u32, VideoContractError> {
    value
        .checked_add(1)
        .map(|value| value / 2)
        .ok_or(VideoContractError::DecodedFrameLayoutMismatch)
}

fn validate_plane_layout(
    planes: &[VideoPlane],
    expected: &[(u32, u32, u32)],
) -> Result<(), VideoContractError> {
    if planes.len() != expected.len() {
        return Err(VideoContractError::DecodedFrameLayoutMismatch);
    }
    for (plane, &(width, height, minimum_stride)) in planes.iter().zip(expected) {
        if plane.width != width || plane.height != height || plane.stride_bytes < minimum_stride {
            return Err(VideoContractError::DecodedFrameLayoutMismatch);
        }
    }
    Ok(())
}

fn validate_plane_buffer_length(
    required_bytes: usize,
    actual_bytes: usize,
) -> Result<(), VideoContractError> {
    if required_bytes > MAX_DECODED_VIDEO_FRAME_BYTES
        || actual_bytes > MAX_DECODED_VIDEO_FRAME_BYTES
    {
        return Err(VideoContractError::PlaneBudgetExceeded);
    }
    if actual_bytes < required_bytes {
        return Err(VideoContractError::PlaneBufferTooShort);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use frd_core::{PixelRect, PixelSize, SessionId};

    use super::{
        validate_plane_buffer_length, ChromaFormat, ChromaLocation, DecodedVideoFrame,
        DecodedVideoFrameInput, VideoBitstreamFormat, VideoCodec, VideoColorimetry,
        VideoContractError, VideoParameterSets, VideoPixelFormat, VideoPlane, VideoProfile,
        VideoRange, VideoStreamConfig, VideoStreamConfigInput, VideoStreamIdentity, VideoTimeBase,
        VideoTimestamp, MAX_DECODED_VIDEO_FRAME_BYTES, MAX_VIDEO_PARAMETER_SET_BYTES,
    };

    fn test_config_with_visible_rect(visible_rect: PixelRect) -> VideoStreamConfigInput {
        VideoStreamConfigInput {
            identity: VideoStreamIdentity {
                session_id: SessionId::allocate(),
                stream_id: 1,
            },
            generation: 1,
            codec: VideoCodec::Hevc,
            profile: VideoProfile::HevcMain,
            chroma: ChromaFormat::Yuv420,
            bit_depth: 8,
            coded_size: PixelSize::new(1920, 1080).expect("测试 coded 尺寸有效"),
            visible_rect,
            time_base: VideoTimeBase::try_new(90_000).expect("测试 timebase 有效"),
            bitstream_format: VideoBitstreamFormat::AnnexB,
            colorimetry: VideoColorimetry::Bt709,
            range: VideoRange::Limited,
            chroma_location: ChromaLocation::Left,
            parameter_sets: VideoParameterSets::try_new(
                None,
                vec![0x42].into_boxed_slice(),
                vec![0x44].into_boxed_slice(),
            )
            .expect("测试参数集有效"),
        }
    }

    #[test]
    fn stream_config_rejects_visible_rect_outside_coded_size() {
        let result = VideoStreamConfig::try_new(test_config_with_visible_rect(PixelRect {
            x: 0,
            y: 0,
            width: 1921,
            height: 1080,
        }));

        assert_eq!(result, Err(VideoContractError::VisibleRectOutOfBounds));
    }

    #[test]
    fn time_base_rejects_zero_ticks_per_second() {
        let result = VideoTimeBase::try_new(0);

        assert_eq!(result, Err(VideoContractError::ZeroTimebase));
    }

    #[test]
    fn plane_rejects_short_buffer_for_stride_and_height() {
        let result = VideoPlane::try_new(1920, 1080, 1920, vec![0; 1920 * 1079].into());

        assert_eq!(result, Err(VideoContractError::PlaneBufferTooShort));
    }

    #[test]
    fn parameter_sets_reject_a_set_over_the_one_mib_budget() {
        let result = VideoParameterSets::try_new(
            None,
            vec![0; 1024 * 1024 + 1].into_boxed_slice(),
            vec![0x44].into_boxed_slice(),
        );

        assert!(matches!(
            result,
            Err(VideoContractError::ParameterSetBudgetExceeded)
        ));
    }

    #[test]
    fn parameter_sets_accept_exact_one_mib_aggregate_budget() {
        let result = VideoParameterSets::try_new(
            Some(vec![0x20].into_boxed_slice()),
            vec![0x42; MAX_VIDEO_PARAMETER_SET_BYTES - 2].into_boxed_slice(),
            vec![0x44].into_boxed_slice(),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn parameter_sets_reject_aggregate_over_one_mib_budget() {
        let result = VideoParameterSets::try_new(
            None,
            vec![0x42; MAX_VIDEO_PARAMETER_SET_BYTES].into_boxed_slice(),
            vec![0x44].into_boxed_slice(),
        );

        assert!(matches!(
            result,
            Err(VideoContractError::ParameterSetBudgetExceeded)
        ));
    }

    #[test]
    fn decoded_frame_rejects_wrong_plane_count_for_each_format() {
        for format in [
            VideoPixelFormat::Yuv420P8,
            VideoPixelFormat::Yuv444P8,
            VideoPixelFormat::Nv12,
            VideoPixelFormat::P010,
        ] {
            assert!(matches!(
                DecodedVideoFrame::try_new(test_frame(format, vec![test_plane(5, 3, 5)])),
                Err(VideoContractError::DecodedFrameLayoutMismatch)
            ));
        }
    }

    #[test]
    fn decoded_frame_rejects_wrong_plane_layout_for_each_format() {
        let cases = [
            (
                VideoPixelFormat::Yuv420P8,
                vec![
                    test_plane(5, 3, 5),
                    test_plane(2, 2, 2),
                    test_plane(3, 2, 3),
                ],
            ),
            (
                VideoPixelFormat::Yuv444P8,
                vec![
                    test_plane(5, 3, 5),
                    test_plane(4, 3, 4),
                    test_plane(5, 3, 5),
                ],
            ),
            (
                VideoPixelFormat::Nv12,
                vec![test_plane(5, 3, 5), test_plane(3, 2, 3)],
            ),
            (
                VideoPixelFormat::P010,
                vec![test_plane(5, 3, 10), test_plane(5, 2, 5)],
            ),
        ];

        for (format, planes) in cases {
            assert!(matches!(
                DecodedVideoFrame::try_new(test_frame(format, planes)),
                Err(VideoContractError::DecodedFrameLayoutMismatch)
            ));
        }
    }

    #[test]
    fn decoded_frame_accepts_canonical_layout_for_each_format() {
        for (format, planes) in [
            (
                VideoPixelFormat::Yuv420P8,
                vec![
                    test_plane(5, 3, 5),
                    test_plane(3, 2, 3),
                    test_plane(3, 2, 3),
                ],
            ),
            (
                VideoPixelFormat::Yuv444P8,
                vec![
                    test_plane(5, 3, 5),
                    test_plane(5, 3, 5),
                    test_plane(5, 3, 5),
                ],
            ),
            (
                VideoPixelFormat::Nv12,
                vec![test_plane(5, 3, 5), test_plane(5, 2, 5)],
            ),
            (
                VideoPixelFormat::P010,
                vec![test_plane(5, 3, 10), test_plane(5, 2, 10)],
            ),
        ] {
            assert!(DecodedVideoFrame::try_new(test_frame(format, planes)).is_ok());
        }
    }

    #[test]
    fn plane_buffer_length_rejects_an_actual_buffer_over_the_budget_without_allocating_it() {
        let result = validate_plane_buffer_length(1, MAX_DECODED_VIDEO_FRAME_BYTES + 1);

        assert_eq!(result, Err(VideoContractError::PlaneBudgetExceeded));
    }

    fn test_plane(width: u32, height: u32, stride_bytes: u32) -> VideoPlane {
        VideoPlane::try_new(
            width,
            height,
            stride_bytes,
            vec![0; stride_bytes as usize * height as usize].into_boxed_slice(),
        )
        .expect("测试 plane 有效")
    }

    fn test_frame(format: VideoPixelFormat, planes: Vec<VideoPlane>) -> DecodedVideoFrameInput {
        DecodedVideoFrameInput {
            identity: VideoStreamIdentity {
                session_id: SessionId::allocate(),
                stream_id: 1,
            },
            generation: 1,
            timestamp: VideoTimestamp {
                ticks: 1,
                timescale: NonZeroU32::new(90_000).expect("测试 timestamp timebase 非零"),
            },
            coded_size: PixelSize::new(5, 3).expect("测试 coded 尺寸有效"),
            visible_rect: PixelRect {
                x: 0,
                y: 0,
                width: 5,
                height: 3,
            },
            format,
            colorimetry: VideoColorimetry::Bt709,
            range: VideoRange::Limited,
            planes: planes.into_boxed_slice(),
        }
    }

    #[test]
    fn decoded_frame_preserves_explicit_neutral_color_metadata() {
        let planes = vec![
            test_plane(5, 3, 5),
            test_plane(5, 3, 5),
            test_plane(5, 3, 5),
        ];
        let mut input = test_frame(VideoPixelFormat::Yuv444P8, planes);
        input.colorimetry = VideoColorimetry::Unspecified;
        input.range = VideoRange::Full;

        let frame = DecodedVideoFrame::try_new(input).unwrap();

        assert_eq!(frame.as_input().colorimetry, VideoColorimetry::Unspecified);
        assert_eq!(frame.as_input().range, VideoRange::Full);
    }
}
