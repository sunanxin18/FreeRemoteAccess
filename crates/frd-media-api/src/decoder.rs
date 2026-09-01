use std::num::NonZeroU32;

use frd_core::PixelSize;

use crate::{
    ChromaFormat, DecodedVideoFrame, EncodedVideoAccessUnit, VideoCodec, VideoPixelFormat,
    VideoProfile, VideoStreamConfig,
};

/// 与协议无关的稳定 decoder backend 标识。
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct VideoBackendId(Box<str>);

impl VideoBackendId {
    pub fn new(id: impl Into<Box<str>>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// backend 所属的实现类别；不得用远程桌面协议身份推断。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoBackendKind {
    Native,
    Ffmpeg,
}

/// `ProbeOnly` 只能报告能力，不能创建或选择 decoder。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoBackendAvailability {
    DecoderReady,
    ProbeOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoRational {
    pub numerator: NonZeroU32,
    pub denominator: NonZeroU32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoDecodeQuery {
    pub codec: VideoCodec,
    pub profile: VideoProfile,
    pub chroma: ChromaFormat,
    pub bit_depth: u8,
    pub coded_size: PixelSize,
    pub frame_rate: Option<VideoRational>,
    pub preferred_outputs: Box<[VideoPixelFormat]>,
}

/// 一个 backend 对某组精确输入的有界能力声明。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoDecodeCapability {
    pub backend_id: VideoBackendId,
    pub codec: VideoCodec,
    pub profile: VideoProfile,
    pub chroma: ChromaFormat,
    pub bit_depth: u8,
    pub max_coded_size: PixelSize,
    pub output_formats: Box<[VideoPixelFormat]>,
    pub requires_bitstream_conversion: bool,
}

impl VideoDecodeCapability {
    /// 只有 codec、profile、色度、位深、尺寸和至少一种期望输出均匹配时才接受。
    pub fn matches_exactly(&self, query: &VideoDecodeQuery) -> bool {
        self.codec == query.codec
            && self.profile == query.profile
            && self.chroma == query.chroma
            && self.bit_depth == query.bit_depth
            && query.coded_size.width <= self.max_coded_size.width
            && query.coded_size.height <= self.max_coded_size.height
            && query
                .preferred_outputs
                .iter()
                .any(|preferred| self.output_formats.iter().any(|output| output == preferred))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoUnsupportedReason {
    BackendUnavailable,
    CodecUnavailable,
    ProfileUnavailable,
    ChromaUnavailable,
    BitDepthUnavailable,
    DimensionsUnavailable,
    OutputFormatUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VideoDecodeSupport {
    HardwareExact(VideoDecodeCapability),
    SoftwareExact(VideoDecodeCapability),
    Unsupported(VideoUnsupportedReason),
}

impl VideoDecodeSupport {
    pub fn is_exact(&self) -> bool {
        matches!(self, Self::HardwareExact(_) | Self::SoftwareExact(_))
    }
}

/// 可安全呈现给状态 UI 与验证日志的候选结果，不含平台原生句柄或第三方错误文本。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoBackendDiagnostic {
    pub backend_id: VideoBackendId,
    pub kind: VideoBackendKind,
    pub availability: VideoBackendAvailability,
    pub support: VideoDecodeSupport,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoDecodeErrorCode {
    BackendUnavailable,
    ExactProfileChromaBitDepthUnsupported,
    OutputFormatUnsupported,
    DecoderCreationFailed,
    MalformedOrOverBudgetAccessUnit,
    StaleStreamOrGeneration,
    DecodeFailedBeforeFirstFrame,
    DecodeFailedAfterFirstFrame,
    DecodedFrameLayoutInvalid,
    FramePublicationFailed,
    BackendVersionMismatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoDecodeError {
    code: VideoDecodeErrorCode,
    diagnostics: Box<[VideoBackendDiagnostic]>,
}

impl VideoDecodeError {
    pub fn new(code: VideoDecodeErrorCode) -> Self {
        Self {
            code,
            diagnostics: Box::default(),
        }
    }

    pub fn with_diagnostics(
        code: VideoDecodeErrorCode,
        diagnostics: Box<[VideoBackendDiagnostic]>,
    ) -> Self {
        Self { code, diagnostics }
    }

    pub const fn code(&self) -> VideoDecodeErrorCode {
        self.code
    }

    pub fn diagnostics(&self) -> &[VideoBackendDiagnostic] {
        &self.diagnostics
    }
}

#[derive(Debug)]
pub enum DecodeOutcome {
    NeedMoreData,
    Frames(Box<[DecodedVideoFrame]>),
}

pub trait VideoCapabilityProvider: Send + Sync {
    fn backend_id(&self) -> VideoBackendId;
    fn backend_kind(&self) -> VideoBackendKind;
    fn availability(&self) -> VideoBackendAvailability;
    fn query(&self, query: &VideoDecodeQuery) -> VideoDecodeSupport;
}

pub trait VideoDecoderFactory: VideoCapabilityProvider {
    fn create(&self, config: &VideoStreamConfig)
        -> Result<Box<dyn VideoDecoder>, VideoDecodeError>;
}

pub trait VideoDecoder: Send {
    fn submit(
        &mut self,
        access_unit: EncodedVideoAccessUnit,
    ) -> Result<DecodeOutcome, VideoDecodeError>;
    fn flush(&mut self) -> Result<Box<[DecodedVideoFrame]>, VideoDecodeError>;
    fn reset(&mut self, generation: u64) -> Result<(), VideoDecodeError>;
}
