use frd_core::PixelSize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SurfaceSizeAction {
    Pause,
    Configure(PixelSize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SurfaceSizeState {
    active: Option<PixelSize>,
}

impl SurfaceSizeState {
    pub(crate) fn new(size: PixelSize) -> Option<Self> {
        is_nonzero(size).then_some(Self { active: Some(size) })
    }

    pub(crate) fn resize(&mut self, size: PixelSize) -> SurfaceSizeAction {
        if is_nonzero(size) {
            self.active = Some(size);
            SurfaceSizeAction::Configure(size)
        } else {
            self.active = None;
            SurfaceSizeAction::Pause
        }
    }

    pub(crate) fn pause(&mut self) {
        self.active = None;
    }

    pub(crate) fn active(&self) -> Option<PixelSize> {
        self.active
    }
}

fn is_nonzero(size: PixelSize) -> bool {
    size.width != 0 && size.height != 0
}

pub(crate) struct ContextPairState<T> {
    compositor: T,
    renderer: T,
}

impl<T: Copy + Eq> ContextPairState<T> {
    pub(crate) fn new(compositor: T, renderer: T) -> Self {
        Self {
            compositor,
            renderer,
        }
    }

    pub(crate) fn matches(&self) -> bool {
        self.compositor == self.renderer
    }

    pub(crate) fn install_recovery(&mut self, context: T) {
        self.compositor = context;
        self.renderer = context;
    }

    #[cfg(test)]
    pub(crate) fn compositor(&self) -> T {
        self.compositor
    }

    #[cfg(test)]
    pub(crate) fn renderer(&self) -> T {
        self.renderer
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AcquisitionAction {
    Render,
    RenderThenReconfigure,
    Reconfigure,
    RecreateSurface,
    Skip,
    ValidationError,
}

pub(crate) enum AcquiredFrame<T> {
    Success(T),
    Suboptimal(T),
    Timeout,
    Occluded,
    Outdated,
    Lost,
    Validation,
}

impl<T> AcquiredFrame<T> {
    pub(crate) fn action(&self) -> AcquisitionAction {
        match self {
            Self::Success(_) => AcquisitionAction::Render,
            Self::Suboptimal(_) => AcquisitionAction::RenderThenReconfigure,
            Self::Outdated => AcquisitionAction::Reconfigure,
            Self::Lost => AcquisitionAction::RecreateSurface,
            Self::Timeout | Self::Occluded => AcquisitionAction::Skip,
            Self::Validation => AcquisitionAction::ValidationError,
        }
    }
}

impl From<wgpu::CurrentSurfaceTexture> for AcquiredFrame<wgpu::SurfaceTexture> {
    fn from(value: wgpu::CurrentSurfaceTexture) -> Self {
        match value {
            wgpu::CurrentSurfaceTexture::Success(texture) => Self::Success(texture),
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => Self::Suboptimal(texture),
            wgpu::CurrentSurfaceTexture::Timeout => Self::Timeout,
            wgpu::CurrentSurfaceTexture::Occluded => Self::Occluded,
            wgpu::CurrentSurfaceTexture::Outdated => Self::Outdated,
            wgpu::CurrentSurfaceTexture::Lost => Self::Lost,
            wgpu::CurrentSurfaceTexture::Validation => Self::Validation,
        }
    }
}

pub(crate) struct OwnedSurfaceAndLease<S, L> {
    surface: Option<S>,
    lease: Option<L>,
}

impl<S, L> OwnedSurfaceAndLease<S, L> {
    pub(crate) fn new(surface: S, lease: L) -> Self {
        Self {
            surface: Some(surface),
            lease: Some(lease),
        }
    }

    pub(crate) fn detach(&mut self) {
        drop(self.surface.take());
        drop(self.lease.take());
    }

    pub(crate) fn surface(&self) -> Option<&S> {
        self.surface.as_ref()
    }

    pub(crate) fn lease(&self) -> Option<&L> {
        self.lease.as_ref()
    }

    pub(crate) fn replace_surface(&mut self, surface: S) -> Option<S> {
        self.surface.replace(surface)
    }
}

impl<S, L> Drop for OwnedSurfaceAndLease<S, L> {
    fn drop(&mut self) {
        self.detach();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{
        AcquiredFrame, AcquisitionAction, ContextPairState, OwnedSurfaceAndLease,
        SurfaceSizeAction, SurfaceSizeState,
    };

    #[test]
    fn wgpu_30_acquisition_actions_match_the_recovery_contract() {
        assert_eq!(
            AcquiredFrame::Success(()).action(),
            AcquisitionAction::Render
        );
        assert_eq!(
            AcquiredFrame::Suboptimal(()).action(),
            AcquisitionAction::RenderThenReconfigure
        );
        assert_eq!(
            AcquiredFrame::<()>::Outdated.action(),
            AcquisitionAction::Reconfigure
        );
        assert_eq!(
            AcquiredFrame::<()>::Lost.action(),
            AcquisitionAction::RecreateSurface
        );
        assert_eq!(
            AcquiredFrame::<()>::Timeout.action(),
            AcquisitionAction::Skip
        );
        assert_eq!(
            AcquiredFrame::<()>::Occluded.action(),
            AcquisitionAction::Skip
        );
        assert_eq!(
            AcquiredFrame::<()>::Validation.action(),
            AcquisitionAction::ValidationError
        );
    }

    #[derive(Clone)]
    struct DropObserver {
        name: &'static str,
        log: Rc<RefCell<Vec<&'static str>>>,
    }

    impl Drop for DropObserver {
        fn drop(&mut self) {
            self.log.borrow_mut().push(self.name);
        }
    }

    #[test]
    fn detach_and_drop_destroy_surface_before_releasing_owned_lease() {
        let explicit_log = Rc::new(RefCell::new(Vec::new()));
        let mut explicit = OwnedSurfaceAndLease::new(
            DropObserver {
                name: "surface",
                log: explicit_log.clone(),
            },
            DropObserver {
                name: "lease",
                log: explicit_log.clone(),
            },
        );
        explicit.detach();
        assert_eq!(&*explicit_log.borrow(), &["surface", "lease"]);

        let drop_log = Rc::new(RefCell::new(Vec::new()));
        {
            let _implicit = OwnedSurfaceAndLease::new(
                DropObserver {
                    name: "surface",
                    log: drop_log.clone(),
                },
                DropObserver {
                    name: "lease",
                    log: drop_log.clone(),
                },
            );
        }
        assert_eq!(&*drop_log.borrow(), &["surface", "lease"]);
    }

    #[test]
    fn initial_surface_size_rejects_every_publicly_constructible_zero_axis() {
        for size in [
            frd_core::PixelSize {
                width: 0,
                height: 1,
            },
            frd_core::PixelSize {
                width: 1,
                height: 0,
            },
            frd_core::PixelSize {
                width: 0,
                height: 0,
            },
        ] {
            assert_eq!(SurfaceSizeState::new(size), None);
        }
    }

    #[test]
    fn zero_resize_pauses_without_configure_and_nonzero_resize_resumes_configuration() {
        let initial = frd_core::PixelSize {
            width: 800,
            height: 600,
        };
        let resumed = frd_core::PixelSize {
            width: 1024,
            height: 768,
        };
        let mut state = SurfaceSizeState::new(initial).expect("初始尺寸非零");

        assert_eq!(
            state.resize(frd_core::PixelSize {
                width: 0,
                height: 600,
            }),
            SurfaceSizeAction::Pause
        );
        assert_eq!(state.active(), None);
        assert_eq!(
            state.resize(frd_core::PixelSize {
                width: 800,
                height: 0,
            }),
            SurfaceSizeAction::Pause
        );
        assert_eq!(state.active(), None);
        assert_eq!(state.resize(resumed), SurfaceSizeAction::Configure(resumed));
        assert_eq!(state.active(), Some(resumed));
    }

    #[test]
    fn context_pair_rejects_mismatch_and_coordinated_recovery_installs_one_identity() {
        let mut pair = ContextPairState::new("compositor-old", "renderer-other");

        assert!(!pair.matches());
        pair.install_recovery("shared-new");
        assert!(pair.matches());
        assert_eq!(pair.compositor(), "shared-new");
        assert_eq!(pair.renderer(), "shared-new");
    }
}
