#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LogicalWindowExtent {
    pub width: f64,
    pub height: f64,
}

impl LogicalWindowExtent {
    pub const fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowPresentationMode {
    CompactLocal,
    RemoteDesktop,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowPresentationTransition {
    CompactLocal { extent: LogicalWindowExtent },
    RemoteDesktop { extent: LogicalWindowExtent },
}

pub struct WindowPresentationController {
    mode: WindowPresentationMode,
    compact_extent: LogicalWindowExtent,
    last_remote_extent: LogicalWindowExtent,
    has_presented_remote: bool,
}

impl WindowPresentationController {
    pub const fn new(
        compact_extent: LogicalWindowExtent,
        initial_remote_extent: LogicalWindowExtent,
    ) -> Self {
        Self {
            mode: WindowPresentationMode::CompactLocal,
            compact_extent,
            last_remote_extent: initial_remote_extent,
            has_presented_remote: false,
        }
    }

    pub const fn mode(&self) -> WindowPresentationMode {
        self.mode
    }

    pub const fn last_remote_extent(&self) -> LogicalWindowExtent {
        self.last_remote_extent
    }

    /// 仅首次完整帧之前接受平台计算的初始尺寸；重连保留用户窗口尺寸。
    pub fn set_initial_remote_extent(&mut self, extent: LogicalWindowExtent) {
        if !self.has_presented_remote && valid_extent(extent) {
            self.last_remote_extent = extent;
        }
    }

    pub fn observe_first_complete_remote_frame(&mut self) -> Option<WindowPresentationTransition> {
        if self.mode != WindowPresentationMode::CompactLocal {
            return None;
        }

        self.mode = WindowPresentationMode::RemoteDesktop;
        self.has_presented_remote = true;
        Some(WindowPresentationTransition::RemoteDesktop {
            extent: self.last_remote_extent,
        })
    }

    pub fn observe_cleanup_returned_local(&mut self) -> Option<WindowPresentationTransition> {
        if self.mode != WindowPresentationMode::RemoteDesktop {
            return None;
        }

        self.mode = WindowPresentationMode::CompactLocal;
        Some(WindowPresentationTransition::CompactLocal {
            extent: self.compact_extent,
        })
    }

    pub fn record_user_resize(&mut self, extent: LogicalWindowExtent) {
        if self.mode != WindowPresentationMode::RemoteDesktop
            || !extent.width.is_finite()
            || !extent.height.is_finite()
            || extent.width <= 0.0
            || extent.height <= 0.0
        {
            return;
        }

        self.last_remote_extent = extent;
    }
}

fn valid_extent(extent: LogicalWindowExtent) -> bool {
    extent.width.is_finite()
        && extent.height.is_finite()
        && extent.width > 0.0
        && extent.height > 0.0
}

/// 按远程画面比例容纳内容，再加固定标题栏；不改变协议请求的分辨率。
#[cfg(any(test, target_os = "macos"))]
pub(crate) fn fit_remote_window(
    available: LogicalWindowExtent,
    remote: LogicalWindowExtent,
    chrome_height: f64,
) -> Option<LogicalWindowExtent> {
    if !valid_extent(available)
        || !valid_extent(remote)
        || !chrome_height.is_finite()
        || chrome_height < 0.0
        || chrome_height >= available.height
    {
        return None;
    }
    let scale =
        (available.width / remote.width).min((available.height - chrome_height) / remote.height);
    let extent =
        LogicalWindowExtent::new(remote.width * scale, remote.height * scale + chrome_height);
    (valid_extent(extent) && extent.height > chrome_height).then_some(extent)
}

#[cfg(test)]
mod tests {
    use super::{
        LogicalWindowExtent, WindowPresentationController, WindowPresentationMode,
        WindowPresentationTransition,
    };

    fn compact_extent() -> LogicalWindowExtent {
        LogicalWindowExtent::new(520.0, 600.0)
    }

    fn connected_controller() -> WindowPresentationController {
        let mut controller = WindowPresentationController::new(
            compact_extent(),
            LogicalWindowExtent::new(1100.0, 720.0),
        );
        assert_eq!(
            controller.observe_first_complete_remote_frame(),
            Some(WindowPresentationTransition::RemoteDesktop {
                extent: LogicalWindowExtent::new(1100.0, 720.0),
            })
        );
        controller
    }

    #[test]
    fn stays_compact_until_first_complete_remote_frame() {
        let mut controller = WindowPresentationController::new(
            compact_extent(),
            LogicalWindowExtent::new(1100.0, 720.0),
        );
        assert_eq!(controller.mode(), WindowPresentationMode::CompactLocal);
        controller.record_user_resize(LogicalWindowExtent::new(640.0, 480.0));
        assert_eq!(controller.mode(), WindowPresentationMode::CompactLocal);
        assert_eq!(
            controller.observe_first_complete_remote_frame(),
            Some(WindowPresentationTransition::RemoteDesktop {
                extent: LogicalWindowExtent::new(1100.0, 720.0),
            })
        );
    }

    #[test]
    fn cleanup_return_restores_compact_without_forgetting_remote_size() {
        let mut controller = connected_controller();
        controller.record_user_resize(LogicalWindowExtent::new(1440.0, 900.0));
        assert_eq!(
            controller.observe_cleanup_returned_local(),
            Some(WindowPresentationTransition::CompactLocal {
                extent: compact_extent(),
            })
        );
        assert_eq!(
            controller.last_remote_extent(),
            LogicalWindowExtent::new(1440.0, 900.0)
        );
        assert_eq!(controller.observe_cleanup_returned_local(), None);
    }

    #[test]
    fn compact_resize_does_not_overwrite_remembered_remote_size() {
        let mut controller = WindowPresentationController::new(
            compact_extent(),
            LogicalWindowExtent::new(1100.0, 720.0),
        );
        controller.record_user_resize(LogicalWindowExtent::new(480.0, 560.0));
        assert_eq!(
            controller.last_remote_extent(),
            LogicalWindowExtent::new(1100.0, 720.0)
        );
    }

    #[test]
    fn invalid_remote_resize_is_ignored() {
        let mut controller = connected_controller();
        let original = controller.last_remote_extent();
        controller.record_user_resize(LogicalWindowExtent::new(0.0, 900.0));
        controller.record_user_resize(LogicalWindowExtent::new(1440.0, -1.0));
        controller.record_user_resize(LogicalWindowExtent::new(f64::NAN, 900.0));
        controller.record_user_resize(LogicalWindowExtent::new(1440.0, f64::INFINITY));
        assert_eq!(controller.last_remote_extent(), original);
    }

    #[test]
    fn first_complete_frame_and_cleanup_each_transition_once() {
        let mut controller = WindowPresentationController::new(
            compact_extent(),
            LogicalWindowExtent::new(1100.0, 720.0),
        );
        assert!(controller.observe_first_complete_remote_frame().is_some());
        assert_eq!(controller.observe_first_complete_remote_frame(), None);
        assert!(controller.observe_cleanup_returned_local().is_some());
        assert_eq!(controller.observe_cleanup_returned_local(), None);
    }
}

#[cfg(test)]
mod fit_tests {
    use super::*;
    #[test]
    fn initial_size_can_only_change_before_first_session() {
        let mut c = WindowPresentationController::new(
            LogicalWindowExtent::new(520.0, 600.0),
            LogicalWindowExtent::new(1100.0, 720.0),
        );
        c.set_initial_remote_extent(LogicalWindowExtent::new(1200.0, 800.0));
        c.observe_first_complete_remote_frame();
        c.record_user_resize(LogicalWindowExtent::new(900.0, 700.0));
        c.observe_cleanup_returned_local();
        c.set_initial_remote_extent(LogicalWindowExtent::new(1200.0, 800.0));
        assert_eq!(
            c.last_remote_extent(),
            LogicalWindowExtent::new(900.0, 700.0)
        );
    }
    #[test]
    fn remote_aspect_fits_work_area_with_fixed_chrome_at_any_scale() {
        for (rw, rh) in [(2560.0, 1440.0), (1440.0, 2560.0), (3840.0, 2160.0)] {
            for dpi in [1.0, 2.0] {
                let available = LogicalWindowExtent::new(2560.0 / dpi, 1380.0 / dpi);
                let result =
                    fit_remote_window(available, LogicalWindowExtent::new(rw, rh), 52.0).unwrap();
                assert!(
                    result.width <= available.width + 1e-6
                        && result.height <= available.height + 1e-6
                );
                assert!((result.width / (result.height - 52.0) - rw / rh).abs() < 1e-10);
            }
        }
    }
    #[test]
    fn finite_geometry_that_overflows_or_underflows_is_rejected() {
        for (available, remote) in [(1e308, 1e-308), (1e-308, 1e308)] {
            assert!(fit_remote_window(
                LogicalWindowExtent::new(available, available),
                LogicalWindowExtent::new(remote, remote),
                0.0,
            )
            .is_none());
        }
    }

    #[test]
    fn invalid_geometry_is_rejected() {
        for h in [f64::NAN, -1.0, 600.0, 700.0] {
            assert!(fit_remote_window(
                LogicalWindowExtent::new(800.0, 600.0),
                LogicalWindowExtent::new(1920.0, 1080.0),
                h
            )
            .is_none());
        }
    }
}
