use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::platform::scancode::PhysicalKeyExtScancode as _;
use winit::window::{Fullscreen, Window, WindowId};

use crate::core::RemoteViewportTransform;
use crate::platform::{PlatformError, SurfaceHandle, WindowHost};
use crate::protocols::adapter_for;
use crate::session::{
    ProtocolContext, SessionCommand, SessionEngine, SessionError, SessionEvent, SessionModel,
    SessionPhase, UiWakeHandle,
};

use super::{
    connection_view, system_fonts, FreeRemoteApplication, GpuFailureGate, RenderOutcome, Renderer,
    RendererFailureAction, UiAction, UiPage,
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceAcquireSchedule {
    Acquire,
    WaitUntil(Instant),
    WaitForVisibility,
}

/// This is the production gate in front of `Renderer::render_egui`, not merely
/// an event-loop hint. It prevents unrelated session, input, or egui wakes from
/// acquiring a surface before the recovery deadline.
#[derive(Debug, Default)]
struct SurfaceAcquireScheduler {
    retry_not_before: Option<Instant>,
    pending_repaint: bool,
    waiting_for_visibility: bool,
}

impl SurfaceAcquireScheduler {
    fn schedule_retry(&mut self, deadline: Instant) {
        self.retry_not_before = Some(deadline);
        self.pending_repaint = true;
    }

    fn wait_for_visibility(&mut self) {
        self.waiting_for_visibility = true;
        self.pending_repaint = true;
    }

    fn on_successful_render(&mut self) {
        self.retry_not_before = None;
        self.pending_repaint = false;
        self.waiting_for_visibility = false;
    }

    fn on_external_wake(&mut self) -> SurfaceAcquireSchedule {
        self.pending_repaint = true;
        self.current_schedule()
    }

    fn on_redraw_requested(&mut self, _now: Instant) -> SurfaceAcquireSchedule {
        if let Some(deadline) = self.retry_not_before {
            self.pending_repaint = true;
            return SurfaceAcquireSchedule::WaitUntil(deadline);
        }
        if self.waiting_for_visibility {
            self.pending_repaint = true;
            return SurfaceAcquireSchedule::WaitForVisibility;
        }
        self.pending_repaint = false;
        SurfaceAcquireSchedule::Acquire
    }

    fn on_resume_time_reached(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.retry_not_before else {
            return false;
        };
        if now < deadline {
            return false;
        }
        self.retry_not_before = None;
        if self.waiting_for_visibility || !self.pending_repaint {
            return false;
        }
        self.pending_repaint = false;
        true
    }

    fn on_visibility_restored(&mut self) -> SurfaceAcquireSchedule {
        self.waiting_for_visibility = false;
        self.pending_repaint = true;
        self.current_schedule()
    }

    fn current_schedule(&self) -> SurfaceAcquireSchedule {
        if let Some(deadline) = self.retry_not_before {
            SurfaceAcquireSchedule::WaitUntil(deadline)
        } else if self.waiting_for_visibility {
            SurfaceAcquireSchedule::WaitForVisibility
        } else {
            SurfaceAcquireSchedule::Acquire
        }
    }
}

struct EventLoopWake {
    proxy: winit::event_loop::EventLoopProxy<DesktopEvent>,
}

impl UiWakeHandle for EventLoopWake {
    fn wake(&self) -> Result<(), SessionError> {
        self.proxy
            .send_event(DesktopEvent::Repaint)
            .map_err(|_| SessionError::new("desktop_event_loop_closed"))
    }
}

struct DesktopApplication {
    host: Option<WinitHost>,
    renderer: Option<Renderer>,
    egui_context: egui::Context,
    egui_state: Option<egui_winit::State>,
    model: FreeRemoteApplication,
    session_model: SessionModel,
    session_engine: Option<SessionEngine>,
    wake: Arc<dyn UiWakeHandle>,
    message: Option<String>,
    startup_error: Option<DesktopError>,
    pointer_buttons: u8,
    cursor_position: Option<(f64, f64)>,
    disconnect_requested: bool,
    gpu_failure_latch: GpuFailureGate,
    surface_scheduler: SurfaceAcquireScheduler,
}

impl DesktopApplication {
    fn new(egui_context: egui::Context, wake: Arc<dyn UiWakeHandle>) -> Self {
        Self {
            host: None,
            renderer: None,
            egui_context,
            egui_state: None,
            model: FreeRemoteApplication::default(),
            session_model: SessionModel::default(),
            session_engine: None,
            wake,
            message: None,
            startup_error: None,
            pointer_buttons: 0,
            cursor_position: None,
            disconnect_requested: false,
            gpu_failure_latch: GpuFailureGate::default(),
            surface_scheduler: SurfaceAcquireScheduler::default(),
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
        self.drain_session_events(event_loop);
        match self.surface_scheduler.on_redraw_requested(Instant::now()) {
            SurfaceAcquireSchedule::Acquire => {}
            schedule => {
                self.apply_surface_schedule(event_loop, schedule);
                return;
            }
        }
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
                        if let Some(message) = message.as_deref() {
                            ui.colored_label(egui::Color32::from_rgb(235, 100, 100), message);
                        }
                        back_clicked = ui.button("取消").clicked();
                    });
                });
            }
            UiPage::Session => {
                egui::Panel::top("session-toolbar").show(root_ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("已连接到原生远程桌面服务");
                        back_clicked = ui.button("断开连接").clicked();
                    });
                    if let Some(message) = message.as_deref() {
                        ui.colored_label(egui::Color32::from_rgb(235, 100, 100), message);
                    }
                });
            }
        });

        let mut output = output;
        state.handle_platform_output_with_event_loop(
            host.window(),
            event_loop,
            std::mem::take(&mut output.platform_output),
        );
        let recovery_window = host.window().clone();
        let render_result = self
            .renderer
            .as_mut()
            .map(|renderer| renderer.render_egui(&self.egui_context, output));
        if let Some(result) = render_result {
            match result {
                Ok(RenderOutcome::RetryAt(deadline)) => {
                    self.surface_scheduler.schedule_retry(deadline);
                    self.apply_surface_schedule(
                        event_loop,
                        SurfaceAcquireSchedule::WaitUntil(deadline),
                    );
                }
                Ok(RenderOutcome::Rendered) => {
                    self.surface_scheduler.on_successful_render();
                    event_loop.set_control_flow(ControlFlow::Wait);
                }
                Ok(RenderOutcome::WaitForVisibility) => {
                    self.surface_scheduler.wait_for_visibility();
                    event_loop.set_control_flow(ControlFlow::Wait);
                }
                Err(_) if self.gpu_failure_latch.blocks_session_progress() => {}
                Err(error) => {
                    self.fail_renderer_session(error, event_loop);
                    recovery_window.request_redraw();
                    return;
                }
            }
        }
        if self.gpu_failure_latch.blocks_session_progress() {
            return;
        }
        if connect_clicked {
            match self.model.submit_connection() {
                Ok(UiAction::Connect(connection)) => self.start_session(connection),
                Ok(_) => self.message = Some("连接操作无效".to_owned()),
                Err(error) => self.message = Some(error.to_string()),
            }
        }
        if back_clicked {
            if self.session_engine.is_some() {
                self.request_disconnect();
            } else {
                self.model.show_connection();
                self.message = None;
            }
        }
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.resize(size);
        }
        if !self.gpu_failure_latch.blocks_remote_input()
            && self.session_model.snapshot().phase() == SessionPhase::Connected
        {
            self.send_session_command(SessionCommand::Resize {
                width: size.width,
                height: size.height,
            });
        }
    }

    fn schedule_repaint(&mut self, event_loop: &ActiveEventLoop) {
        let schedule = self.surface_scheduler.on_external_wake();
        self.apply_surface_schedule(event_loop, schedule);
    }

    fn apply_surface_schedule(
        &self,
        event_loop: &ActiveEventLoop,
        schedule: SurfaceAcquireSchedule,
    ) {
        match schedule {
            SurfaceAcquireSchedule::Acquire => {
                if let Some(host) = &self.host {
                    host.window().request_redraw();
                }
            }
            SurfaceAcquireSchedule::WaitUntil(deadline) => {
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            }
            SurfaceAcquireSchedule::WaitForVisibility => {
                event_loop.set_control_flow(ControlFlow::Wait);
            }
        }
    }

    fn start_session(&mut self, connection: crate::app::connection::ValidatedConnection) {
        if !self.gpu_failure_latch.permits_new_session() {
            self.message = Some("图形会话仍在安全关闭中".to_owned());
            return;
        }
        if self.session_engine.is_some() {
            self.message = Some("现有会话尚未结束".to_owned());
            return;
        }
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.begin_authenticated_session();
        }
        let protocol = connection.protocol;
        let result = adapter_for(protocol).and_then(|adapter| {
            SessionEngine::spawn(
                adapter,
                ProtocolContext::with_platform_services(
                    connection,
                    crate::platform::production_platform_services(),
                ),
                self.wake.clone(),
            )
        });
        match result {
            Ok(engine) => {
                self.session_engine = Some(engine);
                self.session_model = SessionModel::default();
                self.message = None;
                self.disconnect_requested = false;
            }
            Err(error) => {
                self.model.show_connection();
                self.message = Some(error.to_string());
            }
        }
    }

    fn drain_session_events(&mut self, event_loop: &ActiveEventLoop) {
        let Some(engine) = self.session_engine.as_ref() else {
            return;
        };
        let mut pending = Vec::new();
        let mut channel_closed = false;
        loop {
            match engine.try_next_event() {
                Ok(Some(event)) => pending.push(event),
                Ok(None) => break,
                Err(_) => {
                    channel_closed = true;
                    break;
                }
            }
        }

        for event in pending {
            if self.gpu_failure_latch.blocks_session_progress() {
                continue;
            }
            if let SessionEvent::Render(update) = &event {
                if let Some(renderer) = self.renderer.as_mut() {
                    if let Err(error) = renderer.apply_update(update.clone()) {
                        self.message = Some(error.to_string());
                    }
                }
            }
            let failed_code = match &event {
                SessionEvent::Failed { code } => Some(*code),
                _ => None,
            };
            let disconnected = matches!(event, SessionEvent::Disconnected);
            let connected = matches!(event, SessionEvent::Connected { .. });
            if let Err(error) = self.session_model.apply(event) {
                self.message = Some(error.to_string());
            }
            if connected {
                self.model.show_session();
                self.message = None;
            } else if disconnected {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.finish_disconnected_session();
                }
                self.model.show_connection();
                self.message = None;
                self.disconnect_requested = false;
            } else if let Some(code) = failed_code {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.finish_failed_session();
                }
                self.model.show_connection();
                self.message = Some(format!("连接失败 ({code})"));
                self.disconnect_requested = false;
            }
        }

        if channel_closed {
            let expected_terminal = matches!(
                self.session_model.snapshot().phase(),
                SessionPhase::Idle | SessionPhase::Failed
            );
            let joined = self
                .session_engine
                .take()
                .map(SessionEngine::join)
                .transpose()
                .is_ok();
            self.disconnect_requested = false;
            if !joined {
                self.message = Some("图形会话线程清理失败，客户端已安全停止".to_owned());
                self.startup_error = Some(DesktopError::new("session_worker_join_failed"));
                event_loop.exit();
                return;
            }
            let gpu_failure = self
                .gpu_failure_latch
                .release_after_worker_completion(joined);
            if gpu_failure {
                self.model.show_connection();
            } else if !expected_terminal {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.finish_failed_session();
                }
                self.model.show_connection();
                self.message = Some("连接线程意外结束".to_owned());
            }
        }
        if self.startup_error.is_some() {
            event_loop.exit();
        }
    }

    fn send_session_command(&mut self, command: SessionCommand) {
        let result = match self.session_engine.as_ref() {
            Some(engine) => engine.send(command),
            None => Err(SessionError::new("session_not_running")),
        };
        if let Err(error) = result {
            self.message = Some(error.to_string());
            if command_failure_requires_disconnect(error) {
                self.request_disconnect();
            }
        }
    }

    fn request_disconnect(&mut self) {
        if self.disconnect_requested {
            return;
        }
        self.disconnect_requested = true;
        let result = self
            .session_engine
            .as_ref()
            .ok_or_else(|| SessionError::new("session_not_running"))
            .and_then(|engine| engine.send(SessionCommand::Disconnect));
        if let Err(error) = result {
            if !self.gpu_failure_latch.blocks_session_progress() {
                self.message = Some(error.to_string());
            }
        }
    }

    fn fail_renderer_session(&mut self, error: super::RenderError, event_loop: &ActiveEventLoop) {
        if self.gpu_failure_latch.blocks_session_progress() {
            return;
        }
        match self
            .gpu_failure_latch
            .on_terminal_renderer_error(error.code(), self.session_engine.is_some())
        {
            RendererFailureAction::ExitFailClosed => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.finish_failed_session();
                }
                self.message = Some(error.to_string());
                self.startup_error = Some(DesktopError::new("renderer_terminal_failure"));
                event_loop.exit();
                return;
            }
            RendererFailureAction::OrderlyDisconnect => {}
        }
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.finish_failed_session();
        }
        self.message = Some(error.to_string());
        self.request_disconnect();
    }

    fn remote_cursor_position(&self, window: &Window) -> Option<(u32, u32)> {
        let surface = self.session_model.snapshot().surface()?;
        let size = window.inner_size();
        let transform =
            RemoteViewportTransform::new((size.width, size.height), surface.dimensions(), 1.0)
                .ok()?;
        transform.remote_point(self.cursor_position?)
    }

    fn send_pointer(&mut self, window: &Window, buttons: u8) {
        if let Some((x, y)) = self.remote_cursor_position(window) {
            self.send_session_command(SessionCommand::Pointer { x, y, buttons });
        }
    }

    fn handle_remote_input(&mut self, window: &Window, event: &WindowEvent, consumed: bool) {
        if consumed
            || self.gpu_failure_latch.blocks_remote_input()
            || self.session_model.snapshot().phase() != SessionPhase::Connected
        {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = Some((position.x, position.y));
                self.send_pointer(window, self.pointer_buttons);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let bit = match button {
                    MouseButton::Left => 1,
                    MouseButton::Middle => 2,
                    MouseButton::Right => 4,
                    _ => 0,
                };
                if *state == ElementState::Pressed {
                    self.pointer_buttons |= bit;
                } else {
                    self.pointer_buttons &= !bit;
                }
                self.send_pointer(window, self.pointer_buttons);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (horizontal, vertical) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (f64::from(*x), f64::from(*y)),
                    MouseScrollDelta::PixelDelta(position) => (position.x, position.y),
                };
                let mut wheel = 0u8;
                if vertical > 0.0 {
                    wheel |= 8;
                } else if vertical < 0.0 {
                    wheel |= 16;
                }
                if horizontal > 0.0 {
                    wheel |= 32;
                } else if horizontal < 0.0 {
                    wheel |= 64;
                }
                if wheel != 0 {
                    self.send_pointer(window, self.pointer_buttons | wheel);
                    self.send_pointer(window, self.pointer_buttons);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let physical_code = event.physical_key.to_scancode();
                let keysym = winit_key_to_keysym(&event.logical_key);
                if physical_code.is_some() || keysym.is_some() {
                    self.send_session_command(SessionCommand::Key {
                        physical_code,
                        keysym,
                        pressed: event.state == ElementState::Pressed,
                    });
                }
            }
            _ => {}
        }
    }
}

fn command_failure_requires_disconnect(error: SessionError) -> bool {
    matches!(
        error.code(),
        "session_command_channel_full"
            | "session_command_channel_closed"
            | "session_command_closing"
            | "session_command_mailbox_poisoned"
    )
}

impl ApplicationHandler<DesktopEvent> for DesktopApplication {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached {
            requested_resume, ..
        } = cause
        {
            if self
                .surface_scheduler
                .on_resume_time_reached(requested_resume)
            {
                self.apply_surface_schedule(event_loop, SurfaceAcquireSchedule::Acquire);
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.host.is_none() {
            if let Err(error) = self.create_window(event_loop) {
                self.startup_error = Some(error);
                event_loop.exit();
                return;
            }
        }
        event_loop.set_control_flow(ControlFlow::Wait);
        self.schedule_repaint(event_loop);
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
        let mut consumed = false;
        if let Some(state) = self.egui_state.as_mut() {
            let response = state.on_window_event(host.window(), &event);
            consumed = response.consumed;
            if response.repaint {
                self.schedule_repaint(event_loop);
            }
        }
        self.handle_remote_input(&window, &event, consumed);
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.resize(size);
                self.schedule_repaint(event_loop);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.resize(window.inner_size());
                self.schedule_repaint(event_loop);
            }
            WindowEvent::Occluded(false) => {
                let schedule = self.surface_scheduler.on_visibility_restored();
                self.apply_surface_schedule(event_loop, schedule);
            }
            WindowEvent::RedrawRequested => self.redraw(event_loop),
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _event: DesktopEvent) {
        self.schedule_repaint(event_loop);
    }
}

fn winit_key_to_keysym(key: &Key) -> Option<u32> {
    match key {
        Key::Character(text) => text.chars().next().map(u32::from),
        Key::Named(named) => Some(match named {
            NamedKey::Backspace => 0xff08,
            NamedKey::Tab => 0xff09,
            NamedKey::Enter => 0xff0d,
            NamedKey::Escape => 0xff1b,
            NamedKey::Home => 0xff50,
            NamedKey::ArrowLeft => 0xff51,
            NamedKey::ArrowUp => 0xff52,
            NamedKey::ArrowRight => 0xff53,
            NamedKey::ArrowDown => 0xff54,
            NamedKey::PageUp => 0xff55,
            NamedKey::PageDown => 0xff56,
            NamedKey::End => 0xff57,
            NamedKey::Insert => 0xff63,
            NamedKey::Delete => 0xffff,
            NamedKey::F1 => 0xffbe,
            NamedKey::F2 => 0xffbf,
            NamedKey::F3 => 0xffc0,
            NamedKey::F4 => 0xffc1,
            NamedKey::F5 => 0xffc2,
            NamedKey::F6 => 0xffc3,
            NamedKey::F7 => 0xffc4,
            NamedKey::F8 => 0xffc5,
            NamedKey::F9 => 0xffc6,
            NamedKey::F10 => 0xffc7,
            NamedKey::F11 => 0xffc8,
            NamedKey::F12 => 0xffc9,
            NamedKey::Shift => 0xffe1,
            NamedKey::Control => 0xffe3,
            NamedKey::Alt => 0xffe9,
            NamedKey::Meta => 0xffeb,
            _ => return None,
        }),
        Key::Dead(Some(character)) => Some(u32::from(*character)),
        Key::Dead(None) | Key::Unidentified(_) => None,
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
    let repaint_proxy = proxy.clone();
    context.set_request_repaint_callback(move |request| {
        if request.viewport_id == egui::ViewportId::ROOT {
            let _ = repaint_proxy.send_event(DesktopEvent::Repaint);
        }
    });
    let wake: Arc<dyn UiWakeHandle> = Arc::new(EventLoopWake { proxy });
    let mut application = DesktopApplication::new(context, wake);
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        command_failure_requires_disconnect, SurfaceAcquireSchedule, SurfaceAcquireScheduler,
    };
    use crate::session::SessionError;

    #[test]
    fn key_or_pointer_send_failure_keeps_the_session_until_disconnect() {
        assert!(command_failure_requires_disconnect(SessionError::new(
            "session_command_channel_full"
        )));
        assert!(command_failure_requires_disconnect(SessionError::new(
            "session_command_channel_closed"
        )));
        assert!(!command_failure_requires_disconnect(SessionError::new(
            "session_not_running"
        )));
    }

    #[test]
    fn recovery_deadline_blocks_early_wakes_and_retries_once_at_deadline() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(50);
        let mut scheduler = SurfaceAcquireScheduler::default();
        scheduler.schedule_retry(deadline);

        assert_eq!(
            scheduler.on_external_wake(),
            SurfaceAcquireSchedule::WaitUntil(deadline)
        );
        assert_eq!(
            scheduler.on_redraw_requested(now),
            SurfaceAcquireSchedule::WaitUntil(deadline)
        );
        assert!(!scheduler.on_resume_time_reached(now));
        assert!(scheduler.on_resume_time_reached(deadline));
        assert!(!scheduler.on_resume_time_reached(deadline));
        assert_eq!(
            scheduler.on_redraw_requested(deadline),
            SurfaceAcquireSchedule::Acquire
        );

        scheduler.schedule_retry(deadline);
        scheduler.on_successful_render();
        assert_eq!(
            scheduler.on_redraw_requested(now),
            SurfaceAcquireSchedule::Acquire
        );
    }
}
