use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use frd_app::{AppAction, AppIntent, AppLaunch, AppPage};
use frd_compositor_wgpu::{
    PresentationCompositor, PresentationHooks, PresentationSurface, PresentationSurfaceLease,
};
use frd_core::{
    ButtonState, ContentViewport, KeyState, Modifiers, PhysicalViewport, PixelRect, PixelSize,
    PointerButton, SessionId, TargetSystem,
};
use frd_frame::{
    FrameCompleteness, FrameMailbox, PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate,
};
use frd_media_api::{AudioOutput, AudioOutputError, MediaFrame, MediaPublishError, MediaPublisher};
use frd_protocol_api::{
    ConnectRequest, MailboxSurfacePublisher, ProtocolCatalog, ProtocolError, ProtocolFactory,
    ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionCommand, SessionEvent,
};
use frd_render_wgpu::{GpuContext, RemoteRenderer};
use frd_session::{
    CleanupComplete, CleanupError, CleanupOperations, SessionCleanupHandle, SessionCoordinator,
    SessionStartFailure, SessionStartOutcome, SessionStartPermit,
};
use frd_ui_model::{LaunchOptions, Page};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::platform::scancode::PhysicalKeyExtScancode;
use winit::window::{Window, WindowId};

use crate::{InputGate, InputRouter};

const FRAME_MAILBOX_ENTRY_LIMIT: usize = 256;
const FRAME_MAILBOX_PIXEL_LIMIT: usize = 64 * 1024 * 1024;
const MEDIA_MAILBOX_ENTRY_LIMIT: usize = 16;

pub trait WakeSink: Send + Sync {
    fn wake(&self) -> Result<(), ProtocolError>;
}

pub trait AudioOutputFactory: Send + Sync {
    fn open(&self) -> Result<Box<dyn AudioOutput>, AudioOutputError>;
}

pub enum ProductLaunchOutcome {
    Started,
    LaunchRolledBack(SessionStartFailure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionHostError {
    NoActiveSession,
    CommandClosed,
    Cleanup(CleanupError),
}

struct ChannelEventSink(mpsc::Sender<SessionEvent>);

impl RuntimeEventSink for ChannelEventSink {
    fn publish(&self, event: SessionEvent) -> Result<(), ProtocolError> {
        self.0
            .send(event)
            .map_err(|_| ProtocolError::EventPortClosed)
    }
}

struct SharedWake(Arc<dyn WakeSink>);

impl RuntimeWake for SharedWake {
    fn wake(&self) -> Result<(), ProtocolError> {
        self.0.wake()
    }
}

struct AudioMediaPublisher(mpsc::SyncSender<MediaFrame>);

impl MediaPublisher for AudioMediaPublisher {
    fn publish(&self, frame: MediaFrame) -> Result<(), MediaPublishError> {
        self.0.try_send(frame).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => MediaPublishError::Full,
            mpsc::TrySendError::Disconnected(_) => MediaPublishError::Closed,
        })
    }
}

struct LiveSessionPorts {
    commands: mpsc::Sender<SessionCommand>,
    events: mpsc::Receiver<SessionEvent>,
    mailbox: Arc<Mutex<FrameMailbox>>,
}

struct LiveSessionCleanup {
    commands: Option<mpsc::Sender<SessionCommand>>,
    protocol_worker: Option<JoinHandle<()>>,
    audio_worker: Option<JoinHandle<()>>,
    mailbox: Option<Arc<Mutex<FrameMailbox>>>,
}

impl CleanupOperations for LiveSessionCleanup {
    fn cancel(&mut self) -> Result<(), CleanupError> {
        if let Some(commands) = &self.commands {
            let _ = commands.send(SessionCommand::Disconnect);
        }
        Ok(())
    }

    fn shutdown_writer(&mut self) -> Result<(), CleanupError> {
        drop(self.commands.take());
        Ok(())
    }

    fn join_workers_and_audio(&mut self) -> Result<(), CleanupError> {
        let protocol_panicked = self
            .protocol_worker
            .take()
            .is_some_and(|worker| worker.join().is_err());
        let audio_panicked = self
            .audio_worker
            .take()
            .is_some_and(|worker| worker.join().is_err());
        if protocol_panicked || audio_panicked {
            Err(CleanupError::JoinWorkersAndAudio)
        } else {
            Ok(())
        }
    }

    fn dispose_mailbox(&mut self) -> Result<(), CleanupError> {
        if let Some(mailbox) = self.mailbox.take() {
            let mut mailbox = mailbox.lock().map_err(|_| CleanupError::DisposeMailbox)?;
            while mailbox.pop().is_some() {}
        }
        Ok(())
    }
}

pub struct SessionHost {
    factories: Vec<Arc<dyn ProtocolFactory>>,
    coordinator: SessionCoordinator,
    wake: Arc<dyn WakeSink>,
    audio_factory: Arc<dyn AudioOutputFactory>,
    active: Option<LiveSessionPorts>,
    cleanup_handle: Option<SessionCleanupHandle>,
}

impl SessionHost {
    pub fn new(
        factories: impl IntoIterator<Item = Arc<dyn ProtocolFactory>>,
        wake: Arc<dyn WakeSink>,
        audio_factory: Arc<dyn AudioOutputFactory>,
    ) -> Self {
        let factories = factories.into_iter().collect::<Vec<_>>();
        let catalog = ProtocolCatalog::new(factories.iter().map(|factory| factory.descriptor().id));
        Self {
            factories,
            coordinator: SessionCoordinator::new(catalog),
            wake,
            audio_factory,
            active: None,
            cleanup_handle: None,
        }
    }

    pub fn launch(
        &mut self,
        permit: SessionStartPermit,
        target: TargetSystem,
        request: ConnectRequest,
    ) -> ProductLaunchOutcome {
        let selected_factory = self
            .factories
            .iter()
            .find(|factory| factory.descriptor().id == request.protocol_id)
            .cloned();
        let wake = self.wake.clone();
        let audio_factory = self.audio_factory.clone();
        let mut launched_ports = None;
        let outcome = self.coordinator.start(permit, target, request, |request| {
            let factory = selected_factory.ok_or(ProtocolError::UnregisteredProtocol)?;
            let (cleanup, ports) = launch_live_session(factory, wake, audio_factory, request)?;
            launched_ports = Some(ports);
            Ok(Box::new(cleanup) as Box<dyn CleanupOperations>)
        });
        match outcome {
            SessionStartOutcome::Started(handle) => {
                self.active = launched_ports;
                self.cleanup_handle = Some(handle);
                ProductLaunchOutcome::Started
            }
            SessionStartOutcome::LaunchRolledBack(failure) => {
                debug_assert!(launched_ports.is_none());
                ProductLaunchOutcome::LaunchRolledBack(failure)
            }
        }
    }

    pub fn send_command(&self, command: SessionCommand) -> Result<(), SessionHostError> {
        self.active
            .as_ref()
            .ok_or(SessionHostError::NoActiveSession)?
            .commands
            .send(command)
            .map_err(|_| SessionHostError::CommandClosed)
    }

    pub fn drain_session_events(&mut self) -> Vec<SessionEvent> {
        self.active
            .as_mut()
            .map(|active| active.events.try_iter().collect())
            .unwrap_or_default()
    }

    pub fn drain_frame_updates(&mut self) -> Vec<SurfaceUpdate> {
        let Some(active) = self.active.as_ref() else {
            return Vec::new();
        };
        let Ok(mut mailbox) = active.mailbox.lock() else {
            return Vec::new();
        };
        let mut updates = Vec::with_capacity(mailbox.len());
        while let Some(update) = mailbox.pop() {
            updates.push(update);
        }
        updates
    }

    pub fn finish_cleanup(&mut self) -> Result<Option<CleanupComplete>, SessionHostError> {
        let Some(handle) = self.cleanup_handle.as_ref() else {
            return Ok(None);
        };
        drop(self.active.take());
        let completion = self
            .coordinator
            .complete_cleanup(handle)
            .map_err(SessionHostError::Cleanup)?;
        self.cleanup_handle = None;
        Ok(Some(completion))
    }

    pub fn is_active(&self) -> bool {
        self.cleanup_handle.is_some()
    }
}

impl Drop for SessionHost {
    fn drop(&mut self) {
        if self.cleanup_handle.is_some() {
            if let Some(active) = self.active.as_ref() {
                let _ = active.commands.send(SessionCommand::Disconnect);
            }
            let _ = self.finish_cleanup();
        }
    }
}

fn launch_live_session(
    factory: Arc<dyn ProtocolFactory>,
    wake: Arc<dyn WakeSink>,
    audio_factory: Arc<dyn AudioOutputFactory>,
    request: ConnectRequest,
) -> Result<(LiveSessionCleanup, LiveSessionPorts), ProtocolError> {
    let session_id = request.session_id;
    let (command_tx, command_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let mailbox = Arc::new(Mutex::new(FrameMailbox::new(
        FRAME_MAILBOX_ENTRY_LIMIT,
        FRAME_MAILBOX_PIXEL_LIMIT,
    )));
    let (media_tx, media_rx) = mpsc::sync_channel(MEDIA_MAILBOX_ENTRY_LIMIT);
    let runtime = ProtocolRuntime::new(
        session_id,
        command_rx,
        Box::new(ChannelEventSink(event_tx.clone())),
        Box::new(MailboxSurfacePublisher::new(mailbox.clone())),
        Some(Box::new(AudioMediaPublisher(media_tx))),
        Box::new(SharedWake(wake.clone())),
    );
    let session = factory.create(request, runtime)?;

    let audio_worker = std::thread::Builder::new()
        .name(format!("frd-audio-{}", session_id.get()))
        .spawn(move || run_audio_worker(audio_factory, media_rx))
        .map_err(|_| ProtocolError::Terminal)?;
    let final_events = event_tx;
    let final_wake = wake;
    let protocol_worker = match std::thread::Builder::new()
        .name(format!("frd-session-{}", session_id.get()))
        .spawn(move || {
            let exit = session.run();
            let _ = final_events.send(SessionEvent::Closed(exit));
            let _ = final_wake.wake();
        }) {
        Ok(worker) => worker,
        Err(_) => {
            // Dropping the unstarted session closes its runtime media sender, so the
            // already-created audio worker exits before rollback is reported.
            let _ = audio_worker.join();
            return Err(ProtocolError::Terminal);
        }
    };

    Ok((
        LiveSessionCleanup {
            commands: Some(command_tx.clone()),
            protocol_worker: Some(protocol_worker),
            audio_worker: Some(audio_worker),
            mailbox: Some(mailbox.clone()),
        },
        LiveSessionPorts {
            commands: command_tx,
            events: event_rx,
            mailbox,
        },
    ))
}

fn run_audio_worker(factory: Arc<dyn AudioOutputFactory>, media: mpsc::Receiver<MediaFrame>) {
    let Ok(mut output) = factory.open() else {
        return;
    };
    while let Ok(frame) = media.recv() {
        if let MediaFrame::Pcm {
            sample_rate_hz,
            channels,
            samples,
        } = frame
        {
            if output
                .enqueue_pcm(sample_rate_hz, channels, samples)
                .is_err()
            {
                return;
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum DesktopUserEvent {
    Wake,
    Repaint,
    ExitTestTexture,
}

struct EventLoopWake(EventLoopProxy<DesktopUserEvent>);

impl WakeSink for EventLoopWake {
    fn wake(&self) -> Result<(), ProtocolError> {
        self.0
            .send_event(DesktopUserEvent::Wake)
            .map_err(|_| ProtocolError::WakeFailed)
    }
}

struct WindowPresentationHook(Arc<Window>);

impl PresentationHooks for WindowPresentationHook {
    fn before_submit(&self) {
        self.0.pre_present_notify();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TestTextureOptions {
    pub exit_after: Option<std::time::Duration>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TestTextureStage {
    Connection,
    RemoteSession,
}

enum DesktopMode {
    Product,
    TestTexture {
        stage: TestTextureStage,
        session_id: SessionId,
        exit_after: Option<std::time::Duration>,
        timer_started: bool,
    },
}

#[derive(Clone, Copy)]
struct RemoteBinding {
    session_id: SessionId,
    generation: u64,
    size: PixelSize,
}

struct DesktopWindowState {
    window: Arc<Window>,
    gpu: GpuContext,
    renderer: RemoteRenderer,
    compositor: PresentationCompositor,
    egui_context: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    physical_size: PixelSize,
    remote: Option<RemoteBinding>,
}

pub struct DesktopApplication {
    launch: AppLaunch,
    catalog: ProtocolCatalog,
    store: Arc<dyn frd_platform_api::ServerIdentityStore>,
    sessions: SessionHost,
    input: InputRouter,
    proxy: EventLoopProxy<DesktopUserEvent>,
    window: Option<DesktopWindowState>,
    mode: DesktopMode,
    exit_when_clean: bool,
}

impl DesktopApplication {
    pub fn new_product(
        launch: AppLaunch,
        factories: impl IntoIterator<Item = Arc<dyn ProtocolFactory>>,
        store: Arc<dyn frd_platform_api::ServerIdentityStore>,
        audio_factory: Arc<dyn AudioOutputFactory>,
        proxy: EventLoopProxy<DesktopUserEvent>,
    ) -> Self {
        let factories = factories.into_iter().collect::<Vec<_>>();
        let catalog = ProtocolCatalog::new(factories.iter().map(|factory| factory.descriptor().id));
        let wake = Arc::new(EventLoopWake(proxy.clone()));
        Self {
            launch,
            catalog,
            store,
            sessions: SessionHost::new(factories, wake, audio_factory),
            input: InputRouter::default(),
            proxy,
            window: None,
            mode: DesktopMode::Product,
            exit_when_clean: false,
        }
    }

    pub fn new_test_texture(
        proxy: EventLoopProxy<DesktopUserEvent>,
        options: TestTextureOptions,
    ) -> Self {
        let catalog = ProtocolCatalog::new([]);
        let launch = AppLaunch::new(LaunchOptions::default(), &UnavailableCredentials, &catalog);
        let wake = Arc::new(EventLoopWake(proxy.clone()));
        let session_id = SessionId::allocate();
        Self {
            launch,
            catalog,
            store: Arc::new(UnavailableIdentityStore),
            sessions: SessionHost::new(
                std::iter::empty::<Arc<dyn ProtocolFactory>>(),
                wake,
                Arc::new(UnavailableAudioFactory),
            ),
            input: InputRouter::default(),
            proxy,
            window: None,
            mode: DesktopMode::TestTexture {
                stage: TestTextureStage::Connection,
                session_id,
                exit_after: options.exit_after,
                timer_started: false,
            },
            exit_when_clean: false,
        }
    }

    fn initialize_window(event_loop: &ActiveEventLoop) -> Result<DesktopWindowState, String> {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("FreeRemoteDesk")
                        .with_inner_size(LogicalSize::new(1100.0, 720.0))
                        .with_resizable(true),
                )
                .map_err(|error| format!("window_create:{error}"))?,
        );
        let physical = window.inner_size();
        let physical_size = PixelSize::new(physical.width, physical.height)
            .ok_or_else(|| "window_zero_size".to_owned())?;
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::DX12;
        let instance = wgpu::Instance::new(descriptor);
        let presentation =
            PresentationSurface::create(&instance, PresentationSurfaceLease::new(window.clone()))
                .map_err(|error| format!("surface_create:{error:?}"))?;
        let gpu = pollster::block_on(presentation.request_gpu_context(instance))
            .map_err(|error| format!("gpu_context:{error:?}"))?;
        let compositor = PresentationCompositor::new(presentation, gpu.clone(), physical_size)
            .map_err(|error| format!("compositor:{error:?}"))?;
        let renderer =
            RemoteRenderer::new(gpu.clone()).map_err(|error| format!("renderer:{error:?}"))?;
        let target_format = compositor
            .target_format()
            .ok_or_else(|| "surface_format_unavailable".to_owned())?;
        let egui_context = egui::Context::default();
        let egui_state = egui_winit::State::new(
            egui_context.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(gpu.device().limits().max_texture_dimension_2d as usize),
        );
        let egui_renderer = egui_wgpu::Renderer::new(
            gpu.device(),
            target_format,
            egui_wgpu::RendererOptions::default(),
        );
        Ok(DesktopWindowState {
            window,
            gpu,
            renderer,
            compositor,
            egui_context,
            egui_state,
            egui_renderer,
            physical_size,
            remote: None,
        })
    }

    fn install_repaint_callback(&self, context: &egui::Context) {
        let proxy = self.proxy.clone();
        context.set_request_repaint_callback(move |request| {
            if request.delay.is_zero() {
                let _ = proxy.send_event(DesktopUserEvent::Repaint);
            } else if request.delay < std::time::Duration::from_secs(60) {
                let delayed_proxy = proxy.clone();
                let _ = std::thread::Builder::new()
                    .name("frd-egui-repaint".to_owned())
                    .spawn(move || {
                        std::thread::sleep(request.delay);
                        let _ = delayed_proxy.send_event(DesktopUserEvent::Repaint);
                    });
            }
        });
    }

    fn dispatch_pending_connect(&mut self) {
        if !matches!(self.mode, DesktopMode::Product) {
            return;
        }
        if let Some(intent) = self.launch.take_connect_intent() {
            self.dispatch_intent(intent);
        }
    }

    fn dispatch_intent(&mut self, intent: AppIntent) {
        if matches!(intent, AppIntent::CancelConnect | AppIntent::Disconnect) {
            self.block_and_release_input();
        }
        let action = match self.launch.controller_mut().handle_intent(
            intent,
            &self.catalog,
            self.store.as_ref(),
        ) {
            Ok(action) => action,
            Err(error) => {
                eprintln!("应用操作失败：{error:?}");
                self.request_redraw();
                return;
            }
        };
        match action {
            Some(AppAction::SessionCommand(command)) => {
                if let Err(error) = self.sessions.send_command(command) {
                    eprintln!("会话命令发送失败：{error:?}");
                }
            }
            Some(AppAction::StartSession(request, permit)) => {
                let target = self
                    .launch
                    .controller()
                    .page()
                    .retained_draft()
                    .target_system
                    .expect("validated connection retains a target system");
                match self.sessions.launch(permit, target, request) {
                    ProductLaunchOutcome::Started => {}
                    ProductLaunchOutcome::LaunchRolledBack(failure) => {
                        if let Err(error) = self
                            .launch
                            .controller_mut()
                            .consume_launch_rollback(&failure)
                        {
                            eprintln!("会话启动回滚能力不匹配：{error:?}");
                        }
                    }
                }
            }
            None => {}
        }
        self.request_redraw();
    }

    fn drain_runtime(&mut self) {
        let events = self.sessions.drain_session_events();
        let mut saw_terminal = false;
        let mut detach_remote = false;
        for event in events {
            if matches!(
                event,
                SessionEvent::SurfaceGenerationChanged { .. }
                    | SessionEvent::StageChanged(frd_protocol_api::ConnectionStage::Disconnecting)
                    | SessionEvent::Error(_)
                    | SessionEvent::Closed(_)
            ) {
                self.block_and_release_input();
            }
            saw_terminal |= matches!(event, SessionEvent::Closed(_));
            detach_remote |= matches!(
                event,
                SessionEvent::StageChanged(frd_protocol_api::ConnectionStage::Disconnecting)
                    | SessionEvent::Error(_)
                    | SessionEvent::Closed(_)
            );
            self.launch.controller_mut().handle_session_event(event);
        }

        if detach_remote {
            if let Some(window) = self.window.as_mut() {
                window.renderer.detach();
                window.remote = None;
            }
        }
        if saw_terminal {
            match self.sessions.finish_cleanup() {
                Ok(Some(completion)) => {
                    if let Err(error) = self
                        .launch
                        .controller_mut()
                        .finish_session_cleanup(completion)
                    {
                        eprintln!("会话清理完成能力不匹配：{error:?}");
                    }
                }
                Ok(None) => {}
                Err(error) => eprintln!("会话资源清理失败：{error:?}"),
            }
        }

        // SessionEvent is deliberately reduced before frame mailbox data.
        let updates = self.sessions.drain_frame_updates();
        let had_events_or_frames = !updates.is_empty() || saw_terminal || detach_remote;
        for update in updates {
            let binding = match &update {
                SurfaceUpdate::Reset {
                    session_id,
                    generation,
                    size,
                    ..
                } => Some(RemoteBinding {
                    session_id: *session_id,
                    generation: *generation,
                    size: *size,
                }),
                _ => None,
            };
            let Some(window) = self.window.as_mut() else {
                continue;
            };
            match window.renderer.apply_update(update) {
                Ok(_) => {
                    if let Some(binding) = binding {
                        window.remote = Some(binding);
                    }
                }
                Err(error) => eprintln!("远端帧更新被拒绝：{error:?}"),
            }
        }
        if had_events_or_frames {
            self.request_redraw();
        }
    }

    fn block_and_release_input(&mut self) {
        if let Some(event) = self.input.set_gate(InputGate::Blocked) {
            self.send_input(event);
        }
    }

    fn send_input(&mut self, event: frd_core::InputEvent) {
        if let Some(command) = self.launch.controller().route_input(event) {
            if let Err(error) = self.sessions.send_command(command) {
                eprintln!("远端输入发送失败：{error:?}");
            }
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.window.request_redraw();
        }
    }

    fn content_viewport(&self) -> Option<ContentViewport> {
        let window = self.window.as_ref()?;
        let remote = window.remote?;
        Some(ContentViewport::fit(remote.size, window.physical_size))
    }

    fn send_viewport_changed(&self) {
        let Some((session_id, generation)) = self.input.interactive_epoch() else {
            return;
        };
        let Some(remote) = self.window.as_ref().and_then(|window| window.remote) else {
            return;
        };
        if remote.session_id != session_id || remote.generation != generation {
            return;
        }
        let Some(viewport) = self.content_viewport() else {
            return;
        };
        let Some(viewport) =
            PhysicalViewport::new(viewport.drawable, viewport.content, viewport.remote)
        else {
            return;
        };
        let _ = self.sessions.send_command(SessionCommand::ViewportChanged {
            session_id,
            generation,
            viewport,
        });
    }

    fn render(&mut self) {
        let Some(window) = self.window.as_mut() else {
            return;
        };
        let raw_input = window.egui_state.take_egui_input(&window.window);
        let egui_context = window.egui_context.clone();
        let mode = &mut self.mode;
        let controller = self.launch.controller_mut();
        let catalog = &self.catalog;
        let mut intent = None;
        let output = egui_context.run_ui(raw_input, |root_ui| match mode {
            DesktopMode::Product => {
                if let Some(form) = controller.connection_form_mut() {
                    egui::CentralPanel::default_margins().show(root_ui, |ui| {
                        intent = frd_ui_egui::show_connection_form(ui, form, catalog);
                    });
                } else if matches!(controller.page(), AppPage::RemoteSession { .. }) {
                    egui::Panel::top("remote-toolbar").show(root_ui, |ui| {
                        intent = frd_ui_egui::show_session_page(
                            ui,
                            controller.page(),
                            controller.current_server_identity_challenge(),
                        );
                    });
                } else {
                    egui::CentralPanel::default_margins().show(root_ui, |ui| {
                        intent = frd_ui_egui::show_session_page(
                            ui,
                            controller.page(),
                            controller.current_server_identity_challenge(),
                        );
                    });
                }
            }
            DesktopMode::TestTexture { stage, .. } => match stage {
                TestTextureStage::Connection => {
                    egui::CentralPanel::default_margins().show(root_ui, |ui| {
                        ui.heading("连接远程桌面");
                        ui.label("离线测试纹理正在初始化，不会读取凭据或连接网络。");
                    });
                }
                TestTextureStage::RemoteSession => {
                    egui::Panel::top("test-remote-toolbar").show(root_ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.heading("测试远程会话");
                            ui.label("离线 2×2 BGRX 纹理");
                        });
                    });
                }
            },
        });
        window
            .egui_state
            .handle_platform_output(&window.window, output.platform_output);
        let paint_jobs = egui_context.tessellate(output.shapes, output.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [window.physical_size.width, window.physical_size.height],
            pixels_per_point: output.pixels_per_point,
        };
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                window.egui_renderer.update_texture(
                    window.gpu.device(),
                    window.gpu.queue(),
                    *id,
                    delta,
                );
            }
        }
        let hook = WindowPresentationHook(window.window.clone());
        let gpu = window.gpu.clone();
        let egui_renderer = &mut window.egui_renderer;
        let render_result = window.compositor.render(
            &mut window.renderer,
            |encoder, target| {
                let callbacks = egui_renderer.update_buffers(
                    gpu.device(),
                    gpu.queue(),
                    encoder,
                    &paint_jobs,
                    &screen,
                );
                debug_assert!(callbacks.is_empty(), "product UI has no egui GPU callbacks");
                let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("FreeRemoteDesk egui overlay"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                egui_renderer.render(&mut render_pass.forget_lifetime(), &paint_jobs, &screen);
            },
            &hook,
        );
        for id in &output.textures_delta.free {
            window.egui_renderer.free_texture(id);
        }

        if let Some(intent) = intent {
            self.dispatch_intent(intent);
        }
        match render_result {
            Ok(Some(event)) => {
                let (session_id, generation, completeness) = match &event {
                    frd_protocol_api::PresentationEvent::FramePresented {
                        session_id,
                        generation,
                        completeness,
                        ..
                    } => (*session_id, *generation, *completeness),
                };
                self.launch.controller_mut().handle_presentation(event);
                if matches!(
                    self.launch.controller().page(),
                    AppPage::RemoteSession { .. }
                ) && completeness == FrameCompleteness::FullBaseline
                {
                    if let Some(release) = self.input.set_gate(InputGate::Interactive {
                        session_id,
                        generation,
                    }) {
                        self.send_input(release);
                    }
                    self.send_viewport_changed();
                    self.request_redraw();
                }
            }
            Ok(None) => {}
            Err(error) => eprintln!("窗口合成失败：{error:?}"),
        }

        if let DesktopMode::TestTexture {
            stage, session_id, ..
        } = &mut self.mode
        {
            if *stage == TestTextureStage::Connection {
                let updates = test_texture_updates(*session_id);
                if let Some(window) = self.window.as_mut() {
                    for update in updates {
                        let _ = window.renderer.apply_update(update);
                    }
                    window.remote = Some(RemoteBinding {
                        session_id: *session_id,
                        generation: 1,
                        size: PixelSize::new(2, 2).expect("test texture is non-zero"),
                    });
                    window.window.set_title("FreeRemoteDesk — 离线测试远程会话");
                }
                *stage = TestTextureStage::RemoteSession;
                self.request_redraw();
            }
        }
    }

    fn handle_remote_window_event(&mut self, event: &WindowEvent, consumed_by_egui: bool) {
        if consumed_by_egui {
            return;
        }
        let Some(viewport) = self.content_viewport() else {
            return;
        };
        let input = match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.input
                    .pointer_moved(position.x as f32, position.y as f32, viewport)
            }
            WindowEvent::MouseInput { state, button, .. } => {
                map_mouse_button(*button).and_then(|button| {
                    self.input
                        .pointer_button(button, map_button_state(*state), viewport)
                })
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (horizontal, vertical) = wheel_signs(*delta);
                self.input.wheel(horizontal, vertical, viewport)
            }
            WindowEvent::KeyboardInput { event, .. } => event
                .physical_key
                .to_scancode()
                .and_then(|code| self.input.key(code, map_key_state(event.state))),
            WindowEvent::Ime(Ime::Commit(text)) => self.input.text(text.clone()),
            _ => None,
        };
        if let Some(input) = input {
            self.send_input(input);
        }
    }

    fn begin_shutdown(&mut self, event_loop: &ActiveEventLoop) {
        self.block_and_release_input();
        if self.sessions.is_active() {
            self.exit_when_clean = true;
            if !matches!(self.launch.controller().page(), Page::Disconnecting { .. }) {
                self.dispatch_intent(AppIntent::Disconnect);
            }
        } else {
            event_loop.exit();
        }
    }

    fn maybe_finish_exit(&self, event_loop: &ActiveEventLoop) {
        if self.exit_when_clean && !self.sessions.is_active() {
            event_loop.exit();
        }
    }
}

impl ApplicationHandler<DesktopUserEvent> for DesktopApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Wait);
        if self.window.is_none() {
            match Self::initialize_window(event_loop) {
                Ok(window) => {
                    self.install_repaint_callback(&window.egui_context);
                    self.window = Some(window);
                    self.request_redraw();
                    self.dispatch_pending_connect();
                }
                Err(error) => {
                    eprintln!("Windows 客户端初始化失败：{error}");
                    event_loop.exit();
                    return;
                }
            }
        }
        if let DesktopMode::TestTexture {
            exit_after: Some(delay),
            timer_started,
            ..
        } = &mut self.mode
        {
            if !*timer_started {
                *timer_started = true;
                let proxy = self.proxy.clone();
                let delay = *delay;
                let _ = std::thread::Builder::new()
                    .name("frd-test-texture-exit".to_owned())
                    .spawn(move || {
                        std::thread::sleep(delay);
                        let _ = proxy.send_event(DesktopUserEvent::ExitTestTexture);
                    });
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: DesktopUserEvent) {
        match event {
            DesktopUserEvent::Wake => {
                self.drain_runtime();
                self.request_redraw();
                self.maybe_finish_exit(event_loop);
            }
            DesktopUserEvent::Repaint => self.request_redraw(),
            DesktopUserEvent::ExitTestTexture => event_loop.exit(),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.as_mut() else {
            return;
        };
        if window.window.id() != window_id {
            return;
        }
        let response = window.egui_state.on_window_event(&window.window, &event);
        if response.repaint {
            window.window.request_redraw();
        }
        match &event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                self.begin_shutdown(event_loop);
                return;
            }
            WindowEvent::Focused(true) => self.input.focus_gained(),
            WindowEvent::Focused(false) => {
                if let Some(release) = self.input.focus_lost() {
                    self.send_input(release);
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(release) = self.input.cursor_left() {
                    self.send_input(release);
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.input.set_modifiers(map_modifiers(modifiers.state()));
            }
            WindowEvent::Resized(size) => {
                if let Some(size) = PixelSize::new(size.width, size.height) {
                    if let Some(window) = self.window.as_mut() {
                        window.physical_size = size;
                        if let Err(error) = window.compositor.resize(size) {
                            eprintln!("窗口尺寸更新失败：{error:?}");
                        }
                    }
                    self.send_viewport_changed();
                    self.request_redraw();
                } else if let Some(window) = self.window.as_mut() {
                    window.compositor.pause_presenting();
                }
            }
            WindowEvent::Occluded(false) => self.request_redraw(),
            WindowEvent::RedrawRequested => {
                self.drain_runtime();
                self.render();
                self.maybe_finish_exit(event_loop);
                return;
            }
            _ => {}
        }
        self.handle_remote_window_event(&event, response.consumed);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(release) = self.input.shutdown() {
            self.send_input(release);
        }
        if self.sessions.is_active() {
            let _ = self.sessions.send_command(SessionCommand::Disconnect);
        }
    }
}

fn map_mouse_button(button: MouseButton) -> Option<PointerButton> {
    match button {
        MouseButton::Left => Some(PointerButton::Primary),
        MouseButton::Middle => Some(PointerButton::Middle),
        MouseButton::Right => Some(PointerButton::Secondary),
        MouseButton::Back => Some(PointerButton::Back),
        MouseButton::Forward => Some(PointerButton::Forward),
        MouseButton::Other(_) => None,
    }
}

fn map_button_state(state: ElementState) -> ButtonState {
    match state {
        ElementState::Pressed => ButtonState::Pressed,
        ElementState::Released => ButtonState::Released,
    }
}

fn map_key_state(state: ElementState) -> KeyState {
    match state {
        ElementState::Pressed => KeyState::Pressed,
        ElementState::Released => KeyState::Released,
    }
}

fn map_modifiers(modifiers: ModifiersState) -> Modifiers {
    Modifiers {
        shift: modifiers.shift_key(),
        control: modifiers.control_key(),
        alt: modifiers.alt_key(),
        meta: modifiers.super_key(),
    }
}

fn wheel_signs(delta: MouseScrollDelta) -> (i8, i8) {
    let (horizontal, vertical) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (x, y),
        MouseScrollDelta::PixelDelta(position) => (position.x as f32, position.y as f32),
    };
    (axis_sign(horizontal), axis_sign(vertical))
}

fn axis_sign(value: f32) -> i8 {
    if value > 0.0 {
        1
    } else if value < 0.0 {
        -1
    } else {
        0
    }
}

fn test_texture_updates(session_id: SessionId) -> [SurfaceUpdate; 3] {
    [
        SurfaceUpdate::Reset {
            session_id,
            generation: 1,
            size: PixelSize::new(2, 2).expect("test texture size is non-zero"),
            format: PixelFormat::Bgrx8UnormSrgb,
        },
        SurfaceUpdate::Damage {
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
        },
        SurfaceUpdate::FrameBoundary {
            session_id,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::FullBaseline,
        },
    ]
}

struct UnavailableCredentials;

impl frd_platform_api::CredentialProvider for UnavailableCredentials {
    fn load_username(
        &self,
        _provider: &frd_core::CredentialProviderId,
    ) -> Result<String, frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::CredentialProviderFailed)
    }

    fn load_password(
        &self,
        _provider: &frd_core::CredentialProviderId,
    ) -> Result<frd_core::SecretBuffer, frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::CredentialProviderFailed)
    }
}

struct UnavailableIdentityStore;

impl frd_platform_api::ServerIdentityStore for UnavailableIdentityStore {
    fn load_pin(
        &self,
        _protocol: &frd_core::ProtocolId,
        _endpoint: &frd_core::Endpoint,
    ) -> Result<Option<[u8; 32]>, frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::Unavailable)
    }

    fn store_pin(
        &self,
        _protocol: &frd_core::ProtocolId,
        _endpoint: &frd_core::Endpoint,
        _pin: [u8; 32],
    ) -> Result<(), frd_platform_api::PlatformError> {
        Err(frd_platform_api::PlatformError::Unavailable)
    }
}

struct UnavailableAudioFactory;

impl AudioOutputFactory for UnavailableAudioFactory {
    fn open(&self) -> Result<Box<dyn AudioOutput>, AudioOutputError> {
        Err(AudioOutputError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use frd_core::{Endpoint, ProtocolId, SecretBuffer, SessionId, TargetSystem};
    use frd_media_api::{AudioOutput, AudioOutputError};
    use frd_protocol_api::{
        ConnectRequest, CredentialRequirements, Credentials, ProtocolDescriptor, ProtocolError,
        ProtocolExit, ProtocolFactory, ProtocolRuntime, ProtocolSession, SessionCommand,
        SessionEvent,
    };
    use frd_session::reserve_session_start;

    use super::{AudioOutputFactory, ProductLaunchOutcome, SessionHost, WakeSink};

    struct CountingWake(AtomicUsize);

    impl WakeSink for CountingWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    struct TestAudioFactory;

    impl AudioOutputFactory for TestAudioFactory {
        fn open(&self) -> Result<Box<dyn AudioOutput>, AudioOutputError> {
            Ok(Box::new(TestAudioOutput))
        }
    }

    struct TestAudioOutput;

    impl AudioOutput for TestAudioOutput {
        fn enqueue_pcm(
            &mut self,
            _sample_rate_hz: u32,
            _channels: u8,
            _samples: Box<[i16]>,
        ) -> Result<(), AudioOutputError> {
            Ok(())
        }
    }

    struct BlockingFactory {
        worker_started: mpsc::Sender<()>,
    }

    impl ProtocolFactory for BlockingFactory {
        fn descriptor(&self) -> ProtocolDescriptor {
            ProtocolDescriptor {
                id: ProtocolId::apple_hpss_mvs(),
                display_name: "test-protocol".to_owned(),
                default_port: 5900,
                credential_requirements: CredentialRequirements::username_password(),
            }
        }

        fn create(
            &self,
            _request: ConnectRequest,
            runtime: ProtocolRuntime,
        ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
            Ok(Box::new(BlockingSession {
                runtime,
                worker_started: self.worker_started.clone(),
            }))
        }
    }

    struct BlockingSession {
        runtime: ProtocolRuntime,
        worker_started: mpsc::Sender<()>,
    }

    impl ProtocolSession for BlockingSession {
        fn run(mut self: Box<Self>) -> ProtocolExit {
            self.worker_started.send(()).expect("test observer alive");
            loop {
                if matches!(
                    self.runtime.try_next_command(),
                    Some(SessionCommand::Disconnect)
                ) {
                    return ProtocolExit::Closed;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    #[test]
    fn connect_action_starts_a_real_worker_without_blocking_the_event_loop() {
        let (worker_started_tx, worker_started_rx) = mpsc::channel();
        let wake = Arc::new(CountingWake(AtomicUsize::new(0)));
        let mut host = SessionHost::new(
            [Arc::new(BlockingFactory {
                worker_started: worker_started_tx,
            }) as Arc<dyn ProtocolFactory>],
            wake.clone(),
            Arc::new(TestAudioFactory),
        );
        let session_id = SessionId::allocate();
        let (_owner, permit) = reserve_session_start(session_id);
        let request = ConnectRequest {
            session_id,
            endpoint: Endpoint::new("test.invalid", 5900).expect("valid test endpoint"),
            protocol_id: ProtocolId::apple_hpss_mvs(),
            credentials: Some(Credentials {
                username: "test-user".to_owned(),
                password: SecretBuffer::new(vec![0x41]).take(),
            }),
            saved_server_pin: None,
        };

        let before = Instant::now();
        let outcome = host.launch(permit, TargetSystem::MacOs, request);
        assert!(before.elapsed() < Duration::from_millis(250));
        assert!(matches!(outcome, ProductLaunchOutcome::Started));
        worker_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("protocol worker starts asynchronously");

        host.send_command(SessionCommand::Disconnect)
            .expect("live command channel accepts disconnect");
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let events = host.drain_session_events();
            if events
                .iter()
                .any(|event| matches!(event, SessionEvent::Closed(ProtocolExit::Closed)))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker must publish terminal event"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let cleanup = host
            .finish_cleanup()
            .expect("real worker/channel/mailbox resources clean up")
            .expect("started session issues cleanup completion");
        assert_eq!(cleanup.session_id(), session_id);
        assert!(wake.0.load(Ordering::Relaxed) > 0);
    }
}
