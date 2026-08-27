mod gpu_fault;
mod pass;
mod remote_texture;

pub use gpu_fault::{GpuFaultClass, GpuFaultScope};
pub use remote_texture::{
    ApplyOutcome, PresentationReceipt, RecoveryRequirement, RemoteRenderer, RendererError,
};

pub(crate) use gpu_fault::GpuFaultObserver;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static NEXT_CONTEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GpuContextId(u64);

#[derive(Clone)]
pub struct GpuContext {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    observer: Arc<GpuFaultObserver>,
    context_id: GpuContextId,
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
        Ok(Self::from_parts(instance, adapter, device, queue))
    }

    pub fn from_parts(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
    ) -> Self {
        let observer = Arc::new(GpuFaultObserver::new());
        let uncaptured_observer = observer.clone();
        device.on_uncaptured_error(Arc::new(move |error| {
            uncaptured_observer.record(GpuFaultClass::from_wgpu_error(&error));
        }));
        let lost_observer = observer.clone();
        device.set_device_lost_callback(move |_reason, _diagnostic| {
            lost_observer.record(GpuFaultClass::DeviceLost);
        });
        Self {
            instance,
            adapter,
            device,
            queue,
            observer,
            context_id: GpuContextId(NEXT_CONTEXT_ID.fetch_add(1, Ordering::Relaxed)),
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

    pub fn begin_fault_scope(&self) -> Result<GpuFaultScope, GpuFaultClass> {
        GpuFaultScope::new(self.device.clone(), self.observer.clone())
    }

    pub fn observed_fault(&self) -> Option<GpuFaultClass> {
        self.observer.current()
    }

    pub fn observe_fault(&self, fault: GpuFaultClass) {
        self.observer.record(fault);
    }

    pub fn is_same_context(&self, other: &Self) -> bool {
        self.context_id == other.context_id && Arc::ptr_eq(&self.observer, &other.observer)
    }

    pub fn context_id(&self) -> GpuContextId {
        self.context_id
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

    fn create_renderer_contract(context: GpuContext) -> Result<RemoteRenderer, RendererError> {
        RemoteRenderer::new(context)
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
    ) -> Result<Option<RecoveryRequirement>, RendererError> {
        renderer.recover_device(context)
    }

    #[test]
    fn public_renderer_boundary_accepts_only_frame_updates_and_caller_owned_targets() {
        let _ = apply_update_contract;
        let _ = create_renderer_contract;
        let _ = record_contract;
        let _ = recovery_contract;
    }
}

#[cfg(test)]
mod fault_observer_tests {
    use super::{GpuFaultClass, GpuFaultObserver};

    fn source(label: &'static str) -> wgpu::ErrorSource {
        Box::new(std::io::Error::other(label))
    }

    #[test]
    fn observer_exposes_only_stable_fault_classes_and_monotonic_epochs() {
        let observer = GpuFaultObserver::new();
        let operation_epoch = observer.begin_operation().unwrap();

        observer.record(GpuFaultClass::Validation);

        assert_eq!(
            observer.fault_since(operation_epoch),
            Some(GpuFaultClass::Validation)
        );
        assert_eq!(observer.begin_operation(), Err(GpuFaultClass::Validation));
        assert_eq!(observer.epoch(), 1);
    }

    #[test]
    fn wgpu_errors_and_device_loss_map_to_redacted_stable_classifications() {
        let validation = wgpu::Error::Validation {
            source: source("validation-source-secret"),
            description: "validation-description-secret".to_owned(),
        };
        let out_of_memory = wgpu::Error::OutOfMemory {
            source: source("oom-source-secret"),
        };
        let internal = wgpu::Error::Internal {
            source: source("internal-source-secret"),
            description: "internal-description-secret".to_owned(),
        };

        assert_eq!(
            GpuFaultClass::from_wgpu_error(&validation),
            GpuFaultClass::Validation
        );
        assert_eq!(
            GpuFaultClass::from_wgpu_error(&out_of_memory),
            GpuFaultClass::OutOfMemory
        );
        assert_eq!(
            GpuFaultClass::from_wgpu_error(&internal),
            GpuFaultClass::Internal
        );

        let observer = GpuFaultObserver::new();
        observer.record(GpuFaultClass::DeviceLost);
        observer.record(GpuFaultClass::Validation);
        assert_eq!(observer.current(), Some(GpuFaultClass::DeviceLost));
        assert_eq!(format!("{:?}", observer.current()), "Some(DeviceLost)");
    }
}
