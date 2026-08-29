use frd_core::PixelRect;

pub const TITLE_BAR_HEIGHT_POINTS: f64 = 40.0;
const SESSION_SLOT_POINTS: f64 = 32.0;
const SESSION_SPACING_POINTS: f64 = 4.0;

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
pub enum ChromeHit {
    Client,
    Drag,
    Connection,
    Audio,
    Clipboard,
    SessionAction,
    Minimize,
    Maximize,
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChromeLayout {
    pub title_bar: ChromeRect,
    pub content_rect: PixelRect,
    pub session_cluster: ChromeRect,
    pub session_buttons: [ChromeRect; 4],
    pub minimize_button: Option<ChromeRect>,
    pub maximize_button: Option<ChromeRect>,
    pub close_button: Option<ChromeRect>,
    native: NativeChromeInsets,
}

impl ChromeLayout {
    pub fn for_window(
        width_px: u32,
        height_px: u32,
        scale_factor: f64,
        native_leading_px: u32,
        native_trailing_px: u32,
    ) -> Option<Self> {
        if width_px == 0 || height_px == 0 || !scale_factor.is_finite() || scale_factor <= 0.0 {
            return None;
        }
        let title_height = scaled(TITLE_BAR_HEIGHT_POINTS, scale_factor)?.min(height_px);
        if title_height == height_px {
            return None;
        }
        let slot = scaled(SESSION_SLOT_POINTS, scale_factor)?;
        let spacing = scaled(SESSION_SPACING_POINTS, scale_factor)?;
        let cluster_width = slot.checked_mul(4)?.checked_add(spacing.checked_mul(3)?)?;
        if cluster_width > width_px {
            return None;
        }
        let cluster_x = (width_px - cluster_width) / 2;
        let cluster_right = cluster_x.checked_add(cluster_width)?;
        let native_right = width_px.checked_sub(native_trailing_px.min(width_px))?;
        if cluster_x < native_leading_px.min(width_px) || cluster_right > native_right {
            return None;
        }
        let cluster_y = (title_height.saturating_sub(slot)) / 2;
        let session_cluster = ChromeRect {
            x: cluster_x,
            y: cluster_y,
            width: cluster_width,
            height: slot,
        };
        let session_buttons = std::array::from_fn(|index| ChromeRect {
            x: cluster_x + index as u32 * (slot + spacing),
            y: cluster_y,
            width: slot,
            height: slot,
        });
        let (minimize_button, maximize_button, close_button) =
            native_button_rects(width_px, title_height, native_trailing_px);
        Some(Self {
            title_bar: ChromeRect {
                x: 0,
                y: 0,
                width: width_px,
                height: title_height,
            },
            content_rect: PixelRect {
                x: 0,
                y: title_height,
                width: width_px,
                height: height_px - title_height,
            },
            session_cluster,
            session_buttons,
            minimize_button,
            maximize_button,
            close_button,
            native: NativeChromeInsets {
                leading_px: native_leading_px,
                trailing_px: native_trailing_px,
            },
        })
    }

    pub fn session_center_x(self) -> u32 {
        self.session_cluster.center().0
    }

    pub fn hit_test(self, x: u32, y: u32) -> ChromeHit {
        let custom = [
            ChromeHit::Connection,
            ChromeHit::Audio,
            ChromeHit::Clipboard,
            ChromeHit::SessionAction,
        ];
        if let Some((index, _)) = self
            .session_buttons
            .iter()
            .enumerate()
            .find(|(_, rect)| rect.contains(x, y))
        {
            return custom[index];
        }
        for (rect, hit) in [
            (self.minimize_button, ChromeHit::Minimize),
            (self.maximize_button, ChromeHit::Maximize),
            (self.close_button, ChromeHit::Close),
        ] {
            if rect.is_some_and(|rect| rect.contains(x, y)) {
                return hit;
            }
        }
        if self.title_bar.contains(x, y)
            && x >= self.native.leading_px
            && x < self.title_bar.width.saturating_sub(self.native.trailing_px)
        {
            ChromeHit::Drag
        } else {
            ChromeHit::Client
        }
    }
}

fn scaled(points: f64, factor: f64) -> Option<u32> {
    let value = (points * factor).ceil();
    (value.is_finite() && value > 0.0 && value <= u32::MAX as f64).then_some(value as u32)
}

fn native_button_rects(
    width_px: u32,
    title_height: u32,
    trailing_px: u32,
) -> (Option<ChromeRect>, Option<ChromeRect>, Option<ChromeRect>) {
    let trailing = trailing_px.min(width_px);
    if trailing < 3 {
        return (None, None, None);
    }
    let start = width_px - trailing;
    let third = trailing / 3;
    let remainder = trailing - third * 3;
    (
        Some(ChromeRect {
            x: start,
            y: 0,
            width: third,
            height: title_height,
        }),
        Some(ChromeRect {
            x: start + third,
            y: 0,
            width: third,
            height: title_height,
        }),
        Some(ChromeRect {
            x: start + third * 2,
            y: 0,
            width: third + remainder,
            height: title_height,
        }),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChromeHitRegions {
    pub layout: ChromeLayout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowChromeAction {
    Minimize,
    ToggleMaximize,
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowChromeError {
    UnsupportedWindow,
    PlatformCallFailed,
    InvalidGeometry,
}

pub trait WindowChromeAdapter {
    fn configure(&mut self, window: &winit::window::Window) -> Result<(), WindowChromeError>;
    fn refresh_for_dpi(&mut self, window: &winit::window::Window) -> Result<(), WindowChromeError>;
    fn native_insets(&self, window: &winit::window::Window) -> NativeChromeInsets;
    fn publish_hit_regions(&mut self, regions: ChromeHitRegions);
    fn execute(&mut self, window: &winit::window::Window, action: WindowChromeAction);
}

#[cfg(test)]
mod tests {
    use super::{ChromeHit, ChromeLayout};

    #[test]
    fn session_cluster_is_centered_despite_asymmetric_native_controls() {
        let layout = ChromeLayout::for_window(1200, 800, 1.5, 72, 144).unwrap();

        assert_eq!(layout.session_center_x(), 600);
        assert_eq!(layout.content_rect.y, 60);
        assert_eq!(layout.content_rect.height, 740);
        assert_ne!(
            layout.hit_test(
                layout.session_buttons[1].center().0,
                layout.session_buttons[1].center().1
            ),
            ChromeHit::Drag
        );
        assert_eq!(
            layout.hit_test(
                layout.maximize_button.unwrap().center().0,
                layout.maximize_button.unwrap().center().1,
            ),
            ChromeHit::Maximize
        );
    }

    #[test]
    fn drag_region_excludes_every_session_and_window_button() {
        let layout = ChromeLayout::for_window(1000, 700, 1.0, 0, 138).unwrap();
        for button in layout.session_buttons {
            let (x, y) = button.center();
            assert_ne!(layout.hit_test(x, y), ChromeHit::Drag);
        }
        for button in [
            layout.minimize_button.unwrap(),
            layout.maximize_button.unwrap(),
            layout.close_button.unwrap(),
        ] {
            let (x, y) = button.center();
            assert_ne!(layout.hit_test(x, y), ChromeHit::Drag);
        }
        assert_eq!(layout.hit_test(100, 20), ChromeHit::Drag);
    }

    #[test]
    fn dpi_changes_recompute_titlebar_and_content_as_one_geometry() {
        let at_100 = ChromeLayout::for_window(1100, 720, 1.0, 0, 138).unwrap();
        let at_200 = ChromeLayout::for_window(2200, 1440, 2.0, 0, 276).unwrap();

        assert_eq!(at_100.content_rect.y, 40);
        assert_eq!(at_200.content_rect.y, 80);
        assert_eq!(at_100.session_center_x() * 2, at_200.session_center_x());
    }
}
