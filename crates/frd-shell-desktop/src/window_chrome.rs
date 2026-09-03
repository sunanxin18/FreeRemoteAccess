use frd_ui_model::IslandWindowCapabilities;

use crate::floating_chrome::ChromeHitMap;

pub const TITLE_BAR_HEIGHT_POINTS: f64 = 44.0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NativeChromeInsets {
    pub leading_px: u32,
    pub trailing_px: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChromeRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl ChromeRect {
    pub fn contains(self, x: u32, y: u32) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x.saturating_add(self.width)
            && y < self.y.saturating_add(self.height)
    }

    pub fn center(self) -> (u32, u32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowChromeCommand {
    BeginMove,
    Minimize,
    ToggleMaximize,
    Close,
    ShowSystemMenu,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowChromeError {
    UnsupportedWindow,
    PlatformCallFailed,
    InvalidGeometry,
}

#[cfg(any(test, target_os = "macos", target_os = "linux"))]
pub(crate) const fn unverified_desktop_capabilities() -> IslandWindowCapabilities {
    IslandWindowCapabilities::NONE
}

pub trait WindowChromeAdapter {
    fn configure(&mut self, window: &winit::window::Window) -> Result<(), WindowChromeError>;
    fn refresh_for_dpi(&mut self, window: &winit::window::Window) -> Result<(), WindowChromeError>;
    fn native_insets(&self, window: &winit::window::Window) -> NativeChromeInsets;
    fn capabilities(&self) -> IslandWindowCapabilities;
    fn native_interaction_active(&self) -> bool;
    fn publish_hit_map(&mut self, hit_map: ChromeHitMap);
    fn execute(
        &mut self,
        window: &winit::window::Window,
        command: WindowChromeCommand,
    ) -> Result<(), WindowChromeError>;
}

#[cfg(test)]
mod tests {
    use frd_ui_model::IslandWindowCapabilities;

    #[test]
    fn unverified_desktop_adapters_do_not_advertise_windows_caption_actions() {
        assert_eq!(
            super::unverified_desktop_capabilities(),
            IslandWindowCapabilities::NONE
        );
    }
}
