mod pass;
mod remote_texture;

pub use remote_texture::{
    ApplyOutcome, PresentationReceipt, RecoveryRequirement, RemoteRenderer, RendererError,
};

#[derive(Clone)]
pub struct GpuContext {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GpuContextError {
    AdapterUnavailable,
    DeviceUnavailable,
}

impl GpuContext {
    pub async fn request(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self, GpuContextError> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface,
                ..Default::default()
            })
            .await
            .map_err(|_| GpuContextError::AdapterUnavailable)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("FreeRemoteDesk wgpu device"),
                ..Default::default()
            })
            .await
            .map_err(|_| GpuContextError::DeviceUnavailable)?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    pub fn from_parts(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
    ) -> Self {
        Self {
            instance,
            adapter,
            device,
            queue,
        }
    }

    pub fn instance(&self) -> &wgpu::Instance {
        &self.instance
    }

    pub fn adapter(&self) -> &wgpu::Adapter {
        &self.adapter
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }
}

#[cfg(test)]
mod api_tests {
    use frd_core::PixelSize;
    use frd_frame::SurfaceUpdate;

    use super::{ApplyOutcome, GpuContext, RecoveryRequirement, RemoteRenderer, RendererError};

    fn apply_update_contract(
        renderer: &mut RemoteRenderer,
        update: SurfaceUpdate,
    ) -> Result<ApplyOutcome, RendererError> {
        renderer.apply_update(update)
    }

    fn record_contract(
        renderer: &mut RemoteRenderer,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        drawable: PixelSize,
        target_format: wgpu::TextureFormat,
    ) -> Result<Option<super::PresentationReceipt>, RendererError> {
        renderer.record(encoder, target, drawable, target_format)
    }

    fn recovery_contract(
        renderer: &mut RemoteRenderer,
        context: GpuContext,
    ) -> Option<RecoveryRequirement> {
        renderer.recover_device(context)
    }

    #[test]
    fn public_renderer_boundary_accepts_only_frame_updates_and_caller_owned_targets() {
        let _ = apply_update_contract;
        let _ = record_contract;
        let _ = recovery_contract;
    }
}
