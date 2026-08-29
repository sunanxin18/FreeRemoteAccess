use crate::{
    ChromeHitRegions, NativeChromeInsets, WindowChromeAction, WindowChromeAdapter,
    WindowChromeError,
};

pub(crate) struct PlatformWindowChrome;

impl PlatformWindowChrome {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl WindowChromeAdapter for PlatformWindowChrome {
    fn configure(&mut self, _window: &winit::window::Window) -> Result<(), WindowChromeError> {
        Ok(())
    }

    fn refresh_for_dpi(
        &mut self,
        _window: &winit::window::Window,
    ) -> Result<(), WindowChromeError> {
        Ok(())
    }

    fn native_insets(&self, _window: &winit::window::Window) -> NativeChromeInsets {
        NativeChromeInsets {
            leading_px: 0,
            trailing_px: 138,
        }
    }

    fn publish_hit_regions(&mut self, _regions: ChromeHitRegions) {}

    fn execute(&mut self, window: &winit::window::Window, action: WindowChromeAction) {
        match action {
            WindowChromeAction::Minimize => window.set_minimized(true),
            WindowChromeAction::ToggleMaximize => window.set_maximized(!window.is_maximized()),
            WindowChromeAction::Close => window.set_visible(false),
        }
    }
}
