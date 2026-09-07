//! 协议无关的远程显示尺寸意图与规划。
//!
//! 这里的尺寸全部使用物理像素。窗口系统的逻辑点只用于保留本地窗口
//! 几何信息，绝不能直接作为远端桌面尺寸发送。

use crate::{PixelRect, PixelSize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolutionMode {
    NativeDisplay,
    DisplayWorkArea,
    WindowContent,
    Fixed(PixelSize),
    ServerManaged,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayGeometry {
    pub logical_width: f64,
    pub logical_height: f64,
    pub physical_size: PixelSize,
    pub work_area: PixelRect,
    pub content_area: PixelRect,
    pub scale_factor_milli: u32,
    pub fullscreen: bool,
}

impl DisplayGeometry {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        logical_width: f64,
        logical_height: f64,
        physical_size: PixelSize,
        work_area: PixelRect,
        content_area: PixelRect,
        scale_factor_milli: u32,
        fullscreen: bool,
    ) -> Option<Self> {
        if !logical_width.is_finite()
            || !logical_height.is_finite()
            || logical_width <= 0.0
            || logical_height <= 0.0
            || scale_factor_milli == 0
        {
            return None;
        }
        for area in [work_area, content_area] {
            let (_, end) = area.checked_bounds()?;
            if end.x > physical_size.width || end.y > physical_size.height {
                return None;
            }
        }
        Some(Self {
            logical_width,
            logical_height,
            physical_size,
            work_area,
            content_area,
            scale_factor_milli,
            fullscreen,
        })
    }

    pub fn from_physical(physical_size: PixelSize, scale_factor_milli: u32) -> Option<Self> {
        let work_area = PixelRect {
            x: 0,
            y: 0,
            width: physical_size.width,
            height: physical_size.height,
        };
        Self::new(
            f64::from(physical_size.width) * 1000.0 / f64::from(scale_factor_milli),
            f64::from(physical_size.height) * 1000.0 / f64::from(scale_factor_milli),
            physical_size,
            work_area,
            work_area,
            scale_factor_milli,
            false,
        )
    }

    fn size_for(self, mode: ResolutionMode) -> Option<PixelSize> {
        match mode {
            ResolutionMode::NativeDisplay => Some(self.physical_size),
            ResolutionMode::DisplayWorkArea => {
                PixelSize::new(self.work_area.width, self.work_area.height)
            }
            ResolutionMode::WindowContent => {
                PixelSize::new(self.content_area.width, self.content_area.height)
            }
            ResolutionMode::Fixed(size) => Some(size),
            ResolutionMode::ServerManaged => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayIntent {
    pub mode: ResolutionMode,
    pub geometry: Option<DisplayGeometry>,
}

impl DisplayIntent {
    pub const fn server_managed() -> Self {
        Self {
            mode: ResolutionMode::ServerManaged,
            geometry: None,
        }
    }

    pub fn native_display(geometry: DisplayGeometry) -> Self {
        Self {
            mode: ResolutionMode::NativeDisplay,
            geometry: Some(geometry),
        }
    }

    pub fn display_work_area(geometry: DisplayGeometry) -> Self {
        Self {
            mode: ResolutionMode::DisplayWorkArea,
            geometry: Some(geometry),
        }
    }

    pub fn window_content(geometry: DisplayGeometry) -> Self {
        Self {
            mode: ResolutionMode::WindowContent,
            geometry: Some(geometry),
        }
    }

    pub const fn fixed(size: PixelSize) -> Self {
        Self {
            mode: ResolutionMode::Fixed(size),
            geometry: None,
        }
    }

    pub const fn is_server_managed(self) -> bool {
        matches!(self.mode, ResolutionMode::ServerManaged)
    }
}

impl Default for DisplayIntent {
    fn default() -> Self {
        Self::server_managed()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplayConstraints {
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub max_area: Option<u64>,
    pub max_texture_dimension: Option<u32>,
    pub frame_budget_bytes: Option<u64>,
    pub bytes_per_pixel: u32,
}

impl Default for DisplayConstraints {
    fn default() -> Self {
        Self {
            max_width: None,
            max_height: None,
            max_area: None,
            max_texture_dimension: None,
            frame_budget_bytes: None,
            bytes_per_pixel: 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayLimit {
    Width,
    Height,
    Area,
    Texture,
    FrameBudget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplayLimitReport {
    pub width: bool,
    pub height: bool,
    pub area: bool,
    pub texture: bool,
    pub frame_budget: bool,
}

impl DisplayLimitReport {
    fn for_candidate(candidate: PixelSize, constraints: DisplayConstraints) -> Self {
        Self {
            width: constraints
                .max_width
                .is_some_and(|limit| candidate.width > limit),
            height: constraints
                .max_height
                .is_some_and(|limit| candidate.height > limit),
            area: constraints.max_area.is_some_and(|limit| {
                u64::from(candidate.width) * u64::from(candidate.height) > limit
            }),
            texture: constraints
                .max_texture_dimension
                .is_some_and(|limit| candidate.width > limit || candidate.height > limit),
            frame_budget: constraints.frame_budget_bytes.is_some_and(|limit| {
                u64::from(candidate.width)
                    .saturating_mul(u64::from(candidate.height))
                    .saturating_mul(u64::from(constraints.bytes_per_pixel))
                    > limit
            }),
        }
    }

    fn any(self) -> bool {
        self.width || self.height || self.area || self.texture || self.frame_budget
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayPlanReason {
    ServerManaged,
    Requested,
    Constrained(DisplayLimitReport),
    NoUsableSize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplayPlan {
    pub requested_size: Option<PixelSize>,
    pub remote_size: Option<PixelSize>,
    pub reason: DisplayPlanReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayPlanError {
    InvalidConstraint,
    InvalidIntent,
}

pub struct DisplayPlanner;

impl DisplayPlanner {
    pub fn plan(
        intent: DisplayIntent,
        constraints: DisplayConstraints,
    ) -> Result<DisplayPlan, DisplayPlanError> {
        if constraints.bytes_per_pixel == 0
            || constraints.max_width == Some(0)
            || constraints.max_height == Some(0)
            || constraints.max_texture_dimension == Some(0)
            || constraints.max_area == Some(0)
            || constraints.frame_budget_bytes == Some(0)
        {
            return Err(DisplayPlanError::InvalidConstraint);
        }

        let requested_size = match intent.mode {
            ResolutionMode::ServerManaged => None,
            ResolutionMode::Fixed(size) => PixelSize::new(size.width, size.height),
            mode => intent.geometry.and_then(|geometry| geometry.size_for(mode)),
        };
        let Some(requested_size) = requested_size else {
            return Ok(DisplayPlan {
                requested_size: None,
                remote_size: None,
                reason: if intent.is_server_managed() {
                    DisplayPlanReason::ServerManaged
                } else {
                    DisplayPlanReason::NoUsableSize
                },
            });
        };

        let report = DisplayLimitReport::for_candidate(requested_size, constraints);
        if !report.any() {
            return Ok(DisplayPlan {
                requested_size: Some(requested_size),
                remote_size: Some(requested_size),
                reason: DisplayPlanReason::Requested,
            });
        }

        let mut low = 0.0f64;
        let mut high = 1.0f64;
        for _ in 0..40 {
            let scale = (low + high) * 0.5;
            let candidate = scaled_size(requested_size, scale);
            if candidate.is_some_and(|size| fits(size, constraints)) {
                low = scale;
            } else {
                high = scale;
            }
        }
        let Some(remote_size) =
            scaled_size(requested_size, low).filter(|size| fits(*size, constraints))
        else {
            return Ok(DisplayPlan {
                requested_size: Some(requested_size),
                remote_size: None,
                reason: DisplayPlanReason::NoUsableSize,
            });
        };

        Ok(DisplayPlan {
            requested_size: Some(requested_size),
            remote_size: Some(remote_size),
            reason: DisplayPlanReason::Constrained(report),
        })
    }
}

fn scaled_size(size: PixelSize, scale: f64) -> Option<PixelSize> {
    let width = (f64::from(size.width) * scale).floor();
    let height = (f64::from(size.height) * scale).floor();
    if !width.is_finite() || !height.is_finite() || width < 1.0 || height < 1.0 {
        return None;
    }
    PixelSize::new(width as u32, height as u32)
}

fn fits(size: PixelSize, constraints: DisplayConstraints) -> bool {
    !DisplayLimitReport::for_candidate(size, constraints).any()
}

#[cfg(test)]
mod tests {
    use super::{
        DisplayConstraints, DisplayGeometry, DisplayIntent, DisplayLimitReport, DisplayPlanReason,
        DisplayPlanner, ResolutionMode,
    };
    use crate::{PixelRect, PixelSize};

    fn retina_geometry() -> DisplayGeometry {
        DisplayGeometry::new(
            1728.0,
            1117.0,
            PixelSize::new(3456, 2234).unwrap(),
            PixelRect {
                x: 0,
                y: 0,
                width: 3456,
                height: 2234,
            },
            PixelRect {
                x: 0,
                y: 0,
                width: 3456,
                height: 2200,
            },
            2000,
            false,
        )
        .unwrap()
    }

    #[test]
    fn native_display_prefers_physical_pixels_without_2560_cap() {
        let plan = DisplayPlanner::plan(
            DisplayIntent::native_display(retina_geometry()),
            DisplayConstraints::default(),
        )
        .unwrap();
        assert_eq!(plan.requested_size.unwrap().width, 3456);
        assert_eq!(plan.remote_size.unwrap().height, 2234);
        assert_eq!(plan.reason, DisplayPlanReason::Requested);
    }

    #[test]
    fn five_k_fixed_size_is_allowed_when_constraints_allow_it() {
        let requested = PixelSize::new(5120, 2880).unwrap();
        let plan = DisplayPlanner::plan(
            DisplayIntent::fixed(requested),
            DisplayConstraints::default(),
        )
        .unwrap();
        assert_eq!(plan.remote_size, Some(requested));
    }

    #[test]
    fn work_area_and_window_content_are_distinct_physical_targets() {
        let geometry = retina_geometry();
        assert_eq!(
            DisplayPlanner::plan(
                DisplayIntent::display_work_area(geometry),
                DisplayConstraints::default()
            )
            .unwrap()
            .remote_size,
            PixelSize::new(3456, 2234)
        );
        assert_eq!(
            DisplayPlanner::plan(
                DisplayIntent::window_content(geometry),
                DisplayConstraints::default()
            )
            .unwrap()
            .remote_size,
            PixelSize::new(3456, 2200)
        );
    }

    #[test]
    fn server_managed_does_not_invent_a_local_size() {
        let plan =
            DisplayPlanner::plan(DisplayIntent::default(), DisplayConstraints::default()).unwrap();
        assert_eq!(plan.remote_size, None);
        assert_eq!(plan.reason, DisplayPlanReason::ServerManaged);
    }

    #[test]
    fn area_budget_clamps_aspect_preservingly_and_reports_limit() {
        let requested = PixelSize::new(3840, 2160).unwrap();
        let plan = DisplayPlanner::plan(
            DisplayIntent::fixed(requested),
            DisplayConstraints {
                max_area: Some(1920 * 1080),
                ..Default::default()
            },
        )
        .unwrap();
        let size = plan.remote_size.unwrap();
        assert!(u64::from(size.width) * u64::from(size.height) <= 1920 * 1080);
        assert!((f64::from(size.width) / f64::from(size.height) - 16.0 / 9.0).abs() < 0.01);
        assert_eq!(
            plan.reason,
            DisplayPlanReason::Constrained(DisplayLimitReport {
                width: false,
                height: false,
                area: true,
                texture: false,
                frame_budget: false,
            })
        );
    }

    #[test]
    fn frame_budget_and_texture_limits_are_applied() {
        let plan = DisplayPlanner::plan(
            DisplayIntent::fixed(PixelSize::new(3840, 2160).unwrap()),
            DisplayConstraints {
                max_texture_dimension: Some(2048),
                frame_budget_bytes: Some(2048 * 2048 * 4),
                ..Default::default()
            },
        )
        .unwrap();
        let size = plan.remote_size.unwrap();
        assert!(size.width <= 2048 && size.height <= 2048);
        assert!(u64::from(size.width) * u64::from(size.height) * 4 <= 2048 * 2048 * 4);
    }

    #[test]
    fn malformed_geometry_or_fixed_size_does_not_produce_zero() {
        assert!(DisplayGeometry::new(
            0.0,
            1.0,
            PixelSize::new(1, 1).unwrap(),
            PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            1000,
            false,
        )
        .is_none());
        let plan = DisplayPlanner::plan(
            DisplayIntent {
                mode: ResolutionMode::Fixed(PixelSize {
                    width: 0,
                    height: 1,
                }),
                geometry: None,
            },
            DisplayConstraints::default(),
        )
        .unwrap();
        assert_eq!(plan.remote_size, None);
        assert_eq!(plan.reason, DisplayPlanReason::NoUsableSize);
    }
}
