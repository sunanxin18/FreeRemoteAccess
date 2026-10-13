mod gpu_fault;
mod pass;
mod remote_texture;
mod transaction_state;
mod video_texture;

pub use frd_render_state::{
    BatchApplyOutcome, FrameBatchIdentity, InstalledSurface, PresentationReceipt,
    RecoveryRequirement,
};
pub use gpu_fault::{
    complete_scope_before_resuming_unwind, GpuCleanToken, GpuFaultClass, GpuFaultScope,
    GpuScopeObservation,
};
pub use remote_texture::{
    BatchApplyFailure, BatchApplySuccess, BatchScopeDiagnostics, ConfirmedPresentation,
    RemoteRenderer, RendererError,
};
pub use video_texture::{
    ConfirmedVideoPresentation, VideoColorSelection, VideoFrameLayout, VideoPresentationReceipt,
    VideoRenderer, VideoRendererError, VideoStreamEpoch, VideoUploadOutcome,
};

pub(crate) use gpu_fault::{
    AtomicScopeLifecycleObserver, GpuFaultObserver, ScopeLifecycleObserver,
};

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
    scope_observer: Arc<AtomicScopeLifecycleObserver>,
    context_id: GpuContextId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GpuContextError {
    AdapterUnavailable,
    DeviceUnavailable,
}

/// 设备创建的诊断仅供 composition root 使用；原 request 仍返回稳定错误分类。
#[derive(Debug)]
pub enum GpuContextRequestError {
    Adapter(wgpu::RequestAdapterError),
    Device(wgpu::RequestDeviceError),
}

impl GpuContext {
    pub async fn request(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self, GpuContextError> {
        Self::request_with_limits(instance, compatible_surface, |_| wgpu::Limits::default())
            .await
            .map_err(|error| match error {
                GpuContextRequestError::Adapter(_) => GpuContextError::AdapterUnavailable,
                GpuContextRequestError::Device(_) => GpuContextError::DeviceUnavailable,
            })
    }

    /// 调用方显式选择设备 limits，仍用真实 surface 选择 adapter，且沿用同一
    /// GPU fault/device-lost 观察路径。策略不改变桌面默认 request 的行为。
    pub async fn request_with_limits(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'_>>,
        limits: impl FnOnce(wgpu::Limits) -> wgpu::Limits,
    ) -> Result<Self, GpuContextRequestError> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface,
                ..Default::default()
            })
            .await
            .map_err(GpuContextRequestError::Adapter)?;
        let required_limits = limits(adapter.limits());
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("FreeRemoteDesk wgpu device"),
                required_limits,
                ..Default::default()
            })
            .await
            .map_err(GpuContextRequestError::Device)?;
        Ok(Self::from_parts(instance, adapter, device, queue))
    }

    pub fn from_parts(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
    ) -> Self {
        let observer = Arc::new(GpuFaultObserver::new());
        let scope_observer = Arc::new(AtomicScopeLifecycleObserver::default());
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
            scope_observer,
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

    /// GPU 错误作用域借用创建它的上下文，不能把设备所有权延长到上下文之外。
    ///
    /// ```compile_fail
    /// fn scope_cannot_outlive_context(context: frd_render_wgpu::GpuContext) {
    ///     let scope = context.begin_fault_scope().unwrap();
    ///     drop(context);
    ///     scope.finish().unwrap();
    /// }
    /// ```
    pub fn begin_fault_scope(&self) -> Result<GpuFaultScope<'_>, GpuFaultClass> {
        GpuFaultScope::new(
            &self.device,
            self.observer.as_ref(),
            self.scope_observer.as_ref(),
            self.context_id,
        )
    }

    pub fn scope_observation(&self) -> GpuScopeObservation {
        self.scope_observer.snapshot()
    }

    pub fn commit_if_unchanged<R>(
        &self,
        token: GpuCleanToken,
        commit: impl FnOnce() -> R,
    ) -> Result<R, GpuFaultClass> {
        // 临界区只允许最终 CPU 所有权/状态移动；调用点不得在闭包内执行 wgpu API，
        // 被替换的 GPU handle 必须由闭包返回并在锁释放后析构。
        self.observer
            .commit_if_unchanged(self.context_id, token, commit)
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
    use frd_frame::FrameTransaction;

    use super::{
        BatchApplyFailure, BatchApplySuccess, GpuContext, RecoveryRequirement, RemoteRenderer,
        RendererError,
    };

    fn apply_update_batch_contract(
        renderer: &mut RemoteRenderer,
        transactions: Vec<FrameTransaction>,
    ) -> Result<BatchApplySuccess, BatchApplyFailure> {
        renderer.apply_update_batch(transactions)
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
    fn public_renderer_boundary_accepts_only_transaction_batches_and_caller_owned_targets() {
        let _ = apply_update_batch_contract;
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

        assert_eq!(observer.current(), Some(GpuFaultClass::Validation));
        assert_eq!(observer.begin_operation(), Err(GpuFaultClass::Validation));
        assert!(observer.epoch() > operation_epoch);
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

#[cfg(all(test, target_os = "windows"))]
mod dx12_scope_smoke_tests {
    use std::sync::Arc;

    use super::{GpuContext, GpuScopeObservation};

    async fn dx12_context(label: &'static str) -> Option<GpuContext> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::DX12;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
                ..Default::default()
            })
            .await
        {
            Ok(adapter) => adapter,
            Err(_) => {
                println!("SKIP adapter_unavailable");
                return None;
            }
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some(label),
                ..Default::default()
            })
            .await
            .expect("DX12 adapter must create the smoke-test device");
        Some(GpuContext::from_parts(instance, adapter, device, queue))
    }

    #[test]
    fn dx12_scope_observation_smoke_reports_real_begin_finish_poll() {
        pollster::block_on(async {
            let Some(context) = dx12_context("FreeRemoteDesk scope observation smoke").await else {
                return;
            };
            let before = context.scope_observation();
            context
                .begin_fault_scope()
                .expect("scope acquisition must succeed")
                .finish()
                .expect("scope validation must succeed");
            assert_eq!(
                context.scope_observation().checked_delta(before),
                Some(GpuScopeObservation {
                    begins: 1,
                    finishes: 1,
                    polls: 1,
                })
            );
        });
    }

    #[test]
    fn dx12_scope_borrows_context_observers_without_retaining_handles() {
        pollster::block_on(async {
            let Some(context) = dx12_context("FreeRemoteDesk borrowed scope smoke").await else {
                return;
            };
            let before = (
                Arc::strong_count(&context.observer),
                Arc::strong_count(&context.scope_observer),
            );

            let scope = context
                .begin_fault_scope()
                .expect("scope acquisition must succeed");
            assert_eq!(
                (
                    Arc::strong_count(&context.observer),
                    Arc::strong_count(&context.scope_observer),
                ),
                before,
                "scope begin must borrow stable observers instead of cloning their handles",
            );

            let token = scope.finish().expect("scope validation must succeed");
            assert_eq!(
                (
                    Arc::strong_count(&context.observer),
                    Arc::strong_count(&context.scope_observer),
                ),
                before,
                "clean token must not retain an observer handle",
            );
            context
                .commit_if_unchanged(token, || ())
                .expect("clean token must commit against its original context");
        });
    }
}

#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
mod limits_request_tests {
    use super::GpuContext;
    fn instance() -> wgpu::Instance {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::METAL;
        wgpu::Instance::new(descriptor)
    }
    #[test]
    fn transformed_letterbox_and_fractional_negative_pan_are_pixel_bounded() {
        use frd_core::{ContentViewport, PixelRect, PixelSize, SessionId, TransformedViewport};
        use frd_frame::{
            FrameCompleteness, FrameReset, FrameRevision, FrameTransaction, PixelBuffer,
            PixelFormat, PixelPatch,
        };
        let context = pollster::block_on(GpuContext::request(instance(), None)).unwrap();
        let mut renderer = super::RemoteRenderer::new(context.clone()).unwrap();
        let session = SessionId::allocate();
        let remote = PixelSize::new(4, 2).unwrap();
        renderer
            .apply_update_batch(vec![FrameTransaction::Startup {
                earliest_constituent_enqueue_at: std::time::Instant::now(),
                reset: FrameReset {
                    session_id: session,
                    generation: 1,
                    size: remote,
                    format: PixelFormat::Bgrx8UnormSrgb,
                },
                revision: FrameRevision {
                    session_id: session,
                    generation: 1,
                    revision: 1,
                    completeness: FrameCompleteness::FullBaseline,
                    patches: vec![PixelPatch {
                        rect: PixelRect {
                            x: 0,
                            y: 0,
                            width: 4,
                            height: 2,
                        },
                        stride_bytes: 16,
                        pixels: PixelBuffer::new([0, 0, 255, 0].repeat(8)),
                    }],
                },
            }])
            .unwrap();
        let drawable = PixelSize::new(8, 8).unwrap();
        let base = ContentViewport::fit(remote, drawable);
        let full = PixelRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        };
        let variants = [
            TransformedViewport::new(base, full, 1.0, [0.0; 2]).unwrap(),
            TransformedViewport::new(
                base,
                PixelRect {
                    x: 2,
                    y: 1,
                    width: 4,
                    height: 6,
                },
                1.5,
                [-2.25, 0.25],
            )
            .unwrap(),
        ];
        // 每种几何连续绘制两次，覆盖同一顶点缓存复用及下一种几何替换。
        for viewport in variants
            .into_iter()
            .flat_map(|viewport| [viewport, viewport])
        {
            let target = context.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("远端 letterbox 像素回读"),
                size: wgpu::Extent3d {
                    width: 8,
                    height: 8,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = target.create_view(&Default::default());
            let readback = context.device().create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 256 * 8,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = context.device().create_command_encoder(&Default::default());
            renderer
                .record_transformed(
                    &mut encoder,
                    &view,
                    Some(viewport),
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                )
                .unwrap();
            encoder.copy_texture_to_buffer(
                target.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(8),
                    },
                },
                wgpu::Extent3d {
                    width: 8,
                    height: 8,
                    depth_or_array_layers: 1,
                },
            );
            context.queue().submit([encoder.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
            context
                .device()
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            rx.recv().unwrap().unwrap();
            let bytes = readback.slice(..).get_mapped_range().unwrap();
            for y in 0..8 {
                for x in 0..8 {
                    let visible = viewport
                        .map_pointer(x as f32 + 0.5, y as f32 + 0.5)
                        .is_some();
                    let expected = if visible {
                        [255, 0, 0, 255]
                    } else {
                        [0, 0, 0, 255]
                    };
                    assert_eq!(
                        &bytes[y * 256 + x * 4..y * 256 + x * 4 + 4],
                        &expected,
                        "pixel ({x},{y}), viewport {viewport:?}"
                    );
                }
            }
            drop(bytes);
            readback.unmap();
        }
    }
    #[test]
    fn limits_policy_rejects_impossible_texture_size_before_renderer_creation() {
        let result = pollster::block_on(GpuContext::request_with_limits(
            instance(),
            None,
            |available| {
                let mut requested =
                    wgpu::Limits::downlevel_webgl2_defaults().using_resolution(available);
                requested.max_texture_dimension_2d = u32::MAX;
                requested
            },
        ));
        assert!(result.is_err(), "设备必须执行调用方指定的 limits");
    }
    #[test]
    fn downlevel_device_keeps_existing_renderer_and_fault_observers() {
        let context = pollster::block_on(GpuContext::request_with_limits(
            instance(),
            None,
            |available| {
                wgpu::Limits::downlevel_webgl2_defaults()
                    .using_resolution(available.clone())
                    .using_alignment(available)
            },
        ))
        .expect("Metal 测试 adapter 必须提供 downlevel renderer limits");
        assert!(super::RemoteRenderer::new(context.clone()).is_ok());
        assert!(context.observed_fault().is_none());
        assert!(context.begin_fault_scope().unwrap().finish().is_ok());
    }
}
