use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use frd_app::{AppAction, AppIntent, AppLaunch, AppPage};
use frd_compositor_wgpu::{
    PresentError, PresentationCompositor, PresentationHooks, PresentationSurface,
    PresentationSurfaceLease,
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
    ConnectRequest, MailboxSurfacePublisher, ProtocolCatalog, ProtocolError, ProtocolExit,
    ProtocolFactory, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionCommand, SessionEvent,
};
use frd_render_wgpu::{GpuContext, RecoveryRequirement, RemoteRenderer};
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

use crate::cleanup::{
    spawn_cleanup, BackgroundCleanupFailure, BackgroundCleanupOutcome, CleanupPolicy,
    PendingCleanup,
};
use crate::lifecycle::{OcclusionAction, PresentFailureAction, PresentationLifecycle};
use crate::repaint::{RepaintPlan, RepaintScheduler};
use crate::{InputGate, InputOwnership, InputRouter};

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
    CleanupFatal(BackgroundCleanupFailure),
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
        let (protocol_pending, protocol_panicked) = poll_worker(&mut self.protocol_worker);
        let (audio_pending, audio_panicked) = poll_worker(&mut self.audio_worker);
        if protocol_pending || audio_pending || protocol_panicked || audio_panicked {
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

fn poll_worker(worker: &mut Option<JoinHandle<()>>) -> (bool, bool) {
    let Some(handle) = worker.as_ref() else {
        return (false, false);
    };
    if !handle.is_finished() {
        return (true, false);
    }
    let panicked = worker
        .take()
        .expect("finished worker handle remains owned")
        .join()
        .is_err();
    (false, panicked)
}

pub struct SessionHost {
    factories: Vec<Arc<dyn ProtocolFactory>>,
    coordinator: Option<SessionCoordinator>,
    wake: Arc<dyn WakeSink>,
    audio_factory: Arc<dyn AudioOutputFactory>,
    active: Option<LiveSessionPorts>,
    cleanup_handle: Option<SessionCleanupHandle>,
    cleanup_in_flight: bool,
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
            coordinator: Some(SessionCoordinator::new(catalog)),
            wake,
            audio_factory,
            active: None,
            cleanup_handle: None,
            cleanup_in_flight: false,
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
        let outcome = self
            .coordinator
            .as_mut()
            .expect("app slot prevents launch during cleanup")
            .start(permit, target, request, |request| {
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

    pub fn is_active(&self) -> bool {
        self.cleanup_handle.is_some() || self.cleanup_in_flight
    }

    pub fn begin_cleanup(
        &mut self,
        notify: impl FnOnce(BackgroundCleanupOutcome) + Send + 'static,
    ) -> Result<bool, BackgroundCleanupFailure> {
        if self.cleanup_in_flight {
            return Ok(false);
        }
        let Some(handle) = self.cleanup_handle.take() else {
            return Ok(false);
        };
        drop(self.active.take());
        let coordinator = self
            .coordinator
            .take()
            .expect("started session owns its coordinator");
        self.cleanup_in_flight = true;
        match spawn_cleanup(
            PendingCleanup::new(coordinator, handle),
            CleanupPolicy::new(500, std::time::Duration::from_millis(10)),
            notify,
        ) {
            Ok(()) => Ok(true),
            Err(error) => {
                self.cleanup_in_flight = false;
                Err(error)
            }
        }
    }

    pub fn accept_cleanup_outcome(
        &mut self,
        outcome: BackgroundCleanupOutcome,
    ) -> Result<CleanupComplete, SessionHostError> {
        self.cleanup_in_flight = false;
        match outcome {
            BackgroundCleanupOutcome::Complete {
                coordinator,
                completion,
            } => {
                self.coordinator = Some(coordinator);
                Ok(completion)
            }
            BackgroundCleanupOutcome::Fatal(failure) => {
                Err(SessionHostError::CleanupFatal(failure))
            }
        }
    }
}

impl Drop for SessionHost {
    fn drop(&mut self) {
        if let Some(active) = self.active.as_ref() {
            let _ = active.commands.send(SessionCommand::Disconnect);
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

    let audio_events = event_tx.clone();
    let audio_wake = wake.clone();
    let audio_worker = std::thread::Builder::new()
        .name(format!("frd-audio-{}", session_id.get()))
        .spawn(move || {
            let degraded = match catch_unwind(AssertUnwindSafe(|| {
                run_audio_worker(audio_factory, media_rx)
            })) {
                Ok(AudioWorkerExit::Closed) => false,
                Ok(AudioWorkerExit::Failed) | Err(_) => true,
            };
            if degraded {
                let _ = audio_events.send(SessionEvent::AudioState(
                    frd_protocol_api::AudioState::Failed,
                ));
                let _ = audio_wake.wake();
            }
        })
        .map_err(|_| ProtocolError::Terminal)?;
    let final_events = event_tx;
    let final_wake = wake;
    let protocol_worker = match std::thread::Builder::new()
        .name(format!("frd-session-{}", session_id.get()))
        .spawn(move || {
            let exit = catch_unwind(AssertUnwindSafe(|| session.run()))
                .unwrap_or(ProtocolExit::Failed(ProtocolError::Terminal));
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AudioWorkerExit {
    Closed,
    Failed,
}

fn run_audio_worker(
    factory: Arc<dyn AudioOutputFactory>,
    media: mpsc::Receiver<MediaFrame>,
) -> AudioWorkerExit {
    let Ok(mut output) = factory.open() else {
        return AudioWorkerExit::Failed;
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
                return AudioWorkerExit::Failed;
            }
        }
    }
    AudioWorkerExit::Closed
}

pub enum DesktopUserEvent {
    Wake,
    Repaint,
    CleanupFinished(BackgroundCleanupOutcome),
    PresentationFatal,
    ResizeTestTexture,
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
    pub resize_after: Option<std::time::Duration>,
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
        resize_after: Option<std::time::Duration>,
        driver_started: bool,
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
    lifecycle: PresentationLifecycle,
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
    repaint_scheduler: RepaintScheduler,
    armed_repaint: Option<RepaintPlan>,
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
            repaint_scheduler: RepaintScheduler::default(),
            armed_repaint: None,
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
                resize_after: options.resize_after,
                driver_started: false,
            },
            exit_when_clean: false,
            repaint_scheduler: RepaintScheduler::default(),
            armed_repaint: None,
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
        let instance = dx12_instance();
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
            lifecycle: PresentationLifecycle::new(physical_size),
            remote: None,
        })
    }

    fn install_repaint_callback(&self, context: &egui::Context) {
        let proxy = self.proxy.clone();
        let scheduler = self.repaint_scheduler.clone();
        context.set_request_repaint_callback(move |request| {
            scheduler.request_after(std::time::Instant::now(), request.delay, || {
                let _ = proxy.send_event(DesktopUserEvent::Repaint);
            });
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
        let mut cleanup_needed = false;
        let mut detach_remote = false;
        for event in events {
            if matches!(
                event,
                SessionEvent::CapabilitiesChanged(frd_protocol_api::SessionCapabilities {
                    text_input: false,
                    ..
                })
            ) {
                if let Some(release) = self.input.keyboard_capability_lost() {
                    self.send_input(release);
                }
            }
            if matches!(
                event,
                SessionEvent::SurfaceGenerationChanged { .. }
                    | SessionEvent::StageChanged(frd_protocol_api::ConnectionStage::Disconnecting)
                    | SessionEvent::Error(_)
                    | SessionEvent::Closed(_)
            ) {
                self.block_and_release_input();
            }
            cleanup_needed |= matches!(event, SessionEvent::Error(_) | SessionEvent::Closed(_));
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
        if cleanup_needed {
            self.start_background_cleanup();
        }

        // SessionEvent is deliberately reduced before frame mailbox data.
        let updates = self.sessions.drain_frame_updates();
        let had_events_or_frames = !updates.is_empty() || cleanup_needed || detach_remote;
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
            if window.lifecycle.accepts_redraw() {
                window.window.request_redraw();
            }
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
        if !window.lifecycle.accepts_redraw() {
            return;
        }
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

        let mut presentation_fatal = false;
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
            Err(error) => match PresentationLifecycle::classify_present_error(error) {
                PresentFailureAction::RecoverGpu => match recover_window_gpu(window) {
                    Ok(None) => window.window.request_redraw(),
                    Ok(Some(RecoveryRequirement::ResetAndFullSnapshot { .. })) => {
                        let test_session_id = match &self.mode {
                            DesktopMode::TestTexture { session_id, .. } => Some(*session_id),
                            DesktopMode::Product => None,
                        };
                        if let Some(session_id) = test_session_id {
                            for update in test_texture_updates(session_id) {
                                let _ = window.renderer.apply_update(update);
                            }
                            window.window.request_redraw();
                        } else {
                            presentation_fatal = true;
                        }
                    }
                    Err(recovery_error) => {
                        eprintln!("GPU 恢复失败：{recovery_error:?}");
                        presentation_fatal = true;
                    }
                },
                PresentFailureAction::Fatal => {
                    eprintln!("窗口合成进入不可恢复状态：{error:?}");
                    presentation_fatal = true;
                }
            },
        }

        if let Some(intent) = intent {
            self.dispatch_intent(intent);
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
        if presentation_fatal {
            let _ = self.proxy.send_event(DesktopUserEvent::PresentationFatal);
        }
    }

    fn handle_remote_window_event(&mut self, event: &WindowEvent, consumed_by_egui: bool) {
        let Some(viewport) = self.content_viewport() else {
            return;
        };
        let pointer_ownership = if consumed_by_egui {
            InputOwnership::Ui
        } else {
            InputOwnership::Remote
        };
        let keyboard_ownership =
            if consumed_by_egui || !self.launch.controller().effective_capabilities().text_input {
                InputOwnership::Ui
            } else {
                InputOwnership::Remote
            };
        let input = match event {
            WindowEvent::CursorMoved { position, .. } => self.input.pointer_moved(
                position.x as f32,
                position.y as f32,
                viewport,
                pointer_ownership,
            ),
            WindowEvent::MouseInput { state, button, .. } => {
                map_mouse_button(*button).and_then(|button| {
                    self.input.pointer_button(
                        button,
                        map_button_state(*state),
                        viewport,
                        pointer_ownership,
                    )
                })
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (horizontal, vertical) = wheel_signs(*delta);
                self.input
                    .wheel(horizontal, vertical, viewport, pointer_ownership)
            }
            WindowEvent::KeyboardInput { event, .. } => {
                event.physical_key.to_scancode().and_then(|code| {
                    self.input
                        .key(code, map_key_state(event.state), keyboard_ownership)
                })
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.input.text(text.clone(), keyboard_ownership)
            }
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
            self.start_background_cleanup();
        } else {
            event_loop.exit();
        }
    }

    fn start_background_cleanup(&mut self) {
        let proxy = self.proxy.clone();
        if let Err(failure) = self.sessions.begin_cleanup(move |outcome| {
            let _ = proxy.send_event(DesktopUserEvent::CleanupFinished(outcome));
        }) {
            let _ = self.proxy.send_event(DesktopUserEvent::CleanupFinished(
                BackgroundCleanupOutcome::Fatal(failure),
            ));
        }
    }

    fn handle_cleanup_finished(
        &mut self,
        event_loop: &ActiveEventLoop,
        outcome: BackgroundCleanupOutcome,
    ) {
        match self.sessions.accept_cleanup_outcome(outcome) {
            Ok(completion) => {
                if let Err(error) = self
                    .launch
                    .controller_mut()
                    .finish_session_cleanup(completion)
                {
                    eprintln!("会话清理完成能力不匹配：{error:?}");
                    self.detach_window();
                    event_loop.exit();
                }
            }
            Err(SessionHostError::CleanupFatal(failure)) => {
                eprintln!("会话资源清理超过有界策略：{failure:?}");
                self.launch
                    .controller_mut()
                    .handle_session_event(SessionEvent::Error(ProtocolError::Terminal));
                self.detach_window();
                event_loop.exit();
            }
            Err(error) => {
                eprintln!("会话资源清理状态无效：{error:?}");
                self.detach_window();
                event_loop.exit();
            }
        }
        self.maybe_finish_exit(event_loop);
        self.request_redraw();
    }

    fn detach_window(&mut self) {
        self.repaint_scheduler.shutdown();
        self.armed_repaint = None;
        if let Some(mut window) = self.window.take() {
            window.lifecycle.destroy();
            window.compositor.detach();
            window.renderer.detach();
        }
    }

    fn handle_presentation_fatal(&mut self, event_loop: &ActiveEventLoop) {
        self.block_and_release_input();
        self.launch
            .controller_mut()
            .handle_session_event(SessionEvent::Error(ProtocolError::Terminal));
        self.detach_window();
        if self.sessions.is_active() {
            self.exit_when_clean = true;
            self.start_background_cleanup();
        } else {
            event_loop.exit();
        }
    }

    fn maybe_finish_exit(&self, event_loop: &ActiveEventLoop) {
        if self.exit_when_clean && !self.sessions.is_active() {
            event_loop.exit();
        }
    }

    fn synchronize_repaint_deadline(&mut self, event_loop: &ActiveEventLoop) {
        self.armed_repaint = self.repaint_scheduler.take_plan();
        let now = std::time::Instant::now();
        match self.armed_repaint {
            Some(plan) if plan.deadline <= now => {
                if self.repaint_scheduler.fire(plan, now) {
                    self.request_redraw();
                }
                self.armed_repaint = None;
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(plan) => event_loop.set_control_flow(ControlFlow::WaitUntil(plan.deadline)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
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
            exit_after,
            resize_after,
            driver_started,
            ..
        } = &mut self.mode
        {
            if !*driver_started && (exit_after.is_some() || resize_after.is_some()) {
                *driver_started = true;
                spawn_test_texture_driver(self.proxy.clone(), *resize_after, *exit_after);
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
            DesktopUserEvent::Repaint => self.synchronize_repaint_deadline(event_loop),
            DesktopUserEvent::CleanupFinished(outcome) => {
                self.handle_cleanup_finished(event_loop, outcome)
            }
            DesktopUserEvent::PresentationFatal => self.handle_presentation_fatal(event_loop),
            DesktopUserEvent::ResizeTestTexture => {
                if matches!(self.mode, DesktopMode::TestTexture { .. }) {
                    if let Some(window) = self.window.as_ref() {
                        let _ = window
                            .window
                            .request_inner_size(winit::dpi::PhysicalSize::new(960_u32, 640_u32));
                    }
                }
            }
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
            WindowEvent::CloseRequested => {
                self.begin_shutdown(event_loop);
                return;
            }
            WindowEvent::Destroyed => {
                self.detach_window();
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
                    let mut committed = false;
                    if let Some(window) = self.window.as_mut() {
                        let result = window.compositor.resize(size);
                        if window.lifecycle.finish_resize(size, result) {
                            window.physical_size = size;
                            committed = true;
                        } else {
                            eprintln!("窗口尺寸更新失败：{result:?}");
                        }
                    }
                    if committed {
                        self.send_viewport_changed();
                        self.request_redraw();
                    }
                } else if let Some(window) = self.window.as_mut() {
                    window.compositor.pause_presenting();
                }
            }
            WindowEvent::Occluded(occluded) => {
                let action = self
                    .window
                    .as_mut()
                    .map(|window| window.lifecycle.set_occluded(*occluded))
                    .unwrap_or(OcclusionAction::None);
                match action {
                    OcclusionAction::None => {}
                    OcclusionAction::Pause => {
                        if let Some(window) = self.window.as_mut() {
                            window.compositor.pause_presenting();
                        }
                    }
                    OcclusionAction::ResumeAndRedraw => {
                        let resumed = self.window.as_mut().is_some_and(|window| {
                            window
                                .compositor
                                .resize(window.lifecycle.committed_size())
                                .is_ok()
                        });
                        if resumed {
                            self.request_redraw();
                        } else {
                            let _ = self.proxy.send_event(DesktopUserEvent::PresentationFatal);
                        }
                    }
                }
            }
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
        self.repaint_scheduler.shutdown();
        if let Some(release) = self.input.shutdown() {
            self.send_input(release);
        }
        if self.sessions.is_active() {
            let _ = self.sessions.send_command(SessionCommand::Disconnect);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(plan) = self.armed_repaint else {
            return;
        };
        let now = std::time::Instant::now();
        if now < plan.deadline {
            event_loop.set_control_flow(ControlFlow::WaitUntil(plan.deadline));
            return;
        }
        self.armed_repaint = None;
        if self.repaint_scheduler.fire(plan, now) {
            self.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::Wait);
    }
}

fn spawn_test_texture_driver(
    proxy: EventLoopProxy<DesktopUserEvent>,
    resize_after: Option<std::time::Duration>,
    exit_after: Option<std::time::Duration>,
) {
    let _ = std::thread::Builder::new()
        .name("frd-test-texture-driver".to_owned())
        .spawn(move || {
            let started = std::time::Instant::now();
            if let Some(delay) =
                resize_after.filter(|delay| exit_after.is_none_or(|exit| *delay < exit))
            {
                std::thread::sleep(delay.saturating_sub(started.elapsed()));
                if proxy
                    .send_event(DesktopUserEvent::ResizeTestTexture)
                    .is_err()
                {
                    return;
                }
            }
            if let Some(delay) = exit_after {
                std::thread::sleep(delay.saturating_sub(started.elapsed()));
                let _ = proxy.send_event(DesktopUserEvent::ExitTestTexture);
            }
        });
}

fn dx12_instance() -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::DX12;
    wgpu::Instance::new(descriptor)
}

fn recover_window_gpu(
    window: &mut DesktopWindowState,
) -> Result<Option<RecoveryRequirement>, PresentError> {
    let (requirement, gpu) = pollster::block_on(
        window
            .compositor
            .recover_gpu_with_new_instance(&mut window.renderer, dx12_instance()),
    )?;
    let target_format = window
        .compositor
        .target_format()
        .ok_or(PresentError::SurfaceUnsupported)?;
    window.egui_renderer = egui_wgpu::Renderer::new(
        gpu.device(),
        target_format,
        egui_wgpu::RendererOptions::default(),
    );
    window
        .egui_context
        .set_fonts(egui::FontDefinitions::default());
    window.gpu = gpu;
    Ok(requirement)
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use frd_app::{AppAction, AppController, AppIntent, PresentationEvent, ProductPolicy};
    use frd_core::{
        Endpoint, InputEvent, KeyState, Modifiers, PhysicalKeyCode, ProtocolId, SecretBuffer,
        SessionId, TargetSystem,
    };
    use frd_frame::{FrameCompleteness, PixelFormat};
    use frd_media_api::{AudioOutput, AudioOutputError, MediaFrame};
    use frd_platform_api::{PlatformCapabilities, PlatformError, ServerIdentityStore};
    use frd_protocol_api::{
        ConnectRequest, ConnectionStage, CredentialRequirements, Credentials, ProtocolCatalog,
        ProtocolDescriptor, ProtocolError, ProtocolExit, ProtocolFactory, ProtocolRuntime,
        ProtocolSession, SessionCapabilities, SessionCommand, SessionEvent,
    };
    use frd_session::reserve_session_start;
    use frd_ui_model::{ConnectionDraft, ConnectionForm, ProtocolChoice};

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

    struct TestIdentityStore;

    impl ServerIdentityStore for TestIdentityStore {
        fn load_pin(
            &self,
            _protocol: &ProtocolId,
            _endpoint: &Endpoint,
        ) -> Result<Option<[u8; 32]>, PlatformError> {
            Ok(None)
        }

        fn store_pin(
            &self,
            _protocol: &ProtocolId,
            _endpoint: &Endpoint,
            _pin: [u8; 32],
        ) -> Result<(), PlatformError> {
            Ok(())
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

    struct OpenFailureAudioFactory;

    impl AudioOutputFactory for OpenFailureAudioFactory {
        fn open(&self) -> Result<Box<dyn AudioOutput>, AudioOutputError> {
            Err(AudioOutputError::Unavailable)
        }
    }

    enum FailingAudioMode {
        EnqueueError,
        Panic,
    }

    struct FailingAudioFactory(FailingAudioMode);

    impl AudioOutputFactory for FailingAudioFactory {
        fn open(&self) -> Result<Box<dyn AudioOutput>, AudioOutputError> {
            Ok(Box::new(FailingAudioOutput(match self.0 {
                FailingAudioMode::EnqueueError => FailingAudioMode::EnqueueError,
                FailingAudioMode::Panic => FailingAudioMode::Panic,
            })))
        }
    }

    struct FailingAudioOutput(FailingAudioMode);

    impl AudioOutput for FailingAudioOutput {
        fn enqueue_pcm(
            &mut self,
            _sample_rate_hz: u32,
            _channels: u8,
            _samples: Box<[i16]>,
        ) -> Result<(), AudioOutputError> {
            match self.0 {
                FailingAudioMode::EnqueueError => Err(AudioOutputError::Closed),
                FailingAudioMode::Panic => panic!("sanitized test audio panic"),
            }
        }
    }

    struct BlockingFactory {
        worker_started: mpsc::Sender<()>,
    }

    struct FailOnceFactory {
        fail_next: AtomicBool,
        worker_started: mpsc::Sender<()>,
    }

    impl ProtocolFactory for FailOnceFactory {
        fn descriptor(&self) -> ProtocolDescriptor {
            ProtocolDescriptor {
                id: ProtocolId::apple_hpss_mvs(),
                display_name: "fail-once-protocol".to_owned(),
                default_port: 5900,
                credential_requirements: CredentialRequirements::username_password(),
            }
        }

        fn create(
            &self,
            _request: ConnectRequest,
            runtime: ProtocolRuntime,
        ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
            if self.fail_next.swap(false, Ordering::AcqRel) {
                return Err(ProtocolError::Adapter {
                    protocol_id: ProtocolId::apple_hpss_mvs(),
                    code: "test_factory_failed",
                });
            }
            Ok(Box::new(BlockingSession {
                runtime,
                worker_started: self.worker_started.clone(),
            }))
        }
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

    struct PanicFactory;

    impl ProtocolFactory for PanicFactory {
        fn descriptor(&self) -> ProtocolDescriptor {
            ProtocolDescriptor {
                id: ProtocolId::apple_hpss_mvs(),
                display_name: "panic-protocol".to_owned(),
                default_port: 5900,
                credential_requirements: CredentialRequirements::username_password(),
            }
        }

        fn create(
            &self,
            _request: ConnectRequest,
            _runtime: ProtocolRuntime,
        ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
            Ok(Box::new(PanicSession))
        }
    }

    struct PanicSession;

    impl ProtocolSession for PanicSession {
        fn run(self: Box<Self>) -> ProtocolExit {
            panic!("sanitized test protocol panic")
        }
    }

    struct MediaFactory;

    impl ProtocolFactory for MediaFactory {
        fn descriptor(&self) -> ProtocolDescriptor {
            ProtocolDescriptor {
                id: ProtocolId::apple_hpss_mvs(),
                display_name: "media-protocol".to_owned(),
                default_port: 5900,
                credential_requirements: CredentialRequirements::username_password(),
            }
        }

        fn create(
            &self,
            _request: ConnectRequest,
            runtime: ProtocolRuntime,
        ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
            Ok(Box::new(MediaSession { runtime }))
        }
    }

    struct MediaSession {
        runtime: ProtocolRuntime,
    }

    struct UnsupportedInputFactory;

    impl ProtocolFactory for UnsupportedInputFactory {
        fn descriptor(&self) -> ProtocolDescriptor {
            ProtocolDescriptor {
                id: ProtocolId::apple_hpss_mvs(),
                display_name: "unsupported-input-protocol".to_owned(),
                default_port: 5900,
                credential_requirements: CredentialRequirements::username_password(),
            }
        }

        fn create(
            &self,
            request: ConnectRequest,
            runtime: ProtocolRuntime,
        ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
            Ok(Box::new(UnsupportedInputSession {
                session_id: request.session_id,
                runtime,
            }))
        }
    }

    struct UnsupportedInputSession {
        session_id: SessionId,
        runtime: ProtocolRuntime,
    }

    impl ProtocolSession for UnsupportedInputSession {
        fn run(mut self: Box<Self>) -> ProtocolExit {
            self.runtime
                .publish_event(SessionEvent::StageChanged(ConnectionStage::TransportReady))
                .unwrap();
            self.runtime
                .publish_event(SessionEvent::CapabilitiesChanged(SessionCapabilities {
                    dynamic_resolution: true,
                    clipboard_read: false,
                    clipboard_write: false,
                    remote_audio: false,
                    text_input: false,
                }))
                .unwrap();
            self.runtime
                .begin_generation(
                    self.session_id,
                    1,
                    frd_core::PixelSize::new(2, 2).unwrap(),
                    PixelFormat::Bgrx8UnormSrgb,
                )
                .unwrap();
            loop {
                match self.runtime.try_next_command() {
                    Some(SessionCommand::Input(_)) => {
                        return ProtocolExit::Failed(ProtocolError::Adapter {
                            protocol_id: ProtocolId::apple_hpss_mvs(),
                            code: "unsupported_keyboard_input",
                        });
                    }
                    Some(SessionCommand::Disconnect) => return ProtocolExit::Closed,
                    _ => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        }
    }

    impl ProtocolSession for MediaSession {
        fn run(mut self: Box<Self>) -> ProtocolExit {
            let _ = self.runtime.try_publish_optional_media(MediaFrame::Pcm {
                sample_rate_hz: 48_000,
                channels: 2,
                samples: vec![1_i16, -1_i16].into_boxed_slice(),
            });
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

        let before_cleanup = Instant::now();
        let cleanup = complete_background_cleanup(&mut host);
        assert!(
            before_cleanup.elapsed() < Duration::from_millis(250),
            "the caller waits for a typed event, never a worker join"
        );
        assert_eq!(cleanup.session_id(), session_id);
        assert!(wake.0.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn factory_failure_rolls_back_partial_runtime_resources_and_allows_a_fresh_launch() {
        let (worker_started_tx, worker_started_rx) = mpsc::channel();
        let mut host = test_host(
            Arc::new(FailOnceFactory {
                fail_next: AtomicBool::new(true),
                worker_started: worker_started_tx,
            }),
            Arc::new(TestAudioFactory),
        );

        let first_session = SessionId::allocate();
        let (_first_owner, first_permit) = reserve_session_start(first_session);
        let first_request = ConnectRequest {
            session_id: first_session,
            endpoint: Endpoint::new("test.invalid", 5900).unwrap(),
            protocol_id: ProtocolId::apple_hpss_mvs(),
            credentials: Some(Credentials {
                username: "test-user".to_owned(),
                password: SecretBuffer::new(vec![0x41]).take(),
            }),
            saved_server_pin: None,
        };
        let ProductLaunchOutcome::LaunchRolledBack(failure) =
            host.launch(first_permit, TargetSystem::MacOs, first_request)
        else {
            panic!("factory failure must return only after rollback");
        };
        assert!(matches!(
            failure.error(),
            ProtocolError::Adapter {
                code: "test_factory_failed",
                ..
            }
        ));
        assert!(!host.is_active());

        let second_session = launch_test_session(&mut host);
        worker_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("fresh request starts after rollback reclaimed the coordinator");
        let completion = complete_background_cleanup(&mut host);
        assert_eq!(completion.session_id(), second_session);
    }

    #[test]
    fn unsupported_keyboard_input_never_reaches_or_closes_the_fake_adapter_session() {
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        let mut form = ConnectionForm::new(ConnectionDraft {
            target_system: Some(TargetSystem::MacOs),
            address: "test.invalid".to_owned(),
            port: Some(5900),
            protocol: ProtocolChoice::Explicit(ProtocolId::apple_hpss_mvs()),
            username: "test-user".to_owned(),
        });
        form.set_password(SecretBuffer::new(vec![0x41]));
        let mut controller = AppController::connection_form(form);
        controller.set_platform_capabilities(PlatformCapabilities {
            dynamic_resolution: true,
            clipboard_read: false,
            clipboard_write: false,
            remote_audio: false,
            text_input: true,
        });
        controller.set_product_policy(ProductPolicy {
            dynamic_resolution: true,
            clipboard_read: false,
            clipboard_write: false,
            remote_audio: false,
            text_input: true,
        });

        let intent = controller
            .connection_form_mut()
            .expect("connection form remains editable")
            .take_connect_intent(&catalog)
            .expect("complete form creates one connection intent");
        let AppAction::StartSession(request, permit) = controller
            .handle_intent(intent, &catalog, &TestIdentityStore)
            .expect("controller accepts the connection")
            .expect("connection starts one session")
        else {
            panic!("connection must start through the shared transaction");
        };
        let session_id = request.session_id;
        let mut host = test_host(
            Arc::new(UnsupportedInputFactory),
            Arc::new(TestAudioFactory),
        );
        assert!(matches!(
            host.launch(permit, TargetSystem::MacOs, request),
            ProductLaunchOutcome::Started
        ));

        let events = wait_for_event(&mut host, |event| {
            matches!(event, SessionEvent::SurfaceGenerationChanged { .. })
        });
        for event in events {
            controller.handle_session_event(event);
        }
        controller.handle_presentation(PresentationEvent::FramePresented {
            session_id,
            generation: 1,
            revision: 1,
            completeness: FrameCompleteness::FullBaseline,
        });

        let unsupported_key = InputEvent::PhysicalKey {
            code: PhysicalKeyCode(30),
            state: KeyState::Pressed,
            modifiers: Modifiers::default(),
        };
        if let Some(command) = controller.route_input(unsupported_key) {
            host.send_command(command)
                .expect("session command channel open");
        }
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            !host
                .drain_session_events()
                .iter()
                .any(|event| matches!(event, SessionEvent::Closed(_))),
            "an unsupported key must not reach the fail-closed fake adapter"
        );

        let Some(AppAction::SessionCommand(disconnect)) = controller
            .handle_intent(AppIntent::Disconnect, &catalog, &TestIdentityStore)
            .expect("disconnect is valid while remote")
        else {
            panic!("disconnect must emit exactly one protocol command");
        };
        host.send_command(disconnect)
            .expect("disconnect reaches the worker");
        let completion = complete_background_cleanup(&mut host);
        controller
            .finish_session_cleanup(completion)
            .expect("shared cleanup token releases the controller slot");
    }

    #[test]
    fn protocol_worker_panic_publishes_one_sanitized_terminal_event_and_cleans_up() {
        let mut host = test_host(Arc::new(PanicFactory), Arc::new(TestAudioFactory));
        let session_id = launch_test_session(&mut host);

        let events = wait_for_event(&mut host, |event| {
            matches!(
                event,
                SessionEvent::Closed(ProtocolExit::Failed(ProtocolError::Terminal))
            )
        });
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, SessionEvent::Closed(_)))
                .count(),
            1
        );

        let completion = complete_background_cleanup(&mut host);
        assert_eq!(completion.session_id(), session_id);
    }

    #[test]
    fn audio_open_failure_is_one_degradation_and_session_cleanup_stays_safe() {
        assert_audio_degradation_and_cleanup(Arc::new(OpenFailureAudioFactory));
    }

    #[test]
    fn audio_enqueue_failure_is_one_degradation_and_session_cleanup_stays_safe() {
        assert_audio_degradation_and_cleanup(Arc::new(FailingAudioFactory(
            FailingAudioMode::EnqueueError,
        )));
    }

    #[test]
    fn audio_worker_panic_is_one_sanitized_degradation_and_session_cleanup_stays_safe() {
        assert_audio_degradation_and_cleanup(Arc::new(FailingAudioFactory(
            FailingAudioMode::Panic,
        )));
    }

    fn assert_audio_degradation_and_cleanup(audio: Arc<dyn AudioOutputFactory>) {
        let mut host = test_host(Arc::new(MediaFactory), audio);
        let session_id = launch_test_session(&mut host);
        let events = wait_for_event(&mut host, |event| {
            matches!(
                event,
                SessionEvent::AudioState(frd_protocol_api::AudioState::Failed)
            )
        });
        std::thread::sleep(Duration::from_millis(10));
        let mut all_events = events;
        all_events.extend(host.drain_session_events());
        assert_eq!(
            all_events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        SessionEvent::AudioState(frd_protocol_api::AudioState::Failed)
                    )
                })
                .count(),
            1
        );
        assert!(
            host.is_active(),
            "audio degradation must not close the desktop"
        );
        assert!(!all_events
            .iter()
            .any(|event| matches!(event, SessionEvent::Closed(_))));

        let completion = complete_background_cleanup(&mut host);
        assert_eq!(completion.session_id(), session_id);
    }

    fn test_host(
        protocol: Arc<dyn ProtocolFactory>,
        audio: Arc<dyn AudioOutputFactory>,
    ) -> SessionHost {
        SessionHost::new(
            [protocol],
            Arc::new(CountingWake(AtomicUsize::new(0))),
            audio,
        )
    }

    fn launch_test_session(host: &mut SessionHost) -> SessionId {
        let session_id = SessionId::allocate();
        let (_owner, permit) = reserve_session_start(session_id);
        let request = ConnectRequest {
            session_id,
            endpoint: Endpoint::new("test.invalid", 5900).expect("valid test endpoint"),
            protocol_id: ProtocolId::apple_hpss_mvs(),
            credentials: None,
            saved_server_pin: None,
        };
        assert!(matches!(
            host.launch(permit, TargetSystem::MacOs, request),
            ProductLaunchOutcome::Started
        ));
        session_id
    }

    fn wait_for_event(
        host: &mut SessionHost,
        predicate: impl Fn(&SessionEvent) -> bool,
    ) -> Vec<SessionEvent> {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut observed = Vec::new();
        loop {
            observed.extend(host.drain_session_events());
            if observed.iter().any(&predicate) {
                return observed;
            }
            assert!(
                Instant::now() < deadline,
                "worker must publish the expected event"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn complete_background_cleanup(host: &mut SessionHost) -> frd_session::CleanupComplete {
        let (outcome_tx, outcome_rx) = mpsc::channel();
        assert!(host
            .begin_cleanup(move |outcome| outcome_tx.send(outcome).unwrap())
            .expect("cleanup worker starts"));
        assert!(!host
            .begin_cleanup(|_| panic!("a repeated close must not start a second cleanup worker"))
            .expect("repeated close is an idempotent no-op"));
        let outcome = outcome_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("background cleanup reports completion");
        host.accept_cleanup_outcome(outcome)
            .expect("cleanup completes without a fatal state")
    }
}
