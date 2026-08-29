mod state;
mod surface;

use frd_core::{ContentViewport, PixelSize};
use frd_protocol_api::PresentationEvent;
use frd_render_wgpu::{
    GpuCleanToken, GpuContext, GpuContextError, GpuFaultClass, RecoveryRequirement, RemoteRenderer,
    RendererError,
};

use state::{
    AcquiredFrame, AcquisitionAction, ContextPairState, SurfaceSizeAction, SurfaceSizeState,
};
pub use surface::{PresentationSurface, PresentationSurfaceLease};

pub trait PresentationHooks {
    fn before_submit(&self);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentError {
    SurfaceCreation,
    SurfaceUnsupported,
    SurfaceDetached,
    InvalidPhysicalSize,
    ContextMismatch,
    GpuUnavailable,
    GpuFault(GpuFaultClass),
    Renderer(RendererError),
}

impl From<RendererError> for PresentError {
    fn from(value: RendererError) -> Self {
        Self::Renderer(value)
    }
}

impl From<GpuFaultClass> for PresentError {
    fn from(value: GpuFaultClass) -> Self {
        Self::GpuFault(value)
    }
}

impl From<GpuContextError> for PresentError {
    fn from(_value: GpuContextError) -> Self {
        Self::GpuUnavailable
    }
}

fn require_context_match(matches: bool) -> Result<(), PresentError> {
    matches.then_some(()).ok_or(PresentError::ContextMismatch)
}

pub struct PresentationCompositor {
    context: GpuContext,
    presentation: PresentationSurface,
    configuration: Option<wgpu::SurfaceConfiguration>,
    size_state: SurfaceSizeState,
}

impl PresentationCompositor {
    pub fn new(
        presentation: PresentationSurface,
        context: GpuContext,
        physical_size: PixelSize,
    ) -> Result<Self, PresentError> {
        let size_state =
            SurfaceSizeState::new(physical_size).ok_or(PresentError::InvalidPhysicalSize)?;
        let mut compositor = Self {
            context,
            presentation,
            configuration: None,
            size_state,
        };
        compositor.configure_surface()?;
        Ok(compositor)
    }

    pub fn target_format(&self) -> Option<wgpu::TextureFormat> {
        self.configuration.as_ref().map(|config| config.format)
    }

    pub fn resize(&mut self, size: PixelSize) -> Result<(), PresentError> {
        let previous = self.size_state;
        let result = (|| {
            match self.size_state.resize(size) {
                SurfaceSizeAction::Pause => {}
                SurfaceSizeAction::Configure(size) => {
                    if let Some(mut configuration) = self.configuration.clone() {
                        configuration.width = size.width;
                        configuration.height = size.height;
                        let token = self.configure_existing_with(&configuration)?;
                        let context = self.context.clone();
                        context.commit_if_unchanged(token, || {
                            self.configuration = Some(configuration);
                        })?;
                    } else {
                        self.configure_surface()?;
                    }
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.size_state = previous;
        }
        result
    }

    pub fn pause_presenting(&mut self) {
        self.size_state.pause();
    }

    pub fn render(
        &mut self,
        remote: &mut RemoteRenderer,
        overlay: impl FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView),
        hooks: &dyn PresentationHooks,
    ) -> Result<Option<PresentationEvent>, PresentError> {
        self.render_with_viewport(remote, None, false, overlay, hooks)
    }

    pub fn render_in(
        &mut self,
        remote: &mut RemoteRenderer,
        viewport: Option<ContentViewport>,
        overlay: impl FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView),
        hooks: &dyn PresentationHooks,
    ) -> Result<Option<PresentationEvent>, PresentError> {
        self.render_with_viewport(remote, viewport, true, overlay, hooks)
    }

    fn render_with_viewport(
        &mut self,
        remote: &mut RemoteRenderer,
        viewport: Option<ContentViewport>,
        explicit_viewport: bool,
        overlay: impl FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView),
        hooks: &dyn PresentationHooks,
    ) -> Result<Option<PresentationEvent>, PresentError> {
        let context_pair = ContextPairState::new(self.context.context_id(), remote.context_id());
        require_context_match(context_pair.matches() && remote.uses_context(&self.context))?;
        if let Some(fault) = self.context.observed_fault() {
            return Err(PresentError::GpuFault(fault));
        }
        let Some(physical_size) = self.size_state.active() else {
            return Ok(None);
        };
        let target_format = self
            .configuration
            .as_ref()
            .ok_or(PresentError::SurfaceDetached)?
            .format;
        let acquisition_scope = self.context.begin_fault_scope()?;
        let acquired = self
            .presentation
            .surface()
            .ok_or(PresentError::SurfaceDetached)?
            .get_current_texture();
        let acquisition_token = acquisition_scope.finish()?;
        self.context.commit_if_unchanged(acquisition_token, || ())?;
        let acquired = AcquiredFrame::from(acquired);
        let action = acquired.action();

        match action {
            AcquisitionAction::Render | AcquisitionAction::RenderThenReconfigure => {
                let texture = match acquired {
                    AcquiredFrame::Success(texture) | AcquiredFrame::Suboptimal(texture) => texture,
                    _ => unreachable!("渲染动作必须包含 SurfaceTexture"),
                };
                let frame_scope = self.context.begin_fault_scope()?;
                let view = texture
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                let mut encoder =
                    self.context
                        .device()
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("FreeRemoteDesk presentation encoder"),
                        });
                let receipt = if explicit_viewport {
                    remote.record_in(&mut encoder, &view, viewport, target_format)?
                } else {
                    remote.record(&mut encoder, &view, physical_size, target_format)?
                };
                overlay(&mut encoder, &view);
                hooks.before_submit();
                self.context.queue().submit([encoder.finish()]);
                self.context.queue().present(texture);
                // submit/present 在 wgpu 30 中不返回逐帧 Result；同一错误作用域和共享
                // observer 负责在确认回执前收集同步验证错误与设备丢失。finish 使用
                // 非阻塞 Poll，不把呈现路径变成等待 GPU 完成的 fence。
                let frame_token = frame_scope.finish()?;
                if action == AcquisitionAction::RenderThenReconfigure {
                    self.reconfigure_existing()?;
                }
                let event = if let Some(receipt) = receipt {
                    let receipt = remote
                        .confirm_presented(frame_token, receipt)?
                        .into_receipt();
                    Some(PresentationEvent::FramePresented {
                        session_id: receipt.session_id,
                        generation: receipt.generation,
                        revision: receipt.revision,
                        completeness: receipt.completeness,
                    })
                } else {
                    self.context.commit_if_unchanged(frame_token, || ())?;
                    None
                };
                Ok(event)
            }
            AcquisitionAction::Reconfigure => {
                self.reconfigure_existing()?;
                Ok(None)
            }
            AcquisitionAction::RecreateSurface => {
                self.presentation.recreate(self.context.instance())?;
                self.configure_surface()?;
                Ok(None)
            }
            AcquisitionAction::Skip => Ok(None),
            AcquisitionAction::ValidationError => {
                self.context.observe_fault(GpuFaultClass::Validation);
                Err(PresentError::GpuFault(GpuFaultClass::Validation))
            }
        }
    }

    pub fn recover_gpu(
        &mut self,
        remote: &mut RemoteRenderer,
        context: GpuContext,
    ) -> Result<Option<RecoveryRequirement>, PresentError> {
        let mut context_pair =
            ContextPairState::new(self.context.context_id(), remote.context_id());
        let new_context_id = context.context_id();
        let candidate = self.presentation.create_candidate(context.instance())?;
        let configuration = if let Some(size) = self.size_state.active() {
            let configuration = surface_configuration(&candidate, &context, size)?;
            let token = configure_surface_with(&candidate, &context, &configuration)?;
            context.commit_if_unchanged(token, || ())?;
            Some(configuration)
        } else {
            None
        };
        let renderer_context = context.clone();
        let (requirement, detached_presentation) =
            remote.recover_device_coordinated(renderer_context, || {
                let old_surface = self.presentation.replace_surface(candidate);
                let old_context = std::mem::replace(&mut self.context, context);
                let old_configuration = std::mem::replace(&mut self.configuration, configuration);
                (old_surface, old_context, old_configuration)
            })?;
        drop(detached_presentation);
        context_pair.install_recovery(new_context_id);
        debug_assert!(context_pair.matches());
        debug_assert_eq!(self.context.context_id(), remote.context_id());
        Ok(requirement)
    }

    pub async fn recover_gpu_with_new_instance(
        &mut self,
        remote: &mut RemoteRenderer,
        instance: wgpu::Instance,
    ) -> Result<(Option<RecoveryRequirement>, GpuContext), PresentError> {
        let candidate = self.presentation.create_candidate(&instance)?;
        let context = GpuContext::request(instance, Some(&candidate)).await?;
        drop(candidate);
        let shell_context = context.clone();
        let requirement = self.recover_gpu(remote, context)?;
        Ok((requirement, shell_context))
    }

    pub fn detach(&mut self) {
        self.configuration = None;
        self.size_state.pause();
        self.presentation.detach();
    }

    fn reconfigure_existing(&mut self) -> Result<(), PresentError> {
        let configuration = self
            .configuration
            .as_ref()
            .ok_or(PresentError::SurfaceDetached)?;
        let token = self.configure_existing_with(configuration)?;
        self.context.commit_if_unchanged(token, || ())?;
        Ok(())
    }

    fn configure_existing_with(
        &self,
        configuration: &wgpu::SurfaceConfiguration,
    ) -> Result<GpuCleanToken, PresentError> {
        let surface = self
            .presentation
            .surface()
            .ok_or(PresentError::SurfaceDetached)?;
        configure_surface_with(surface, &self.context, configuration)
    }

    fn configure_surface(&mut self) -> Result<(), PresentError> {
        let size = self
            .size_state
            .active()
            .ok_or(PresentError::InvalidPhysicalSize)?;
        let surface = self
            .presentation
            .surface()
            .ok_or(PresentError::SurfaceDetached)?;
        let configuration = surface_configuration(surface, &self.context, size)?;
        let token = configure_surface_with(surface, &self.context, &configuration)?;
        let context = self.context.clone();
        context.commit_if_unchanged(token, || {
            self.configuration = Some(configuration);
        })?;
        Ok(())
    }
}

fn surface_configuration(
    surface: &wgpu::Surface<'_>,
    context: &GpuContext,
    size: PixelSize,
) -> Result<wgpu::SurfaceConfiguration, PresentError> {
    if size.width == 0 || size.height == 0 {
        return Err(PresentError::InvalidPhysicalSize);
    }
    let capabilities = surface.get_capabilities(context.adapter());
    let format = capabilities
        .formats
        .iter()
        .copied()
        .find(|format| *format == wgpu::TextureFormat::Bgra8UnormSrgb)
        .or_else(|| {
            capabilities
                .formats
                .iter()
                .copied()
                .find(wgpu::TextureFormat::is_srgb)
        })
        .ok_or(PresentError::SurfaceUnsupported)?;
    let present_mode = capabilities
        .present_modes
        .first()
        .copied()
        .ok_or(PresentError::SurfaceUnsupported)?;
    let alpha_mode = capabilities
        .alpha_modes
        .first()
        .copied()
        .ok_or(PresentError::SurfaceUnsupported)?;
    Ok(wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::Auto,
        width: size.width,
        height: size.height,
        desired_maximum_frame_latency: 2,
        present_mode,
        alpha_mode,
        view_formats: Vec::new(),
    })
}

fn configure_surface_with(
    surface: &wgpu::Surface<'_>,
    context: &GpuContext,
    configuration: &wgpu::SurfaceConfiguration,
) -> Result<GpuCleanToken, PresentError> {
    let scope = context.begin_fault_scope()?;
    surface.configure(context.device(), configuration);
    scope.finish().map_err(PresentError::from)
}

impl Drop for PresentationCompositor {
    fn drop(&mut self) {
        self.detach();
    }
}

#[cfg(test)]
mod api_tests {
    use frd_core::PixelSize;
    use frd_protocol_api::PresentationEvent;
    use frd_render_wgpu::{GpuContext, GpuContextError, RecoveryRequirement, RemoteRenderer};

    use super::{
        PresentError, PresentationCompositor, PresentationHooks, PresentationSurface,
        PresentationSurfaceLease,
    };

    fn create_surface_contract(
        instance: &wgpu::Instance,
        lease: PresentationSurfaceLease,
    ) -> Result<PresentationSurface, PresentError> {
        PresentationSurface::create(instance, lease)
    }

    fn render_contract(
        compositor: &mut PresentationCompositor,
        remote: &mut RemoteRenderer,
        hooks: &dyn PresentationHooks,
    ) -> Result<Option<PresentationEvent>, PresentError> {
        compositor.render(remote, |_encoder, _target| {}, hooks)
    }

    async fn request_context_contract(
        presentation: &PresentationSurface,
        instance: wgpu::Instance,
    ) -> Result<GpuContext, GpuContextError> {
        presentation.request_gpu_context(instance).await
    }

    fn resize_contract(compositor: &mut PresentationCompositor, size: PixelSize) {
        compositor.resize(size).unwrap();
    }

    fn coordinated_recovery_contract(
        compositor: &mut PresentationCompositor,
        renderer: &mut RemoteRenderer,
        context: GpuContext,
    ) -> Result<Option<RecoveryRequirement>, PresentError> {
        compositor.recover_gpu(renderer, context)
    }

    async fn new_instance_recovery_contract(
        compositor: &mut PresentationCompositor,
        renderer: &mut RemoteRenderer,
        instance: wgpu::Instance,
    ) -> Result<(Option<RecoveryRequirement>, GpuContext), PresentError> {
        compositor
            .recover_gpu_with_new_instance(renderer, instance)
            .await
    }

    #[test]
    fn public_compositor_boundary_keeps_surface_ownership_and_overlay_recording_local() {
        let _ = create_surface_contract;
        let _ = request_context_contract;
        let _ = render_contract;
        let _ = resize_contract;
        let _ = coordinated_recovery_contract;
        let _ = new_instance_recovery_contract;
    }

    #[test]
    fn context_mismatch_is_a_stable_fail_closed_error() {
        assert_eq!(
            super::require_context_match(false),
            Err(PresentError::ContextMismatch)
        );
        assert_eq!(super::require_context_match(true), Ok(()));
    }
}
