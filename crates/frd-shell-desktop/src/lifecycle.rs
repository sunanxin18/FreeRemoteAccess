use frd_compositor_wgpu::PresentError;
use frd_core::PixelSize;
use frd_render_wgpu::{GpuFaultClass, RendererError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OcclusionAction {
    None,
    Pause,
    ResumeAndRedraw,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PresentFailureAction {
    RecoverGpu,
    Fatal,
}

pub(crate) struct PresentationLifecycle {
    committed_size: PixelSize,
    occluded: bool,
    destroyed: bool,
}

impl PresentationLifecycle {
    pub(crate) fn new(committed_size: PixelSize) -> Self {
        Self {
            committed_size,
            occluded: false,
            destroyed: false,
        }
    }

    pub(crate) fn committed_size(&self) -> PixelSize {
        self.committed_size
    }

    pub(crate) fn finish_resize(
        &mut self,
        requested: PixelSize,
        result: Result<(), PresentError>,
    ) -> bool {
        if self.destroyed || result.is_err() {
            return false;
        }
        self.committed_size = requested;
        true
    }

    pub(crate) fn set_occluded(&mut self, occluded: bool) -> OcclusionAction {
        if self.destroyed || self.occluded == occluded {
            return OcclusionAction::None;
        }
        self.occluded = occluded;
        if occluded {
            OcclusionAction::Pause
        } else {
            OcclusionAction::ResumeAndRedraw
        }
    }

    pub(crate) fn destroy(&mut self) -> bool {
        if self.destroyed {
            return false;
        }
        self.destroyed = true;
        self.occluded = true;
        true
    }

    pub(crate) fn accepts_redraw(&self) -> bool {
        !self.destroyed && !self.occluded
    }

    pub(crate) fn classify_present_error(error: PresentError) -> PresentFailureAction {
        match error {
            PresentError::ContextMismatch => PresentFailureAction::RecoverGpu,
            PresentError::GpuFault(fault)
            | PresentError::Renderer(RendererError::GpuFault(fault))
                if recoverable_gpu_fault(fault) =>
            {
                PresentFailureAction::RecoverGpu
            }
            _ => PresentFailureAction::Fatal,
        }
    }
}

fn recoverable_gpu_fault(fault: GpuFaultClass) -> bool {
    matches!(
        fault,
        GpuFaultClass::OutOfMemory
            | GpuFaultClass::Internal
            | GpuFaultClass::DeviceLost
            | GpuFaultClass::ObservationIncomplete
    )
}

#[cfg(test)]
mod tests {
    use frd_compositor_wgpu::PresentError;
    use frd_core::PixelSize;
    use frd_render_wgpu::GpuFaultClass;

    use super::{OcclusionAction, PresentFailureAction, PresentationLifecycle};

    #[test]
    fn failed_resize_keeps_the_committed_drawable_and_suppresses_viewport_commit() {
        let initial = PixelSize::new(800, 600).unwrap();
        let requested = PixelSize::new(1200, 700).unwrap();
        let mut lifecycle = PresentationLifecycle::new(initial);

        assert!(!lifecycle.finish_resize(requested, Err(PresentError::SurfaceDetached)));
        assert_eq!(lifecycle.committed_size(), initial);
        assert!(lifecycle.finish_resize(requested, Ok(())));
        assert_eq!(lifecycle.committed_size(), requested);
    }

    #[test]
    fn occlusion_pauses_presentation_and_resume_requests_one_reconfigure_redraw() {
        let mut lifecycle = PresentationLifecycle::new(PixelSize::new(800, 600).unwrap());

        assert_eq!(lifecycle.set_occluded(true), OcclusionAction::Pause);
        assert!(!lifecycle.accepts_redraw());
        assert_eq!(
            lifecycle.set_occluded(false),
            OcclusionAction::ResumeAndRedraw
        );
        assert!(lifecycle.accepts_redraw());
        assert_eq!(lifecycle.set_occluded(false), OcclusionAction::None);
    }

    #[test]
    fn destroy_rejects_pending_wakes_and_is_idempotent() {
        let mut lifecycle = PresentationLifecycle::new(PixelSize::new(800, 600).unwrap());

        assert!(lifecycle.destroy());
        assert!(!lifecycle.accepts_redraw());
        assert!(!lifecycle.destroy());
        assert_eq!(lifecycle.set_occluded(false), OcclusionAction::None);
    }

    #[test]
    fn present_faults_select_recovery_only_for_recoverable_gpu_classes() {
        assert_eq!(
            PresentationLifecycle::classify_present_error(PresentError::GpuFault(
                GpuFaultClass::DeviceLost,
            )),
            PresentFailureAction::RecoverGpu
        );
        assert_eq!(
            PresentationLifecycle::classify_present_error(PresentError::ContextMismatch),
            PresentFailureAction::RecoverGpu
        );
        assert_eq!(
            PresentationLifecycle::classify_present_error(PresentError::GpuFault(
                GpuFaultClass::Validation,
            )),
            PresentFailureAction::Fatal
        );
        assert_eq!(
            PresentationLifecycle::classify_present_error(PresentError::SurfaceDetached),
            PresentFailureAction::Fatal
        );
    }
}
