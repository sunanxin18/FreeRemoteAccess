mod application;
mod cleanup;
mod fatal;
mod frame_metrics;
mod frame_metrics_sink;
mod input;
mod lifecycle;
mod platform;
mod repaint;
mod ui_fonts;
mod window_chrome;

pub use application::{
    AudioOutputFactory, BackgroundLaunchOutcome, DesktopApplication, DesktopPlatformStores,
    DesktopUserEvent, DesktopWindowConfiguration, PresentationFailure, SessionHost,
    SessionHostError, TestTextureOptions, WakeSink,
};
pub use cleanup::{BackgroundCleanupFailure, BackgroundCleanupOutcome};
pub use fatal::{FatalComponent, FatalOperation, FatalReason, FatalReport};
pub use input::{InputGate, InputOwnership, InputRouter};
pub use lifecycle::PresentationOperation;
pub use window_chrome::{
    ChromeHit, ChromeHitRegions, ChromeLayout, ChromeRect, NativeChromeInsets, WindowChromeAction,
    WindowChromeAdapter, WindowChromeError, TITLE_BAR_HEIGHT_POINTS,
};
