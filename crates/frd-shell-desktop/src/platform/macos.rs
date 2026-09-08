use frd_ui_model::IslandWindowCapabilities;

use crate::{
    AppearancePolicy, ChromeHitMap, NativeChromeInsets, WindowChromeAdapter, WindowChromeCommand,
    WindowChromeError,
};

pub(crate) struct PlatformWindowChrome {
    initial_extent_prepared: bool,
    initial_position_pending: bool,
}

impl PlatformWindowChrome {
    pub(crate) fn new() -> Self {
        Self {
            initial_extent_prepared: false,
            initial_position_pending: false,
        }
    }
}

fn native_window(
    window: &winit::window::Window,
) -> Option<objc2::rc::Retained<objc2_app_kit::NSWindow>> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let _main_thread = objc2_foundation::MainThreadMarker::new()?;
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // winit 的 AppKit 句柄保证 NSView 在窗口存活期间有效；仅主线程读取。
    let view = unsafe { handle.ns_view.cast::<objc2_app_kit::NSView>().as_ref() };
    view.window()
}

impl WindowChromeAdapter for PlatformWindowChrome {
    fn initial_remote_extent(
        &mut self,
        window: &winit::window::Window,
        remote: frd_core::PixelSize,
        chrome_height: f64,
    ) -> Option<crate::LogicalWindowExtent> {
        if self.initial_extent_prepared {
            return None;
        }
        self.initial_extent_prepared = true;
        if window.fullscreen().is_some() {
            return None;
        }
        let native = native_window(window)?;
        let screen = native.screen()?;
        // 使用真实可见工作区，并由当前 NSWindow 样式换算 content rect。
        // full-size content view 的标题栏几何仍由 shell 的 chrome_height 负责。
        let available = native.contentRectForFrameRect(screen.visibleFrame()).size;
        let extent = crate::window_presentation::fit_remote_window(
            crate::LogicalWindowExtent::new(available.width, available.height),
            crate::LogicalWindowExtent::new(f64::from(remote.width), f64::from(remote.height)),
            chrome_height,
        )?;
        self.initial_position_pending = true;
        Some(extent)
    }

    fn constrain_initial_remote_window(&mut self, window: &winit::window::Window) {
        if !std::mem::take(&mut self.initial_position_pending) || window.fullscreen().is_some() {
            return;
        }
        let Some(native) = native_window(window) else {
            return;
        };
        let Some(screen) = native.screen() else {
            return;
        };
        let visible = screen.visibleFrame();
        let mut frame = native.frame();
        // 只平移已按工作区适配的初始窗口，保留精确的远程内容宽高比。
        frame.origin.x = frame.origin.x.clamp(
            visible.origin.x,
            (visible.origin.x + visible.size.width - frame.size.width).max(visible.origin.x),
        );
        frame.origin.y = frame.origin.y.clamp(
            visible.origin.y,
            (visible.origin.y + visible.size.height - frame.size.height).max(visible.origin.y),
        );
        unsafe {
            native.setFrameOrigin(frame.origin);
        }
    }

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
