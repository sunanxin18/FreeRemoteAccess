use std::error::Error;
use std::fmt;

use raw_window_handle::{DisplayHandle, WindowHandle};

pub struct SurfaceHandle<'a> {
    pub window: WindowHandle<'a>,
    pub display: DisplayHandle<'a>,
}

pub trait WindowHost {
    fn request_redraw(&self) -> Result<(), PlatformError>;
    fn surface_handle(&self) -> Result<SurfaceHandle<'_>, PlatformError>;
    fn set_fullscreen(&self, enabled: bool) -> Result<(), PlatformError>;
}

pub trait PlatformServices: Send + Sync {
    fn set_clipboard_text(&self, text: &str) -> Result<(), PlatformError>;
    fn open_external_url(&self, url: &str) -> Result<(), PlatformError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformError {
    code: &'static str,
}

impl PlatformError {
    pub const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Display for PlatformError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "本地平台操作失败 ({})", self.code)
    }
}

impl Error for PlatformError {}
