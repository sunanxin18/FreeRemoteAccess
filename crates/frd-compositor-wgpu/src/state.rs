use frd_protocol_api::PresentationEvent;
use frd_render_wgpu::PresentationReceipt;

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

pub(crate) fn acknowledge_after_present(
    receipt: Option<PresentationReceipt>,
    presented: bool,
) -> Option<PresentationEvent> {
    presented
        .then_some(receipt)
        .flatten()
        .map(|receipt| PresentationEvent::FramePresented {
            session_id: receipt.session_id,
            generation: receipt.generation,
            revision: receipt.revision,
            completeness: receipt.completeness,
        })
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

    pub(crate) fn drop_surface(&mut self) {
        drop(self.surface.take());
    }

    pub(crate) fn replace_surface(&mut self, surface: S) {
        debug_assert!(self.surface.is_none());
        self.surface = Some(surface);
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

    use frd_core::SessionId;
    use frd_frame::FrameCompleteness;
    use frd_protocol_api::PresentationEvent;

    use super::{
        acknowledge_after_present, AcquiredFrame, AcquisitionAction, OwnedSurfaceAndLease,
    };
    use frd_render_wgpu::PresentationReceipt;

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

    #[test]
    fn acknowledgement_exists_only_after_successful_present() {
        let receipt = PresentationReceipt {
            session_id: SessionId::allocate(),
            generation: 3,
            revision: 5,
            completeness: FrameCompleteness::FullBaseline,
        };

        assert_eq!(acknowledge_after_present(Some(receipt), false), None);
        assert_eq!(
            acknowledge_after_present(Some(receipt), true),
            Some(PresentationEvent::FramePresented {
                session_id: receipt.session_id,
                generation: 3,
                revision: 5,
                completeness: FrameCompleteness::FullBaseline,
            })
        );
        assert_eq!(acknowledge_after_present(None, true), None);
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
}
