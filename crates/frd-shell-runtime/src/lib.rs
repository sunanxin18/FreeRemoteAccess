//! 平台无关的会话执行器。窗口、凭据与解码器后端由各平台组合入口提供。

mod cleanup;
pub mod frame_metrics;
pub mod session_host;
pub mod video_decode_worker;
pub mod video_rate_fallback;

pub use cleanup::{BackgroundCleanupFailure, BackgroundCleanupOutcome};
pub use frame_metrics::FrameBatchMetricsSnapshot;
pub use session_host::{
    AcceptedLaunchOutcome, AudioOutputFactory, BackgroundLaunchOutcome, CompiledFrameDrain,
    FrameCompileFailure, SessionHost, SessionHostError, WakeSink,
};
pub use video_decode_worker::{
    DecodedVideoFrameHandoff, StreamRegistryLoader, VideoDecodeLoadSnapshot, VideoDecodeSender,
    VideoDecodeWorker, VideoDecoderDiagnostics, VideoFrameToken, VideoStreamAdmission,
    VideoWorkerEvent, VideoWorkerEvents, VideoWorkerSendError,
};
