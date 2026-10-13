//! ArkUI 不拥有 controller/password/session；专用 owner 串行处理意图与真正的 runtime 事件。
use crate::input_adapter::{InputAdapter, ViewGeometry};
use frd_app::{persist_profile_job, AppAction, AppController, AppIntent, AppPlatformStores};
use frd_core::{InputEvent, ProtocolId, SecretBuffer, TargetSystem};
use frd_platform_api::{
    ConnectionProfileStore, PlatformError, SecureCredentialStore, ServerIdentityStore,
};
use frd_platform_ohos::{OhosConnectionProfileStore, OhosCredentialStore, OhosServerIdentityStore};
use frd_protocol_api::{
    ConnectionStage, PresentationEvent, ProtocolCatalog, ProtocolError, ProtocolFactory,
    SessionCommand, SessionEvent,
};
use frd_shell_runtime::{
    AcceptedLaunchOutcome, AudioOutputFactory, BackgroundCleanupOutcome, BackgroundLaunchOutcome,
    SessionHost, StreamRegistryLoader, WakeSink,
};
use frd_ui_model::{
    ConnectionDraft, ConnectionForm, Page, ProfilePersistenceWarning, ProtocolChoice,
    SavedPasswordLoadTicket,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
const STOP: u64 = 1;
const CANCEL: u64 = 2;
const FOCUS_LOST: u64 = 4;
const FORM_INVALID: u64 = 8;
const CONTROL_MASK: u64 = STOP | CANCEL | FOCUS_LOST | FORM_INVALID;
const CONTROL_REVISION: u64 = 1 << 32;
// 标志与代际同时发布，队列满也不会丢失取消/失焦，旧 UI 意图不跨此屏障。
fn signal(control: &AtomicU64, flags: u64) {
    let _ = control.fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
        Some(
            old.checked_add(CONTROL_REVISION)
                .map_or(old | STOP, |next| next | flags),
        )
    });
}
#[derive(Clone)]
pub struct Stores {
    pub context_root: Option<PathBuf>,
    pub identities: Arc<dyn ServerIdentityStore>,
    pub profiles: Arc<dyn ConnectionProfileStore>,
    pub credentials: Arc<dyn SecureCredentialStore>,
}
impl Stores {
    pub fn at(root: PathBuf) -> Self {
        Self {
            context_root: Some(root.clone()),
            identities: Arc::new(OhosServerIdentityStore::at_path(root.join("pins"))),
            profiles: Arc::new(OhosConnectionProfileStore::at_path(
                root.join("connections-v1.json"),
            )),
            credentials: Arc::new(OhosCredentialStore::new(root.join("credentials"))),
        }
    }
    fn borrow(&self) -> AppPlatformStores<'_> {
        AppPlatformStores {
            server_identities: self.identities.as_ref(),
            profiles: self.profiles.as_ref(),
            credentials: self.credentials.as_ref(),
        }
    }
}
struct Wake;
impl WakeSink for Wake {
    fn wake(&self) -> Result<(), ProtocolError> {
        Ok(())
    }
}
struct NoAudio;
impl AudioOutputFactory for NoAudio {
    fn open(&self) -> Result<Box<dyn frd_media_api::AudioOutput>, frd_media_api::AudioOutputError> {
        Err(frd_media_api::AudioOutputError::Unavailable)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub target: u32,
    pub address: String,
    pub port: u32,
    pub username: String,
    pub protocol: String,
    pub remember: bool,
}
#[derive(Clone, Copy)]
pub struct InputEpoch {
    pub session: u64,
    pub generation: u64,
    pub revision: u64,
}
pub enum Command {
    Queued(u64, Box<Command>),
    Edit(Edit),
    Password(SecretBuffer),
    Select(usize),
    Submit {
        event: u64,
        composition: bool,
        repeat: bool,
    },
    Intent(AppIntent),
    Focus {
        foreground: bool,
        focused: bool,
        remote: bool,
    },
    Geometry(ViewGeometry),
    Input(InputEpoch, InputEvent, bool),
    NativePointer(
        u64,
        u64,
        InputEpoch,
        f32,
        f32,
        frd_core::PointerButtons,
        bool,
    ),
    Pointer(
        InputEpoch,
        f32,
        f32,
        frd_core::PointerButtons,
        frd_core::WheelDelta,
    ),
    Launch(BackgroundLaunchOutcome),
    Cleanup(BackgroundCleanupOutcome),
    Loaded(u64, Result<Option<SecretBuffer>, PlatformError>),
    Persisted(frd_core::SessionId, Option<ProfilePersistenceWarning>),
    #[cfg(debug_assertions)]
    Probe(PathBuf),
    #[cfg(debug_assertions)]
    Probed(i32),
}
pub struct AppEngine {
    sender: SyncSender<Command>,
    snapshot: Arc<Mutex<Value>>,
    control: Arc<AtomicU64>,
    renderer: Arc<Mutex<Option<Arc<crate::presentation::Link>>>>,
}
impl AppEngine {
    pub fn spawn(
        stores: Stores,
        factories: Vec<Arc<dyn ProtocolFactory>>,
        loader: StreamRegistryLoader,
        audio: Arc<dyn AudioOutputFactory>,
    ) -> Result<Self, i32> {
        let (sender, receiver) = mpsc::sync_channel(64);
        let snapshot = Arc::new(Mutex::new(
            json!({"abiVersion":1,"page":0,"initialized":false,"available":false}),
        ));
        let control = Arc::new(AtomicU64::new(0));
        let published = snapshot.clone();
        let renderer = Arc::new(Mutex::new(None));
        let owner_renderer = renderer.clone();
        let flags = control.clone();
        let tx = sender.clone();
        std::thread::Builder::new()
            .name("frd-ohos-app".into())
            .spawn(move || {
                let _scope = crate::panic_scope::Scope::enter();
                let storage_failed = stores.context_root.as_ref().is_some_and(|root| {
                    frd_platform_ohos::initialize_context_storage(root).is_err()
                });
                let catalog = ProtocolCatalog::new(factories.iter().map(|f| f.descriptor().id));
                let controller = AppController::connection_form_with_stores(
                    ConnectionForm::new(ConnectionDraft::default()),
                    stores.borrow(),
                );
                let mut owner = Owner {
                    controller,
                    renderer: owner_renderer,
                    ready: None,
                    remote_epoch: None,
                    host: SessionHost::new(factories, Arc::new(Wake), audio, loader),
                    catalog,
                    stores,
                    input: InputAdapter::default(),
                    pending: BTreeMap::new(),
                    tx,
                    last_submit: 0,
                    form_invalid_before: 0,
                    cancelled_launch: false,
                    stopping: false,
                    notice: storage_failed.then_some("平台应用沙盒初始化失败"),
                    probe: 0,
                    control: flags.clone(),
                };
                #[cfg(all(debug_assertions, feature = "native-presentation-fixture"))]
                {
                    owner.command(Command::Edit(Edit {
                        target: 1,
                        address: "fixture.invalid".into(),
                        port: 3389,
                        username: "synthetic".into(),
                        protocol: "auto".into(),
                        remember: false,
                    }));
                    owner.command(Command::Password(SecretBuffer::from_text(
                        "synthetic-no-network".into(),
                    )));
                    owner.command(Command::Submit {
                        event: 1,
                        composition: false,
                        repeat: false,
                    });
                }
                #[cfg(all(
                    debug_assertions,
                    feature = "normal-hap-codec-fixture",
                    target_env = "ohos"
                ))]
                let mut codec_fixture = crate::codec_fixture::Worker::start();
                #[cfg(all(
                    target_env = "ohos",
                    any(
                        feature = "normal-hap-codec-benchmark",
                        feature = "normal-hap-avc444-profile",
                        feature = "normal-hap-hevc444-profile"
                    )
                ))]
                let mut codec_measurement = crate::codec_measurement::Worker::start();
                loop {
                    #[cfg(all(
                        target_env = "ohos",
                        any(
                            feature = "normal-hap-codec-benchmark",
                            feature = "normal-hap-avc444-profile",
                            feature = "normal-hap-hevc444-profile"
                        )
                    ))]
                    codec_measurement.reap();
                    #[cfg(all(
                        debug_assertions,
                        feature = "normal-hap-codec-fixture",
                        target_env = "ohos"
                    ))]
                    codec_fixture.reap();
                    owner.apply_control();
                    // 关键 control 先于任何已排队成功事件；取消前认证 job 从未 admitted。
                    match receiver.recv_timeout(Duration::from_millis(15)) {
                        Ok(command) => owner.command(command),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => {
                            owner.cancel();
                            owner.stopping = true
                        }
                    }
                    owner.poll();
                    let next_snapshot = owner.snapshot();
                    if let Ok(mut value) = published.lock() {
                        *value = next_snapshot;
                    }
                    if owner.stopping && !owner.host.is_active() {
                        let _ = owner.stores.credentials.purge_pending();
                        break;
                    }
                }
            })
            .map_err(|_| crate::WORKER_STOPPED)?;
        Ok(Self {
            sender,
            snapshot,
            control,
            renderer,
        })
    }
    pub(crate) fn native_pointer(
        &self,
        local: u64,
        x: f32,
        y: f32,
        buttons: frd_core::PointerButtons,
        focus: bool,
    ) -> Result<(), i32> {
        let link = self
            .renderer
            .lock()
            .map_err(|_| crate::PANIC)?
            .clone()
            .ok_or(crate::NOT_ATTACHED)?;
        let state = link.0.lock().map_err(|_| crate::PANIC)?;
        if state.surface.local != local {
            return Err(crate::STALE_GENERATION);
        }
        let lifecycle = state.surface.lifecycle;
        let input = state
            .epoch()
            .map(|epoch| InputEpoch {
                session: epoch.session.get(),
                generation: epoch.remote,
                revision: epoch.geometry,
            })
            .unwrap_or(InputEpoch {
                session: 0,
                generation: 0,
                revision: state.geometry_revision,
            });
        drop(state);
        self.send(Command::NativePointer(
            local, lifecycle, input, x, y, buttons, focus,
        ))
    }
    pub(crate) fn bind(&self, link: Arc<crate::presentation::Link>) -> Result<(), i32> {
        let mut current = self.renderer.lock().map_err(|_| crate::PANIC)?;
        if current.as_ref().is_some_and(|old| !Arc::ptr_eq(old, &link)) {
            return Err(crate::INVALID_ARGUMENT);
        }
        link.0.lock().map_err(|_| crate::PANIC)?.bound = true;
        *current = Some(link);
        Ok(())
    }
    pub fn unregistered(root: PathBuf) -> Result<Self, i32> {
        Self::spawn(
            Stores::at(root),
            {
                #[cfg(all(debug_assertions, feature = "native-presentation-fixture"))]
                {
                    vec![Arc::new(crate::fixture::Factory) as Arc<dyn ProtocolFactory>]
                }
                #[cfg(not(all(debug_assertions, feature = "native-presentation-fixture")))]
                {
                    vec![]
                }
            },
            Arc::new(|_| {
                Err(frd_media_api::VideoDecodeError::new(
                    frd_media_api::VideoDecodeErrorCode::BackendUnavailable,
                ))
            }),
            Arc::new(NoAudio),
        )
    }
    pub fn send(&self, command: Command) -> Result<(), i32> {
        let recovery = match &command {
            Command::Edit(_) | Command::Password(_) => FORM_INVALID,
            Command::Input(..) | Command::Pointer(..) | Command::NativePointer(..) => FOCUS_LOST,
            _ => 0,
        };
        let revision = self.control.load(Ordering::Acquire) / CONTROL_REVISION;
        self.sender
            .try_send(Command::Queued(revision, Box::new(command)))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    if recovery != 0 {
                        signal(&self.control, recovery);
                    }
                    crate::QUEUE_FULL
                }
                mpsc::TrySendError::Disconnected(_) => crate::WORKER_STOPPED,
            })
    }
    pub fn cancel(&self) {
        signal(&self.control, CANCEL);
    }
    pub fn lose_focus(&self) {
        signal(&self.control, FOCUS_LOST);
    }
    pub fn stop(&self) {
        signal(&self.control, STOP);
    }
    pub fn snapshot(&self) -> Result<Vec<u8>, i32> {
        let copy = self.snapshot.lock().map_err(|_| crate::PANIC)?.clone();
        serde_json::to_vec(&copy).map_err(|_| crate::PANIC)
    }
}
impl Drop for AppEngine {
    fn drop(&mut self) {
        self.stop()
    }
}
struct PendingLoad {
    ticket: SavedPasswordLoadTicket,
    deadline: Instant,
    timed_out: bool,
}
struct Owner {
    renderer: Arc<Mutex<Option<Arc<crate::presentation::Link>>>>,
    ready: Option<crate::presentation::Epoch>,
    remote_epoch: Option<(frd_core::SessionId, u64)>,
    controller: AppController,
    host: SessionHost,
    catalog: ProtocolCatalog,
    stores: Stores,
    input: InputAdapter,
    pending: BTreeMap<u64, PendingLoad>,
    tx: SyncSender<Command>,
    last_submit: u64,
    form_invalid_before: u64,
    cancelled_launch: bool,
    stopping: bool,
    notice: Option<&'static str>,
    probe: i32,
    control: Arc<AtomicU64>,
}
impl Owner {
    fn apply_control(&mut self) {
        let state = self.control.fetch_and(!CONTROL_MASK, Ordering::AcqRel);
        let flags = state & CONTROL_MASK;
        if flags & FOCUS_LOST != 0 {
            self.focus(false, false, false);
        }
        if flags & FORM_INVALID != 0 {
            self.form_invalid_before = state / CONTROL_REVISION;
            if let Some(form) = self.controller.connection_form_mut() {
                form.set_password(SecretBuffer::new(Vec::new()));
            }
            self.notice = Some("操作队列暂满，请重新确认表单与密码");
        }
        if flags & (STOP | CANCEL) != 0 {
            self.cancel();
            self.stopping |= flags & STOP != 0;
        }
    }
    fn send(&self, c: Option<SessionCommand>) {
        if let Some(c) = c {
            let _ = self.host.send_command(c);
        }
    }
    fn release(&mut self) {
        let c = self.input.release(&self.controller);
        self.send(c)
    }
    fn focus(&mut self, foreground: bool, focused: bool, remote: bool) {
        let c = self
            .input
            .focus(&self.controller, foreground, focused, remote);
        self.send(c)
    }
    fn cancel(&mut self) {
        self.release();
        self.clear_remote();
        if self.host.launch_is_pending() {
            self.host.cancel_pending_launch();
            self.cancelled_launch = true;
            return;
        }
        let intent = if matches!(
            self.controller.page(),
            Page::Connecting { .. } | Page::AwaitingFirstFrame { .. }
        ) {
            AppIntent::CancelConnect
        } else {
            AppIntent::Disconnect
        };
        self.intent(intent);
        self.cleanup();
    }
    fn cleanup(&mut self) {
        let tx = self.tx.clone();
        if self
            .host
            .begin_cleanup(move |result| {
                let _ = tx.send(Command::Cleanup(result));
            })
            .is_err()
        {
            self.notice = Some("会话清理失败，请重新启动应用");
        }
    }
    fn intent(&mut self, intent: AppIntent) {
        if matches!(
            intent,
            AppIntent::CancelConnect | AppIntent::Disconnect | AppIntent::ReturnToConnection
        ) {
            self.release();
            self.clear_remote();
        }
        match self
            .controller
            .handle_intent_with_stores(intent, &self.catalog, self.stores.borrow())
        {
            Ok(Some(AppAction::SessionCommand(c))) => {
                self.send(Some(c));
                self.cleanup()
            }
            Ok(Some(AppAction::StartSession(request, permit))) => {
                self.cancelled_launch = false;
                let target = self
                    .controller
                    .page()
                    .retained_draft()
                    .target_system
                    .unwrap_or(TargetSystem::Custom);
                let tx = self.tx.clone();
                match self.host.begin_launch(permit, target, request, move |r| {
                    let _ = tx.send(Command::Launch(r));
                }) {
                    Ok(true) => {}
                    _ => self.notice = Some("会话启动未被接受"),
                }
            }
            Ok(None) => {}
            Err(_) => self.notice = Some("当前操作不可用"),
        }
    }
    fn command(&mut self, c: Command) {
        self.apply_control();
        self.sync_presentation(false);
        let c = if let Command::Queued(revision, command) = c {
            let control = self.control.load(Ordering::Acquire);
            // 已接受的身份/密码编辑按 FIFO 落实；远程焦点屏障不能丢弃用户输入。
            // 仅显式表单溢出屏障作废旧编辑，同时已清除旧密码及 load ticket。
            let form_edit = matches!(*command, Command::Edit(_) | Command::Password(_));
            if (form_edit && revision < self.form_invalid_before)
                || (!form_edit
                    && (revision != control / CONTROL_REVISION || control & CONTROL_MASK != 0))
            {
                return;
            }
            *command
        } else {
            c
        };
        if self.stopping
            && !matches!(
                c,
                Command::Launch(_)
                    | Command::Cleanup(_)
                    | Command::Loaded(..)
                    | Command::Persisted(..)
            )
        {
            return;
        }
        match c {
            Command::Queued(..) => {}
            Command::Edit(e) => {
                let choice = if e.protocol == "auto" {
                    ProtocolChoice::Automatic
                } else if let Some(p) = ProtocolId::new(e.protocol) {
                    ProtocolChoice::Explicit(p)
                } else {
                    self.notice = Some("此协议当前不可用");
                    return;
                };
                if let Some(f) = self.controller.connection_form_mut() {
                    let old = f.draft.clone();
                    f.draft.target_system = match e.target {
                        1 => Some(TargetSystem::Windows),
                        2 => Some(TargetSystem::MacOs),
                        3 => Some(TargetSystem::Linux),
                        _ => None,
                    };
                    f.draft.address = e.address;
                    f.draft.port = u16::try_from(e.port).ok().filter(|p| *p != 0);
                    f.draft.username = e.username;
                    f.draft.protocol = choice;
                    f.remember_on_this_device = e.remember;
                    f.invalidate_loaded_secret_after_identity_edit(&old);
                }
            }
            Command::Password(secret) => {
                if let Some(f) = self.controller.connection_form_mut() {
                    f.set_password(secret)
                }
            }
            Command::Select(index) => {
                if self.pending.len() >= 2 {
                    self.notice = Some("凭据读取仍在处理中，请稍后重试");
                    return;
                }
                let key = match self.controller.page() {
                    Page::ConnectionForm(f) => f.profiles.get(index).map(|p| p.key.clone()),
                    _ => None,
                };
                if let Some(key) = key {
                    if let Some(ticket) = self.controller.begin_saved_profile_load(&key) {
                        let id = ticket.id;
                        self.pending.insert(
                            id,
                            PendingLoad {
                                ticket,
                                deadline: Instant::now() + Duration::from_secs(5),
                                timed_out: false,
                            },
                        );
                        let store = self.stores.credentials.clone();
                        let tx = self.tx.clone();
                        if std::thread::Builder::new()
                            .name("frd-ohos-vault-read".into())
                            .spawn(move || {
                                let _scope = crate::panic_scope::Scope::enter();
                                let result = store.load(&key);
                                let _ = tx.send(Command::Loaded(id, result));
                            })
                            .is_err()
                        {
                            if let Some(p) = self.pending.remove(&id) {
                                self.controller.finish_saved_profile_load(
                                    &p.ticket,
                                    Err(PlatformError::Unavailable),
                                );
                            }
                        }
                    }
                }
            }
            Command::Submit {
                event,
                composition,
                repeat,
            } => {
                if composition
                    || repeat
                    || event == 0
                    || event <= self.last_submit
                    || !matches!(self.controller.page(), Page::ConnectionForm(_))
                {
                    return;
                }
                self.last_submit = event;
                if let Some(f) = self.controller.connection_form_mut() {
                    if let Some(s) = f.take_submission(&self.catalog) {
                        self.intent(AppIntent::Connect(s));
                    } else {
                        self.notice = Some("请检查表单；此设备尚未注册可用协议");
                    }
                }
            }
            Command::Intent(i) => self.intent(i),
            Command::Focus {
                foreground,
                focused,
                remote,
            } => self.focus(foreground, focused, remote),
            Command::Geometry(g) => {
                let c = self.input.set_geometry(&self.controller, g);
                self.send(c)
            }
            Command::Input(e, event, pressed) => {
                if !self.input_ready() {
                    self.input.observe_blocked(&event);
                    return;
                }
                let c = self.input.route(
                    &self.controller,
                    e.session,
                    e.generation,
                    e.revision,
                    event,
                    pressed,
                );
                self.send(c)
            }
            Command::NativePointer(local, lifecycle, input, x, y, buttons, take_focus) => {
                if !self.input_ready() {
                    self.input.observe_blocked_pointer(buttons);
                    return;
                }
                if self.ready.is_none_or(|e| {
                    e.local != local
                        || e.lifecycle != lifecycle
                        || e.session.get() != input.session
                        || e.remote != input.generation
                        || e.geometry != input.revision
                }) {
                    return;
                }
                let epoch = self.ready.unwrap();
                if take_focus {
                    self.focus(true, true, true);
                }
                let command = self.input.pointer(
                    &self.controller,
                    epoch.session.get(),
                    epoch.remote,
                    epoch.geometry,
                    x,
                    y,
                    buttons,
                    Default::default(),
                );
                self.send(command);
            }
            Command::Pointer(e, x, y, buttons, wheel) => {
                if !self.input_ready() {
                    self.input.observe_blocked_pointer(buttons);
                    return;
                }
                let c = self.input.pointer(
                    &self.controller,
                    e.session,
                    e.generation,
                    e.revision,
                    x,
                    y,
                    buttons,
                    wheel,
                );
                self.send(c)
            }
            Command::Loaded(id, result) => {
                if let Some(p) = self.pending.remove(&id) {
                    if !p.timed_out && Instant::now() <= p.deadline {
                        self.controller.finish_saved_profile_load(&p.ticket, result)
                    }
                }
            }
            Command::Persisted(s, w) => self.controller.complete_profile_persistence(s, w),
            Command::Launch(r) => {
                let tx = self.tx.clone();
                match self.host.accept_launch_outcome(r, move |r| {
                    let _ = tx.send(Command::Cleanup(r));
                }) {
                    Ok(AcceptedLaunchOutcome::Started) => {}
                    Ok(AcceptedLaunchOutcome::CancelledStarted) => {
                        self.intent(AppIntent::CancelConnect)
                    }
                    Ok(
                        AcceptedLaunchOutcome::LaunchRolledBack(f)
                        | AcceptedLaunchOutcome::CancelledLaunchRolledBack(f),
                    ) => {
                        let _ = self
                            .controller
                            .consume_launch_rollback_with_stores(&f, self.stores.borrow());
                        if self.cancelled_launch {
                            self.intent(AppIntent::ReturnToConnection)
                        }
                    }
                    Err(_) => self.notice = Some("会话启动失败"),
                }
            }
            Command::Cleanup(r) => match self.host.accept_cleanup_outcome(r) {
                Ok(c) => {
                    let _ = self
                        .controller
                        .finish_session_cleanup_with_stores(c, self.stores.borrow());
                }
                Err(_) => self.notice = Some("会话清理失败，请重新启动应用"),
            },
            #[cfg(debug_assertions)]
            Command::Probe(root) => {
                if self.probe == 1 {
                    return;
                }
                self.probe = 1;
                let tx = self.tx.clone();
                let _ = std::thread::spawn(move || {
                    let _scope = crate::panic_scope::Scope::enter();
                    let code = frd_platform_ohos::debug_huks_probe(root);
                    let _ = tx.send(Command::Probed(code));
                });
            }
            #[cfg(debug_assertions)]
            Command::Probed(code) => self.probe = code,
        }
    }
    fn poll(&mut self) {
        let now = Instant::now();
        for p in self.pending.values_mut() {
            if !p.timed_out && now > p.deadline {
                p.timed_out = true;
                self.controller
                    .saved_profile_load_notice(&p.ticket, "安全存储读取超时；请输入密码或稍后重试");
            }
        }
        for (s, e) in self.host.drain_session_events() {
            self.runtime_event(s, e);
        }
        self.sync_presentation(true);
    }
    fn clear_remote(&mut self) {
        self.ready = None;
        self.remote_epoch = None;
        self.input.remote = None;
        if let Some(link) = self.renderer.lock().ok().and_then(|r| r.clone()) {
            if let Ok(mut state) = link.0.lock() {
                state.session = None;
                state.batch = None;
                state.feedback = None;
                state.busy = false;
            }
        }
    }
    fn input_ready(&self) -> bool {
        let Some(ready) = self.ready else {
            return false;
        };
        if self.controller.interactive_input_epoch() != Some((ready.session, ready.remote)) {
            return false;
        }
        self.renderer
            .lock()
            .ok()
            .and_then(|r| r.clone())
            .is_some_and(|link| {
                link.0
                    .lock()
                    .ok()
                    .is_some_and(|s| s.ready_epoch() == Some(ready))
            })
    }
    fn sync_presentation(&mut self, drain: bool) {
        let Some(link) = self.renderer.lock().ok().and_then(|r| r.clone()) else {
            return;
        };
        let (feedback, eligible, failed) = {
            let Ok(mut state) = link.0.lock() else {
                return;
            };
            state.session = self.remote_epoch;
            state.geometry = self.input.geometry;
            state.geometry_revision = self.input.revision;
            state.remote_size = self.input.remote;
            let feedback = state.feedback.take();
            if feedback.is_some() {
                state.busy = false;
            }
            (feedback, state.ready_epoch(), state.failure)
        };
        if self.ready.is_some() && self.ready != eligible {
            self.release();
            self.ready = None;
        }
        if failed.is_some() {
            self.release();
            self.ready = None;
            self.notice = Some("原生呈现失败，请重新启动应用");
            if let Some((session, _)) = self.remote_epoch {
                self.runtime_event(
                    session,
                    SessionEvent::Error(ProtocolError::adapter(
                        ProtocolId::rdp(),
                        "native_presentation_failed",
                    )),
                );
                self.clear_remote();
            }
            return;
        }
        if let Some(crate::presentation::Feedback {
            epoch,
            event: Some(event),
        }) = feedback
        {
            if eligible == Some(epoch) {
                if let PresentationEvent::FramePresented {
                    session_id,
                    generation,
                    completeness,
                    ..
                } = &event
                {
                    if *session_id == epoch.session
                        && *generation == epoch.remote
                        && *completeness == frd_frame::FrameCompleteness::FullBaseline
                    {
                        self.controller.handle_presentation(event);
                        if self.controller.interactive_input_epoch()
                            == Some((epoch.session, epoch.remote))
                        {
                            self.ready = Some(epoch);
                        }
                    }
                }
            }
        }
        if !drain || eligible.is_none() {
            return;
        }
        let can_drain = link
            .0
            .lock()
            .ok()
            .is_some_and(|s| !s.busy && s.batch.is_none() && s.feedback.is_none());
        if !can_drain {
            return;
        }
        match self.host.drain_frame_transactions() {
            Ok(batch) => {
                let batch = batch.into_transactions();
                if batch.is_empty() {
                    return;
                }
                // mailbox 自身有界；单批次槽使暂停/背压期间不继续排空生产者。
                if let Ok(mut state) = link.0.lock() {
                    state.batch = Some((eligible.unwrap(), batch));
                    state.busy = true;
                }
            }
            Err(_) => link.fail(crate::GPU_FAILED),
        }
    }
    fn runtime_event(&mut self, s: frd_core::SessionId, e: SessionEvent) {
        if matches!(
            e,
            SessionEvent::StageChanged(ConnectionStage::TransportReady)
        ) {
            // 认证事件已有真实runtime来源；此原子比较是写入admission的线性化点。
            // 此点前收到的取消必须先discard；此点后取消不撤销已经授权的OS写入。
            loop {
                let flags = self.control.load(Ordering::Acquire);
                if flags & (STOP | CANCEL) != 0 {
                    self.cancel();
                    return;
                }
                if self
                    .control
                    .compare_exchange(flags, flags, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    break;
                }
            }
        }

        let terminal = matches!(
            e,
            SessionEvent::Error(_)
                | SessionEvent::Closed(_)
                | SessionEvent::SurfaceGenerationChanged { .. }
                | SessionEvent::StageChanged(ConnectionStage::Disconnecting)
        );
        if terminal {
            self.release()
        }
        if let SessionEvent::SurfaceGenerationChanged {
            session_id,
            generation,
            size,
        } = &e
        {
            if *session_id == s
                && self.host.active_session_id() == Some(s)
                && self
                    .remote_epoch
                    .is_none_or(|(old, g)| old != s || *generation > g)
            {
                self.ready = None;
                self.remote_epoch = Some((s, *generation));
                self.input.remote = Some(*size);
            }
        }
        if matches!(
            e,
            SessionEvent::Error(_)
                | SessionEvent::Closed(_)
                | SessionEvent::StageChanged(ConnectionStage::Disconnecting)
        ) {
            if self.remote_epoch.is_some_and(|(session, _)| session == s) {
                self.clear_remote();
            }
        }
        let success = matches!(
            e,
            SessionEvent::StageChanged(ConnectionStage::TransportReady)
        );
        self.controller
            .handle_session_event_with_stores_deferred_profile(s, e, self.stores.borrow());
        let identity = self.controller.take_pending_server_identity_command();
        self.send(identity);
        if success {
            if let Some(job) = self.controller.take_pending_profile_job(s) {
                let stores = self.stores.clone();
                let tx = self.tx.clone();
                let spawn = std::thread::Builder::new()
                    .name("frd-ohos-vault-commit".into())
                    .spawn(move || {
                        let _scope = crate::panic_scope::Scope::enter();
                        let w = persist_profile_job(
                            job,
                            stores.profiles.as_ref(),
                            stores.credentials.as_ref(),
                        );
                        let _ = tx.send(Command::Persisted(s, w));
                    });
                if spawn.is_err() {
                    let _ = self.stores.credentials.discard(s);
                    self.controller.complete_profile_persistence(
                        s,
                        Some(ProfilePersistenceWarning::SaveFailed),
                    );
                }
            }
        }
        if terminal
            && !matches!(
                self.controller.page(),
                Page::RemoteSession { .. } | Page::AwaitingFirstFrame { .. }
            )
        {
            self.cleanup()
        }
    }
    fn snapshot(&self) -> Value {
        let page = match self.controller.page() {
            Page::ConnectionForm(_) => 0,
            Page::Connecting { .. } => 1,
            Page::AwaitingFirstFrame { .. } => 2,
            Page::RemoteSession { .. } => 3,
            Page::Disconnecting { .. } => 4,
            Page::Failed { .. } => 5,
        };
        let (status_code, status_message) = match self.controller.page() {
            Page::Failed { code, .. } => match code.as_str() {
                "native_presentation_failed" => {
                    ("native_presentation_failed", "原生呈现失败，请重新启动应用")
                }
                "rdp_server_identity_changed" => (
                    "rdp_server_identity_changed",
                    "服务器证书指纹已变化，已停止自动连接；请核实服务器身份",
                ),
                "rdp_identity_store_failed" => (
                    "rdp_identity_store_failed",
                    "无法读取或保存服务器证书记录，已停止连接",
                ),
                "rdp_server_identity_rejected" => (
                    "rdp_server_identity_rejected",
                    "服务器身份未获信任，已停止连接",
                ),
                "authentication_failed" | "rdp_logon_failed" | "rdp_nla_failed" => (
                    "authentication_failed",
                    "身份验证失败，请检查账户和密码后重试",
                ),
                "rdp_dns_failed" => ("rdp_dns_failed", "无法解析服务器地址，请检查主机名"),
                "rdp_tcp_failed" => ("rdp_tcp_failed", "无法连接服务器，请检查地址、端口和网络"),
                "rdp_tls_failed" => ("rdp_tls_failed", "安全连接建立失败，请检查服务器设置"),
                "rdp_license_failed" => ("rdp_license_failed", "服务器未接受远程桌面许可"),
                "rdp_activation_failed" | "rdp_egfx_failed" => (
                    "remote_display_failed",
                    "远程画面初始化失败，请检查服务器支持的显示设置",
                ),
                _ => ("session_failed", "连接失败，请检查服务器与网络后重试"),
            },
            Page::Connecting { .. } => ("connecting", "正在连接"),
            Page::AwaitingFirstFrame { .. } => ("awaiting_first_frame", "等待首个完整画面"),
            Page::RemoteSession { .. } => ("connected", "已连接"),
            Page::Disconnecting { .. } => ("disconnecting", "正在断开"),
            Page::ConnectionForm(_) => ("connection_form", "准备连接"),
        };
        let d = self.controller.page().retained_draft();
        let protocol = match d.protocol {
            ProtocolChoice::Automatic => "auto".into(),
            ProtocolChoice::Explicit(p) => p.as_str().to_owned(),
        };
        // 保留已建立的远端身份用于接收本地 key-up；发送仍由当前原生呈现证明门控。
        let epoch = self
            .controller
            .interactive_input_epoch()
            .map(|(s, g)| (s.get().to_string(), g.to_string()))
            .unwrap_or_default();
        let available = self
            .catalog
            .select(
                d.target_system.unwrap_or(TargetSystem::Custom),
                match &protocol[..] {
                    "auto" => frd_protocol_api::ProtocolSelection::Automatic,
                    _ => frd_protocol_api::ProtocolSelection::Explicit(
                        ProtocolId::new(protocol.clone()).unwrap(),
                    ),
                },
            )
            .is_ok();
        let mut value = json!({"abiVersion":1,"initialized":true,"page":page,"target":match d.target_system{Some(TargetSystem::Windows)=>1,Some(TargetSystem::MacOs)=>2,Some(TargetSystem::Linux)=>3,_=>0},"address":d.address,"port":d.port.unwrap_or(0),"username":d.username,"protocol":protocol,"available":available,"session":epoch.0,"generation":epoch.1,"geometryRevision":self.input.revision.to_string(),"notice":self.notice.unwrap_or(""),"probe":self.probe});
        #[cfg(all(debug_assertions, feature = "native-presentation-fixture"))]
        {
            value["syntheticFixture"] = json!(true);
            value["fixtureInputs"] = json!(crate::fixture::INPUTS.load(Ordering::Relaxed));
            value["fixtureReleases"] = json!(crate::fixture::RELEASES.load(Ordering::Relaxed));
        }
        if let Some(link) = self.renderer.lock().ok().and_then(|r| r.clone()) {
            if let Ok(s) = link.0.lock() {
                #[cfg(all(
                    debug_assertions,
                    feature = "native-presentation-fixture",
                    target_env = "ohos"
                ))]
                crate::fixture::observe(&s, self.ready.is_some() && s.ready_epoch() == self.ready);
                value["nativePresentation"] = json!({"local":s.surface.local.to_string(),"lifecycle":s.surface.lifecycle.to_string(),"appliedBatches":s.applied,"presentedFrames":s.presented,"ready":self.ready.is_some() && s.ready_epoch()==self.ready,"failure":s.failure.unwrap_or(0)});
            }
        }
        value["inputReady"] = json!(self.input_ready());
        value["statusCode"] = json!(status_code);
        value["statusMessage"] = json!(status_message);
        if let Page::ConnectionForm(f) = self.controller.page() {
            value["selectedIndex"] = json!(f
                .selected_profile
                .as_ref()
                .and_then(|key| f.profiles.iter().position(|p| &p.key == key)));
            value["remember"] = json!(f.remember_on_this_device);
            value["credentialLoaded"] =
                json!(f.selected_profile.is_some() && !f.password_is_empty());
            value["loading"] = json!(f.password_load_status().unwrap_or(""));
            value["profiles"]=json!(f.profiles.iter().map(|p|json!({"address":p.key.address(),"username":p.key.username(),"port":p.key.port()})).collect::<Vec<_>>());
            let e = f.errors();
            value["errors"] = json!({"address":e.address,"port":e.port,"username":e.username,"password":e.password,"protocol":e.protocol,"target":e.target_system,"profile":e.profile});
            value["canConnect"] = json!(
                available
                    && !f.password_is_empty()
                    && !f.draft.address.trim().is_empty()
                    && !f.draft.username.trim().is_empty()
                    && f.draft.port.is_some()
            );
        } else {
            value["canConnect"] = json!(false)
        }
        if let Some(w) = self.controller.profile_persistence_warning() {
            value["persistenceWarning"] = json!(w.message());
        }
        if let Some(c) = self.controller.current_server_identity_challenge() {
            value["certificate"] = json!({"fingerprint":c.sha256_fingerprint.iter().map(|b|format!("{b:02x}")).collect::<String>(),"subject":c.subject,"issuer":c.issuer});
        }
        if let Some(v) = self.input.viewport() {
            value["viewport"] = json!({"x":v.x,"y":v.y,"width":v.width,"height":v.height,"clipX":v.clip.x,"clipY":v.clip.y,"clipWidth":v.clip.width,"clipHeight":v.clip.height});
        }
        // 明确字段投影；从不序列化 controller/form 或 protocol error/debug payload。
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frd_platform_api::{ConnectionProfileKey, SavedConnectionProfile};
    struct Profiles;
    fn profile() -> SavedConnectionProfile {
        SavedConnectionProfile {
            key: ConnectionProfileKey::new(ProtocolId::rdp(), "fixture.invalid", 3389, "synthetic")
                .unwrap(),
            target_system: TargetSystem::Windows,
            last_success_order: 1,
        }
    }
    impl ConnectionProfileStore for Profiles {
        fn list(&self) -> Result<Vec<SavedConnectionProfile>, PlatformError> {
            Ok(vec![profile()])
        }
        fn upsert(&self, _: &SavedConnectionProfile) -> Result<(), PlatformError> {
            Ok(())
        }
        fn delete(&self, _: &ConnectionProfileKey) -> Result<(), PlatformError> {
            Ok(())
        }
    }
    struct Pins;
    impl ServerIdentityStore for Pins {
        fn load_pin(
            &self,
            _: &ProtocolId,
            _: &frd_core::Endpoint,
        ) -> Result<Option<[u8; 32]>, PlatformError> {
            Ok(None)
        }
        fn store_pin(
            &self,
            _: &ProtocolId,
            _: &frd_core::Endpoint,
            _: [u8; 32],
        ) -> Result<(), PlatformError> {
            Ok(())
        }
    }
    struct NoCredentials;
    impl SecureCredentialStore for NoCredentials {
        fn load(&self, _: &ConnectionProfileKey) -> Result<Option<SecretBuffer>, PlatformError> {
            Ok(None)
        }
        fn stage(
            &self,
            _: frd_core::SessionId,
            _: &ConnectionProfileKey,
            _: &SecretBuffer,
        ) -> Result<(), PlatformError> {
            Ok(())
        }
        fn commit(
            &self,
            _: frd_core::SessionId,
            _: &ConnectionProfileKey,
        ) -> Result<(), PlatformError> {
            panic!("未认证不得commit")
        }
        fn discard(&self, _: frd_core::SessionId) -> Result<(), PlatformError> {
            Ok(())
        }
        fn delete(&self, _: &ConnectionProfileKey) -> Result<(), PlatformError> {
            Ok(())
        }
        fn purge_pending(&self) -> Result<(), PlatformError> {
            Ok(())
        }
    }
    fn owner() -> Owner {
        let stores = Stores {
            context_root: None,
            identities: Arc::new(Pins),
            profiles: Arc::new(Profiles),
            credentials: Arc::new(NoCredentials),
        };
        let controller = AppController::connection_form_with_stores(
            ConnectionForm::new(ConnectionDraft::default()),
            stores.borrow(),
        );
        let (tx, _rx) = mpsc::sync_channel(64);
        Owner {
            renderer: Arc::new(Mutex::new(None)),
            ready: None,
            remote_epoch: None,
            controller,
            host: SessionHost::new(
                vec![],
                Arc::new(Wake),
                Arc::new(NoAudio),
                Arc::new(|_| {
                    Err(frd_media_api::VideoDecodeError::new(
                        frd_media_api::VideoDecodeErrorCode::BackendUnavailable,
                    ))
                }),
            ),
            catalog: ProtocolCatalog::new([]),
            stores,
            input: InputAdapter::default(),
            pending: BTreeMap::new(),
            tx,
            last_submit: 0,
            form_invalid_before: 0,
            cancelled_launch: false,
            stopping: false,
            notice: None,
            probe: 0,
            control: Arc::new(AtomicU64::new(0)),
        }
    }
    fn pending(o: &mut Owner) -> u64 {
        let ticket = o
            .controller
            .begin_saved_profile_load(&profile().key)
            .unwrap();
        let id = ticket.id;
        o.pending.insert(
            id,
            PendingLoad {
                ticket,
                deadline: Instant::now() + Duration::from_secs(5),
                timed_out: false,
            },
        );
        id
    }
    fn presentation_owner() -> (Owner, frd_core::SessionId, Arc<crate::presentation::Link>) {
        let (mut owner, session, _commits) = staged_owner();
        owner.controller.handle_session_event_with_stores(
            session,
            SessionEvent::StageChanged(ConnectionStage::TransportReady),
            owner.stores.borrow(),
        );
        owner.controller.handle_session_event_with_stores(
            session,
            SessionEvent::SurfaceGenerationChanged {
                session_id: session,
                generation: 1,
                size: frd_core::PixelSize {
                    width: 2,
                    height: 2,
                },
            },
            owner.stores.borrow(),
        );
        owner.remote_epoch = Some((session, 1));
        owner.input.remote = Some(frd_core::PixelSize {
            width: 2,
            height: 2,
        });
        owner.input.geometry = Some(ViewGeometry {
            drawable: frd_core::PixelSize {
                width: 100,
                height: 100,
            },
            bounds: frd_core::PixelRect {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
            scale: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
        });
        let link = Arc::new(crate::presentation::Link::default());
        link.surface(crate::PocStatus {
            surface_generation: 1,
            width: 100,
            height: 100,
            attached: 1,
            foreground: 1,
            ..Default::default()
        });
        link.0.lock().unwrap().bound = true;
        *owner.renderer.lock().unwrap() = Some(link.clone());
        owner.sync_presentation(false);
        (owner, session, link)
    }
    fn feedback(
        link: &crate::presentation::Link,
        epoch: crate::presentation::Epoch,
        completeness: frd_frame::FrameCompleteness,
    ) {
        link.0.lock().unwrap().feedback = Some(crate::presentation::Feedback {
            epoch,
            event: Some(PresentationEvent::FramePresented {
                session_id: epoch.session,
                generation: epoch.remote,
                revision: 1,
                completeness,
            }),
        });
    }
    #[test]
    fn only_actual_current_full_receipt_opens_local_input_and_lifecycle_rejects_late_feedback() {
        let (mut owner, _, link) = presentation_owner();
        let epoch = link.0.lock().unwrap().epoch().unwrap();
        assert!(!owner.input_ready());
        feedback(&link, epoch, frd_frame::FrameCompleteness::Incremental);
        owner.sync_presentation(false);
        assert!(!owner.input_ready());
        for stale in [
            crate::presentation::Epoch {
                session: frd_core::SessionId::allocate(),
                ..epoch
            },
            crate::presentation::Epoch { remote: 2, ..epoch },
            crate::presentation::Epoch { local: 2, ..epoch },
            crate::presentation::Epoch {
                geometry: 2,
                ..epoch
            },
        ] {
            feedback(&link, stale, frd_frame::FrameCompleteness::FullBaseline);
            owner.sync_presentation(false);
            assert!(!owner.input_ready());
        }
        feedback(&link, epoch, frd_frame::FrameCompleteness::FullBaseline);
        owner.sync_presentation(false);
        assert!(owner.input_ready());
        for mode in 0..3 {
            let old = link.0.lock().unwrap().epoch().unwrap();
            link.surface(crate::PocStatus {
                surface_generation: 1,
                width: 100,
                height: 100,
                attached: u32::from(mode != 0),
                foreground: u32::from(mode != 1),
                ..Default::default()
            });
            assert!(!owner.input_ready());
            feedback(&link, old, frd_frame::FrameCompleteness::FullBaseline);
            owner.sync_presentation(false);
            assert!(!owner.input_ready());
            link.surface(crate::PocStatus {
                surface_generation: 2,
                width: 100,
                height: 100,
                attached: 1,
                foreground: 1,
                ..Default::default()
            });
            owner.sync_presentation(false);
            let new = link.0.lock().unwrap().epoch().unwrap();
            feedback(&link, new, frd_frame::FrameCompleteness::FullBaseline);
            owner.sync_presentation(false);
            assert!(owner.input_ready());
        }
        owner.clear_remote();
        assert!(!owner.input_ready());
        feedback(&link, epoch, frd_frame::FrameCompleteness::FullBaseline);
        owner.sync_presentation(false);
        assert!(!owner.input_ready());
    }
    #[test]
    fn native_failure_is_visible_terminal_and_old_feedback_cannot_reopen() {
        let (mut owner, _, link) = presentation_owner();
        let epoch = link.0.lock().unwrap().epoch().unwrap();
        feedback(&link, epoch, frd_frame::FrameCompleteness::FullBaseline);
        owner.sync_presentation(false);
        assert!(owner.input_ready());
        link.fail(crate::GPU_FAILED);
        owner.sync_presentation(false);
        assert!(!owner.input_ready());
        assert!(owner.remote_epoch.is_none());
        assert_eq!(owner.snapshot()["statusCode"], "native_presentation_failed");
        feedback(&link, epoch, frd_frame::FrameCompleteness::FullBaseline);
        owner.sync_presentation(false);
        assert!(!owner.input_ready());
    }
    #[test]
    fn real_mailbox_compiler_drains_only_one_owned_batch_until_feedback() {
        use frd_frame::{
            FrameCompleteness, FrameMailbox, PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate,
        };
        let (mut owner, session, link) = presentation_owner();
        let mailbox = Arc::new(Mutex::new(FrameMailbox::new(8, 128)));
        owner
            .host
            .attach_frame_test_mailbox(session, mailbox.clone());
        mailbox.lock().unwrap().push(SurfaceUpdate::Reset {
            session_id: session,
            generation: 1,
            size: frd_core::PixelSize {
                width: 2,
                height: 2,
            },
            format: PixelFormat::Bgrx8UnormSrgb,
        });
        let push = |revision| {
            let mut mailbox = mailbox.lock().unwrap();
            mailbox.push(SurfaceUpdate::Damage {
                session_id: session,
                generation: 1,
                revision,
                patches: vec![PixelPatch {
                    rect: frd_core::PixelRect {
                        x: 0,
                        y: 0,
                        width: 2,
                        height: 2,
                    },
                    stride_bytes: 8,
                    pixels: PixelBuffer::new(vec![0; 16]),
                }],
            });
            mailbox.push(SurfaceUpdate::FrameBoundary {
                session_id: session,
                generation: 1,
                revision,
                completeness: if revision == 1 {
                    FrameCompleteness::FullBaseline
                } else {
                    FrameCompleteness::Incremental
                },
            });
        };
        push(1);
        owner.sync_presentation(true);
        assert!(link.0.lock().unwrap().batch.is_some());
        push(2);
        owner.sync_presentation(true);
        assert_eq!(mailbox.lock().unwrap().len(), 2);
        link.surface(crate::PocStatus {
            surface_generation: 1,
            width: 100,
            height: 100,
            attached: 0,
            foreground: 1,
            ..Default::default()
        });
        owner.sync_presentation(true);
        assert_eq!(mailbox.lock().unwrap().len(), 2);
        let mut state = link.0.lock().unwrap();
        let (_, batch) = state.batch.take().unwrap();
        assert!(matches!(
            batch.as_slice(),
            [frd_frame::FrameTransaction::Startup { .. }]
        ));
        state.feedback = Some(crate::presentation::Feedback {
            epoch: state.epoch().unwrap(),
            event: None,
        });
        drop(state);
        owner.sync_presentation(true);
        assert_eq!(mailbox.lock().unwrap().len(), 2);
        link.surface(crate::PocStatus {
            surface_generation: 2,
            width: 100,
            height: 100,
            attached: 1,
            foreground: 1,
            ..Default::default()
        });
        owner.sync_presentation(true);
        assert_eq!(mailbox.lock().unwrap().len(), 0);
        assert!(
            matches!(link.0.lock().unwrap().batch.as_ref().unwrap().1.as_slice(), [frd_frame::FrameTransaction::Revision { revision, .. }] if revision.revision==2)
        );
    }

    #[test]
    fn stale_password_result_after_edit_and_timeout_are_wiped_not_reselected() {
        let mut o = owner();
        let id = pending(&mut o);
        o.command(Command::Password(SecretBuffer::from_text(
            "manual-ephemeral-fixture".into(),
        )));
        o.command(Command::Loaded(
            id,
            Ok(Some(SecretBuffer::from_text(
                "late-ephemeral-fixture".into(),
            ))),
        ));
        assert_eq!(
            o.controller
                .connection_form_mut()
                .unwrap()
                .password_mut()
                .expose_text(),
            Some("manual-ephemeral-fixture")
        );
        let id = pending(&mut o);
        o.pending.get_mut(&id).unwrap().deadline = Instant::now() - Duration::from_secs(1);
        o.poll();
        assert!(o.pending[&id].timed_out);
        o.command(Command::Loaded(
            id,
            Ok(Some(SecretBuffer::from_text(
                "late-deadline-fixture".into(),
            ))),
        ));
        assert!(o
            .controller
            .connection_form_mut()
            .unwrap()
            .password_is_empty());
        assert!(o.pending.is_empty());
    }
    #[test]
    fn snapshot_and_queue_rejections_never_export_password_or_secret_length() {
        let mut o = owner();
        let id = pending(&mut o);
        o.command(Command::Loaded(
            id,
            Ok(Some(SecretBuffer::from_text(
                "snapshot-forbidden-secret".into(),
            ))),
        ));
        let v = o.snapshot();
        let text = serde_json::to_string(&v).unwrap();
        assert!(!text.contains("snapshot-forbidden-secret"));
        assert!(!text.contains("passwordLength"));
        assert_eq!(v["credentialLoaded"], true);
        assert_eq!(v["available"], false);
        assert_eq!(v["canConnect"], false);
        o.command(Command::Submit {
            event: 1,
            composition: true,
            repeat: false,
        });
        assert_eq!(o.last_submit, 0);
        o.command(Command::Submit {
            event: 1,
            composition: false,
            repeat: true,
        });
        assert_eq!(o.last_submit, 0);
        o.command(Command::Submit {
            event: 1,
            composition: false,
            repeat: false,
        });
        assert_eq!(o.last_submit, 1);
        assert!(matches!(o.controller.page(), Page::ConnectionForm(_)));
        o.command(Command::Submit {
            event: 1,
            composition: false,
            repeat: false,
        });
        assert_eq!(o.last_submit, 1);
        let (tx, _rx) = mpsc::sync_channel(1);
        let e = AppEngine {
            sender: tx,
            renderer: Arc::new(Mutex::new(None)),
            snapshot: Arc::new(Mutex::new(json!({}))),
            control: Arc::new(AtomicU64::new(0)),
        };
        e.send(Command::Password(SecretBuffer::from_text(
            "queue-secret".into(),
        )))
        .unwrap();
        assert_eq!(
            e.send(Command::Password(SecretBuffer::from_text(
                "rejected-secret".into()
            ))),
            Err(crate::QUEUE_FULL)
        );
        e.cancel();
        e.lose_focus();
        e.stop();
        assert_eq!(
            e.control.load(Ordering::Acquire) & CONTROL_MASK,
            STOP | CANCEL | FOCUS_LOST | FORM_INVALID
        );
    }
    #[test]
    fn focus_fence_preserves_accepted_password_and_identity_edits_and_invalidates_late_load() {
        for has_load in [false, true] {
            let mut o = owner();
            let ticket = has_load.then(|| pending(&mut o));
            if !has_load {
                o.command(Command::Password(SecretBuffer::from_text("old-A".into())));
            }
            let (sender, receiver) = mpsc::sync_channel(4);
            let e = AppEngine {
                sender,
                renderer: Arc::new(Mutex::new(None)),
                snapshot: Arc::new(Mutex::new(json!({}))),
                control: o.control.clone(),
            };
            e.send(Command::Password(SecretBuffer::from_text(
                "accepted-B".into(),
            )))
            .unwrap();
            e.lose_focus();
            o.command(receiver.recv().unwrap());
            if let Some(ticket) = ticket {
                o.command(Command::Loaded(
                    ticket,
                    Ok(Some(SecretBuffer::from_text("late-A".into()))),
                ));
            }
            assert_eq!(
                o.controller
                    .connection_form_mut()
                    .unwrap()
                    .password_mut()
                    .expose_text(),
                Some("accepted-B")
            );
        }
        let mut o = owner();
        let ticket = pending(&mut o);
        let (sender, receiver) = mpsc::sync_channel(4);
        let e = AppEngine {
            sender,
            renderer: Arc::new(Mutex::new(None)),
            snapshot: Arc::new(Mutex::new(json!({}))),
            control: o.control.clone(),
        };
        e.send(Command::Edit(Edit {
            target: 1,
            address: "replacement.invalid".into(),
            port: 3389,
            username: "replacement".into(),
            protocol: "auto".into(),
            remember: false,
        }))
        .unwrap();
        e.lose_focus();
        o.command(receiver.recv().unwrap());
        o.command(Command::Loaded(
            ticket,
            Ok(Some(SecretBuffer::from_text("late-old-identity".into()))),
        ));
        let form = o.controller.connection_form_mut().unwrap();
        assert_eq!(form.draft.address, "replacement.invalid");
        assert!(form.password_is_empty());
    }
    struct UnavailablePins;
    impl ServerIdentityStore for UnavailablePins {
        fn load_pin(
            &self,
            _: &ProtocolId,
            _: &frd_core::Endpoint,
        ) -> Result<Option<[u8; 32]>, PlatformError> {
            Err(PlatformError::StorageFailed)
        }
        fn store_pin(
            &self,
            _: &ProtocolId,
            _: &frd_core::Endpoint,
            _: [u8; 32],
        ) -> Result<(), PlatformError> {
            Err(PlatformError::StorageFailed)
        }
    }
    #[test]
    fn snapshots_explain_real_changed_identity_store_failure_and_auth_failure_without_raw_payloads()
    {
        for store_failure in [false, true] {
            let (mut o, session, _commits) = staged_owner();
            if store_failure {
                o.stores.identities = Arc::new(UnavailablePins);
            }
            o.runtime_event(
                session,
                SessionEvent::ServerIdentityChallenge(frd_protocol_api::ServerIdentityChallenge {
                    session_id: session,
                    challenge_id: 1,
                    protocol_id: ProtocolId::rdp(),
                    endpoint: frd_core::Endpoint::new("fixture.invalid", 3389).unwrap(),
                    sha256_fingerprint: [2; 32],
                    subject: "fixture.invalid".into(),
                    issuer: "test".into(),
                    validation: frd_protocol_api::evaluate_server_identity(Some([1; 32]), [2; 32]),
                }),
            );
            let snapshot = o.snapshot();
            assert_eq!(snapshot["page"], 5);
            assert_eq!(
                snapshot["statusCode"],
                if store_failure {
                    "rdp_identity_store_failed"
                } else {
                    "rdp_server_identity_changed"
                }
            );
            assert!(snapshot["statusMessage"]
                .as_str()
                .unwrap()
                .contains(if store_failure {
                    "证书记录"
                } else {
                    "指纹已变化"
                }));
        }
        for code in [
            "rdp_logon_failed",
            "rdp_tcp_failed",
            "untrusted-raw-secret-diagnostic",
        ] {
            let (mut o, session, _commits) = staged_owner();
            o.runtime_event(
                session,
                SessionEvent::Error(ProtocolError::adapter(ProtocolId::rdp(), code)),
            );
            let snapshot = o.snapshot();
            assert_eq!(snapshot["page"], 5);
            assert!(!snapshot
                .to_string()
                .contains("untrusted-raw-secret-diagnostic"));
            assert_eq!(
                snapshot["statusCode"],
                match code {
                    "rdp_logon_failed" => "authentication_failed",
                    "rdp_tcp_failed" => "rdp_tcp_failed",
                    _ => "session_failed",
                }
            );
        }
    }
    struct CommitSignal(mpsc::Sender<()>);
    impl SecureCredentialStore for CommitSignal {
        fn load(&self, _: &ConnectionProfileKey) -> Result<Option<SecretBuffer>, PlatformError> {
            Ok(None)
        }
        fn stage(
            &self,
            _: frd_core::SessionId,
            _: &ConnectionProfileKey,
            _: &SecretBuffer,
        ) -> Result<(), PlatformError> {
            Ok(())
        }
        fn commit(
            &self,
            _: frd_core::SessionId,
            _: &ConnectionProfileKey,
        ) -> Result<(), PlatformError> {
            self.0.send(()).unwrap();
            Ok(())
        }
        fn discard(&self, _: frd_core::SessionId) -> Result<(), PlatformError> {
            Ok(())
        }
        fn delete(&self, _: &ConnectionProfileKey) -> Result<(), PlatformError> {
            Ok(())
        }
        fn purge_pending(&self) -> Result<(), PlatformError> {
            Ok(())
        }
    }
    fn staged_owner() -> (Owner, frd_core::SessionId, mpsc::Receiver<()>) {
        let mut o = owner();
        let (tx, rx) = mpsc::channel();
        o.stores.credentials = Arc::new(CommitSignal(tx));
        o.catalog = ProtocolCatalog::new([ProtocolId::rdp()]);
        let f = o.controller.connection_form_mut().unwrap();
        f.draft.target_system = Some(TargetSystem::Windows);
        f.draft.address = "fixture.invalid".into();
        f.draft.port = Some(3389);
        f.draft.username = "synthetic".into();
        f.remember_on_this_device = true;
        f.set_password(SecretBuffer::from_text("in-memory-auth-fixture".into()));
        let submission = f.take_submission(&o.catalog).unwrap();
        let action = o
            .controller
            .handle_intent_with_stores(
                AppIntent::Connect(submission),
                &o.catalog,
                o.stores.borrow(),
            )
            .unwrap()
            .unwrap();
        let AppAction::StartSession(request, _permit) = action else {
            panic!("start");
        };
        (o, request.session_id, rx)
    }
    #[test]
    fn adapter_defers_commit_until_exact_authenticated_event_and_cancel_wins_admission() {
        let (mut o, session, commits) = staged_owner();
        assert!(commits.try_recv().is_err());
        o.runtime_event(
            frd_core::SessionId::allocate(),
            SessionEvent::StageChanged(ConnectionStage::TransportReady),
        );
        assert!(commits.try_recv().is_err());
        o.runtime_event(
            session,
            SessionEvent::StageChanged(ConnectionStage::TransportReady),
        );
        commits.recv_timeout(Duration::from_secs(2)).unwrap();
        o.runtime_event(
            session,
            SessionEvent::StageChanged(ConnectionStage::TransportReady),
        );
        assert!(commits.try_recv().is_err());
        let (mut cancelled, session, commits) = staged_owner();
        signal(&cancelled.control, CANCEL);
        cancelled.runtime_event(
            session,
            SessionEvent::StageChanged(ConnectionStage::TransportReady),
        );
        assert!(commits.try_recv().is_err());
        assert!(!matches!(
            cancelled.controller.page(),
            Page::AwaitingFirstFrame { .. } | Page::RemoteSession { .. }
        ));
    }
    #[test]
    fn emergency_barrier_rejects_queued_submit_focus_but_preserves_accepted_edit_and_new_geometry()
    {
        let mut o = owner();
        let (tx, rx) = mpsc::sync_channel(4);
        let e = AppEngine {
            sender: tx,
            renderer: Arc::new(Mutex::new(None)),
            snapshot: Arc::new(Mutex::new(json!({}))),
            control: o.control.clone(),
        };
        e.send(Command::Password(SecretBuffer::from_text(
            "obsolete-secret".into(),
        )))
        .unwrap();
        e.send(Command::Submit {
            event: 9,
            composition: false,
            repeat: false,
        })
        .unwrap();
        e.send(Command::Focus {
            foreground: true,
            focused: true,
            remote: true,
        })
        .unwrap();
        e.cancel();
        e.lose_focus();
        while let Ok(command) = rx.try_recv() {
            o.command(command);
        }
        assert_eq!(o.last_submit, 0);
        assert_eq!(
            o.controller
                .connection_form_mut()
                .unwrap()
                .password_mut()
                .expose_text(),
            Some("obsolete-secret")
        );
        // 新代际的几何可紧随失焦通知到达，不依赖 owner 恰好先被唤醒。
        e.lose_focus();
        let geometry = ViewGeometry {
            drawable: frd_core::PixelSize {
                width: 10,
                height: 10,
            },
            bounds: frd_core::PixelRect {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
            scale: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
        };
        e.send(Command::Geometry(geometry)).unwrap();
        o.command(rx.recv().unwrap());
        assert_eq!(o.input.geometry, Some(geometry));
    }
    #[test]
    fn pending_capacity_survives_deadline_and_destruction_drops_late_result() {
        let mut o = owner();
        for _ in 0..2 {
            let id = pending(&mut o);
            o.pending.get_mut(&id).unwrap().deadline = Instant::now() - Duration::from_secs(1);
        }
        o.poll();
        assert_eq!(o.pending.len(), 2);
        o.command(Command::Select(0));
        assert_eq!(o.pending.len(), 2);
        assert!(o.notice.is_some());
        let (tx, rx) = mpsc::sync_channel(1);
        drop(rx);
        assert!(tx
            .send(Command::Loaded(
                1,
                Ok(Some(SecretBuffer::from_text("orphan-secret".into())))
            ))
            .is_err());
    }
}
