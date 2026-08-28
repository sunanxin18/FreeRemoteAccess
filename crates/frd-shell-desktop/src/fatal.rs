use std::fmt;

use frd_compositor_wgpu::PresentError;
use frd_render_wgpu::{GpuFaultClass, RendererError};

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
    Launch,
    LaunchAccept,
    LaunchCancel,
    Cleanup,
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FatalReason {
    InvalidState,
    WindowCreateFailed,
    WindowSizeInvalid,
    SurfaceCreateFailed,
    GpuUnavailable,
    CompositorConfigureFailed,
    RendererInitializeFailed,
    SurfaceFormatUnavailable,
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
        FatalReason::WindowCreateFailed => "window_create_failed",
        FatalReason::WindowSizeInvalid => "window_size_invalid",
        FatalReason::SurfaceCreateFailed => "surface_create_failed",
        FatalReason::GpuUnavailable => "gpu_unavailable",
        FatalReason::CompositorConfigureFailed => "compositor_configure_failed",
        FatalReason::RendererInitializeFailed => "renderer_initialize_failed",
        FatalReason::SurfaceFormatUnavailable => "surface_format_unavailable",
        FatalReason::CleanupPolicyExhausted => "cleanup_policy_exhausted",
        FatalReason::ShutdownTimeout => "shutdown_timeout",
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
    use frd_render_wgpu::GpuFaultClass;

    use super::{sanitize_safe_detail, FatalReport};
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
}
