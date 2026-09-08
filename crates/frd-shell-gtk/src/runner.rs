//! GTK 产品事件泵；仅消费实际窗口提交证明后发布首帧，输入仍由 controller 门控。
use crate::{AdapterEvent, GtkFrameArea, SubmitError};
use frd_app::{persist_profile_job, AppAction, AppIntent, AppLaunch, AppPage, AppPlatformStores};
use frd_core::{DisplayIntent, PixelSize, ResolutionMode, SecretBuffer, SessionId, TargetSystem};
use frd_frame::FrameTransaction;
use frd_platform_api::{
    ConnectionProfileKey, ConnectionProfileStore, PlatformError, SavedConnectionProfile,
    SecureCredentialStore, ServerIdentityStore,
};
use frd_protocol_api::{
    ConnectionStage, PresentationEvent, ProtocolCatalog, ProtocolError, ProtocolFactory,
    SessionCommand, SessionEvent,
};
use frd_shell_desktop::{
    AcceptedLaunchOutcome, AudioOutputFactory, BackgroundCleanupOutcome, BackgroundLaunchOutcome,
    SessionHost, WakeSink,
};
use frd_ui_model::ProfilePersistenceWarning;
use gtk4::{glib, prelude::*};
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
};

#[derive(Default)]
struct ProfileSnapshot(RwLock<Vec<SavedConnectionProfile>>);
impl ProfileSnapshot {
    fn replace(&self, profiles: Vec<SavedConnectionProfile>) {
        if let Ok(mut snapshot) = self.0.write() {
            *snapshot = profiles;
        }
    }
}
impl ConnectionProfileStore for ProfileSnapshot {
    fn list(&self) -> Result<Vec<SavedConnectionProfile>, PlatformError> {
        self.0
            .read()
            .map(|profiles| profiles.clone())
            .map_err(|_| PlatformError::StorageFailed)
    }
    fn upsert(&self, _: &SavedConnectionProfile) -> Result<(), PlatformError> {
        Err(PlatformError::Unavailable)
    }
    fn delete(&self, _: &ConnectionProfileKey) -> Result<(), PlatformError> {
        Err(PlatformError::Unavailable)
    }
}

/// 组合端口只持有现有平台服务；本组件不实现凭据存储。
#[derive(Clone)]
pub struct GtkRunnerStores {
    identities: Arc<dyn ServerIdentityStore>,
    profiles: Arc<dyn ConnectionProfileStore>,
    profile_snapshot: Arc<ProfileSnapshot>,
    credentials: Arc<dyn SecureCredentialStore>,
}
impl GtkRunnerStores {
    pub fn new(
        identities: Arc<dyn ServerIdentityStore>,
        profiles: Arc<dyn ConnectionProfileStore>,
        credentials: Arc<dyn SecureCredentialStore>,
    ) -> Self {
        Self {
            identities,
            profiles,
            profile_snapshot: Arc::new(ProfileSnapshot::default()),
            credentials,
        }
    }
    fn app(&self) -> AppPlatformStores<'_> {
        AppPlatformStores {
            server_identities: self.identities.as_ref(),
            profiles: self.profile_snapshot.as_ref(),
            credentials: self.credentials.as_ref(),
        }
    }
}
enum Message {
    Wake,
    Launch(BackgroundLaunchOutcome),
    Cleanup(BackgroundCleanupOutcome),
    Profile(SessionId, Option<ProfilePersistenceWarning>),
    Loaded(
        ConnectionProfileKey,
        Result<Option<SecretBuffer>, PlatformError>,
    ),
}
struct Wake {
    sender: async_channel::Sender<Message>,
    pending: AtomicBool,
}
impl WakeSink for Wake {
    fn wake(&self) -> Result<(), ProtocolError> {
        if !self.pending.swap(true, Ordering::AcqRel) {
            self.sender
                .try_send(Message::Wake)
                .map_err(|_| ProtocolError::Terminal)?;
        }
        Ok(())
    }
}
struct Form {
    root: gtk4::Box,
    target: gtk4::DropDown,
    profiles: gtk4::DropDown,
    address: gtk4::Entry,
    port: gtk4::Entry,
    username: gtk4::Entry,
    password: gtk4::PasswordEntry,
    resolution: gtk4::DropDown,
    fixed_box: gtk4::Box,
    fixed_width: gtk4::Entry,
    fixed_height: gtk4::Entry,
    resolution_error: gtk4::Label,
    remember: gtk4::CheckButton,
    connect: gtk4::Button,
    errors: [gtk4::Label; 4],
    profile_keys: Vec<ConnectionProfileKey>,
}
fn label(text: &str) -> gtk4::Label {
    let label = gtk4::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label
}
fn field(root: &gtk4::Box, title: &str, widget: &impl IsA<gtk4::Widget>) -> gtk4::Label {
    let title = label(title);
    title.set_mnemonic_widget(Some(widget));
    root.append(&title);
    widget.set_size_request(-1, 44);
    widget.set_tooltip_text(title.text().as_str().into());
    root.append(widget);
    let error = label("");
    error.add_css_class("error");
    error.set_visible(false);
    root.append(&error);
    error
}
impl Form {
    fn new() -> Self {
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        root.set_halign(gtk4::Align::Center);
        root.set_valign(gtk4::Align::Center);
        root.set_margin_top(24);
        root.set_margin_bottom(24);
        root.set_margin_start(24);
        root.set_margin_end(24);
        let heading = label("连接远程电脑");
        heading.add_css_class("title-1");
        root.append(&heading);
        let profiles = gtk4::DropDown::from_strings(&["新连接"]);
        profiles.set_widget_name("frd-profiles");
        field(&root, "最近连接", &profiles);
        let target = gtk4::DropDown::from_strings(&["Windows", "macOS"]);
        target.set_widget_name("frd-target");
        field(&root, "远程系统", &target);
        let resolution = gtk4::DropDown::from_strings(&[
            "显示器原生（推荐）",
            "显示器工作区",
            "窗口内容区域",
            "服务器管理",
            "固定 1920 × 1080",
            "固定 2560 × 1440",
            "固定 3840 × 2160",
            "固定 7680 × 4320",
            "自定义固定尺寸",
        ]);
        resolution.set_widget_name("frd-resolution");
        let resolution_error = field(&root, "远程分辨率", &resolution);
        let fixed_box = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        let fixed_width = gtk4::Entry::new();
        fixed_width.set_widget_name("frd-fixed-width");
        fixed_width.set_input_purpose(gtk4::InputPurpose::Digits);
        fixed_width.set_text("1920");
        let fixed_height = gtk4::Entry::new();
        fixed_height.set_widget_name("frd-fixed-height");
        fixed_height.set_input_purpose(gtk4::InputPurpose::Digits);
        fixed_height.set_text("1080");
        field(&fixed_box, "宽度（像素）", &fixed_width);
        field(&fixed_box, "高度（像素）", &fixed_height);
        fixed_box.set_visible(false);
        root.append(&fixed_box);
        let fixed_controls = fixed_box.clone();
        let fixed_error = resolution_error.clone();
        resolution.connect_selected_notify(move |dropdown| {
            fixed_controls.set_visible(dropdown.selected() == 8);
            fixed_error.set_visible(false);
        });
        let address = gtk4::Entry::new();
        address.set_widget_name("frd-address");
        address.set_width_chars(28);
        let port = gtk4::Entry::new();
        port.set_widget_name("frd-port");
        port.set_input_purpose(gtk4::InputPurpose::Digits);
        let username = gtk4::Entry::new();
        username.set_widget_name("frd-username");
        let password = gtk4::PasswordEntry::new();
        password.set_widget_name("frd-password");
        password.set_show_peek_icon(false);
        let errors = [
            field(&root, "地址", &address),
            field(&root, "端口", &port),
            field(&root, "用户名", &username),
            field(&root, "密码", &password),
        ];
        let remember = gtk4::CheckButton::with_label("在此设备安全保存登录信息");
        remember.set_widget_name("frd-remember");
        remember.set_size_request(-1, 44);
        root.append(&remember);
        let connect = gtk4::Button::with_label("连接");
        connect.set_widget_name("frd-connect");
        connect.add_css_class("suggested-action");
        connect.set_size_request(-1, 44);
        root.append(&connect);
        Self {
            root,
            target,
            profiles,
            address,
            port,
            username,
            password,
            resolution,
            fixed_box,
            fixed_width,
            fixed_height,
            resolution_error,
            remember,
            connect,
            errors,
            profile_keys: vec![],
        }
    }
}
struct State {
    owner: Weak<RefCell<State>>,
    launch: AppLaunch,
    catalog: ProtocolCatalog,
    stores: GtkRunnerStores,
    sessions: SessionHost,
    sender: async_channel::Sender<Message>,
    wake: Arc<Wake>,
    window: gtk4::Window,
    stack: gtk4::Stack,
    remote_container: gtk4::Box,
    status: gtk4::Label,
    details: gtk4::Label,
    action: gtk4::Button,
    form: Form,
    frames: GtkFrameArea,
    pending_frames: Option<Vec<FrameTransaction>>,
    submission_enabled: bool,
    active_session: Option<SessionId>,
    cleanup_pending: bool,
    cancel_pending: bool,
    closing: bool,
    allow_close: bool,
    loading_profile: Option<ConnectionProfileKey>,
    failure: Option<&'static str>,
}
/// 主线程 runner 所有者；必须保留至 window close/异步 cleanup 完成。
pub struct GtkRunner {
    state: Rc<RefCell<State>>,
    pump: glib::JoinHandle<()>,
}
impl GtkRunner {
    pub fn new(
        launch: AppLaunch,
        factories: Vec<Arc<dyn ProtocolFactory>>,
        stores: GtkRunnerStores,
        audio: Arc<dyn AudioOutputFactory>,
    ) -> Self {
        if let AppPage::ConnectionForm(form) = launch.controller().page() {
            stores.profile_snapshot.replace(form.profiles.clone());
        }
        let (sender, receiver) = async_channel::unbounded();
        let wake = Arc::new(Wake {
            sender: sender.clone(),
            pending: AtomicBool::new(false),
        });
        let catalog = ProtocolCatalog::new(factories.iter().map(|f| f.descriptor().id));
        let sessions = SessionHost::new(factories, wake.clone(), audio);
        let window = gtk4::Window::builder()
            .title("FreeRemoteDesk")
            .default_width(720)
            .default_height(680)
            .build();
        let header = gtk4::HeaderBar::new();
        let center = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        let status = label("未连接");
        status.set_widget_name("frd-status");
        status.set_max_width_chars(16);
        status.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        center.append(&status);
        let details = label("");
        details.set_max_width_chars(60);
        details.set_selectable(true);
        let popover = gtk4::Popover::new();
        popover.set_child(Some(&details));
        let info = gtk4::MenuButton::builder()
            .label("连接详情")
            .popover(&popover)
            .build();
        info.set_size_request(-1, 44);
        center.append(&info);
        let action = gtk4::Button::with_label("取消");
        action.set_widget_name("frd-session-action");
        action.set_size_request(-1, 44);
        action.set_visible(false);
        center.append(&action);
        header.set_title_widget(Some(&center));
        window.set_titlebar(Some(&header));
        let form = Form::new();
        let frames = GtkFrameArea::new();
        frames.widget().set_widget_name("frd-remote");
        frames.widget().set_visible(false);
        let scroller = gtk4::ScrolledWindow::new();
        scroller.set_child(Some(&form.root));
        scroller.set_hscrollbar_policy(gtk4::PolicyType::Never);
        let stack = gtk4::Stack::new();
        stack.add_named(&scroller, Some("login"));
        let remote_container = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        remote_container.append(frames.widget());
        stack.add_named(&remote_container, Some("remote"));
        window.set_child(Some(&stack));
        let state = Rc::new(RefCell::new(State {
            owner: Weak::new(),
            launch,
            catalog,
            stores,
            sessions,
            sender,
            wake,
            window,
            stack,
            remote_container,
            status,
            details,
            action,
            form,
            frames,
            pending_frames: None,
            submission_enabled: false,
            active_session: None,
            cleanup_pending: false,
            cancel_pending: false,
            closing: false,
            allow_close: false,
            loading_profile: None,
            failure: None,
        }));
        state.borrow_mut().owner = Rc::downgrade(&state);
        wire(&state);
        state.borrow_mut().refresh_form();
        state.borrow_mut().refresh();
        let weak = Rc::downgrade(&state);
        let pump = glib::MainContext::default().spawn_local(async move {
            while let Ok(message) = receiver.recv().await {
                let Some(state) = weak.upgrade() else {
                    break;
                };
                let mut state = state.borrow_mut();
                state.message(message);
                state.drain();
                state.refresh();
            }
        });
        Self { state, pump }
    }
    pub fn window(&self) -> gtk4::Window {
        self.state.borrow().window.clone()
    }
    pub fn present(&self) {
        self.window().present();
        let intent = self.state.borrow_mut().launch.take_connect_intent();
        if let Some(intent) = intent {
            let weak = Rc::downgrade(&self.state);
            glib::idle_add_local_once(move || {
                if let Some(state) = weak.upgrade() {
                    state.borrow_mut().intent(intent);
                }
            });
        }
    }
}
impl Drop for GtkRunner {
    fn drop(&mut self) {
        self.pump.abort();
        let mut state = self.state.borrow_mut();
        let _ = state.sessions.cancel_pending_launch();
        let _ = state.sessions.send_command(SessionCommand::Disconnect);
        state.frames.detach();
        state.sender.close();
    }
}
fn wire(state: &Rc<RefCell<State>>) {
    let widgets = state.borrow();
    let weak = Rc::downgrade(state);
    widgets.form.connect.connect_clicked(move |_| {
        if let Some(s) = weak.upgrade() {
            s.borrow_mut().submit();
        }
    });
    let weak = Rc::downgrade(state);
    widgets.form.password.connect_activate(move |_| {
        if let Some(s) = weak.upgrade() {
            s.borrow_mut().submit();
        }
    });
    for entry in [&widgets.form.fixed_width, &widgets.form.fixed_height] {
        let weak = Rc::downgrade(state);
        entry.connect_activate(move |_| {
            if let Some(s) = weak.upgrade() {
                s.borrow_mut().submit();
            }
        });
    }
    let weak = Rc::downgrade(state);
    widgets.action.connect_clicked(move |_| {
        if let Some(s) = weak.upgrade() {
            let mut s = s.borrow_mut();
            let intent = if matches!(s.launch.controller().page(), AppPage::Failed { .. }) {
                AppIntent::ReturnToConnection
            } else if matches!(s.launch.controller().page(), AppPage::RemoteSession { .. }) {
                AppIntent::Disconnect
            } else {
                AppIntent::CancelConnect
            };
            s.intent(intent);
        }
    });
    let weak = Rc::downgrade(state);
    widgets.form.target.connect_selected_notify(move |target| {
        if let Some(s) = weak.upgrade() {
            if let Ok(mut s) = s.try_borrow_mut() {
                s.form.port.set_text(if target.selected() == 1 {
                    "5900"
                } else {
                    "3389"
                });
                s.identity_edited();
            }
        }
    });
    for entry in [
        &widgets.form.address,
        &widgets.form.port,
        &widgets.form.username,
    ] {
        let weak = Rc::downgrade(state);
        entry.connect_changed(move |_| {
            if let Some(s) = weak.upgrade() {
                if let Ok(mut s) = s.try_borrow_mut() {
                    s.identity_edited();
                }
            }
        });
    }
    let weak = Rc::downgrade(state);
    widgets.form.profiles.connect_selected_notify(move |combo| {
        if let Some(s) = weak.upgrade() {
            if let Ok(mut s) = s.try_borrow_mut() {
                if combo.selected() == 0 {
                    s.loading_profile = None;
                    if let Some(form) = s.launch.controller_mut().connection_form_mut() {
                        form.selected_profile = None;
                        form.set_password(SecretBuffer::new(Vec::new()));
                        form.remember_on_this_device = false;
                    }
                    s.form.password.set_text("");
                    s.form.remember.set_active(false);
                    s.refresh();
                } else if let Some(index) = combo.selected().checked_sub(1) {
                    if let Some(key) = s.form.profile_keys.get(index as usize).cloned() {
                        s.load_profile(key);
                    }
                }
            }
        }
    });
    let weak = Rc::downgrade(state);
    widgets.window.connect_close_request(move |_| {
        let Some(s) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let mut s = s.borrow_mut();
        if s.allow_close {
            return glib::Propagation::Proceed;
        }
        s.closing = true;
        s.intent(AppIntent::CancelConnect);
        s.finish_close();
        glib::Propagation::Stop
    });
    install_frame_pump(widgets.frames.widget(), Rc::downgrade(state));
}
fn install_frame_pump(area: &gtk4::GLArea, weak: Weak<RefCell<State>>) {
    area.add_tick_callback(move |_, _| {
        let Some(s) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let mut s = s.borrow_mut();
        s.drain();
        s.refresh();
        glib::ControlFlow::Continue
    });
}

impl State {
    fn reset_canvas(&mut self) {
        self.submission_enabled = false;
        self.pending_frames = None;
        self.frames.detach();
        self.remote_container.remove(self.frames.widget());
        self.frames = GtkFrameArea::new();
        self.frames.widget().set_widget_name("frd-remote");
        self.frames.widget().set_visible(false);
        self.remote_container.append(self.frames.widget());
        install_frame_pump(self.frames.widget(), self.owner.clone());
    }
    fn identity_edited(&mut self) {
        let Some(form) = self.launch.controller_mut().connection_form_mut() else {
            return;
        };
        let original = form.draft.clone();
        form.draft.target_system = Some(if self.form.target.selected() == 1 {
            TargetSystem::MacOs
        } else {
            TargetSystem::Windows
        });
        if original.target_system != form.draft.target_system {
            form.draft.protocol = frd_ui_model::ProtocolChoice::Automatic;
        }
        form.draft.address = self.form.address.text().to_string();
        form.draft.port = self.form.port.text().parse().ok();
        form.draft.username = self.form.username.text().to_string();
        if form.invalidate_loaded_secret_after_identity_edit(&original) {
            self.form.password.set_text("");
            self.form.profiles.set_selected(0);
        }
    }
    fn submit(&mut self) {
        if self.loading_profile.is_some() || self.sessions.launch_is_pending() {
            return;
        }
        let mode = match self.form.resolution.selected() {
            0 => ResolutionMode::NativeDisplay,
            1 => ResolutionMode::DisplayWorkArea,
            2 => ResolutionMode::WindowContent,
            3 => ResolutionMode::ServerManaged,
            index => {
                let size = match index {
                    4 => PixelSize::new(1920, 1080),
                    5 => PixelSize::new(2560, 1440),
                    6 => PixelSize::new(3840, 2160),
                    7 => PixelSize::new(7680, 4320),
                    8 => self
                        .form
                        .fixed_width
                        .text()
                        .trim()
                        .parse::<u32>()
                        .ok()
                        .zip(self.form.fixed_height.text().trim().parse::<u32>().ok())
                        .and_then(|(width, height)| PixelSize::new(width, height)),
                    _ => None,
                };
                let Some(size) = size else {
                    self.form
                        .resolution_error
                        .set_text("请输入有效的正整数宽度和高度（像素）");
                    self.form.resolution_error.set_visible(true);
                    return;
                };
                ResolutionMode::Fixed(size)
            }
        };
        self.form.resolution_error.set_visible(false);
        let Some(form) = self.launch.controller_mut().connection_form_mut() else {
            return;
        };
        form.draft.resolution_mode = mode;
        let original = form.draft.clone();
        form.draft.target_system = Some(if self.form.target.selected() == 1 {
            TargetSystem::MacOs
        } else {
            TargetSystem::Windows
        });
        form.draft.address = self.form.address.text().to_string();
        form.draft.port = self.form.port.text().parse().ok();
        form.draft.username = self.form.username.text().to_string();
        // 编辑已保存连接身份后不能复用旧密码；显式输入总是进入独占SecretBuffer。
        if form.invalidate_loaded_secret_after_identity_edit(&original) {
            self.form.password.set_text("");
        }
        form.set_password(SecretBuffer::from_text(
            self.form.password.text().to_string(),
        ));
        form.remember_on_this_device = self.form.remember.is_active();
        if let Some(submission) = form.take_submission(&self.catalog) {
            self.form.password.set_text("");
            self.failure = None;
            self.intent(AppIntent::Connect(submission));
        } else {
            self.refresh();
        }
    }
    fn intent(&mut self, intent: AppIntent) {
        if matches!(intent, AppIntent::CancelConnect | AppIntent::Disconnect)
            && self.sessions.launch_is_pending()
        {
            self.cancel_pending = true;
            self.sessions.cancel_pending_launch();
            self.refresh();
            return;
        }
        let returning = matches!(intent, AppIntent::ReturnToConnection);
        let action = self.launch.controller_mut().handle_intent_with_stores(
            intent,
            &self.catalog,
            self.stores.app(),
        );
        match action {
            Ok(Some(AppAction::StartSession(mut request, permit))) => {
                self.reset_canvas();
                self.active_session = Some(request.session_id);
                // Stack 是标题栏下方的同一个内容矩形；连接前远程 GLArea 尚未分配尺寸。
                if let Some(geometry) =
                    crate::display_geometry::from_window(&self.window, &self.stack)
                {
                    request.display_intent = match request.display_intent.mode {
                        ResolutionMode::NativeDisplay => DisplayIntent::native_display(geometry),
                        ResolutionMode::DisplayWorkArea => {
                            DisplayIntent::display_work_area(geometry)
                        }
                        ResolutionMode::WindowContent => DisplayIntent::window_content(geometry),
                        ResolutionMode::Fixed(size) => DisplayIntent::fixed(size),
                        ResolutionMode::ServerManaged => DisplayIntent::server_managed(),
                    };
                }
                let target = self
                    .launch
                    .controller()
                    .page()
                    .retained_draft()
                    .target_system
                    .expect("提交已验证系统");
                let sender = self.sender.clone();
                if !matches!(
                    self.sessions
                        .begin_launch(permit, target, request, move |outcome| {
                            let _ = sender.try_send(Message::Launch(outcome));
                        }),
                    Ok(true)
                ) {
                    self.failure = Some("无法启动会话，请关闭窗口后重试");
                }
            }
            Ok(Some(AppAction::SessionCommand(command))) => {
                if self.sessions.send_command(command).is_err() {
                    self.failure = Some("会话命令发送失败");
                    self.cleanup();
                }
            }
            Ok(None) => {}
            Err(_) => {
                self.failure = Some("连接操作无法完成，请检查输入或当前会话状态");
                self.refresh_form();
            }
        }
        if returning {
            self.refresh_form();
        }
        self.refresh();
    }
    fn cleanup(&mut self) {
        if self.cleanup_pending {
            return;
        }
        if let Some(session) = self.active_session.take() {
            self.sessions.retire_frame_presentation(session);
        }
        self.reset_canvas();
        let sender = self.sender.clone();
        match self.sessions.begin_cleanup(move |outcome| {
            let _ = sender.try_send(Message::Cleanup(outcome));
        }) {
            Ok(started) => {
                self.cleanup_pending = started;
            }
            Err(_) => {
                self.failure = Some("会话清理失败，请关闭客户端");
                self.cleanup_pending = true;
            }
        }
        self.pending_frames = None;
    }
    fn message(&mut self, message: Message) {
        match message {
            Message::Wake => {
                self.wake.pending.store(false, Ordering::Release);
            }
            Message::Launch(outcome) => {
                let sender = self.sender.clone();
                let accepted = self
                    .sessions
                    .accept_launch_outcome(outcome, move |outcome| {
                        let _ = sender.try_send(Message::Cleanup(outcome));
                    });
                match accepted {
                    Ok(AcceptedLaunchOutcome::Started) => {}
                    Ok(AcceptedLaunchOutcome::LaunchRolledBack(failure))
                    | Ok(AcceptedLaunchOutcome::CancelledLaunchRolledBack(failure)) => {
                        if self
                            .launch
                            .controller_mut()
                            .consume_launch_rollback_with_stores(&failure, self.stores.app())
                            .is_err()
                        {
                            self.failure = Some("启动回滚状态无效");
                        }
                        if self.cancel_pending {
                            self.intent(AppIntent::ReturnToConnection);
                        }
                    }
                    Ok(AcceptedLaunchOutcome::CancelledStarted) => {
                        // host已启动异步cleanup；controller仍须进入取消页面，不能重发启动。
                        self.cleanup_pending = true;
                        let _ = self.launch.controller_mut().handle_intent_with_stores(
                            AppIntent::CancelConnect,
                            &self.catalog,
                            self.stores.app(),
                        );
                    }
                    Err(_) => {
                        self.failure = Some("会话启动结果无效");
                    }
                }
                self.cancel_pending = false;
            }
            Message::Cleanup(outcome) => match self.sessions.accept_cleanup_outcome(outcome) {
                Ok(completion) => {
                    self.cleanup_pending = false;
                    if self
                        .launch
                        .controller_mut()
                        .finish_session_cleanup_with_stores(completion, self.stores.app())
                        .is_err()
                    {
                        self.failure = Some("会话清理状态无效");
                    }
                    self.refresh_form();
                }
                Err(_) => {
                    self.failure = Some("会话清理失败，请重新启动客户端");
                }
            },
            Message::Profile(session, warning) => {
                self.launch
                    .controller_mut()
                    .complete_profile_persistence(session, warning);
                if let Some(form) = self.launch.controller_mut().connection_form_mut() {
                    if let Ok(profiles) = self.stores.profile_snapshot.list() {
                        form.set_profiles(profiles);
                    }
                    self.refresh_form();
                }
            }
            Message::Loaded(key, result) => {
                if self.loading_profile.as_ref() == Some(&key) {
                    self.loading_profile = None;
                    self.launch
                        .controller_mut()
                        .apply_saved_profile_load(key, result);
                    self.refresh_form();
                }
            }
        }
        self.finish_close();
    }
    fn load_profile(&mut self, key: ConnectionProfileKey) {
        if self.loading_profile.is_some()
            || !matches!(self.launch.controller().page(), AppPage::ConnectionForm(_))
        {
            return;
        }
        self.loading_profile = Some(key.clone());
        self.form.password.set_text("");
        let credentials = self.stores.credentials.clone();
        let sender = self.sender.clone();
        let fail_key = key.clone();
        if std::thread::Builder::new()
            .name("frd-gtk-profile-load".into())
            .spawn(move || {
                let result = credentials.load(&key);
                let _ = sender.try_send(Message::Loaded(key, result));
            })
            .is_err()
        {
            self.loading_profile = None;
            self.launch
                .controller_mut()
                .apply_saved_profile_load(fail_key, Err(PlatformError::StorageFailed));
        }
        self.refresh();
    }
    fn profile_job(&mut self, session: SessionId) {
        let Some(job) = self
            .launch
            .controller_mut()
            .take_pending_profile_job(session)
        else {
            return;
        };
        let stores = self.stores.clone();
        let sender = self.sender.clone();
        if std::thread::Builder::new()
            .name("frd-gtk-profile-save".into())
            .spawn(move || {
                let warning =
                    persist_profile_job(job, stores.profiles.as_ref(), stores.credentials.as_ref());
                // 文件目录读取只在后台进行；controller收到的端口始终是内存快照。
                if let Ok(profiles) = stores.profiles.list() {
                    stores.profile_snapshot.replace(profiles);
                }
                let _ = sender.try_send(Message::Profile(session, warning));
            })
            .is_err()
        {
            let _ = self.stores.credentials.discard(session);
            self.launch
                .controller_mut()
                .complete_profile_persistence(session, Some(ProfilePersistenceWarning::SaveFailed));
        }
    }
    fn drain(&mut self) {
        let mut cleanup = false;
        for (session, event) in self.sessions.drain_session_events() {
            if Some(session) != self.active_session {
                continue;
            }
            cleanup |= matches!(event, SessionEvent::Error(_) | SessionEvent::Closed(_));
            let persist = matches!(
                event,
                SessionEvent::StageChanged(ConnectionStage::TransportReady)
            );
            self.launch
                .controller_mut()
                .handle_session_event_with_stores_deferred_profile(
                    session,
                    event,
                    self.stores.app(),
                );
            if persist {
                self.profile_job(session);
            }
        }
        if let Some(command) = self
            .launch
            .controller_mut()
            .take_pending_server_identity_command()
        {
            if self.sessions.send_command(command).is_err() {
                self.failure = Some("证书决定发送失败");
                cleanup = true;
            }
        }
        if cleanup {
            self.cleanup();
        } else {
            self.drain_frames();
        }
        self.finish_close();
    }
    fn drain_frames(&mut self) {
        for event in self.frames.drain_events() {
            match event {
                AdapterEvent::Drawn { .. } => {} // 无窗口证明的 draw 永远不能升级 controller。
                AdapterEvent::Failed(_) => {
                    self.failure = Some("远程画面渲染失败，已停止会话");
                    self.intent(AppIntent::CancelConnect);
                    self.cleanup();
                    return;
                }
                AdapterEvent::Invalidated {
                    discarded_transactions,
                } => {
                    if (self.submission_enabled || discarded_transactions > 0)
                        && self.sessions.is_active()
                        && !self.cleanup_pending
                    {
                        self.failure = Some("画面上下文已失效，请重新连接");
                        self.intent(AppIntent::CancelConnect);
                        self.cleanup();
                        return;
                    }
                }
            }
        }
        if !self.sessions.is_active() || self.cleanup_pending || self.cancel_pending {
            return;
        }
        if !matches!(
            self.launch.controller().page(),
            AppPage::Connecting { .. }
                | AppPage::AwaitingFirstFrame { .. }
                | AppPage::RemoteSession { .. }
        ) {
            return;
        }
        if self.submission_enabled {
            if !self.frames.widget().is_realized() || self.frames.widget().error().is_some() {
                self.fail_presentation("画面上下文已失效，请重新连接");
                return;
            }
            if self.frames.take_submission_error().is_some() {
                self.fail_presentation("窗口画面提交失败，请重新连接");
                return;
            }
            // 先消费上一轮 after-paint 的精确证明，再允许新上传/draw 撤销 serial。
            match self.frames.take_confirmed_presentation() {
                Ok(Some(confirmed)) => {
                    let receipt = confirmed.into_receipt();
                    if Some(receipt.session_id) == self.active_session {
                        self.launch.controller_mut().handle_presentation(
                            PresentationEvent::FramePresented {
                                session_id: receipt.session_id,
                                generation: receipt.generation,
                                revision: receipt.revision,
                                completeness: receipt.completeness,
                            },
                        );
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    self.fail_presentation("窗口画面确认失效，请重新连接");
                    return;
                }
            }
        }
        if self.pending_frames.is_none() {
            match self.sessions.drain_frame_transactions() {
                Ok(drain) => {
                    let batch = drain.into_transactions();
                    if !batch.is_empty() {
                        self.pending_frames = Some(batch);
                    }
                }
                Err(_) => {
                    self.failure = Some("远程画面事务无效，已停止会话");
                    self.intent(AppIntent::CancelConnect);
                    self.cleanup();
                    return;
                }
            }
        }
        if self.pending_frames.is_some() {
            self.frames.widget().set_visible(true);
            // 显示可能只排队 realize；等待实际 realized 后再绑定，绝不先上传。
            if !self.frames.widget().is_realized() {
                return;
            }
            if !self.submission_enabled {
                if self.frames.enable_window_submission(&self.window).is_err() {
                    self.fail_presentation("此窗口暂不支持画面提交确认，请重新连接");
                    return;
                }
                self.submission_enabled = true;
            }
        }
        if let Some(batch) = self.pending_frames.take() {
            if let Err(rejected) = self.frames.submit_batch(batch) {
                if rejected.reason == SubmitError::Busy {
                    self.pending_frames = Some(rejected.transactions);
                } else {
                    self.failure = Some("远程画面暂不可用，请重新连接");
                    self.intent(AppIntent::CancelConnect);
                    self.cleanup();
                }
            }
        }
    }
    fn fail_presentation(&mut self, message: &'static str) {
        self.failure = Some(message);
        self.intent(AppIntent::Disconnect);
        self.cleanup();
    }
    fn refresh_form(&mut self) {
        let Some(form) = self.launch.controller_mut().connection_form_mut() else {
            return;
        };
        form.draft
            .target_system
            .get_or_insert(TargetSystem::Windows);
        let port = if form.draft.target_system == Some(TargetSystem::MacOs) {
            5900
        } else {
            3389
        };
        form.draft.port.get_or_insert(port);
        self.form
            .target
            .set_selected(if form.draft.target_system == Some(TargetSystem::MacOs) {
                1
            } else {
                0
            });
        self.form.address.set_text(&form.draft.address);
        self.form
            .port
            .set_text(&form.draft.port.unwrap_or(3389).to_string());
        self.form.username.set_text(&form.draft.username);
        self.form
            .password
            .set_text(form.password_mut().expose_text().unwrap_or(""));
        let selected = match form.draft.resolution_mode {
            ResolutionMode::NativeDisplay => 0,
            ResolutionMode::DisplayWorkArea => 1,
            ResolutionMode::WindowContent => 2,
            ResolutionMode::ServerManaged => 3,
            ResolutionMode::Fixed(size) => {
                self.form.fixed_width.set_text(&size.width.to_string());
                self.form.fixed_height.set_text(&size.height.to_string());
                match (size.width, size.height) {
                    (1920, 1080) => 4,
                    (2560, 1440) => 5,
                    (3840, 2160) => 6,
                    (7680, 4320) => 7,
                    _ => 8,
                }
            }
        };
        self.form.resolution.set_selected(selected);
        self.form.fixed_box.set_visible(selected == 8);
        self.form.resolution_error.set_visible(false);
        self.form.remember.set_active(form.remember_on_this_device);
        self.form.profile_keys = form
            .profiles
            .iter()
            .map(|profile| profile.key.clone())
            .collect();
        let mut names = vec!["新连接".to_owned()];
        names.extend(
            self.form
                .profile_keys
                .iter()
                .map(|key| format!("{} — {}", key.address(), key.username())),
        );
        let names = names.iter().map(String::as_str).collect::<Vec<_>>();
        self.form
            .profiles
            .set_model(Some(&gtk4::StringList::new(&names)));
        let selected = form
            .selected_profile
            .as_ref()
            .and_then(|key| {
                self.form
                    .profile_keys
                    .iter()
                    .position(|candidate| candidate == key)
            })
            .map_or(0, |index| index as u32 + 1);
        self.form.profiles.set_selected(selected);
    }
    fn refresh(&mut self) {
        let page = self.launch.controller().page();
        let local = matches!(page, AppPage::ConnectionForm(_) | AppPage::Failed { .. });
        self.stack
            .set_visible_child_name(if local { "login" } else { "remote" });
        let status = if self.cancel_pending {
            "正在取消连接"
        } else {
            match page {
                AppPage::ConnectionForm(_) => "未连接",
                AppPage::Connecting { .. } => "正在连接",
                AppPage::AwaitingFirstFrame { .. } => "正在准备远程画面",
                AppPage::RemoteSession { .. } => "已连接",
                AppPage::Disconnecting { .. } => "正在断开",
                AppPage::Failed { .. } => "连接失败",
            }
        };
        self.status.set_text(status);
        self.action
            .set_visible(!matches!(page, AppPage::ConnectionForm(_)));
        self.action.set_label(match page {
            AppPage::Failed { .. } => "返回",
            AppPage::RemoteSession { .. } => "断开连接",
            _ => "取消",
        });
        self.action
            .set_sensitive(!self.cancel_pending && !self.cleanup_pending);
        self.form.root.set_sensitive(
            matches!(page, AppPage::ConnectionForm(_)) && self.loading_profile.is_none(),
        );
        if let AppPage::ConnectionForm(form) = page {
            let errors = form.errors();
            for (label, value) in self.form.errors.iter().zip([
                &errors.address,
                &errors.port,
                &errors.username,
                &errors.password,
            ]) {
                label.set_visible(value.is_some());
                label.set_text(if value.is_some() {
                    "请检查此项或重新输入"
                } else {
                    ""
                });
            }
        }
        let detail = self
            .failure
            .map(str::to_owned)
            .or_else(|| {
                self.launch
                    .controller()
                    .session_chrome()
                    .and_then(|chrome| chrome.diagnostics)
            })
            .unwrap_or_default();
        self.details.set_text(&detail);
        if !local {
            self.form.password.set_text("");
        }
    }
    fn finish_close(&mut self) {
        if self.closing
            && !self.sessions.launch_is_pending()
            && !self.sessions.is_active()
            && !self.cleanup_pending
            && !self.allow_close
        {
            self.allow_close = true;
            let window = self.window.clone();
            glib::idle_add_local_once(move || {
                window.close();
            });
        }
    }
}
