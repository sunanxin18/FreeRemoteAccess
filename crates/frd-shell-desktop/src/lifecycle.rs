use frd_compositor_wgpu::PresentError;
use frd_core::PixelSize;
use frd_render_wgpu::{GpuFaultClass, RecoveryRequirement, RendererError};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentationOperation {
    Redraw,
    Resize,
    OcclusionResume,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PresentationRecoveryContext {
    Redraw,
    Resize { requested: PixelSize },
    OcclusionResume { committed: PixelSize },
}

impl PresentationRecoveryContext {
    pub(crate) fn operation(self) -> PresentationOperation {
        match self {
            Self::Redraw => PresentationOperation::Redraw,
            Self::Resize { .. } => PresentationOperation::Resize,
            Self::OcclusionResume { .. } => PresentationOperation::OcclusionResume,
        }
    }

    fn configure_size(self) -> Option<PixelSize> {
        match self {
            Self::Redraw => None,
            Self::Resize { requested } => Some(requested),
            Self::OcclusionResume { committed } => Some(committed),
        }
    }
}

pub(crate) trait PresentationRecoveryBackend {
    fn recover_gpu(&mut self) -> Result<(), PresentError>;
    fn configure(&mut self, size: PixelSize) -> Result<(), PresentError>;
    fn finish_gpu_recovery(&mut self) -> Result<Option<RecoveryRequirement>, PresentError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PresentationRecoverySuccess {
    context: PresentationRecoveryContext,
    pub(crate) requirement: Option<RecoveryRequirement>,
}

impl PresentationRecoverySuccess {
    pub(crate) fn geometry_commit(self) -> Option<PixelSize> {
        match self.context {
            PresentationRecoveryContext::Resize { requested } => Some(requested),
            PresentationRecoveryContext::Redraw
            | PresentationRecoveryContext::OcclusionResume { .. } => None,
        }
    }

    pub(crate) fn publish_viewport(self) -> bool {
        matches!(self.context, PresentationRecoveryContext::Resize { .. })
    }

    pub(crate) fn request_redraw(self) -> bool {
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PresentationRecoveryFailure {
    pub(crate) operation: PresentationOperation,
    pub(crate) source: PresentError,
    pub(crate) retry: Option<PresentError>,
    pub(crate) recovery: Option<PresentError>,
}

pub(crate) fn execute_presentation_recovery(
    context: PresentationRecoveryContext,
    source: PresentError,
    backend: &mut impl PresentationRecoveryBackend,
) -> Result<PresentationRecoverySuccess, PresentationRecoveryFailure> {
    let operation = context.operation();
    if PresentationLifecycle::classify_present_error(operation, source)
        == PresentFailureAction::Fatal
    {
        return Err(PresentationRecoveryFailure {
            operation,
            source,
            retry: None,
            recovery: None,
        });
    }
    backend
        .recover_gpu()
        .map_err(|recovery| PresentationRecoveryFailure {
            operation,
            source,
            retry: None,
            recovery: Some(recovery),
        })?;
    if let Some(size) = context.configure_size() {
        backend
            .configure(size)
            .map_err(|retry| PresentationRecoveryFailure {
                operation,
                source,
                retry: Some(retry),
                recovery: None,
            })?;
    }
    let requirement =
        backend
            .finish_gpu_recovery()
            .map_err(|recovery| PresentationRecoveryFailure {
                operation,
                source,
                retry: None,
                recovery: Some(recovery),
            })?;
    Ok(PresentationRecoverySuccess {
        context,
        requirement,
    })
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
    ) -> Result<(), PresentError> {
        result?;
        if self.destroyed {
            return Err(PresentError::SurfaceDetached);
        }
        self.committed_size = requested;
        Ok(())
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

    pub(crate) fn classify_present_error(
        _operation: PresentationOperation,
        error: PresentError,
    ) -> PresentFailureAction {
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
    use frd_render_wgpu::{GpuFaultClass, RecoveryRequirement};

    use super::{
        execute_presentation_recovery, OcclusionAction, PresentFailureAction,
        PresentationLifecycle, PresentationOperation, PresentationRecoveryBackend,
        PresentationRecoveryContext,
    };

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum RecoveryCall {
        Recover,
        Configure(PixelSize),
        Finish,
    }

    struct InjectedRecovery {
        calls: Vec<RecoveryCall>,
        recover_error: Option<PresentError>,
        configure_error: Option<PresentError>,
        finish_error: Option<PresentError>,
    }

    impl InjectedRecovery {
        fn succeeds() -> Self {
            Self {
                calls: Vec::new(),
                recover_error: None,
                configure_error: None,
                finish_error: None,
            }
        }
    }

    impl PresentationRecoveryBackend for InjectedRecovery {
        fn recover_gpu(&mut self) -> Result<(), PresentError> {
            self.calls.push(RecoveryCall::Recover);
            self.recover_error.map_or(Ok(()), Err)
        }

        fn configure(&mut self, size: PixelSize) -> Result<(), PresentError> {
            self.calls.push(RecoveryCall::Configure(size));
            self.configure_error.map_or(Ok(()), Err)
        }

        fn finish_gpu_recovery(&mut self) -> Result<Option<RecoveryRequirement>, PresentError> {
            self.calls.push(RecoveryCall::Finish);
            self.finish_error.map_or(Ok(None), Err)
        }
    }

    #[test]
    fn failed_resize_keeps_the_committed_drawable_and_suppresses_viewport_commit() {
        let initial = PixelSize::new(800, 600).unwrap();
        let requested = PixelSize::new(1200, 700).unwrap();
        let mut lifecycle = PresentationLifecycle::new(initial);

        assert_eq!(
            lifecycle.finish_resize(requested, Err(PresentError::SurfaceDetached)),
            Err(PresentError::SurfaceDetached)
        );
        assert_eq!(lifecycle.committed_size(), initial);
        assert_eq!(lifecycle.finish_resize(requested, Ok(())), Ok(()));
        assert_eq!(lifecycle.committed_size(), requested);
    }

    #[test]
    fn recoverable_and_fatal_resize_errors_preserve_error_without_geometry_commit() {
        let initial = PixelSize::new(800, 600).unwrap();
        let requested = PixelSize::new(1200, 700).unwrap();
        let mut lifecycle = PresentationLifecycle::new(initial);
        let recoverable = PresentError::GpuFault(GpuFaultClass::DeviceLost);
        let fatal = PresentError::GpuFault(GpuFaultClass::Validation);

        assert_eq!(
            lifecycle.finish_resize(requested, Err(recoverable)),
            Err(recoverable)
        );
        assert_eq!(lifecycle.committed_size(), initial);
        assert_eq!(
            PresentationLifecycle::classify_present_error(
                PresentationOperation::Resize,
                recoverable,
            ),
            PresentFailureAction::RecoverGpu
        );
        assert_eq!(lifecycle.finish_resize(requested, Err(fatal)), Err(fatal));
        assert_eq!(lifecycle.committed_size(), initial);
        assert_eq!(
            PresentationLifecycle::classify_present_error(PresentationOperation::Resize, fatal),
            PresentFailureAction::Fatal
        );
    }

    #[test]
    fn recoverable_and_fatal_resume_errors_use_the_same_classifier_and_keep_the_error() {
        let recoverable = PresentError::GpuFault(GpuFaultClass::Internal);
        let fatal = PresentError::SurfaceDetached;

        assert_eq!(
            PresentationLifecycle::classify_present_error(
                PresentationOperation::OcclusionResume,
                recoverable,
            ),
            PresentFailureAction::RecoverGpu
        );
        assert_eq!(
            PresentationLifecycle::classify_present_error(
                PresentationOperation::OcclusionResume,
                fatal,
            ),
            PresentFailureAction::Fatal
        );
        assert_eq!(recoverable, PresentError::GpuFault(GpuFaultClass::Internal));
        assert_eq!(fatal, PresentError::SurfaceDetached);
    }

    #[test]
    fn recoverable_resize_retries_requested_geometry_before_viewport_commit_and_redraw() {
        let requested = PixelSize::new(1200, 700).unwrap();
        let source = PresentError::GpuFault(GpuFaultClass::DeviceLost);
        let mut backend = InjectedRecovery::succeeds();

        let success = execute_presentation_recovery(
            PresentationRecoveryContext::Resize { requested },
            source,
            &mut backend,
        )
        .expect("recoverable resize restores the requested drawable");

        assert_eq!(
            backend.calls,
            vec![
                RecoveryCall::Recover,
                RecoveryCall::Configure(requested),
                RecoveryCall::Finish,
            ]
        );
        assert_eq!(success.geometry_commit(), Some(requested));
        assert!(success.publish_viewport());
        assert!(success.request_redraw());
    }

    #[test]
    fn recoverable_resize_retry_failure_preserves_source_and_retry_without_committing() {
        let requested = PixelSize::new(1200, 700).unwrap();
        let source = PresentError::GpuFault(GpuFaultClass::DeviceLost);
        let retry = PresentError::GpuFault(GpuFaultClass::Validation);
        let mut backend = InjectedRecovery {
            configure_error: Some(retry),
            ..InjectedRecovery::succeeds()
        };

        let failure = execute_presentation_recovery(
            PresentationRecoveryContext::Resize { requested },
            source,
            &mut backend,
        )
        .expect_err("failed retry enters the typed fatal fallback");

        assert_eq!(
            backend.calls,
            vec![RecoveryCall::Recover, RecoveryCall::Configure(requested)]
        );
        assert_eq!(failure.operation, PresentationOperation::Resize);
        assert_eq!(failure.source, source);
        assert_eq!(failure.retry, Some(retry));
        assert_eq!(failure.recovery, None);
    }

    #[test]
    fn recoverable_resume_reactivates_committed_geometry_before_redraw() {
        let committed = PixelSize::new(800, 600).unwrap();
        let source = PresentError::GpuFault(GpuFaultClass::Internal);
        let mut backend = InjectedRecovery::succeeds();

        let success = execute_presentation_recovery(
            PresentationRecoveryContext::OcclusionResume { committed },
            source,
            &mut backend,
        )
        .expect("recoverable resume reactivates the paused surface");

        assert_eq!(
            backend.calls,
            vec![
                RecoveryCall::Recover,
                RecoveryCall::Configure(committed),
                RecoveryCall::Finish,
            ]
        );
        assert_eq!(success.geometry_commit(), None);
        assert!(!success.publish_viewport());
        assert!(success.request_redraw());
    }

    #[test]
    fn recoverable_resume_recovery_failure_preserves_source_and_fallback_error() {
        let committed = PixelSize::new(800, 600).unwrap();
        let source = PresentError::GpuFault(GpuFaultClass::Internal);
        let recovery = PresentError::SurfaceUnsupported;
        let mut backend = InjectedRecovery {
            recover_error: Some(recovery),
            ..InjectedRecovery::succeeds()
        };

        let failure = execute_presentation_recovery(
            PresentationRecoveryContext::OcclusionResume { committed },
            source,
            &mut backend,
        )
        .expect_err("failed recovery enters the typed fatal fallback");

        assert_eq!(backend.calls, vec![RecoveryCall::Recover]);
        assert_eq!(failure.operation, PresentationOperation::OcclusionResume);
        assert_eq!(failure.source, source);
        assert_eq!(failure.retry, None);
        assert_eq!(failure.recovery, Some(recovery));
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
            PresentationLifecycle::classify_present_error(
                PresentationOperation::Redraw,
                PresentError::GpuFault(GpuFaultClass::DeviceLost),
            ),
            PresentFailureAction::RecoverGpu
        );
        assert_eq!(
            PresentationLifecycle::classify_present_error(
                PresentationOperation::Redraw,
                PresentError::ContextMismatch,
            ),
            PresentFailureAction::RecoverGpu
        );
        assert_eq!(
            PresentationLifecycle::classify_present_error(
                PresentationOperation::Redraw,
                PresentError::GpuFault(GpuFaultClass::Validation),
            ),
            PresentFailureAction::Fatal
        );
        assert_eq!(
            PresentationLifecycle::classify_present_error(
                PresentationOperation::Redraw,
                PresentError::SurfaceDetached,
            ),
            PresentFailureAction::Fatal
        );
    }
}
