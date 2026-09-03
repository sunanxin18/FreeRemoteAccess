use frd_core::PixelRect;
use frd_ui_egui::session_chrome_metrics;

pub const TITLE_BAR_HEIGHT_POINTS: f64 = 44.0;
// 280 pt session chrome + twice the 138 pt maximum Windows side inset + 4 pt DPI rounding headroom.
pub(crate) const MINIMUM_WINDOW_WIDTH_POINTS: f64 =
    session_chrome_metrics().total_width as f64 + 2.0 * 138.0 + 4.0;

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
    pub session_buttons: [ChromeRect; 5],
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
        let metrics = session_chrome_metrics();
        let slot = scaled(f64::from(metrics.slot_size), scale_factor)?;
        let session_boundaries = scaled_session_boundaries(metrics, scale_factor)?;
        let cluster_width = *session_boundaries.last()?;
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
        let session_buttons = std::array::from_fn(|index| {
            let start = session_boundaries[index * 2];
            let end = session_boundaries[index * 2 + 1];
            let rect = ChromeRect {
                x: cluster_x + start,
                y: cluster_y,
                width: end - start,
                height: slot,
            };
            rect
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
            ChromeHit::Client,
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
        if self.session_cluster.contains(x, y) {
            return ChromeHit::Client;
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

fn scaled_session_boundaries(
    metrics: frd_ui_egui::SessionChromeMetrics,
    factor: f64,
) -> Option<[u32; 10]> {
    let slot = f64::from(metrics.slot_size);
    let frame_response = f64::from(metrics.frame_response_width);
    let spacing = f64::from(metrics.spacing);
    let boundaries = [
        0.0,
        slot,
        slot + spacing,
        slot + spacing + frame_response,
        slot + 2.0 * spacing + frame_response,
        2.0 * slot + 2.0 * spacing + frame_response,
        2.0 * slot + 3.0 * spacing + frame_response,
        3.0 * slot + 3.0 * spacing + frame_response,
        3.0 * slot + 4.0 * spacing + frame_response,
        f64::from(metrics.total_width),
    ];
    Some([
        scaled_boundary(boundaries[0], factor)?,
        scaled_boundary(boundaries[1], factor)?,
        scaled_boundary(boundaries[2], factor)?,
        scaled_boundary(boundaries[3], factor)?,
        scaled_boundary(boundaries[4], factor)?,
        scaled_boundary(boundaries[5], factor)?,
        scaled_boundary(boundaries[6], factor)?,
        scaled_boundary(boundaries[7], factor)?,
        scaled_boundary(boundaries[8], factor)?,
        scaled_boundary(boundaries[9], factor)?,
    ])
}

fn scaled_boundary(points: f64, factor: f64) -> Option<u32> {
    let value = (points * factor).ceil();
    (value.is_finite() && value >= 0.0 && value <= u32::MAX as f64).then_some(value as u32)
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
    fn publish_hit_regions(&mut self, regions: Option<ChromeHitRegions>);
    fn execute(&mut self, window: &winit::window::Window, action: WindowChromeAction);
}

#[cfg(test)]
mod tests {
    use super::{ChromeHit, ChromeLayout, ChromeRect};

    #[test]
    fn session_cluster_is_centered_despite_asymmetric_native_controls() {
        let layout = ChromeLayout::for_window(1200, 800, 1.5, 72, 144).unwrap();

        assert_eq!(layout.session_center_x(), 600);
        assert_eq!(layout.content_rect.y, 66);
        assert_eq!(layout.content_rect.height, 734);
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

        assert_eq!(at_100.content_rect.y, 44);
        assert_eq!(at_200.content_rect.y, 88);
        assert_eq!(at_100.session_center_x() * 2, at_200.session_center_x());
    }

    #[test]
    fn session_hit_targets_are_at_least_44_pixels_at_supported_scales() {
        for (scale, expected) in [(1.0, 44), (1.5, 66), (2.0, 88)] {
            let layout = ChromeLayout::for_window(1600, 1000, scale, 0, 180).unwrap();
            assert!(layout
                .session_buttons
                .iter()
                .all(|rect| { rect.width >= expected && rect.height >= expected }));
            assert_eq!(layout.content_rect.y, (44.0 * scale).ceil() as u32);
        }
    }

    #[test]
    fn content_rect_and_hit_test_share_the_effective_titlebar_boundary() {
        let layout = ChromeLayout::for_window(1200, 800, 1.5, 72, 144).unwrap();
        assert_eq!(
            layout.hit_test(100, layout.content_rect.y - 1),
            ChromeHit::Drag
        );
        assert_eq!(
            layout.hit_test(100, layout.content_rect.y),
            ChromeHit::Client
        );
        let action = layout.session_buttons[4];
        assert_eq!(
            layout.hit_test(action.center().0, action.center().1),
            ChromeHit::SessionAction
        );
    }

    #[test]
    fn disconnect_action_at_the_fifth_ui_region_is_a_session_action() {
        let layout = ChromeLayout::for_window(1000, 700, 1.0, 0, 138).unwrap();

        // The rendered session chrome is 44 + 4 + 88 + 4 + 44 + 4 + 44 + 4 + 44
        // logical points wide, centered in this 1000 px window. Its final
        // Disconnect action is therefore centered at (618, 22).
        assert_eq!(layout.hit_test(618, 22), ChromeHit::SessionAction);
    }

    #[test]
    fn non_integral_dpi_uses_the_shared_ui_boundaries_for_every_session_region() {
        let layout = ChromeLayout::for_window(1210, 800, 1.1, 0, 153).unwrap();

        assert_eq!(layout.session_cluster.x, 451);
        assert_eq!(layout.session_cluster.width, 308);
        assert_eq!(
            layout.session_buttons[0],
            ChromeRect {
                x: 451,
                y: 0,
                width: 49,
                height: 49,
            }
        );
        assert_eq!(
            layout.session_buttons[1],
            ChromeRect {
                x: 504,
                y: 0,
                width: 97,
                height: 49,
            }
        );
        assert_eq!(
            layout.session_buttons[4],
            ChromeRect {
                x: 711,
                y: 0,
                width: 48,
                height: 49,
            }
        );
        assert_eq!(layout.hit_test(500, 22), ChromeHit::Client);
        assert_eq!(layout.hit_test(735, 22), ChromeHit::SessionAction);
    }

    #[test]
    fn widened_minimum_window_preserves_the_centered_cluster_at_windows_dpi_scales() {
        assert!(ChromeLayout::for_window(520, 360, 1.0, 0, 138).is_none());

        for (scale_factor, width_px, trailing_px) in [
            (1.0, 560, 138),
            (1.1, 616, 153),
            (1.25, 700, 174),
            (1.5, 840, 207),
            (2.0, 1120, 276),
        ] {
            let layout =
                ChromeLayout::for_window(width_px, 720, scale_factor, 0, trailing_px).unwrap();
            assert!(layout.session_cluster.x >= layout.native.leading_px);
            assert!(
                layout.session_cluster.x + layout.session_cluster.width
                    <= layout.title_bar.width - layout.native.trailing_px
            );
        }
    }
}
