use std::time::{Duration, Instant};

use egui::{Rect, Vec2};
use frd_core::{PixelRect, PixelSize};
use frd_ui_egui::control_island_metrics;
use frd_ui_model::{IslandAction, IslandWindowCapabilities};

use crate::window_chrome::{ChromeRect, NativeChromeInsets};

pub const REVEAL_DELAY: Duration = Duration::from_millis(150);
pub const HIDE_DELAY: Duration = Duration::from_millis(700);
pub const TOP_SENSOR_POINTS: f32 = 12.0;

const ISLAND_MARGIN_POINTS: f64 = 4.0;
const ISLAND_HANDLE_POINTS: f64 = 44.0;
const REVEAL_LINE_WIDTH_POINTS: f64 = 160.0;
const REVEAL_LINE_HEIGHT_POINTS: f64 = 2.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlIslandState {
    Hidden,
    RevealPending,
    Visible,
    HidePending,
    Pinned,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlIslandPlacement {
    pub normalized_center_x: f32,
    pub top_points: f32,
}

impl Default for ControlIslandPlacement {
    fn default() -> Self {
        Self {
            normalized_center_x: 0.5,
            top_points: 0.0,
        }
    }
}

pub struct FloatingChromeController {
    state: ControlIslandState,
    deadline: Option<Instant>,
    placement: ControlIslandPlacement,
}

impl FloatingChromeController {
    pub fn connected_default(_now: Instant) -> Self {
        Self {
            state: ControlIslandState::Hidden,
            deadline: None,
            placement: ControlIslandPlacement::default(),
        }
    }

    pub fn state(&self) -> ControlIslandState {
        self.state
    }

    pub fn observe_top_sensor(&mut self, inside: bool, remote_input_held: bool, now: Instant) {
        match self.state {
            ControlIslandState::Hidden if inside && !remote_input_held => {
                self.state = ControlIslandState::RevealPending;
                self.deadline = now.checked_add(REVEAL_DELAY);
            }
            ControlIslandState::RevealPending if !inside || remote_input_held => {
                self.state = ControlIslandState::Hidden;
                self.deadline = None;
            }
            _ => {}
        }
    }

    pub fn observe_island_union(&mut self, hovered: bool, focused_or_pressed: bool, now: Instant) {
        if hovered || focused_or_pressed {
            if matches!(
                self.state,
                ControlIslandState::Visible
                    | ControlIslandState::HidePending
                    | ControlIslandState::Pinned
            ) {
                self.state = ControlIslandState::Pinned;
                self.deadline = None;
            }
            return;
        }

        if matches!(
            self.state,
            ControlIslandState::Visible | ControlIslandState::Pinned
        ) {
            self.state = ControlIslandState::HidePending;
            self.deadline = now.checked_add(HIDE_DELAY);
        }
    }

    pub fn force_reveal_after_release(&mut self, _now: Instant) {
        self.state = ControlIslandState::Pinned;
        self.deadline = None;
    }

    pub fn advance(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.deadline else {
            return false;
        };
        if now < deadline {
            return false;
        }

        match self.state {
            ControlIslandState::RevealPending => self.state = ControlIslandState::Visible,
            ControlIslandState::HidePending => self.state = ControlIslandState::Hidden,
            _ => return false,
        }
        self.deadline = None;
        true
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub fn normalized_position(&self) -> (f32, f32) {
        (
            self.placement.normalized_center_x,
            self.placement.top_points,
        )
    }

    /// `bounds` 是调用方按当前岛尺寸和安全区预先收缩后的合法锚点范围。
    pub fn reposition(&mut self, delta_points: Vec2, bounds: Rect) {
        if !bounds.is_finite()
            || bounds.width() <= 0.0
            || bounds.height() < 0.0
            || !delta_points.x.is_finite()
            || !delta_points.y.is_finite()
        {
            return;
        }

        let center_x = (bounds.left()
            + bounds.width() * self.placement.normalized_center_x.clamp(0.0, 1.0)
            + delta_points.x)
            .clamp(bounds.left(), bounds.right());
        let top = (bounds.top() + self.placement.top_points + delta_points.y)
            .clamp(bounds.top(), bounds.bottom());
        self.placement.normalized_center_x = (center_x - bounds.left()) / bounds.width();
        self.placement.top_points = top - bounds.top();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteContentLayout {
    pub content_rect: PixelRect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChromeOverlayLayout {
    pub reveal_line_rect: ChromeRect,
    pub top_sensor_rect: ChromeRect,
    pub island_rect: Option<ChromeRect>,
    pub island_reposition_handle: Option<ChromeRect>,
    pub window_move_region: Option<ChromeRect>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChromeHitTarget {
    IslandAction(IslandAction),
    IslandRepositionHandle,
    WindowMoveRegion,
    NativeChrome,
    RemoteContent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChromeHitMap {
    island_actions: Vec<(ChromeRect, IslandAction)>,
    island_reposition_handle: Option<ChromeRect>,
    window_move_region: Option<ChromeRect>,
    native_chrome: Vec<ChromeRect>,
    remote_content: PixelRect,
    pub maximize_rect: Option<ChromeRect>,
}

impl ChromeHitMap {
    /// 构造完整候选；失败时不修改调用方当前已发布的 hit map。
    pub fn candidate(
        remote_content: PixelRect,
        island_actions: Vec<(ChromeRect, IslandAction)>,
        island_reposition_handle: Option<ChromeRect>,
        window_move_region: Option<ChromeRect>,
        native_chrome: Vec<ChromeRect>,
    ) -> Option<Self> {
        remote_content.checked_bounds()?;
        if island_actions
            .iter()
            .any(|(rect, _)| !valid_rect(*rect) || !rect_within(remote_content, *rect))
            || island_reposition_handle
                .is_some_and(|rect| !valid_rect(rect) || !rect_within(remote_content, rect))
            || window_move_region
                .is_some_and(|rect| !valid_rect(rect) || !rect_within(remote_content, rect))
            || native_chrome
                .iter()
                .any(|rect| !valid_rect(*rect) || !rect_within(remote_content, *rect))
            || overlaps(island_reposition_handle, window_move_region)
            || island_actions.iter().any(|(rect, _)| {
                overlaps(Some(*rect), island_reposition_handle)
                    || overlaps(Some(*rect), window_move_region)
            })
        {
            return None;
        }
        let maximize_rect = island_actions.iter().find_map(|(rect, action)| {
            (*action == IslandAction::ToggleMaximizeWindow).then_some(*rect)
        });
        Some(Self {
            island_actions,
            island_reposition_handle,
            window_move_region,
            native_chrome,
            remote_content,
            maximize_rect,
        })
    }

    pub fn hit_test(&self, point: (u32, u32)) -> Option<ChromeHitTarget> {
        let (x, y) = point;
        if let Some((_, action)) = self
            .island_actions
            .iter()
            .find(|(rect, _)| rect.contains(x, y))
        {
            return Some(ChromeHitTarget::IslandAction(*action));
        }
        if self
            .island_reposition_handle
            .is_some_and(|rect| rect.contains(x, y))
        {
            return Some(ChromeHitTarget::IslandRepositionHandle);
        }
        if self
            .window_move_region
            .is_some_and(|rect| rect.contains(x, y))
        {
            return Some(ChromeHitTarget::WindowMoveRegion);
        }
        if self.native_chrome.iter().any(|rect| rect.contains(x, y)) {
            return Some(ChromeHitTarget::NativeChrome);
        }
        pixel_rect_contains(self.remote_content, x, y).then_some(ChromeHitTarget::RemoteContent)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChromeLayouts {
    pub remote: RemoteContentLayout,
    pub overlay: ChromeOverlayLayout,
    pub hit_map: ChromeHitMap,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromeGeometrySnapshot {
    pub window_size: PixelSize,
    pub scale_factor: f64,
    pub native: NativeChromeInsets,
    window_capabilities: IslandWindowCapabilities,
}

impl ChromeGeometrySnapshot {
    pub fn new(
        width_px: u32,
        height_px: u32,
        scale_factor: f64,
        native: NativeChromeInsets,
    ) -> Option<Self> {
        let window_size = PixelSize::new(width_px, height_px)?;
        if !scale_factor.is_finite()
            || scale_factor <= 0.0
            || native.leading_px.saturating_add(native.trailing_px) >= width_px
        {
            return None;
        }
        Some(Self {
            window_size,
            scale_factor,
            native,
            window_capabilities: IslandWindowCapabilities::NONE,
        })
    }

    pub fn with_window_capabilities(mut self, capabilities: IslandWindowCapabilities) -> Self {
        self.window_capabilities = capabilities;
        self
    }

    pub fn layouts(
        self,
        placement: ControlIslandPlacement,
        visible: bool,
    ) -> Option<ChromeLayouts> {
        if !placement.normalized_center_x.is_finite()
            || !placement.top_points.is_finite()
            || placement.top_points < 0.0
        {
            return None;
        }

        let remote = RemoteContentLayout {
            content_rect: PixelRect {
                x: 0,
                y: 0,
                width: self.window_size.width,
                height: self.window_size.height,
            },
        };
        let safe_left = self.native.leading_px;
        let safe_right = self
            .window_size
            .width
            .checked_sub(self.native.trailing_px)?;
        let safe_width = safe_right.checked_sub(safe_left)?;
        let metrics = control_island_metrics(self.window_capabilities);
        let island_width = scaled_points(
            f64::from(metrics.total_width) + ISLAND_MARGIN_POINTS * 2.0,
            self.scale_factor,
        )?;
        let island_height = scaled_points(
            f64::from(metrics.height) + ISLAND_MARGIN_POINTS * 2.0,
            self.scale_factor,
        )?;
        if island_width > safe_width || island_height > self.window_size.height {
            return None;
        }

        let normalized_center = placement.normalized_center_x.clamp(0.0, 1.0);
        let desired_center = safe_left as f64 + safe_width as f64 * normalized_center as f64;
        let island_x =
            clamp_origin_around_center(desired_center, island_width, safe_left, safe_right)?;
        let requested_top = (f64::from(placement.top_points) * self.scale_factor).ceil();
        if !requested_top.is_finite() || requested_top > u32::MAX as f64 {
            return None;
        }
        let island_y = (requested_top as u32).min(self.window_size.height - island_height);
        let island_rect = ChromeRect {
            x: island_x,
            y: island_y,
            width: island_width,
            height: island_height,
        };

        let line_width =
            scaled_points(REVEAL_LINE_WIDTH_POINTS, self.scale_factor)?.min(safe_width);
        let line_height = scaled_points(REVEAL_LINE_HEIGHT_POINTS, self.scale_factor)?
            .min(self.window_size.height);
        let clamped_island_center = f64::from(island_rect.center().0);
        let reveal_line_rect = ChromeRect {
            x: clamp_origin_around_center(
                clamped_island_center,
                line_width,
                safe_left,
                safe_right,
            )?,
            y: 0,
            width: line_width,
            height: line_height,
        };
        let sensor_height = scaled_points(f64::from(TOP_SENSOR_POINTS), self.scale_factor)?
            .min(self.window_size.height);
        let top_sensor_rect = ChromeRect {
            x: safe_left,
            y: 0,
            width: safe_width,
            height: sensor_height,
        };

        let (visible_island, reposition_handle, window_move_region) = if visible {
            let margin = scaled_points(ISLAND_MARGIN_POINTS, self.scale_factor)?;
            let handle = scaled_points(ISLAND_HANDLE_POINTS, self.scale_factor)?;
            let reposition = ChromeRect {
                x: island_rect.x.checked_add(margin)?,
                y: island_rect.y.checked_add(margin)?,
                width: handle,
                height: handle,
            };
            let window_move = if self.window_capabilities.begin_move {
                Some(external_move_region(
                    island_rect,
                    handle,
                    margin,
                    safe_left,
                    safe_right,
                )?)
            } else {
                None
            };
            (Some(island_rect), Some(reposition), window_move)
        } else {
            (None, None, None)
        };

        let overlay = ChromeOverlayLayout {
            reveal_line_rect,
            top_sensor_rect,
            island_rect: visible_island,
            island_reposition_handle: reposition_handle,
            window_move_region,
        };
        let hit_map = ChromeHitMap::candidate(
            remote.content_rect,
            Vec::new(),
            overlay.island_reposition_handle,
            overlay.window_move_region,
            Vec::new(),
        )?;
        Some(ChromeLayouts {
            remote,
            overlay,
            hit_map,
        })
    }
}

fn external_move_region(
    island: ChromeRect,
    width: u32,
    gap: u32,
    safe_left: u32,
    safe_right: u32,
) -> Option<ChromeRect> {
    if island.x >= safe_left.checked_add(width)?.checked_add(gap)? {
        return Some(ChromeRect {
            x: island.x.checked_sub(width)?.checked_sub(gap)?,
            y: island.y,
            width,
            height: width.min(island.height),
        });
    }
    let x = island.x.checked_add(island.width)?.checked_add(gap)?;
    (x.checked_add(width)? <= safe_right).then_some(ChromeRect {
        x,
        y: island.y,
        width,
        height: width.min(island.height),
    })
}

fn clamp_origin_around_center(
    desired_center: f64,
    width: u32,
    min_x: u32,
    max_x: u32,
) -> Option<u32> {
    let max_origin = max_x.checked_sub(width)?;
    let desired = desired_center - f64::from(width) / 2.0;
    if !desired.is_finite() {
        return None;
    }
    Some(
        (desired
            .round()
            .clamp(f64::from(min_x), f64::from(max_origin))) as u32,
    )
}

fn scaled_points(points: f64, factor: f64) -> Option<u32> {
    let value = (points * factor).ceil();
    (value.is_finite() && value > 0.0 && value <= u32::MAX as f64).then_some(value as u32)
}

fn valid_rect(rect: ChromeRect) -> bool {
    rect.width > 0
        && rect.height > 0
        && rect.x.checked_add(rect.width).is_some()
        && rect.y.checked_add(rect.height).is_some()
}

fn rect_within(outer: PixelRect, inner: ChromeRect) -> bool {
    let Some((_, outer_end)) = outer.checked_bounds() else {
        return false;
    };
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner
            .x
            .checked_add(inner.width)
            .is_some_and(|end| end <= outer_end.x)
        && inner
            .y
            .checked_add(inner.height)
            .is_some_and(|end| end <= outer_end.y)
}

fn overlaps(left: Option<ChromeRect>, right: Option<ChromeRect>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => {
            left.x < right.x.saturating_add(right.width)
                && right.x < left.x.saturating_add(left.width)
                && left.y < right.y.saturating_add(right.height)
                && right.y < left.y.saturating_add(left.height)
        }
        _ => false,
    }
}

fn pixel_rect_contains(rect: PixelRect, x: u32, y: u32) -> bool {
    x >= rect.x
        && y >= rect.y
        && x < rect.x.saturating_add(rect.width)
        && y < rect.y.saturating_add(rect.height)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use egui::{pos2, vec2, Rect};
    use frd_core::PixelRect;
    use frd_ui_model::{IslandAction, IslandWindowCapabilities};

    use super::{
        ChromeGeometrySnapshot, ChromeHitMap, ChromeHitTarget, ControlIslandPlacement,
        ControlIslandState, FloatingChromeController,
    };
    use crate::window_chrome::{ChromeRect, NativeChromeInsets};

    #[test]
    fn floating_chrome_hover_reveals_after_150_ms_and_hides_700_ms_after_leave() {
        let start = Instant::now();
        let mut chrome = FloatingChromeController::connected_default(start);

        chrome.observe_top_sensor(true, false, start);
        assert_eq!(chrome.state(), ControlIslandState::RevealPending);
        assert_eq!(
            chrome.next_deadline(),
            Some(start + Duration::from_millis(150))
        );
        assert!(!chrome.advance(start + Duration::from_millis(149)));
        assert!(chrome.advance(start + Duration::from_millis(150)));
        assert_eq!(chrome.state(), ControlIslandState::Visible);

        chrome.observe_island_union(false, false, start + Duration::from_millis(150));
        assert_eq!(chrome.state(), ControlIslandState::HidePending);
        assert_eq!(
            chrome.next_deadline(),
            Some(start + Duration::from_millis(850))
        );
        assert!(!chrome.advance(start + Duration::from_millis(849)));
        assert!(chrome.advance(start + Duration::from_millis(850)));
        assert_eq!(chrome.state(), ControlIslandState::Hidden);
        assert_eq!(chrome.next_deadline(), None);
    }

    #[test]
    fn floating_chrome_cancels_reveal_and_defers_it_while_remote_input_is_held() {
        let start = Instant::now();
        let mut chrome = FloatingChromeController::connected_default(start);

        chrome.observe_top_sensor(true, true, start);
        assert_eq!(chrome.state(), ControlIslandState::Hidden);
        assert_eq!(chrome.next_deadline(), None);

        chrome.observe_top_sensor(true, false, start + Duration::from_millis(20));
        assert_eq!(chrome.state(), ControlIslandState::RevealPending);
        chrome.observe_top_sensor(false, false, start + Duration::from_millis(30));
        assert_eq!(chrome.state(), ControlIslandState::Hidden);
        assert_eq!(chrome.next_deadline(), None);
    }

    #[test]
    fn floating_chrome_reentry_and_pinning_cancel_pending_hide() {
        let start = Instant::now();
        let mut chrome = FloatingChromeController::connected_default(start);
        chrome.force_reveal_after_release(start);
        assert_eq!(chrome.state(), ControlIslandState::Pinned);

        chrome.observe_island_union(false, false, start);
        assert_eq!(chrome.state(), ControlIslandState::HidePending);
        chrome.observe_island_union(true, false, start + Duration::from_millis(100));
        assert_eq!(chrome.state(), ControlIslandState::Pinned);
        assert_eq!(chrome.next_deadline(), None);

        chrome.observe_island_union(false, false, start + Duration::from_millis(200));
        assert!(!chrome.advance(start + Duration::from_millis(899)));
        assert!(chrome.advance(start + Duration::from_millis(900)));
        assert_eq!(chrome.state(), ControlIslandState::Hidden);
    }

    #[test]
    fn floating_chrome_position_is_session_local_normalized_and_clamped() {
        let start = Instant::now();
        let mut chrome = FloatingChromeController::connected_default(start);
        let bounds = Rect::from_min_max(pos2(100.0, 20.0), pos2(900.0, 500.0));

        chrome.reposition(vec2(1_000.0, -100.0), bounds);
        assert_eq!(chrome.normalized_position(), (1.0, 0.0));
        chrome.reposition(vec2(-2_000.0, 1_000.0), bounds);
        assert_eq!(chrome.normalized_position(), (0.0, 480.0));
    }

    #[test]
    fn floating_chrome_visibility_never_changes_remote_content() {
        let snapshot =
            ChromeGeometrySnapshot::new(1600, 900, 1.5, NativeChromeInsets::default()).unwrap();
        let hidden = snapshot
            .layouts(ControlIslandPlacement::default(), false)
            .unwrap();
        let visible = snapshot
            .layouts(ControlIslandPlacement::default(), true)
            .unwrap();

        assert_eq!(hidden.remote, visible.remote);
        assert_eq!(
            hidden.remote.content_rect,
            PixelRect {
                x: 0,
                y: 0,
                width: 1600,
                height: 900,
            }
        );
        assert_eq!(
            hidden
                .hit_map
                .hit_test(hidden.overlay.reveal_line_rect.center()),
            Some(ChromeHitTarget::RemoteContent),
            "the visual-only green line must not enter the hit map"
        );
    }

    #[test]
    fn floating_chrome_line_follows_the_clamped_island_center() {
        let layouts = ChromeGeometrySnapshot::new(800, 600, 1.0, NativeChromeInsets::default())
            .unwrap()
            .layouts(
                ControlIslandPlacement {
                    normalized_center_x: 0.0,
                    top_points: 0.0,
                },
                true,
            )
            .unwrap();

        assert_eq!(
            layouts.overlay.reveal_line_rect.center().0,
            layouts.overlay.island_rect.unwrap().center().0
        );
    }

    #[test]
    fn floating_chrome_top_sensor_spans_the_effective_safe_width() {
        let layouts = ChromeGeometrySnapshot::new(
            1200,
            800,
            1.0,
            NativeChromeInsets {
                leading_px: 72,
                trailing_px: 144,
            },
        )
        .unwrap()
        .layouts(ControlIslandPlacement::default(), false)
        .unwrap();

        assert_eq!(layouts.overlay.top_sensor_rect.x, 72);
        assert_eq!(layouts.overlay.top_sensor_rect.width, 984);
        assert!(
            layouts.overlay.reveal_line_rect.width < layouts.overlay.top_sensor_rect.width,
            "the visual line stays bounded while the move-only sensor covers the safe top edge"
        );
    }

    #[test]
    fn floating_chrome_rejects_actions_overlapping_either_move_handle() {
        let remote = PixelRect {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        let shared = ChromeRect {
            x: 100,
            y: 10,
            width: 44,
            height: 44,
        };

        for (reposition, window_move) in [(Some(shared), None), (None, Some(shared))] {
            assert_eq!(
                ChromeHitMap::candidate(
                    remote,
                    vec![(shared, IslandAction::Disconnect)],
                    reposition,
                    window_move,
                    Vec::new(),
                ),
                None
            );
        }
    }

    #[test]
    fn floating_chrome_hit_map_prioritizes_controls_and_separates_both_move_regions() {
        let remote = PixelRect {
            x: 0,
            y: 0,
            width: 1200,
            height: 800,
        };
        let control = ChromeRect {
            x: 500,
            y: 8,
            width: 44,
            height: 44,
        };
        let reposition = ChromeRect {
            x: 450,
            y: 8,
            width: 44,
            height: 44,
        };
        let window_move = ChromeRect {
            x: 550,
            y: 8,
            width: 44,
            height: 44,
        };
        let hit_map = ChromeHitMap::candidate(
            remote,
            vec![(control, IslandAction::Disconnect)],
            Some(reposition),
            Some(window_move),
            Vec::new(),
        )
        .unwrap();

        assert_eq!(
            hit_map.hit_test(control.center()),
            Some(ChromeHitTarget::IslandAction(IslandAction::Disconnect))
        );
        assert_eq!(
            hit_map.hit_test(reposition.center()),
            Some(ChromeHitTarget::IslandRepositionHandle)
        );
        assert_eq!(
            hit_map.hit_test(window_move.center()),
            Some(ChromeHitTarget::WindowMoveRegion)
        );

        let outside_client = ChromeRect {
            x: 1_190,
            y: 8,
            width: 44,
            height: 44,
        };
        assert_eq!(
            ChromeHitMap::candidate(
                remote,
                vec![(outside_client, IslandAction::Disconnect)],
                None,
                None,
                Vec::new(),
            ),
            None,
            "an incomplete geometry candidate must not replace the caller's prior map"
        );

        let windows = ChromeGeometrySnapshot::new(1600, 900, 1.0, NativeChromeInsets::default())
            .unwrap()
            .with_window_capabilities(IslandWindowCapabilities::WINDOWS)
            .layouts(ControlIslandPlacement::default(), true)
            .unwrap();
        let reposition = windows.overlay.island_reposition_handle.unwrap();
        let window_move = windows.overlay.window_move_region.unwrap();
        assert!(
            reposition.x + reposition.width <= window_move.x
                || window_move.x + window_move.width <= reposition.x
                || reposition.y + reposition.height <= window_move.y
                || window_move.y + window_move.height <= reposition.y,
            "repositioning the island and moving the native window need disjoint targets"
        );
    }
}
