use std::sync::Arc;
use std::time::Instant;

use frd_compositor_wgpu::{
    PresentationCompositor, PresentationHooks, PresentationSurface, PresentationSurfaceLease,
};
use frd_core::{PixelRect, PixelSize, SessionId};
use frd_frame::{
    FrameCompleteness, FrameReset, FrameRevision, FrameTransaction, PixelBuffer, PixelFormat,
    PixelPatch,
};
use frd_render_wgpu::RemoteRenderer;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

fn smoke_transactions(session_id: SessionId) -> Vec<FrameTransaction> {
    vec![FrameTransaction::Startup {
        earliest_constituent_enqueue_at: Instant::now(),
        reset: FrameReset {
            session_id,
            generation: 1,
            size: PixelSize::new(2, 2).expect("smoke texture geometry is non-zero"),
            format: PixelFormat::Bgrx8UnormSrgb,
        },
        revision: FrameRevision {
            session_id,
            generation: 1,
            revision: 1,
            patches: vec![PixelPatch {
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                },
                stride_bytes: 8,
                pixels: PixelBuffer::new(vec![
                    0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0,
                ]),
            }],
            completeness: FrameCompleteness::FullBaseline,
        },
    }]
}

struct WindowPresentationHook(Arc<Window>);

impl PresentationHooks for WindowPresentationHook {
    fn before_submit(&self) {
        self.0.pre_present_notify();
    }
}

struct SmokeState {
    window: Arc<Window>,
    renderer: RemoteRenderer,
    compositor: PresentationCompositor,
}

#[derive(Default)]
struct SmokeApplication {
    state: Option<SmokeState>,
}

impl SmokeApplication {
    fn initialize(event_loop: &ActiveEventLoop) -> Result<SmokeState, String> {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("FreeRemoteDesk wgpu 2x2 BGRX smoke")
                        .with_inner_size(LogicalSize::new(800.0, 600.0))
                        .with_resizable(true),
                )
                .map_err(|error| format!("window_create:{error}"))?,
        );
        let physical = window.inner_size();
        let physical_size = PixelSize::new(physical.width, physical.height)
            .ok_or_else(|| "window_zero_size".to_owned())?;

        let mut instance_descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        instance_descriptor.backends = wgpu::Backends::DX12;
        let instance = wgpu::Instance::new(instance_descriptor);
        let lease = PresentationSurfaceLease::new(window.clone());
        let surface = PresentationSurface::create(&instance, lease)
            .map_err(|error| format!("surface_create:{error:?}"))?;
        let context = pollster::block_on(surface.request_gpu_context(instance))
            .map_err(|error| format!("gpu_context:{error:?}"))?;
        let compositor = PresentationCompositor::new(surface, context.clone(), physical_size)
            .map_err(|error| format!("compositor:{error:?}"))?;
        let mut renderer =
            RemoteRenderer::new(context).map_err(|error| format!("renderer_create:{error:?}"))?;
        renderer
            .apply_update_batch(smoke_transactions(SessionId::allocate()))
            .map_err(|error| format!("fixture_upload:{error:?}"))?;

        Ok(SmokeState {
            window,
            renderer,
            compositor,
        })
    }
}

impl ApplicationHandler for SmokeApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match Self::initialize(event_loop) {
            Ok(state) => {
                state.window.request_redraw();
                self.state = Some(state);
            }
            Err(error) => {
                eprintln!("wgpu smoke 初始化失败：{error}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if window_id != state.window.id() {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(size) = PixelSize::new(size.width, size.height) {
                    if let Err(error) = state.compositor.resize(size) {
                        eprintln!("wgpu smoke 调整尺寸失败：{error:?}");
                        event_loop.exit();
                        return;
                    }
                    state.window.request_redraw();
                } else {
                    state.compositor.pause_presenting();
                }
            }
            WindowEvent::RedrawRequested => {
                let hook = WindowPresentationHook(state.window.clone());
                match state
                    .compositor
                    .render(&mut state.renderer, |_encoder, _target| {}, &hook)
                {
                    Ok(Some(_)) => state
                        .window
                        .set_title("FreeRemoteDesk wgpu smoke — 已呈现 2x2 BGRX"),
                    Ok(None) => {}
                    Err(error) => {
                        eprintln!("wgpu smoke 呈现失败：{error:?}");
                        event_loop.exit();
                    }
                }
            }
            WindowEvent::Occluded(false) => state.window.request_redraw(),
            _ => {}
        }
    }
}

fn main() -> Result<(), winit::error::EventLoopError> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop.run_app(&mut SmokeApplication::default())
}

#[cfg(test)]
mod tests {
    use frd_core::SessionId;
    use frd_frame::{FrameCompleteness, FrameTransaction, PixelFormat};

    use super::smoke_transactions;

    #[test]
    fn fixture_is_one_atomic_two_by_two_bgrx_red_green_blue_white_full_baseline_startup() {
        let session_id = SessionId::allocate();
        let transactions = smoke_transactions(session_id);
        assert_eq!(transactions.len(), 1);
        let FrameTransaction::Startup {
            reset, revision, ..
        } = &transactions[0]
        else {
            panic!("smoke fixture 必须是单个原子 startup transaction");
        };
        assert_eq!(reset.session_id, session_id);
        assert_eq!(reset.generation, 1);
        assert_eq!(reset.size.width, 2);
        assert_eq!(reset.size.height, 2);
        assert_eq!(reset.format, PixelFormat::Bgrx8UnormSrgb);
        assert_eq!(revision.session_id, session_id);
        assert_eq!(revision.generation, 1);
        assert_eq!(revision.revision, 1);
        assert_eq!(revision.completeness, FrameCompleteness::FullBaseline);
        assert_eq!(revision.patches.len(), 1);
        assert_eq!(revision.patches[0].stride_bytes, 8);
        assert_eq!(
            revision.patches[0].pixels.as_bytes(),
            &[0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0,]
        );
    }
}
