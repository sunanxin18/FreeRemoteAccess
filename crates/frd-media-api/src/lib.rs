//! 协议和平台之间传递媒体数据的中立契约。

pub mod decoder;
pub mod diagnostics;
pub mod registry;
pub mod video;

pub use decoder::*;
pub use diagnostics::*;
pub use registry::*;
pub use video::*;

#[cfg(test)]
mod media_stage_diagnostic_tests {
    use crate::{
        MediaDecoderBackend, MediaDecoderMode, MediaStageDiagnostic, MediaStageTrace,
        MediaVideoOutput,
    };

    #[test]
    fn media_stage_format_uses_only_fixed_fields() {
        let diagnostic = MediaStageDiagnostic::DecoderSelected {
            generation: 7,
            stream_id: 2,
            backend: MediaDecoderBackend::Native,
            mode: MediaDecoderMode::Hardware,
            output: MediaVideoOutput::Yuv444p8,
        };

        assert_eq!(
            diagnostic.to_string(),
            "[frd-media-stage] stage=decoder_selected generation=7 stream=2 backend=native mode=hardware output=yuv444p8"
        );
    }

    #[test]
    fn media_stage_trace_accepts_each_stage_only_once() {
        let mut trace = MediaStageTrace::default();
        let diagnostic = MediaStageDiagnostic::AuthenticatedVideoRtp {
            generation: 9,
            stream_id: 1,
        };

        assert!(trace.observe(diagnostic));
        assert!(!trace.observe(diagnostic));
        assert!(trace.observe(MediaStageDiagnostic::FrameDecoded {
            generation: 9,
            stream_id: 1,
            width: 1440,
            height: 2560,
        }));
    }
}

/// 可移动的协议无关媒体载荷。所有具体解码器和设备后端都在此边界之外。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaFrame {
    Pcm {
        sample_rate_hz: u32,
        channels: u8,
        samples: Box<[i16]>,
    },
    VideoConfig(VideoStreamConfig),
    EncodedVideo(EncodedVideoAccessUnit),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaPublishError {
    Closed,
    Full,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioOutputError {
    Unavailable,
    UnsupportedFormat,
    Closed,
}

/// 平台音频输出只消费协议已经解码出的 PCM；实现不得打开输入设备。
pub trait AudioOutput: Send {
    fn enqueue_pcm(
        &mut self,
        sample_rate_hz: u32,
        channels: u8,
        samples: Box<[i16]>,
    ) -> Result<(), AudioOutputError>;
}

pub trait MediaPublisher: Send {
    fn publish(&self, frame: MediaFrame) -> Result<(), MediaPublishError>;
}
