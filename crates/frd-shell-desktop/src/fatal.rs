use std::fmt;

use frd_compositor_wgpu::PresentError;
use frd_render_wgpu::{GpuFaultClass, RendererError};
use frd_session::CleanupError;

use crate::cleanup::BackgroundCleanupFailure;
use crate::frame_metrics_sink::MetricSinkError;
use crate::lifecycle::PresentationOperation;

const FATAL_CODE: &str = "FRD-WIN-FATAL-001";
const MAX_SAFE_DETAIL_BYTES: usize = 160;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FatalComponent {
    Application,
    Window,
    Session,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FatalOperation {
    Initialize,
    SingleInstance,
    EventLoopCreate,
    EventLoopRun,
    CliValidation,
    IdentityStore,
    CredentialStore,
    Launch,
    LaunchAccept,
    LaunchCancel,
    Cleanup,
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FatalReason {
    InvalidState,
    InstanceAlreadyRunning,
    SingleInstanceUnavailable,
    EventLoopCreateFailed,
    EventLoopRunFailed,
    InvalidArguments,
    CommandLineOutputFailed,
    IdentityStoreUnavailable,
    CredentialStoreUnavailable,
    WindowCreateFailed,
    WindowChromeFailed,
    WindowSizeInvalid,
    SurfaceCreateFailed,
    GpuUnavailable,
    CompositorConfigureFailed,
    RendererInitializeFailed,
    SurfaceFormatUnavailable,
    CleanupWorkerSpawnFailed,
    CleanupPolicyExhausted,
    ShutdownTimeout,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FatalReport {
    code: &'static str,
    component: &'static str,
    operation: &'static str,
    reason: &'static str,
    details: String,
}

impl FatalReport {
    /// 仅接受封闭的静态分类；调用者无法把凭据、端点或协议载荷塞进 fatal 输出。
    pub fn internal(
        component: FatalComponent,
        operation: FatalOperation,
        reason: FatalReason,
    ) -> Self {
        Self {
            code: FATAL_CODE,
            component: component_code(component),
            operation: operation_code(operation),
            reason: reason_code(reason),
            details: "none".to_owned(),
        }
    }

    pub(crate) fn presentation(
        operation: PresentationOperation,
        source: PresentError,
        retry: Option<PresentError>,
        recovery: Option<PresentError>,
    ) -> Self {
        // PresentError 先穷举映射到静态 token；不使用其 Debug 文本。
        let details = sanitize_safe_detail(&format!(
            "source={};retry={};recovery={}",
            present_error_code(source),
            retry.map(present_error_code).unwrap_or("none"),
            recovery.map(present_error_code).unwrap_or("none"),
        ));
        Self {
            code: FATAL_CODE,
            component: "presentation",
            operation: presentation_operation_code(operation),
            reason: "presentation_unrecoverable",
            details,
        }
    }

    pub(crate) fn cleanup(failure: BackgroundCleanupFailure) -> Self {
        match failure {
            BackgroundCleanupFailure::WorkerSpawn => Self::internal(
                FatalComponent::Session,
                FatalOperation::Cleanup,
                FatalReason::CleanupWorkerSpawnFailed,
            ),
            BackgroundCleanupFailure::Exhausted {
                last_error,
                attempts,
            } => {
                let attempts = if attempts > 999 {
                    "999_plus".to_owned()
                } else {
                    attempts.to_string()
                };
                Self {
                    code: FATAL_CODE,
                    component: "session",
                    operation: "cleanup",
                    reason: "cleanup_policy_exhausted",
                    details: sanitize_safe_detail(&format!(
                        "step={};attempts={attempts}",
                        cleanup_error_code(last_error)
                    )),
                }
            }
        }
    }

    pub(crate) fn frame_metrics_startup(error: MetricSinkError) -> Self {
        let reason = match error {
            MetricSinkError::InvalidConfiguration => "frame_metrics_configuration_invalid",
            MetricSinkError::CreateFailed | MetricSinkError::WriteFailed => {
                "frame_metrics_create_failed"
            }
            MetricSinkError::CapacityExceeded | MetricSinkError::InvalidObservation => {
                "frame_metrics_invalid_startup_state"
            }
        };
        Self {
            code: FATAL_CODE,
            component: "application",
            operation: "frame_metrics",
            reason,
            details: "none".to_owned(),
        }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn component(&self) -> &'static str {
        self.component
    }

    pub fn operation(&self) -> &'static str {
        self.operation
    }

    pub fn reason(&self) -> &'static str {
        self.reason
    }

    pub fn details(&self) -> &str {
        &self.details
    }
}

fn component_code(component: FatalComponent) -> &'static str {
    match component {
        FatalComponent::Application => "application",
        FatalComponent::Window => "window",
        FatalComponent::Session => "session",
    }
}

fn operation_code(operation: FatalOperation) -> &'static str {
    match operation {
        FatalOperation::Initialize => "initialize",
        FatalOperation::SingleInstance => "single_instance",
        FatalOperation::EventLoopCreate => "event_loop_create",
        FatalOperation::EventLoopRun => "event_loop_run",
        FatalOperation::CliValidation => "cli_validation",
        FatalOperation::IdentityStore => "identity_store",
        FatalOperation::CredentialStore => "credential_store",
        FatalOperation::Launch => "launch",
        FatalOperation::LaunchAccept => "launch_accept",
        FatalOperation::LaunchCancel => "launch_cancel",
        FatalOperation::Cleanup => "cleanup",
        FatalOperation::Shutdown => "shutdown",
    }
}

fn reason_code(reason: FatalReason) -> &'static str {
    match reason {
        FatalReason::InvalidState => "invalid_state",
        FatalReason::InstanceAlreadyRunning => "instance_already_running",
        FatalReason::SingleInstanceUnavailable => "single_instance_unavailable",
        FatalReason::EventLoopCreateFailed => "event_loop_create_failed",
        FatalReason::EventLoopRunFailed => "event_loop_run_failed",
        FatalReason::InvalidArguments => "invalid_arguments",
        FatalReason::CommandLineOutputFailed => "command_line_output_failed",
        FatalReason::IdentityStoreUnavailable => "identity_store_unavailable",
        FatalReason::CredentialStoreUnavailable => "credential_store_unavailable",
        FatalReason::WindowCreateFailed => "window_create_failed",
        FatalReason::WindowChromeFailed => "window_chrome_failed",
        FatalReason::WindowSizeInvalid => "window_size_invalid",
        FatalReason::SurfaceCreateFailed => "surface_create_failed",
        FatalReason::GpuUnavailable => "gpu_unavailable",
        FatalReason::CompositorConfigureFailed => "compositor_configure_failed",
        FatalReason::RendererInitializeFailed => "renderer_initialize_failed",
        FatalReason::SurfaceFormatUnavailable => "surface_format_unavailable",
        FatalReason::CleanupWorkerSpawnFailed => "cleanup_worker_spawn_failed",
        FatalReason::CleanupPolicyExhausted => "cleanup_policy_exhausted",
        FatalReason::ShutdownTimeout => "shutdown_timeout",
    }
}

fn cleanup_error_code(error: CleanupError) -> &'static str {
    match error {
        CleanupError::NoActiveSession => "no_active_session",
        CleanupError::WrongSessionHandle => "wrong_session_handle",
        CleanupError::Cancel => "cancel",
        CleanupError::ShutdownWriter => "shutdown_writer",
        CleanupError::JoinWorkersAndAudio => "join_workers_and_audio",
        CleanupError::DisposeMailbox => "dispose_mailbox",
    }
}

impl fmt::Display for FatalReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} component={} operation={} reason={} details={}",
            self.code, self.component, self.operation, self.reason, self.details
        )
    }
}

impl std::error::Error for FatalReport {}

fn sanitize_safe_detail(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len().min(MAX_SAFE_DETAIL_BYTES));
    for character in value.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if sanitized.len() + character.len_utf8() > MAX_SAFE_DETAIL_BYTES {
            break;
        }
        sanitized.push(character);
    }
    sanitized
}

fn presentation_operation_code(operation: PresentationOperation) -> &'static str {
    match operation {
        PresentationOperation::Redraw => "redraw",
        PresentationOperation::Resize => "resize",
        PresentationOperation::OcclusionResume => "occlusion_resume",
    }
}

fn present_error_code(error: PresentError) -> &'static str {
    match error {
        PresentError::SurfaceCreation => "surface_creation",
        PresentError::SurfaceUnsupported => "surface_unsupported",
        PresentError::SurfaceDetached => "surface_detached",
        PresentError::InvalidPhysicalSize => "invalid_physical_size",
        PresentError::ContextMismatch => "context_mismatch",
        PresentError::GpuUnavailable => "gpu_unavailable",
        PresentError::GpuFault(fault) => gpu_fault_code(fault),
        PresentError::Renderer(error) => renderer_error_code(error),
    }
}

fn gpu_fault_code(fault: GpuFaultClass) -> &'static str {
    match fault {
        GpuFaultClass::Validation => "gpu_validation",
        GpuFaultClass::OutOfMemory => "gpu_out_of_memory",
        GpuFaultClass::Internal => "gpu_internal",
        GpuFaultClass::DeviceLost => "gpu_device_lost",
        GpuFaultClass::ObservationIncomplete => "gpu_observation_incomplete",
    }
}

fn renderer_error_code(error: RendererError) -> &'static str {
    match error {
        RendererError::EmptyBatch => "renderer_empty_batch",
        RendererError::BatchExecutionPanicked => "renderer_batch_execution_panicked",
        RendererError::ScopeObservationInvalid => "renderer_scope_observation_invalid",
        RendererError::StaleUpdate => "renderer_stale_update",
        RendererError::InvalidGeometry => "renderer_invalid_geometry",
        RendererError::TextureBudgetExceeded => "renderer_texture_budget_exceeded",
        RendererError::UnsupportedPixelFormat => "renderer_unsupported_pixel_format",
        RendererError::NonMonotonicRevision => "renderer_non_monotonic_revision",
        RendererError::BoundaryWithoutMatchingDamage => "renderer_boundary_without_matching_damage",
        RendererError::InvalidPatch => "renderer_invalid_patch",
        RendererError::ResetRequired => "renderer_reset_required",
        RendererError::StalePresentationReceipt => "renderer_stale_presentation_receipt",
        RendererError::TextureDimensionUnsupported => "renderer_texture_dimension_unsupported",
        RendererError::UnsupportedTargetFormat => "renderer_unsupported_target_format",
        RendererError::GpuFault(fault) => gpu_fault_code(fault),
    }
}

#[cfg(test)]
mod tests {
    use frd_compositor_wgpu::PresentError;
    use frd_render_wgpu::{GpuFaultClass, RendererError};
    use frd_session::CleanupError;

    use super::{present_error_code, sanitize_safe_detail, FatalReport};
    use crate::frame_metrics_sink::MetricSinkError;
    use crate::BackgroundCleanupFailure;
    use crate::PresentationOperation;

    #[test]
    fn fatal_report_contains_stable_code_component_operation_and_safe_reason() {
        let report = FatalReport::presentation(
            PresentationOperation::Resize,
            PresentError::GpuFault(GpuFaultClass::Validation),
            Some(PresentError::SurfaceUnsupported),
            None,
        );

        assert_eq!(report.code(), "FRD-WIN-FATAL-001");
        assert_eq!(report.component(), "presentation");
        assert_eq!(report.operation(), "resize");
        assert_eq!(report.reason(), "presentation_unrecoverable");
        assert_eq!(
            report.details(),
            "source=gpu_validation;retry=surface_unsupported;recovery=none"
        );
        assert_eq!(
            report.to_string(),
            "FRD-WIN-FATAL-001 component=presentation operation=resize reason=presentation_unrecoverable details=source=gpu_validation;retry=surface_unsupported;recovery=none"
        );
    }

    #[test]
    fn fatal_safe_detail_replaces_controls_and_obeys_the_output_bound() {
        let unsafe_text = format!("safe\r\nfield\t{}", "x".repeat(400));
        let sanitized = sanitize_safe_detail(&unsafe_text);

        assert!(!sanitized.contains(['\r', '\n', '\t']));
        assert!(sanitized.len() <= 160);
        assert!(sanitized.starts_with("safe  field "));
    }

    #[test]
    fn cleanup_worker_spawn_failure_keeps_its_distinct_closed_reason() {
        let report = FatalReport::cleanup(BackgroundCleanupFailure::WorkerSpawn);

        assert_eq!(report.component(), "session");
        assert_eq!(report.operation(), "cleanup");
        assert_eq!(report.reason(), "cleanup_worker_spawn_failed");
        assert_eq!(report.details(), "none");
    }

    #[test]
    fn cleanup_policy_exhaustion_keeps_the_closed_step_and_bounded_attempts() {
        let report = FatalReport::cleanup(BackgroundCleanupFailure::Exhausted {
            last_error: CleanupError::JoinWorkersAndAudio,
            attempts: 3,
        });

        assert_eq!(report.component(), "session");
        assert_eq!(report.operation(), "cleanup");
        assert_eq!(report.reason(), "cleanup_policy_exhausted");
        assert_eq!(report.details(), "step=join_workers_and_audio;attempts=3");
        assert!(report.to_string().len() <= 256);

        let bounded = FatalReport::cleanup(BackgroundCleanupFailure::Exhausted {
            last_error: CleanupError::JoinWorkersAndAudio,
            attempts: usize::MAX,
        });
        assert_eq!(
            bounded.details(),
            "step=join_workers_and_audio;attempts=999_plus"
        );
        assert!(bounded.to_string().len() <= 256);
    }

    #[test]
    fn frame_metrics_startup_errors_use_only_closed_safe_fields() {
        let expected = [
            (
                MetricSinkError::InvalidConfiguration,
                "frame_metrics_configuration_invalid",
            ),
            (MetricSinkError::CreateFailed, "frame_metrics_create_failed"),
            (MetricSinkError::WriteFailed, "frame_metrics_create_failed"),
            (
                MetricSinkError::CapacityExceeded,
                "frame_metrics_invalid_startup_state",
            ),
            (
                MetricSinkError::InvalidObservation,
                "frame_metrics_invalid_startup_state",
            ),
        ];
        for (error, reason) in expected {
            let report = FatalReport::frame_metrics_startup(error);
            assert_eq!(report.component(), "application");
            assert_eq!(report.operation(), "frame_metrics");
            assert_eq!(report.reason(), reason);
            assert_eq!(report.details(), "none");
        }
    }

    #[test]
    fn fatal_renderer_error_tokens_cover_new_batch_variants() {
        assert_eq!(
            present_error_code(PresentError::Renderer(RendererError::EmptyBatch)),
            "renderer_empty_batch"
        );
        assert_eq!(
            present_error_code(PresentError::Renderer(
                RendererError::BatchExecutionPanicked,
            )),
            "renderer_batch_execution_panicked"
        );
        assert_eq!(
            present_error_code(PresentError::Renderer(
                RendererError::ScopeObservationInvalid,
            )),
            "renderer_scope_observation_invalid"
        );
    }
}
