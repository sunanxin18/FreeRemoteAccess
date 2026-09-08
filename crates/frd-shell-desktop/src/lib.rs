mod application;
mod cleanup;
mod display_geometry;
mod fatal;
mod floating_chrome;
mod frame_metrics;
mod frame_metrics_sink;
mod input;
mod lifecycle;
mod platform;
mod presentation_timing;
mod repaint;
mod session_host;
mod ui_fonts;
mod video_decode_worker;
mod video_rate_fallback;
mod window_chrome;
mod window_presentation;

pub use application::{
    DesktopApplication, DesktopPlatformStores, DesktopUserEvent, DesktopWindowConfiguration,
    PresentationFailure, TestTextureOptions,
};
pub use cleanup::{BackgroundCleanupFailure, BackgroundCleanupOutcome};
pub use display_geometry::{
    from_window as display_geometry_from_window, logical_extent_from_physical,
    scale_factor_to_milli,
};
pub use fatal::{FatalComponent, FatalOperation, FatalReason, FatalReport};
pub use floating_chrome::{
    ChromeGeometrySnapshot, ChromeHitMap, ChromeHitTarget, ChromeLayouts, ChromeOverlayLayout,
    ControlIslandPlacement, ControlIslandState, FloatingChromeController, RemoteContentLayout,
    HIDE_DELAY, REVEAL_DELAY, TOP_SENSOR_POINTS,
};
pub use frame_metrics::FrameBatchMetricsSnapshot;
pub use input::{InputGate, InputOwnership, InputRouter};
pub use lifecycle::PresentationOperation;
pub use session_host::{
    AcceptedLaunchOutcome, AudioOutputFactory, BackgroundLaunchOutcome, CompiledFrameDrain,
    FrameCompileFailure, SessionHost, SessionHostError, WakeSink,
};
pub use video_decode_worker::{
    DecodedVideoFrameHandoff, VideoDecodeLoadSnapshot, VideoDecodeSender, VideoDecoderDiagnostics,
    VideoFrameToken, VideoStreamAdmission, VideoWorkerEvent, VideoWorkerEvents,
    VideoWorkerSendError,
};
pub use window_chrome::{
    AppearancePolicy, ChromeRect, NativeChromeInsets, WindowChromeAdapter, WindowChromeCommand,
    WindowChromeError, TITLE_BAR_HEIGHT_POINTS,
};
pub use window_presentation::{
    LogicalWindowExtent, WindowPresentationController, WindowPresentationMode,
    WindowPresentationTransition,
};
