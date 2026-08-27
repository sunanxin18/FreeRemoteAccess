mod state;
mod surface;

use frd_core::PixelSize;
use frd_protocol_api::PresentationEvent;
use frd_render_wgpu::{GpuContext, RemoteRenderer, RendererError};

use state::{acknowledge_after_present, AcquiredFrame, AcquisitionAction};
pub use surface::{PresentationSurface, PresentationSurfaceLease};

pub trait PresentationHooks {
    fn before_submit(&self);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentError {
    SurfaceCreation,
    SurfaceUnsupported,
    SurfaceDetached,
    SurfaceAcquisitionValidation,
    Renderer(RendererError),
}

impl From<RendererError> for PresentError {
    fn from(value: RendererError) -> Self {
        Self::Renderer(value)
    }
}

pub struct PresentationCompositor {
    context: GpuContext,
    presentation: PresentationSurface,
    configuration: Option<wgpu::SurfaceConfiguration>,
    physical_size: Option<PixelSize>,
}

impl PresentationCompositor {
    pub fn new(
        presentation: PresentationSurface,
        context: GpuContext,
        physical_size: PixelSize,
    ) -> Result<Self, PresentError> {
        let mut compositor = Self {
            context,
            presentation,
            configuration: None,
            physical_size: Some(physical_size),
        };
        compositor.configure_surface()?;
        Ok(compositor)
    }

    pub fn target_format(&self) -> Option<wgpu::TextureFormat> {
        self.configuration.as_ref().map(|config| config.format)
    }

    pub fn resize(&mut self, size: PixelSize) {
        self.physical_size = Some(size);
        if let Some(configuration) = self.configuration.as_mut() {
            configuration.width = size.width;
            configuration.height = size.height;
            if let Some(surface) = self.presentation.surface() {
                surface.configure(self.context.device(), configuration);
            }
        }
    }

    pub fn pause_presenting(&mut self) {
        self.physical_size = None;
    }

    pub fn render(
        &mut self,
        remote: &mut RemoteRenderer,
        overlay: impl FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView),
        hooks: &dyn PresentationHooks,
    ) -> Result<Option<PresentationEvent>, PresentError> {
        let Some(physical_size) = self.physical_size else {
            return Ok(None);
        };
        let target_format = self
            .configuration
            .as_ref()
            .ok_or(PresentError::SurfaceDetached)?
            .format;
        let acquired = self
            .presentation
            .surface()
            .ok_or(PresentError::SurfaceDetached)?
            .get_current_texture();
        let acquired = AcquiredFrame::from(acquired);
        let action = acquired.action();

        match action {
            AcquisitionAction::Render | AcquisitionAction::RenderThenReconfigure => {
                let texture = match acquired {
                    AcquiredFrame::Success(texture) | AcquiredFrame::Suboptimal(texture) => texture,
                    _ => unreachable!("渲染动作必须包含 SurfaceTexture"),
                };
                let view = texture
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                let mut encoder =
                    self.context
                        .device()
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("FreeRemoteDesk presentation encoder"),
                        });
                let receipt = remote.record(&mut encoder, &view, physical_size, target_format)?;
                overlay(&mut encoder, &view);
                hooks.before_submit();
                self.context.queue().submit([encoder.finish()]);
                self.context.queue().present(texture);

                let event = acknowledge_after_present(receipt, true);
                if let Some(receipt) = receipt {
                    remote.confirm_presented(receipt)?;
                }
                if action == AcquisitionAction::RenderThenReconfigure {
                    self.reconfigure_existing()?;
                }
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
            AcquisitionAction::ValidationError => Err(PresentError::SurfaceAcquisitionValidation),
        }
    }

    pub fn detach(&mut self) {
        self.configuration = None;
        self.physical_size = None;
        self.presentation.detach();
    }

    fn reconfigure_existing(&mut self) -> Result<(), PresentError> {
        let configuration = self
            .configuration
            .as_ref()
            .ok_or(PresentError::SurfaceDetached)?;
        self.presentation
            .surface()
            .ok_or(PresentError::SurfaceDetached)?
            .configure(self.context.device(), configuration);
        Ok(())
    }

    fn configure_surface(&mut self) -> Result<(), PresentError> {
        let size = self.physical_size.ok_or(PresentError::SurfaceDetached)?;
        let surface = self
            .presentation
            .surface()
            .ok_or(PresentError::SurfaceDetached)?;
        let capabilities = surface.get_capabilities(self.context.adapter());
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
        let configuration = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width,
            height: size.height,
            desired_maximum_frame_latency: 2,
            present_mode,
            alpha_mode,
            view_formats: Vec::new(),
        };
        surface.configure(self.context.device(), &configuration);
        self.configuration = Some(configuration);
        Ok(())
    }
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
    use frd_render_wgpu::{GpuContext, GpuContextError, RemoteRenderer};

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
        compositor.resize(size);
    }

    #[test]
    fn public_compositor_boundary_keeps_surface_ownership_and_overlay_recording_local() {
        let _ = create_surface_contract;
        let _ = request_context_contract;
        let _ = render_contract;
        let _ = resize_contract;
    }
}
