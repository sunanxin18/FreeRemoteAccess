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
        }
    }

    pub const fn mode(&self) -> WindowPresentationMode {
        self.mode
    }

    pub const fn last_remote_extent(&self) -> LogicalWindowExtent {
        self.last_remote_extent
    }

    pub fn observe_first_complete_remote_frame(&mut self) -> Option<WindowPresentationTransition> {
        if self.mode != WindowPresentationMode::CompactLocal {
            return None;
        }

        self.mode = WindowPresentationMode::RemoteDesktop;
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
