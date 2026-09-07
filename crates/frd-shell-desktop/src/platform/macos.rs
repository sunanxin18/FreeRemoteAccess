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

    fn native_insets(&self, window: &winit::window::Window) -> NativeChromeInsets {
        NativeChromeInsets {
            leading_px: (80.0 * window.scale_factor()).ceil() as u32,
            trailing_px: 0,
        }
    }

    fn capabilities(&self) -> IslandWindowCapabilities {
        IslandWindowCapabilities::NONE
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
        window: &winit::window::Window,
        command: WindowChromeCommand,
    ) -> Result<(), WindowChromeError> {
        match command {
            WindowChromeCommand::BeginMove => window
                .drag_window()
                .map_err(|_| WindowChromeError::PlatformCallFailed),
            WindowChromeCommand::Minimize => {
                window.set_minimized(true);
                Ok(())
            }
            WindowChromeCommand::ToggleMaximize => {
                let fullscreen = window
                    .fullscreen()
                    .is_none()
                    .then_some(winit::window::Fullscreen::Borderless(None));
                window.set_fullscreen(fullscreen);
                Ok(())
            }
            // 关闭由原生红色按钮和菜单产生 CloseRequested，保留统一的会话清理。
            WindowChromeCommand::Close | WindowChromeCommand::ShowSystemMenu => {
                Err(WindowChromeError::UnsupportedWindow)
            }
        }
    }
}
