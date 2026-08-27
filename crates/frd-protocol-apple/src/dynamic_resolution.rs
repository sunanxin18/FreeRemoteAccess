/// 动态分辨率控制器接受的物理显示尺寸。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplaySize {
    pub width: u16,
    pub height: u16,
}

impl DisplaySize {
    pub fn new(width: u16, height: u16) -> Option<Self> {
        (width != 0 && height != 0).then_some(Self { width, height })
    }

    /// 将查看器视口转换为当前支持的请求尺寸。
    pub fn from_viewport(width: usize, height: usize) -> Option<Self> {
        const MINIMUM: usize = 64;
        const ALIGNMENT: usize = 8;

        if width < MINIMUM || height < MINIMUM {
            return None;
        }

        let width = width.min(u16::MAX as usize) / ALIGNMENT * ALIGNMENT;
        let height = height.min(u16::MAX as usize) / ALIGNMENT * ALIGNMENT;
        Self::new(width as u16, height as u16)
    }
}

/// Apple 客户端中动态分辨率可用前必须满足的证据门槛。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicResolutionCapability {
    pub avc_stream: bool,
    pub display_configuration_accepted: bool,
    pub controlling: bool,
    pub paused: bool,
}

impl DynamicResolutionCapability {
    pub const fn new(
        avc_stream: bool,
        display_configuration_accepted: bool,
        controlling: bool,
        paused: bool,
    ) -> Self {
        Self {
            avc_stream,
            display_configuration_accepted,
            controlling,
            paused,
        }
    }

    pub const fn is_available(self) -> bool {
        self.avc_stream && self.display_configuration_accepted && self.controlling && !self.paused
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DynamicResolutionState {
    Unavailable,
    Disabled {
        stable: DisplaySize,
    },
    Stable {
        generation: u64,
        size: DisplaySize,
    },
    Pending {
        generation: u64,
        previous: DisplaySize,
        target: DisplaySize,
    },
    Switching {
        generation: u64,
        previous: DisplaySize,
        target: DisplaySize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolutionRequest {
    pub generation: u64,
    pub target: DisplaySize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GeometryCommit {
    pub generation: u64,
    pub size: DisplaySize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableDisplay {
    generation: u64,
    size: DisplaySize,
}

/// 纯粹的、由确认驱动的动态分辨率状态机。
pub struct DynamicResolutionController {
    stable: StableDisplay,
    state: DynamicResolutionState,
}

impl DynamicResolutionController {
    pub fn new(
        initial_size: DisplaySize,
        enabled: bool,
        capability: DynamicResolutionCapability,
    ) -> Self {
        let stable = StableDisplay {
            generation: 0,
            size: initial_size,
        };
        let state = if !capability.is_available() {
            DynamicResolutionState::Unavailable
        } else if !enabled {
            DynamicResolutionState::Disabled {
                stable: initial_size,
            }
        } else {
            DynamicResolutionState::Stable {
                generation: stable.generation,
                size: stable.size,
            }
        };

        Self { stable, state }
    }

    pub fn state(&self) -> &DynamicResolutionState {
        &self.state
    }

    pub fn request_target(&mut self, target: DisplaySize) -> Option<ResolutionRequest> {
        if !matches!(self.state, DynamicResolutionState::Stable { .. })
            || target == self.stable.size
        {
            return None;
        }

        let generation = self.stable.generation.checked_add(1)?;
        self.state = DynamicResolutionState::Pending {
            generation,
            previous: self.stable.size,
            target,
        };
        Some(ResolutionRequest { generation, target })
    }

    pub fn observe_server_state(&mut self, size: DisplaySize) -> Option<GeometryCommit> {
        let (generation, previous, target) = match self.state {
            DynamicResolutionState::Pending {
                generation,
                previous,
                target,
            } if target == size => (generation, previous, target),
            _ => return None,
        };

        self.stable = StableDisplay {
            generation,
            size: target,
        };
        self.state = DynamicResolutionState::Switching {
            generation,
            previous,
            target,
        };
        Some(GeometryCommit {
            generation,
            size: target,
        })
    }

    pub fn mark_full_frame(&mut self, generation: u64) -> bool {
        let matches_generation = matches!(
            self.state,
            DynamicResolutionState::Switching {
                generation: switching_generation,
                ..
            } if switching_generation == generation
        );
        if !matches_generation {
            return false;
        }

        self.state = DynamicResolutionState::Stable {
            generation: self.stable.generation,
            size: self.stable.size,
        };
        true
    }

    pub fn timeout_pending(&mut self) -> bool {
        if !matches!(self.state, DynamicResolutionState::Pending { .. }) {
            return false;
        }

        self.state = DynamicResolutionState::Stable {
            generation: self.stable.generation,
            size: self.stable.size,
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DisplaySize, DynamicResolutionCapability, DynamicResolutionController,
        DynamicResolutionState,
    };

    fn enabled_controller(size: DisplaySize) -> DynamicResolutionController {
        DynamicResolutionController::new(
            size,
            true,
            DynamicResolutionCapability::new(true, true, true, false),
        )
    }

    #[test]
    fn matching_ack_commits_exactly_one_generation() {
        let mut controller = enabled_controller(DisplaySize::new(1440, 2560).unwrap());
        let target = DisplaySize::new(1280, 720).unwrap();
        let request = controller.request_target(target).unwrap();
        assert_eq!(request.generation, 1);
        assert_eq!(request.target, target);

        assert!(controller
            .observe_server_state(DisplaySize::new(1024, 768).unwrap())
            .is_none());
        let commit = controller.observe_server_state(target).unwrap();
        assert_eq!(commit.generation, 1);
        assert_eq!(commit.size, target);
        assert!(controller.observe_server_state(target).is_none());
        assert_eq!(
            controller.state(),
            &DynamicResolutionState::Switching {
                generation: 1,
                previous: DisplaySize::new(1440, 2560).unwrap(),
                target,
            }
        );
    }

    #[test]
    fn unavailable_gate_combinations_never_create_requests() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        for capability in [
            DynamicResolutionCapability::new(false, true, true, false),
            DynamicResolutionCapability::new(true, false, true, false),
            DynamicResolutionCapability::new(true, true, false, false),
            DynamicResolutionCapability::new(true, true, true, true),
        ] {
            let mut controller = DynamicResolutionController::new(initial, true, capability);
            assert_eq!(controller.state(), &DynamicResolutionState::Unavailable);
            assert!(controller.request_target(target).is_none());
        }
    }

    #[test]
    fn disabled_mode_keeps_initial_surface_and_ignores_requests() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let mut controller = DynamicResolutionController::new(
            initial,
            false,
            DynamicResolutionCapability::new(true, true, true, false),
        );

        assert_eq!(
            controller.state(),
            &DynamicResolutionState::Disabled { stable: initial }
        );
        assert!(controller
            .request_target(DisplaySize::new(1280, 720).unwrap())
            .is_none());
    }

    #[test]
    fn duplicate_target_and_second_request_do_not_replace_pending_request() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let other = DisplaySize::new(1024, 768).unwrap();
        let mut controller = enabled_controller(initial);

        assert_eq!(controller.request_target(initial), None);
        let request = controller.request_target(target).unwrap();
        assert_eq!(request.generation, 1);
        assert!(controller.request_target(target).is_none());
        assert!(controller.request_target(other).is_none());
        assert_eq!(
            controller.state(),
            &DynamicResolutionState::Pending {
                generation: 1,
                previous: initial,
                target,
            }
        );
    }

    #[test]
    fn viewport_sizes_are_minimum_checked_clamped_and_eight_pixel_aligned() {
        assert_eq!(DisplaySize::from_viewport(63, 64), None);
        assert_eq!(DisplaySize::from_viewport(64, 63), None);
        assert_eq!(
            DisplaySize::from_viewport(79, 71),
            Some(DisplaySize::new(72, 64).unwrap())
        );
        assert_eq!(
            DisplaySize::from_viewport(70_000, 65_535),
            Some(DisplaySize::new(65_528, 65_528).unwrap())
        );
    }

    #[test]
    fn timeout_rolls_back_without_advancing_stable_generation() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let mut controller = enabled_controller(initial);
        controller.request_target(target).unwrap();

        assert!(controller.timeout_pending());
        assert_eq!(
            controller.state(),
            &DynamicResolutionState::Stable {
                generation: 0,
                size: initial,
            }
        );
        assert_eq!(controller.request_target(target).unwrap().generation, 1);
    }

    #[test]
    fn only_matching_generation_full_frame_completes_switching() {
        let initial = DisplaySize::new(1440, 900).unwrap();
        let target = DisplaySize::new(1280, 720).unwrap();
        let mut controller = enabled_controller(initial);
        controller.request_target(target).unwrap();
        controller.observe_server_state(target).unwrap();

        assert!(!controller.mark_full_frame(0));
        assert!(matches!(
            controller.state(),
            DynamicResolutionState::Switching { .. }
        ));
        assert!(controller.mark_full_frame(1));
        assert_eq!(
            controller.state(),
            &DynamicResolutionState::Stable {
                generation: 1,
                size: target,
            }
        );
    }
}
