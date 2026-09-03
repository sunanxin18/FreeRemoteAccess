mod application;
mod cleanup;
mod fatal;
mod floating_chrome;
mod frame_metrics;
mod frame_metrics_sink;
mod input;
mod lifecycle;
mod platform;
mod presentation_timing;
mod repaint;
mod ui_fonts;
mod video_decode_worker;
mod window_chrome;

pub use application::{
    AudioOutputFactory, BackgroundLaunchOutcome, DesktopApplication, DesktopPlatformStores,
    DesktopUserEvent, DesktopWindowConfiguration, PresentationFailure, SessionHost,
    SessionHostError, TestTextureOptions, WakeSink,
};
pub use cleanup::{BackgroundCleanupFailure, BackgroundCleanupOutcome};
pub use fatal::{FatalComponent, FatalOperation, FatalReason, FatalReport};
pub use floating_chrome::{
    ChromeGeometrySnapshot, ChromeHitMap, ChromeHitTarget, ChromeLayouts, ChromeOverlayLayout,
    ControlIslandPlacement, ControlIslandState, FloatingChromeController, RemoteContentLayout,
    HIDE_DELAY, REVEAL_DELAY, TOP_SENSOR_POINTS,
};
pub use input::{InputGate, InputOwnership, InputRouter};
pub use lifecycle::PresentationOperation;
pub use video_decode_worker::{
    DecodedVideoFrameHandoff, VideoDecodeSender, VideoDecoderDiagnostics, VideoFrameToken,
    VideoStreamAdmission, VideoWorkerEvent, VideoWorkerEvents, VideoWorkerSendError,
};
pub use window_chrome::{
    AppearancePolicy, ChromeRect, NativeChromeInsets, WindowChromeAdapter, WindowChromeCommand,
    WindowChromeError, TITLE_BAR_HEIGHT_POINTS,
};
