//! API 26 OHNativeWindow owned lease 与现有 compositor 的适配。
use crate::{worker::Backend, GPU_FAILED, WINDOW_REFERENCE_FAILED};
use frd_compositor_wgpu::{
    PresentationCompositor, PresentationHooks, PresentationSurface, PresentationSurfaceLease,
};
use frd_core::PixelSize;
use frd_render_wgpu::{GpuContext, RemoteRenderer};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, OhosDisplayHandle,
    OhosNdkWindowHandle, RawDisplayHandle, RawWindowHandle, WindowHandle,
};
use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::Arc,
    thread::{self, ThreadId},
};

#[link(name = "native_window")]
extern "C" {
    fn OH_NativeWindow_NativeObjectReference(object: *mut c_void) -> i32;
    fn OH_NativeWindow_NativeObjectUnreference(object: *mut c_void) -> i32;
}
#[link(name = "hilog_ndk.z")]
extern "C" {
    fn OH_LOG_Print(
        log_type: u32,
        level: u32,
        domain: u32,
        tag: *const std::ffi::c_char,
        format: *const std::ffi::c_char,
        ...
    ) -> i32;
}
fn gpu_error(stage: &str, error: impl std::fmt::Debug) -> i32 {
    // 只记录类型化图形错误；本模块不接收地址、协议或秘密。
    if let Ok(message) = std::ffi::CString::new(format!("{stage}: {error:?}")) {
        // SAFETY: 字符串在调用期有效，固定格式与唯一 vararg 类型匹配。
        unsafe {
            OH_LOG_Print(
                0,
                6,
                0xFF00,
                c"FrdOhos".as_ptr(),
                c"%{public}s".as_ptr(),
                message.as_ptr(),
            );
        }
    }
    GPU_FAILED
}
struct NativeWindowLease {
    pointer: NonNull<c_void>,
    worker: ThreadId,
}
// SAFETY: wgpu 的 OwnedWindowTarget 要求 Send+Sync，但本类型私有且只有 worker
// 创建/克隆/销毁 Arc，GPU/context/surface 从不离开该 worker。HasWindowHandle 只
// 返回地址，不调用 native API；所有 native/GPU 操作与 ref/unref 串行。
// 没有公开构造/克隆入口，没有 UI 或网络回调，worker 不等待 UI。Drop 最后
// 执行；compositor.detach 先销毁 surface，remote/context 清理后释放 lease。
// callback 借用在同步 attach ack 前有效；framework 自身行为不受本锁保护。
unsafe impl Send for NativeWindowLease {}
unsafe impl Sync for NativeWindowLease {}
impl NativeWindowLease {
    unsafe fn acquire(pointer: usize) -> Result<Arc<Self>, i32> {
        let pointer = NonNull::new(pointer as *mut c_void).ok_or(WINDOW_REFERENCE_FAILED)?;
        // SAFETY: attach 调用方保证窗口在同步 ack 前有效；只在 worker 调用一次。
        if unsafe { OH_NativeWindow_NativeObjectReference(pointer.as_ptr()) } != 0 {
            return Err(WINDOW_REFERENCE_FAILED);
        }
        Ok(Arc::new(Self {
            pointer,
            worker: thread::current().id(),
        }))
    }
}
impl HasDisplayHandle for NativeWindowLease {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // SAFETY: OHOS 无额外 display 指针，lease 的借用有效。
        Ok(unsafe { DisplayHandle::borrow_raw(RawDisplayHandle::Ohos(OhosDisplayHandle::new())) })
    }
}
impl HasWindowHandle for NativeWindowLease {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        if self.worker != thread::current().id() {
            return Err(HandleError::Unavailable);
        }
        // SAFETY: own ref 从 acquire 到此 lease 最后 drop 有效，所有使用在 worker。
        Ok(unsafe {
            WindowHandle::borrow_raw(RawWindowHandle::OhosNdk(OhosNdkWindowHandle::new(
                self.pointer,
            )))
        })
    }
}
impl Drop for NativeWindowLease {
    fn drop(&mut self) {
        // SAFETY: own ref 与 acquire 配对；最后访问只能发生在 worker 清理 surface 后。
        unsafe {
            OH_NativeWindow_NativeObjectUnreference(self.pointer.as_ptr());
        }
    }
}
struct Hooks(std::cell::Cell<bool>);
impl PresentationHooks for Hooks {
    fn before_submit(&self) {
        self.0.set(true);
    }
}
struct Resident {
    context: GpuContext,
    remote: RemoteRenderer,
}
struct Graphics {
    compositor: PresentationCompositor,
    resident: Option<Resident>,
    pipeline: wgpu::RenderPipeline,
    // 额外保留 lease，确保 remote/pipeline/context 全部销毁后才最终 unref。
    lease: Arc<NativeWindowLease>,
}
impl Graphics {
    fn create(
        lease: Arc<NativeWindowLease>,
        width: u32,
        height: u32,
        resident: Option<Resident>,
    ) -> Result<Self, i32> {
        let (surface, resident) = if let Some(resident) = resident {
            let surface = PresentationSurface::create(
                resident.context.instance(),
                PresentationSurfaceLease::new(lease.clone()),
            )
            .map_err(|_| GPU_FAILED)?;
            (surface, resident)
        } else {
            let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
            descriptor.backends = wgpu::Backends::GL;
            // 官方模拟器 debug EGL context 不可用；保留验证及 device-lost 观察。
            descriptor.flags.remove(wgpu::InstanceFlags::DEBUG);
            let instance = wgpu::Instance::new(descriptor);
            let surface = PresentationSurface::create(
                &instance,
                PresentationSurfaceLease::new(lease.clone()),
            )
            .map_err(|error| gpu_error("surface-create", error))?;
            let context = pollster::block_on(
                surface.request_gpu_context_with_limits(instance, crate::limits::renderer_limits),
            )
            .map_err(|error| gpu_error("gpu-request", error))?;
            let remote = RemoteRenderer::new(context.clone()).map_err(|_| GPU_FAILED)?;
            (surface, Resident { context, remote })
        };
        let context = &resident.context;
        let compositor =
            PresentationCompositor::new(surface, context.clone(), PixelSize { width, height })
                .map_err(|_| GPU_FAILED)?;
        // 明确的离线图形 POC；没有 SessionId、网络帧或伪造 FramePresented。
        let module = context
            .device()
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("仅测试：OHOS 四色图形 POC"),
                source: wgpu::ShaderSource::Wgsl(include_str!("poc.wgsl").into()),
            });
        let pipeline = context
            .device()
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("仅测试：OHOS 图形 POC"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: compositor.target_format().ok_or(GPU_FAILED)?,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            });
        Ok(Self {
            compositor,
            resident: Some(resident),
            pipeline,
            lease,
        })
    }
    fn detach(&mut self) {
        self.compositor.pause_presenting();
        self.compositor.detach();
    }
}
impl Drop for Graphics {
    fn drop(&mut self) {
        self.detach();
        let _ = &self.lease;
    }
}
#[derive(Default)]
pub(crate) struct OhosBackend {
    graphics: Option<Graphics>,
    retained: Option<Resident>,
}
impl Backend for OhosBackend {
    fn attach(&mut self, window: usize, width: u32, height: u32) -> Result<(), i32> {
        self.detach();
        // SAFETY: worker 的同步 attach 调用期间借用仍有效。
        let lease = unsafe { NativeWindowLease::acquire(window)? };
        self.graphics = Some(Graphics::create(
            lease,
            width,
            height,
            self.retained.take(),
        )?);
        Ok(())
    }
    fn resize(&mut self, width: u32, height: u32) -> Result<(), i32> {
        self.graphics
            .as_mut()
            .ok_or(GPU_FAILED)?
            .compositor
            .resize(PixelSize { width, height })
            .map_err(|_| GPU_FAILED)
    }
    fn pause(&mut self) {
        if let Some(graphics) = &mut self.graphics {
            graphics.compositor.pause_presenting();
        }
    }
    fn render_poc(&mut self) -> Result<bool, i32> {
        let graphics = self.graphics.as_mut().ok_or(GPU_FAILED)?;
        let pipeline = &graphics.pipeline;
        let hooks = Hooks(std::cell::Cell::new(false));
        graphics
            .compositor
            .render(
                &mut graphics.resident.as_mut().ok_or(GPU_FAILED)?.remote,
                |encoder, view| {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("仅测试：确定性四色"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    pass.set_pipeline(pipeline);
                    pass.draw(0..3, 0..1);
                },
                &hooks,
            )
            .map_err(|_| GPU_FAILED)?;
        Ok(hooks.0.get())
    }
    fn detach(&mut self) {
        if let Some(mut graphics) = self.graphics.take() {
            graphics.detach();
            self.retained = graphics.resident.take();
            // surface 与 lease 在此线程销毁，保留的 context/texture 不持有 native window。
        }
    }
    fn reset_remote(&mut self) {
        if let Some(resident) = self.resident_mut() {
            resident.remote.detach();
        }
    }
    fn apply(&mut self, transactions: Vec<frd_frame::FrameTransaction>) -> Result<(), i32> {
        self.resident_mut()
            .ok_or(GPU_FAILED)?
            .remote
            .apply_update_batch(transactions)
            .map_err(|_| GPU_FAILED)?;
        Ok(())
    }
    fn render_remote(
        &mut self,
        viewport: frd_core::TransformedViewport,
        repaint: bool,
    ) -> Result<Option<frd_protocol_api::PresentationEvent>, i32> {
        let graphics = self.graphics.as_mut().ok_or(GPU_FAILED)?;
        let remote = &mut graphics.resident.as_mut().ok_or(GPU_FAILED)?.remote;
        if repaint {
            remote.request_repaint();
        }
        graphics
            .compositor
            .render_transformed(
                remote,
                Some(viewport),
                |_, _| {},
                &Hooks(std::cell::Cell::new(false)),
            )
            .map_err(|_| GPU_FAILED)
    }
}
impl OhosBackend {
    fn resident_mut(&mut self) -> Option<&mut Resident> {
        if let Some(graphics) = self.graphics.as_mut() {
            graphics.resident.as_mut()
        } else {
            self.retained.as_mut()
        }
    }
}
