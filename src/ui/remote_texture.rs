use std::error::Error;
use std::fmt;

use crate::core::{GenerationDisposition, RemoteSurfaceState, RenderUpdate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteTextureState {
    remote_surface: Option<RemoteSurfaceState>,
    surface_available: bool,
}

impl RemoteTextureState {
    pub const fn empty() -> Self {
        Self {
            remote_surface: None,
            surface_available: false,
        }
    }

    pub fn fixture(generation: u64, width: u32, height: u32) -> Self {
        Self {
            remote_surface: Some(
                RemoteSurfaceState::new(generation, width, height)
                    .expect("fixture dimensions must be valid"),
            ),
            surface_available: true,
        }
    }

    pub fn apply_reset(
        &mut self,
        generation: u64,
        width: u32,
        height: u32,
    ) -> Result<ResetDisposition, TextureStateError> {
        let candidate = RemoteSurfaceState::new(generation, width, height)
            .map_err(|_| TextureStateError::new("texture_dimensions_invalid"))?;
        let disposition = match self.remote_surface {
            None => ResetDisposition::Created,
            Some(current) if generation < current.generation() => {
                return Ok(ResetDisposition::Stale)
            }
            Some(current) if generation == current.generation() => {
                if current.dimensions() != candidate.dimensions() {
                    return Err(TextureStateError::new(
                        "texture_generation_dimensions_changed",
                    ));
                }
                return Ok(ResetDisposition::Unchanged);
            }
            Some(_) => ResetDisposition::Recreated,
        };
        self.remote_surface = Some(candidate);
        Ok(disposition)
    }

    pub fn classify(
        &self,
        update: &RenderUpdate,
    ) -> Result<TextureUpdateDisposition, TextureStateError> {
        let surface = self
            .remote_surface
            .ok_or_else(|| TextureStateError::new("texture_update_without_reset"))?;
        match surface.classify_generation(update.generation()) {
            GenerationDisposition::Stale => return Ok(TextureUpdateDisposition::Stale),
            GenerationDisposition::Future => {
                return Err(TextureStateError::new("texture_update_before_reset"));
            }
            GenerationDisposition::Current => {}
        }
        if let RenderUpdate::DirtyRect { rect, .. } = update {
            if !surface.contains(*rect) {
                return Err(TextureStateError::new("texture_rect_out_of_bounds"));
            }
        }
        Ok(TextureUpdateDisposition::Current)
    }

    pub fn on_surface_available(&mut self) {
        self.surface_available = true;
    }

    pub fn on_surface_lost(&mut self) {
        self.surface_available = false;
    }

    pub fn clear_remote_surface(&mut self) {
        self.remote_surface = None;
    }

    pub const fn surface_available(self) -> bool {
        self.surface_available
    }

    pub fn generation(self) -> Option<u64> {
        self.remote_surface.map(RemoteSurfaceState::generation)
    }

    pub fn dimensions(self) -> Option<(u32, u32)> {
        self.remote_surface.map(RemoteSurfaceState::dimensions)
    }
}

impl Default for RemoteTextureState {
    fn default() -> Self {
        Self::empty()
    }
}

/// GPU-free policy consumed by the renderer so swapchain recovery and remote
/// texture lifetime can be validated independently of a platform GPU.
#[derive(Debug, Default)]
pub struct RendererRuntimePolicy {
    remote_state: RemoteTextureState,
    remote_texture_identity: Option<u64>,
    next_remote_texture_identity: u64,
}

impl RendererRuntimePolicy {
    pub const fn new() -> Self {
        Self {
            remote_state: RemoteTextureState::empty(),
            remote_texture_identity: None,
            next_remote_texture_identity: 1,
        }
    }

    pub fn begin_authenticated_session(&mut self) -> RemoteTextureAction {
        self.clear_remote_texture()
    }

    pub fn finish_disconnected_session(&mut self) -> RemoteTextureAction {
        self.clear_remote_texture()
    }

    pub fn finish_failed_session(&mut self) -> RemoteTextureAction {
        self.clear_remote_texture()
    }

    pub fn apply_reset(
        &mut self,
        generation: u64,
        width: u32,
        height: u32,
    ) -> Result<ResetDisposition, TextureStateError> {
        let disposition = self.remote_state.apply_reset(generation, width, height)?;
        if matches!(
            disposition,
            ResetDisposition::Created | ResetDisposition::Recreated
        ) {
            self.remote_texture_identity = Some(self.next_remote_texture_identity);
            self.next_remote_texture_identity = self.next_remote_texture_identity.saturating_add(1);
        }
        Ok(disposition)
    }

    pub fn classify(
        &self,
        update: &RenderUpdate,
    ) -> Result<TextureUpdateDisposition, TextureStateError> {
        self.remote_state.classify(update)
    }

    pub fn mark_surface_unavailable(&mut self) {
        self.remote_state.on_surface_lost();
    }

    pub fn mark_surface_available(&mut self) {
        self.remote_state.on_surface_available();
    }

    pub fn on_surface_acquire(&self, outcome: SurfaceAcquireOutcome) -> SurfaceRecoveryPlan {
        match outcome {
            SurfaceAcquireOutcome::Success => SurfaceRecoveryPlan::Render,
            SurfaceAcquireOutcome::Suboptimal => SurfaceRecoveryPlan::RenderThen(&[
                SurfaceRecoveryStep::PresentFrame,
                SurfaceRecoveryStep::ReconfigureExistingSurface,
                SurfaceRecoveryStep::RequestRedraw,
            ]),
            SurfaceAcquireOutcome::Timeout => SurfaceRecoveryPlan::SkipUntilNextWake,
            SurfaceAcquireOutcome::Occluded => SurfaceRecoveryPlan::WaitForVisibility,
            SurfaceAcquireOutcome::Outdated => SurfaceRecoveryPlan::Recover(&[
                SurfaceRecoveryStep::ReconfigureExistingSurface,
                SurfaceRecoveryStep::RequestRedraw,
            ]),
            SurfaceAcquireOutcome::Lost => SurfaceRecoveryPlan::Recover(&[
                SurfaceRecoveryStep::RecreateSurface,
                SurfaceRecoveryStep::ReconfigureExistingSurface,
                SurfaceRecoveryStep::RequestRedraw,
            ]),
            SurfaceAcquireOutcome::Validation => SurfaceRecoveryPlan::FailSession,
        }
    }

    pub fn generation(&self) -> Option<u64> {
        self.remote_state.generation()
    }

    pub fn dimensions(&self) -> Option<(u32, u32)> {
        self.remote_state.dimensions()
    }

    pub const fn surface_available(&self) -> bool {
        self.remote_state.surface_available()
    }

    pub const fn remote_texture_identity(&self) -> Option<u64> {
        self.remote_texture_identity
    }

    fn clear_remote_texture(&mut self) -> RemoteTextureAction {
        self.remote_state.clear_remote_surface();
        self.remote_texture_identity = None;
        RemoteTextureAction::Clear
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteTextureAction {
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceAcquireOutcome {
    Success,
    Suboptimal,
    Timeout,
    Occluded,
    Outdated,
    Lost,
    Validation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceRecoveryPlan {
    Render,
    RenderThen(&'static [SurfaceRecoveryStep]),
    Recover(&'static [SurfaceRecoveryStep]),
    SkipUntilNextWake,
    WaitForVisibility,
    FailSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceRecoveryStep {
    PresentFrame,
    ReconfigureExistingSurface,
    RecreateSurface,
    RequestRedraw,
}

#[derive(Debug, Default)]
pub struct GpuFailureLatch {
    first_error_code: Option<&'static str>,
}

impl GpuFailureLatch {
    pub fn latch(&mut self, error_code: &'static str) -> bool {
        if self.first_error_code.is_some() {
            return false;
        }
        self.first_error_code = Some(error_code);
        true
    }

    pub const fn first_error_code(&self) -> Option<&'static str> {
        self.first_error_code
    }

    pub const fn blocks_session_progress(&self) -> bool {
        self.first_error_code.is_some()
    }

    pub const fn blocks_remote_input(&self) -> bool {
        self.first_error_code.is_some()
    }

    pub const fn admits_queued_progress(&self, _progress: QueuedSessionProgress) -> bool {
        self.first_error_code.is_none()
    }

    pub fn release_after_worker_completion(&mut self) -> Option<&'static str> {
        self.first_error_code.take()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedSessionProgress {
    Render,
    SurfaceReset,
    Connected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetDisposition {
    Created,
    Recreated,
    Unchanged,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureUpdateDisposition {
    Current,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureStateError {
    code: &'static str,
}

impl TextureStateError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Display for TextureStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "远程纹理状态无效 ({})", self.code)
    }
}

impl Error for TextureStateError {}
