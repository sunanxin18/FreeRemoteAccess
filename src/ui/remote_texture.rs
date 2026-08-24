use std::error::Error;
use std::fmt;
use std::time::Duration;

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
}

impl RendererRuntimePolicy {
    pub const fn new() -> Self {
        Self {
            remote_state: RemoteTextureState::empty(),
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

    pub fn generation(&self) -> Option<u64> {
        self.remote_state.generation()
    }

    pub fn dimensions(&self) -> Option<(u32, u32)> {
        self.remote_state.dimensions()
    }

    pub const fn surface_available(&self) -> bool {
        self.remote_state.surface_available()
    }

    fn clear_remote_texture(&mut self) -> RemoteTextureAction {
        self.remote_state.clear_remote_surface();
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

/// Bounded production policy for wgpu's current-surface outcomes. The executor
/// only owns local swapchain resources; remote pixels and their generation stay
/// in `RendererRuntimePolicy` until the session lifecycle explicitly clears them.
#[derive(Debug, Default)]
pub struct SurfaceRecoveryController {
    attempts: u8,
    accumulated_delay: Duration,
}

impl SurfaceRecoveryController {
    const MAX_ATTEMPTS: u8 = 4;
    const MAX_TOTAL_DELAY: Duration = Duration::from_millis(500);

    pub fn on_acquire(&mut self, outcome: SurfaceAcquireOutcome) -> SurfaceRecoveryDecision {
        match outcome {
            SurfaceAcquireOutcome::Success => {
                self.attempts = 0;
                self.accumulated_delay = Duration::ZERO;
                SurfaceRecoveryDecision::Render
            }
            SurfaceAcquireOutcome::Suboptimal => self.next_recovery(false, true),
            SurfaceAcquireOutcome::Timeout => self.next_timeout(),
            SurfaceAcquireOutcome::Occluded => SurfaceRecoveryDecision::WaitForVisibility,
            SurfaceAcquireOutcome::Outdated => self.next_recovery(false, false),
            SurfaceAcquireOutcome::Lost => self.next_recovery(true, false),
            SurfaceAcquireOutcome::Validation => {
                SurfaceRecoveryDecision::Terminal("surface_validation_failed")
            }
        }
    }

    fn next_timeout(&mut self) -> SurfaceRecoveryDecision {
        match self.next_delay(true) {
            Some(delay) => SurfaceRecoveryDecision::RetryAfter(delay),
            None => SurfaceRecoveryDecision::Terminal("surface_recovery_exhausted"),
        }
    }

    fn next_recovery(&mut self, recreate: bool, post_present: bool) -> SurfaceRecoveryDecision {
        let Some(delay) = self.next_delay(false) else {
            return SurfaceRecoveryDecision::Terminal("surface_recovery_exhausted");
        };
        if post_present {
            SurfaceRecoveryDecision::RenderThenReconfigure(delay)
        } else if recreate {
            SurfaceRecoveryDecision::RecreateThenConfigureThenRetry(delay)
        } else {
            SurfaceRecoveryDecision::ReconfigureThenRetry(delay)
        }
    }

    fn next_delay(&mut self, force_delayed: bool) -> Option<Duration> {
        if self.attempts >= Self::MAX_ATTEMPTS {
            return None;
        }
        self.attempts += 1;
        let delay = if self.attempts == 1 && !force_delayed {
            Duration::ZERO
        } else {
            let exponent = u32::from(self.attempts.saturating_sub(1)).min(3);
            Duration::from_millis(25 * (1_u64 << exponent))
        };
        let total = self.accumulated_delay.saturating_add(delay);
        if total > Self::MAX_TOTAL_DELAY {
            return None;
        }
        self.accumulated_delay = total;
        Some(delay)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceRecoveryDecision {
    Render,
    RenderThenReconfigure(Duration),
    RetryAfter(Duration),
    WaitForVisibility,
    ReconfigureThenRetry(Duration),
    RecreateThenConfigureThenRetry(Duration),
    Terminal(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryExecution {
    RetryAfter(Duration),
    WaitForVisibility,
}

/// Port implemented by the production renderer and by deterministic tests. It
/// deliberately exposes only local surface work: it cannot replace remote GPU
/// texture ownership or alter its generation.
pub trait SurfaceRecoveryPort {
    type Error;

    fn recreate_surface(&mut self) -> Result<(), Self::Error>;
    fn configure_surface(&mut self) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SurfaceRecoveryExecutor;

impl SurfaceRecoveryExecutor {
    pub fn execute_post_present<P, F>(
        &self,
        decision: SurfaceRecoveryDecision,
        present: F,
        port: &mut P,
    ) -> Result<RecoveryExecution, P::Error>
    where
        P: SurfaceRecoveryPort,
        F: FnOnce(),
    {
        let SurfaceRecoveryDecision::RenderThenReconfigure(delay) = decision else {
            unreachable!("post-present recovery requires a suboptimal frame decision");
        };
        present();
        port.configure_surface()?;
        Ok(RecoveryExecution::RetryAfter(delay))
    }

    pub fn execute_without_frame<P>(
        &self,
        decision: SurfaceRecoveryDecision,
        port: &mut P,
    ) -> Result<RecoveryExecution, P::Error>
    where
        P: SurfaceRecoveryPort,
    {
        match decision {
            SurfaceRecoveryDecision::RetryAfter(delay) => Ok(RecoveryExecution::RetryAfter(delay)),
            SurfaceRecoveryDecision::WaitForVisibility => Ok(RecoveryExecution::WaitForVisibility),
            SurfaceRecoveryDecision::ReconfigureThenRetry(delay) => {
                port.configure_surface()?;
                Ok(RecoveryExecution::RetryAfter(delay))
            }
            SurfaceRecoveryDecision::RecreateThenConfigureThenRetry(delay) => {
                port.recreate_surface()?;
                port.configure_surface()?;
                Ok(RecoveryExecution::RetryAfter(delay))
            }
            SurfaceRecoveryDecision::Render
            | SurfaceRecoveryDecision::RenderThenReconfigure(_)
            | SurfaceRecoveryDecision::Terminal(_) => {
                unreachable!("surface recovery executor received an invalid no-frame decision")
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct GpuFailureGate {
    first_error_code: Option<&'static str>,
}

impl GpuFailureGate {
    pub fn latch(&mut self, error_code: &'static str) -> bool {
        if self.first_error_code.is_some() {
            return false;
        }
        self.first_error_code = Some(error_code);
        true
    }

    /// Production host decision for every terminal renderer error. A renderer
    /// that fails before a session exists is still latched and must exit rather
    /// than accepting a same-frame connection click.
    pub fn on_terminal_renderer_error(
        &mut self,
        error_code: &'static str,
        has_active_session: bool,
    ) -> RendererFailureAction {
        self.latch(error_code);
        if has_active_session {
            RendererFailureAction::OrderlyDisconnect
        } else {
            RendererFailureAction::ExitFailClosed
        }
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

    pub const fn permits_new_session(&self) -> bool {
        self.first_error_code.is_none()
    }

    /// The only release point after a GPU terminal failure. A failed join must
    /// retain the latch so queued session progress cannot revive the UI.
    pub fn release_after_worker_completion(&mut self, joined: bool) -> bool {
        if joined {
            self.first_error_code.take().is_some()
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererFailureAction {
    OrderlyDisconnect,
    ExitFailClosed,
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
