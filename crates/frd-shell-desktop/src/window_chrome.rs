use frd_ui_model::IslandWindowCapabilities;

use crate::floating_chrome::ChromeHitMap;

pub const TITLE_BAR_HEIGHT_POINTS: f64 = 44.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AppearancePolicy {
    pub opaque_material: bool,
    pub animate: bool,
}

impl AppearancePolicy {
    pub const fn from_probe(probe: Option<(bool, bool)>) -> Self {
        match probe {
            Some((false, true)) => Self {
                opaque_material: false,
                animate: true,
            },
            Some((true, _)) | Some((false, false)) | None => Self::conservative(),
        }
    }

    pub const fn conservative() -> Self {
        Self {
            opaque_material: true,
            animate: false,
        }
    }
}

impl Default for AppearancePolicy {
    fn default() -> Self {
        Self::conservative()
    }
}

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
    fn appearance_policy(&self) -> AppearancePolicy;
    /// 系统外观偏好改变时刷新缓存。没有待处理变化时不得调用平台探测 API。
    fn refresh_appearance_policy(&mut self) -> bool;
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

    use super::AppearancePolicy;

    #[test]
    fn appearance_policy_unknown_preferences_choose_opaque_immediate_rendering() {
        assert_eq!(
            AppearancePolicy::from_probe(None),
            AppearancePolicy {
                opaque_material: true,
                animate: false,
            }
        );
    }

    #[test]
    fn appearance_policy_high_contrast_or_reduced_motion_is_opaque_and_immediate() {
        for probe in [Some((true, true)), Some((false, false))] {
            assert_eq!(
                AppearancePolicy::from_probe(probe),
                AppearancePolicy {
                    opaque_material: true,
                    animate: false,
                }
            );
        }
        assert_eq!(
            AppearancePolicy::from_probe(Some((false, true))),
            AppearancePolicy {
                opaque_material: false,
                animate: true,
            }
        );
    }

    #[test]
    fn unverified_desktop_adapters_do_not_advertise_windows_caption_actions() {
        assert_eq!(
            super::unverified_desktop_capabilities(),
            IslandWindowCapabilities::NONE
        );
    }
}
