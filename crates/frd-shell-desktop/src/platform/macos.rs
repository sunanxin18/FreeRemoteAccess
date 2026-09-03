use frd_ui_model::IslandWindowCapabilities;

use crate::{
    AppearancePolicy, ChromeHitMap, NativeChromeInsets, WindowChromeAdapter, WindowChromeCommand,
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
            leading_px: 72,
            trailing_px: 0,
        }
    }

    fn capabilities(&self) -> IslandWindowCapabilities {
        crate::window_chrome::unverified_desktop_capabilities()
    }

    fn appearance_policy(&self) -> AppearancePolicy {
        AppearancePolicy::conservative()
    }

    fn refresh_appearance_policy(&mut self) -> bool {
        false
    }

    fn native_interaction_active(&self) -> bool {
        false
    }

    fn publish_hit_map(&mut self, _hit_map: ChromeHitMap) {}

    fn execute(
        &mut self,
        _window: &winit::window::Window,
        _command: WindowChromeCommand,
    ) -> Result<(), WindowChromeError> {
        Err(WindowChromeError::UnsupportedWindow)
    }
}
