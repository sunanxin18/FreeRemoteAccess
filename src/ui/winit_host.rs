use std::error::Error;
use std::fmt;
use std::sync::Arc;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Fullscreen, Window, WindowId};

use crate::platform::{PlatformError, SurfaceHandle, WindowHost};

use super::{connection_view, system_fonts, FreeRemoteApplication, Renderer, UiPage};

const INITIAL_WIDTH: f64 = 1080.0;
const INITIAL_HEIGHT: f64 = 720.0;

#[derive(Clone)]
pub struct WinitHost {
    window: Arc<Window>,
}

impl WinitHost {
    fn new(window: Arc<Window>) -> Self {
        Self { window }
    }

    pub fn window(&self) -> &Arc<Window> {
        &self.window
    }
}

impl WindowHost for WinitHost {
    fn request_redraw(&self) -> Result<(), PlatformError> {
        self.window.request_redraw();
        Ok(())
    }

    fn surface_handle(&self) -> Result<SurfaceHandle<'_>, PlatformError> {
        Ok(SurfaceHandle {
            window: self
                .window
                .window_handle()
                .map_err(|_| PlatformError::new("window_handle_unavailable"))?,
            display: self
                .window
                .display_handle()
                .map_err(|_| PlatformError::new("display_handle_unavailable"))?,
        })
    }

    fn set_fullscreen(&self, enabled: bool) -> Result<(), PlatformError> {
        self.window
            .set_fullscreen(enabled.then(|| Fullscreen::Borderless(self.window.current_monitor())));
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum DesktopEvent {
    Repaint,
}

struct DesktopApplication {
    host: Option<WinitHost>,
    renderer: Option<Renderer>,
    egui_context: egui::Context,
    egui_state: Option<egui_winit::State>,
    model: FreeRemoteApplication,
    message: Option<String>,
    startup_error: Option<DesktopError>,
}

impl DesktopApplication {
    fn new(egui_context: egui::Context) -> Self {
        Self {
            host: None,
            renderer: None,
            egui_context,
            egui_state: None,
            model: FreeRemoteApplication::default(),
            message: None,
            startup_error: None,
        }
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> Result<(), DesktopError> {
        let attributes = Window::default_attributes()
            .with_title("FreeRemoteAccess")
            .with_inner_size(LogicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT))
            .with_min_inner_size(LogicalSize::new(720.0, 480.0));
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|_| DesktopError::new("window_create_failed"))?,
        );
        let state = egui_winit::State::new(
            self.egui_context.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        let renderer = pollster::block_on(Renderer::new(window.clone()))
            .map_err(|_| DesktopError::new("renderer_create_failed"))?;
        self.host = Some(WinitHost::new(window));
        self.egui_state = Some(state);
        self.renderer = Some(renderer);
        Ok(())
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(host) = self.host.as_ref() else {
            return;
        };
        let Some(state) = self.egui_state.as_mut() else {
            return;
        };
        let input = state.take_egui_input(host.window());
        let mut connect_clicked = false;
        let mut back_clicked = false;
        let message = self.message.clone();
        let page = self.model.page();
        let output = self.egui_context.run_ui(input, |root_ui| match page {
            UiPage::Connection => {
                egui::CentralPanel::default().show(root_ui, |ui| {
                    ui.vertical_centered_justified(|ui| {
                        ui.add_space(36.0);
                        ui.set_max_width(520.0);
                        connect_clicked =
                            connection_view::show(ui, self.model.connection_form_mut());
                        if let Some(message) = message.as_deref() {
                            ui.colored_label(egui::Color32::from_rgb(235, 100, 100), message);
                        }
                    });
                });
            }
            UiPage::Connecting => {
                egui::CentralPanel::default().show(root_ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(120.0);
                        ui.spinner();
                        ui.heading("正在连接原生远程桌面服务…");
                        ui.label("协议适配器正在建立加密会话");
                        back_clicked = ui.button("取消").clicked();
                    });
                });
            }
            UiPage::Session => {
                egui::Panel::top("session-toolbar").show(root_ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("已连接");
                        back_clicked = ui.button("断开连接").clicked();
                    });
                });
            }
        });

        if connect_clicked {
            match self.model.submit_connection() {
                Ok(action) => {
                    drop(action);
                    self.message = Some("协议适配器尚未启动".to_owned());
                }
                Err(error) => self.message = Some(error.to_string()),
            }
        }
        if back_clicked {
            self.model.show_connection();
            self.message = None;
        }

        let mut output = output;
        state.handle_platform_output_with_event_loop(
            host.window(),
            event_loop,
            std::mem::take(&mut output.platform_output),
        );
        if let Some(renderer) = self.renderer.as_mut() {
            if let Err(error) = renderer.render_egui(&self.egui_context, output) {
                self.message = Some(error.to_string());
                if error.code() == "surface_lost" {
                    match pollster::block_on(Renderer::new(host.window().clone())) {
                        Ok(renderer) => self.renderer = Some(renderer),
                        Err(_) => {
                            self.startup_error = Some(DesktopError::new("surface_recovery_failed"));
                            event_loop.exit();
                        }
                    }
                }
            }
        }
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.resize(size);
        }
    }
}

impl ApplicationHandler<DesktopEvent> for DesktopApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.host.is_none() {
            if let Err(error) = self.create_window(event_loop) {
                self.startup_error = Some(error);
                event_loop.exit();
                return;
            }
        }
        event_loop.set_control_flow(ControlFlow::Wait);
        if let Some(host) = &self.host {
            host.window().request_redraw();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(host) = self.host.as_ref() else {
            return;
        };
        let window = host.window().clone();
        if host.window().id() != window_id {
            return;
        }
        if let Some(state) = self.egui_state.as_mut() {
            let response = state.on_window_event(host.window(), &event);
            if response.repaint {
                host.window().request_redraw();
            }
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.resize(size);
                window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.resize(window.inner_size());
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => self.redraw(event_loop),
            _ => {}
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: DesktopEvent) {
        if let Some(host) = &self.host {
            host.window().request_redraw();
        }
    }
}

pub fn run_desktop() -> Result<(), DesktopError> {
    let event_loop = EventLoop::<DesktopEvent>::with_user_event()
        .build()
        .map_err(|_| DesktopError::new("event_loop_create_failed"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let context = egui::Context::default();
    system_fonts::install_cjk_fallback(&context);
    context.set_request_repaint_callback(move |request| {
        if request.viewport_id == egui::ViewportId::ROOT {
            let _ = proxy.send_event(DesktopEvent::Repaint);
        }
    });
    let mut application = DesktopApplication::new(context);
    event_loop
        .run_app(&mut application)
        .map_err(|_| DesktopError::new("event_loop_failed"))?;
    if let Some(error) = application.startup_error {
        return Err(error);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopError {
    code: &'static str,
}

impl DesktopError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Display for DesktopError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "桌面客户端启动失败 ({})", self.code)
    }
}

impl Error for DesktopError {}
