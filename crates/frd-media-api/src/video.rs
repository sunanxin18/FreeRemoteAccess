use frd_core::{PixelRect, PixelSize, SessionId};
use std::num::NonZeroU32;
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
        for parameter_set in vps
            .iter()
            .map(Box::as_ref)
            .chain([sps.as_ref(), pps.as_ref()])
        {
            if parameter_set.is_empty() {
                return Err(VideoContractError::EmptyParameterSet);
            }
            if parameter_set.len() > MAX_VIDEO_PARAMETER_SET_BYTES {
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

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
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
        if required > MAX_DECODED_VIDEO_FRAME_BYTES {
            return Err(VideoContractError::PlaneBudgetExceeded);
        }
        if bytes.len() < required {
            return Err(VideoContractError::PlaneBufferTooShort);
        }
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

#[cfg(test)]
mod tests {
    use frd_core::{PixelRect, PixelSize, SessionId};

    use super::{
        ChromaFormat, ChromaLocation, VideoBitstreamFormat, VideoCodec, VideoColorimetry,
        VideoContractError, VideoParameterSets, VideoPlane, VideoProfile, VideoRange,
        VideoStreamConfig, VideoStreamConfigInput, VideoStreamIdentity, VideoTimeBase,
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

        assert_eq!(result, Err(VideoContractError::ParameterSetBudgetExceeded));
    }
}
