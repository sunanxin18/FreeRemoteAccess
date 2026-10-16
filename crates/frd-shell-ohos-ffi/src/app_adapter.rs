//! ArkUI 不拥有 controller/password/session；专用 owner 串行处理意图与真正的 runtime 事件。
use crate::ime_policy::{
    Ingress as ImeIngress, Installation as ImeInstallation, Operation as ImeOperation,
    Source as ImeSource, SourceState as ImeSourceState, State as ImeState, Ticket as ImeTicket,
};
use crate::input_adapter::{InputAdapter, ViewGeometry};
use crate::keyboard_policy::{qualified_physical_hid, OhosKeyboardQualification};
use crate::session_diagnostic::{
    Code as DiagnosticCode, Diagnostic, Event as DiagnosticEvent, PageCode, Record,
};
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
// 仅接收 Owner 从同次 snapshot 挑选的三项显示证据；禁止传入通用应用快照。
#[cfg(target_env = "ohos")]
fn emit_display_log(payload: &Value) {
    #[link(name = "hilog_ndk.z")]
    extern "C" {
        fn OH_LOG_Print(
            t: u32,
            l: u32,
            d: u32,
            tag: *const std::ffi::c_char,
            fmt: *const std::ffi::c_char,
            ...
        ) -> i32;
    }
    if let Ok(text) = std::ffi::CString::new(payload.to_string()) {
        // 固定公开格式与标签；JSON只含尺寸/内部会话代际/封闭枚举原因。
        unsafe {
            OH_LOG_Print(
                0,
                4,
                0xFF00,
                c"FrdDisplay".as_ptr(),
                c"%{public}s".as_ptr(),
                text.as_ptr(),
            );
        }
    }
}
#[cfg(not(target_env = "ohos"))]
fn emit_display_log(_: &Value) {}

const STOP: u64 = 1;
const CANCEL: u64 = 2;
const FOCUS_LOST: u64 = 4;
const FORM_INVALID: u64 = 8;
const DISPLAY_INVALID: u64 = 16;
const CONTROL_MASK: u64 = STOP | CANCEL | FOCUS_LOST | FORM_INVALID | DISPLAY_INVALID;
const CONTROL_REVISION: u64 = 1 << 32;
// 标志与代际同时发布，队列满也不会丢失取消/失焦，旧 UI 意图不跨此屏障。
fn signal(control: &AtomicU64, flags: u64) -> u64 {
    let previous = control
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
            Some(
                old.checked_add(CONTROL_REVISION)
                    .map_or(old | STOP, |next| next | flags),
            )
        })
        .expect("control 更新始终返回 Some");
    previous
        .checked_add(CONTROL_REVISION)
        .map_or(previous | STOP, |next| next | flags)
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
#[cfg(test)]
struct NoAudio;
#[cfg(test)]
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
    ImeSource(u64, ImeSource),
    ImeOperation(u64, ImeOperation),
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
    LocalDisplay(crate::local_display::Message),
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
    diagnostic: Arc<Diagnostic>,
    ime_installation: ImeInstallation,
    ime_ingress: Arc<Mutex<ImeIngress>>,
    keyboard: OhosKeyboardQualification,
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
        Self::spawn_with_keyboard(
            stores,
            factories,
            loader,
            audio,
            OhosKeyboardQualification::Unavailable,
        )
    }
    pub(crate) fn spawn_with_keyboard(
        stores: Stores,
        factories: Vec<Arc<dyn ProtocolFactory>>,
        loader: StreamRegistryLoader,
        audio: Arc<dyn AudioOutputFactory>,
        keyboard: OhosKeyboardQualification,
    ) -> Result<Self, i32> {
        Self::spawn_with_input(
            stores,
            factories,
            loader,
            audio,
            keyboard,
            ImeInstallation::Unavailable,
        )
    }
    // 只供未来正常系统 IME adapter 显式安装；既有 composition 不调用此构造器。
    pub(crate) fn spawn_with_input(
        stores: Stores,
        factories: Vec<Arc<dyn ProtocolFactory>>,
        loader: StreamRegistryLoader,
        audio: Arc<dyn AudioOutputFactory>,
        keyboard: OhosKeyboardQualification,
        ime_installation: ImeInstallation,
    ) -> Result<Self, i32> {
        let diagnostic = Arc::new(Diagnostic::default());
        let owner_diagnostic = diagnostic.clone();
        let ime_ingress = Arc::new(Mutex::new(ImeIngress::default()));
        let owner_ime_ingress = ime_ingress.clone();
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
                let mut controller = AppController::connection_form_with_stores(
                    ConnectionForm::new(ConnectionDraft::default()),
                    stores.borrow(),
                );
                keyboard.configure(&mut controller);
                let mut owner = Owner {
                    diagnostic: owner_diagnostic,
                    diagnostic_session: None,
                    diagnostic_full: None,
                    diagnostic_failure: false,
                    ime: ImeState::new(ime_installation),
                    ime_ingress: owner_ime_ingress,
                    keyboard,
                    controller,
                    renderer: owner_renderer,
                    ready: None,
                    remote_epoch: None,
                    host: SessionHost::new(factories, Arc::new(Wake), audio, loader),
                    catalog,
                    stores,
                    input: InputAdapter::default(),
                    local_display: crate::local_display::LocalDisplay::default(),
                    display_invalid_before: 0,
                    frozen_display: None,
                    last_display_log: None,
                    #[cfg(test)]
                    sent_inputs: std::cell::RefCell::new(Vec::new()),
                    #[cfg(test)]
                    ime_admission_hook: None,
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
                owner.configure_ime();
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
                            owner.diagnostic.ingress(
                                DiagnosticEvent::ChannelClosed,
                                owner.control.load(Ordering::Acquire),
                            );
                            owner.cancel();
                            owner.stopping = true
                        }
                    }
                    owner.poll();
                    let next_snapshot = owner.snapshot();
                    if let Some(evidence) = owner.take_display_log(&next_snapshot) {
                        emit_display_log(&evidence);
                    }
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
            diagnostic,
            ime_installation,
            ime_ingress,
            keyboard,
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
    pub fn for_platform(root: PathBuf) -> Result<Self, i32> {
        crate::composition::spawn(root)
    }
    pub(crate) fn ime_source(&self, source: ImeSource) -> Result<(), i32> {
        if !self.ime_installation.installed() {
            return Err(crate::UNSUPPORTED_PLATFORM);
        }
        let mut ingress = self.ime_ingress.lock().map_err(|_| crate::PANIC)?;
        if !ingress.source(&source)? {
            return Ok(());
        }
        let revision = ingress.revision;
        self.enqueue_ime(&mut ingress, Command::ImeSource(revision, source))
    }
    pub(crate) fn ime_operation(&self, op: ImeOperation) -> Result<(), i32> {
        if !self.ime_installation.installed() {
            return Err(crate::UNSUPPORTED_PLATFORM);
        }
        if op.key.is_some() && !self.keyboard.installed() {
            return Err(crate::UNSUPPORTED_PLATFORM);
        }
        let mut ingress = self.ime_ingress.lock().map_err(|_| crate::PANIC)?;
        if ingress.active != Some(op.ticket) {
            return Err(crate::INVALID_ARGUMENT);
        }
        let revision = ingress.revision;
        self.enqueue_ime(&mut ingress, Command::ImeOperation(revision, op))
    }
    fn enqueue_ime(&self, ingress: &mut ImeIngress, command: Command) -> Result<(), i32> {
        let control = self.control.load(Ordering::Acquire) / CONTROL_REVISION;
        self.sender
            .try_send(Command::Queued(control, Box::new(command)))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    let _ = ingress.invalidate("queue_full");
                    crate::QUEUE_FULL
                }
                mpsc::TrySendError::Disconnected(_) => {
                    let _ = ingress.invalidate("worker_stopped");
                    crate::WORKER_STOPPED
                }
            })
    }
    pub(crate) fn legacy_text(&self, epoch: InputEpoch, utf8: &str) -> Result<(), i32> {
        // 显式 IME 路径在复制旧 payload 之前拒绝；普通 send 和 Owner 仍保留二次防御。
        if self.ime_installation.installed() {
            return Err(crate::UNSUPPORTED_PLATFORM);
        }
        self.send(Command::Input(
            epoch,
            InputEvent::Text {
                utf8: utf8.to_owned(),
            },
            false,
        ))
    }
    pub fn send(&self, command: Command) -> Result<(), i32> {
        // 版本化来源必须经过专用 ingress；公开普通 sender 不能绕过 reservation/屏障。
        if matches!(command, Command::ImeSource(..) | Command::ImeOperation(..)) {
            return Err(crate::INVALID_ARGUMENT);
        }
        // ABI 与普通 sender 共用入队前拒绝；不得返回 0 后才静默丢弃新 Down。
        if let Command::Input(_, event, _) = &command {
            if self.ime_installation.installed() && matches!(event, InputEvent::Text { .. }) {
                return Err(crate::UNSUPPORTED_PLATFORM);
            }
            self.keyboard.validate_new_down(event)?;
        }
        let display = matches!(command, Command::LocalDisplay(_));
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
                    if display {
                        signal(&self.control, DISPLAY_INVALID);
                    }
                    crate::QUEUE_FULL
                }
                mpsc::TrySendError::Disconnected(_) => crate::WORKER_STOPPED,
            })
    }
    pub(crate) fn diagnostic_ingress(&self, event: DiagnosticEvent) {
        self.diagnostic
            .ingress(event, self.control.load(Ordering::Acquire));
    }
    pub(crate) fn diagnostic_bind(&self, app: u64) {
        self.diagnostic.bind(app);
    }
    pub fn cancel(&self) {
        let state = signal(&self.control, CANCEL);
        self.diagnostic
            .ingress(DiagnosticEvent::EngineCancel, state);
    }
    pub fn lose_focus(&self) {
        signal(&self.control, FOCUS_LOST);
    }
    pub fn stop(&self) {
        let state = signal(&self.control, STOP);
        self.diagnostic.ingress(DiagnosticEvent::EngineStop, state);
    }
    pub fn snapshot(&self) -> Result<Vec<u8>, i32> {
        let copy = self.snapshot.lock().map_err(|_| crate::PANIC)?.clone();
        serde_json::to_vec(&copy).map_err(|_| crate::PANIC)
    }
}
impl Drop for AppEngine {
    fn drop(&mut self) {
        self.diagnostic_ingress(DiagnosticEvent::EngineDrop);
        self.stop()
    }
}
// 仅投影公开的模式信息；目标兼容与注册结果仍由实际 catalog 判定。
fn target_number(target: TargetSystem) -> u32 {
    match target {
        TargetSystem::Windows => 1,
        TargetSystem::MacOs => 2,
        TargetSystem::Linux => 3,
        TargetSystem::Custom => 0,
    }
}
fn target_label(target: TargetSystem) -> &'static str {
    match target {
        TargetSystem::Windows => "Windows",
        TargetSystem::MacOs => "macOS",
        TargetSystem::Linux => "Linux",
        TargetSystem::Custom => "其他系统",
    }
}
fn mode_labels(protocol: &ProtocolId) -> (&'static str, &'static str) {
    if *protocol == ProtocolId::apple_hpss_mvs() {
        ("标准（MVS）", "标准")
    } else if *protocol == ProtocolId::apple_high_performance() {
        ("高性能", "高性能")
    } else if *protocol == ProtocolId::rdp() {
        ("RDP", "RDP")
    } else if *protocol == ProtocolId::rfb() {
        ("VNC", "VNC")
    } else {
        ("保存的连接模式", "已保存")
    }
}
fn permitted_targets(catalog: &ProtocolCatalog, protocol: &ProtocolId) -> Vec<u32> {
    [
        TargetSystem::Windows,
        TargetSystem::MacOs,
        TargetSystem::Linux,
    ]
    .into_iter()
    .filter(|target| {
        !matches!(
            catalog.select(
                *target,
                frd_protocol_api::ProtocolSelection::Explicit(protocol.clone())
            ),
            Err(ProtocolError::UnsupportedTargetProtocol)
        )
    })
    .map(target_number)
    .collect()
}
fn mode_options(catalog: &ProtocolCatalog, draft: &ConnectionDraft) -> Vec<Value> {
    use frd_protocol_api::ProtocolSelection;
    let target = draft.target_system.unwrap_or(TargetSystem::Custom);
    let mut options = vec![json!({"id":"auto", "label":"自动", "shortLabel":"自动",
        "available": catalog.select(target, ProtocolSelection::Automatic).is_ok(), "targets":[1,2,3]})];
    let mut protocols = vec![
        ProtocolId::rdp(),
        ProtocolId::apple_hpss_mvs(),
        ProtocolId::apple_high_performance(),
        ProtocolId::rfb(),
    ];
    if let ProtocolChoice::Explicit(selected) = &draft.protocol {
        if !protocols.contains(selected) {
            protocols.push(selected.clone());
        }
    }
    for protocol in protocols {
        let result = catalog.select(target, ProtocolSelection::Explicit(protocol.clone()));
        if matches!(result, Err(ProtocolError::UnsupportedTargetProtocol))
            && draft.protocol != ProtocolChoice::Explicit(protocol.clone())
        {
            continue;
        }
        let (label, short_label) = mode_labels(&protocol);
        options.push(
            json!({"id":protocol.as_str(),"label":label,"shortLabel":short_label,
            "available":result.is_ok(),"targets":permitted_targets(catalog, &protocol)}),
        );
    }
    options
}
fn mode_notice(catalog: &ProtocolCatalog, draft: &ConnectionDraft) -> &'static str {
    use frd_protocol_api::ProtocolSelection;
    let target = draft.target_system.unwrap_or(TargetSystem::Custom);
    match catalog.select(target, draft.protocol.clone().into()) {
        Err(ProtocolError::UnsupportedTargetProtocol) => "此模式不适用于当前目标，请重新选择",
        Err(_)
            if target == TargetSystem::MacOs
                && draft.protocol == ProtocolChoice::Automatic
                && catalog
                    .select(
                        target,
                        ProtocolSelection::Explicit(ProtocolId::apple_hpss_mvs()),
                    )
                    .is_ok() =>
        {
            "自动模式当前不可用，请选择标准（MVS）"
        }
        Err(_) => "此设备暂不支持所选连接模式",
        Ok(_) if target == TargetSystem::MacOs => {
            "当前暂支持画面与指针；键盘、文本、音频和剪贴板不可用"
        }
        Ok(_) => "",
    }
}
struct PendingLoad {
    ticket: SavedPasswordLoadTicket,
    deadline: Instant,
    timed_out: bool,
}
struct Owner {
    diagnostic: Arc<Diagnostic>,
    diagnostic_session: Option<(frd_core::SessionId, u64)>,
    diagnostic_full: Option<crate::presentation::Epoch>,
    diagnostic_failure: bool,
    ime: ImeState,
    ime_ingress: Arc<Mutex<ImeIngress>>,
    keyboard: OhosKeyboardQualification,
    renderer: Arc<Mutex<Option<Arc<crate::presentation::Link>>>>,
    ready: Option<crate::presentation::Epoch>,
    remote_epoch: Option<(frd_core::SessionId, u64)>,
    controller: AppController,
    host: SessionHost,
    catalog: ProtocolCatalog,
    stores: Stores,
    input: InputAdapter,
    local_display: crate::local_display::LocalDisplay,
    display_invalid_before: u64,
    frozen_display: Option<Value>,
    last_display_log: Option<Value>,
    #[cfg(test)]
    sent_inputs: std::cell::RefCell<Vec<frd_core::SessionInput>>,
    #[cfg(test)]
    ime_admission_hook: Option<Box<dyn FnOnce()>>,
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
    fn diagnostic_record(&self, event: DiagnosticEvent, from: PageCode) -> Record {
        let mut record = Record::new(event, from, self.controller.page().into());
        if let Some((session, generation)) = self.diagnostic_session {
            record.session = session.get();
            record.generation = generation;
            record.current = true;
            record.active = self.host.active_session_id() == Some(session);
        }
        record
    }

    fn configure_ime(&mut self) {
        if !self.ime.installation.installed() {
            return;
        }
        self.controller.set_product_policy(frd_app::ProductPolicy {
            physical_key_input: self.keyboard.installed(),
            text_input: true,
            ..Default::default()
        });
        self.controller
            .set_platform_capabilities(frd_platform_api::PlatformCapabilities {
                physical_key_input: self.keyboard.installed(),
                text_input: self.ime.ticket.is_some(),
                ..Default::default()
            });
    }
    fn ime_barrier(&mut self) {
        let barrier = self.ime_ingress.lock().ok().map(|s| (s.revision, s.reason));
        if let Some((revision, reason)) = barrier {
            if self.ime.revision != revision || revision == u64::MAX {
                self.ime.revision = revision;
                self.ime.clear(reason);
                self.configure_ime();
            }
        } else {
            self.ime.clear("internal_failure");
            self.configure_ime();
        }
    }
    fn invalidate_ime(&mut self, reason: &'static str) {
        // 清除待发文本且保留 editor high-water；新 editor 可以在屏障之后重新附着。
        if let Ok(mut ingress) = self.ime_ingress.lock() {
            let _ = ingress.invalidate(reason);
            self.ime.revision = ingress.revision;
        }
        self.ime.clear(reason);
        self.configure_ime();
    }
    fn ime_context_available(&self) -> bool {
        self.ime.installation.installed()
            && self.controller.negotiated_capabilities().text_input
            && self.input.owns_remote_focus()
            && self.remote_epoch.is_some()
            && self.controller.interactive_input_epoch() == self.remote_epoch
            && self
                .renderer
                .lock()
                .ok()
                .and_then(|r| r.clone())
                .is_some_and(|link| {
                    link.0
                        .lock()
                        .ok()
                        .is_some_and(|s| s.surface.attached && s.surface.foreground)
                })
    }
    fn valid_ime_ticket(&self, ticket: ImeTicket) -> bool {
        self.ime_context_available()
            && self.local_display.active_window() == Some((ticket.window_stage, ticket.window_id))
            && self.remote_epoch.map(|(s, g)| (s.get(), g))
                == Some((ticket.session, ticket.generation))
    }
    fn publish_ime_window(&self) {
        let identity = self.local_display.active_window();
        let available = self.ime_context_available();
        if let Ok(mut ingress) = self.ime_ingress.lock() {
            ingress.publish_window(identity, available);
        }
    }
    fn check_ime(&mut self) {
        self.publish_ime_window();
        self.ime_barrier();
        if self.ime.ticket.is_some_and(|t| !self.valid_ime_ticket(t)) {
            self.invalidate_ime(if !self.controller.negotiated_capabilities().text_input {
                "protocol_text_unavailable"
            } else {
                "source_invalidated"
            });
        }
    }
    fn source_ime(&mut self, revision: u64, source: ImeSource) {
        self.ime_barrier();
        if revision != self.ime.revision {
            return;
        }
        match source.state {
            ImeSourceState::Detached => {} // 入队前已可靠发布屏障。
            ImeSourceState::Attached => {
                if !self.valid_ime_ticket(source.ticket) {
                    self.invalidate_ime("source_unavailable");
                    return;
                }
                self.release();
                self.ime.ticket = Some(source.ticket);
                self.ime.ack_ticket = Some(source.ticket);
                self.ime.accepted = 0;
                self.ime.dispatched = 0;
                self.ime.reason = "source_attached";
                self.configure_ime();
            }
        }
    }
    fn operation_ime(&mut self, revision: u64, op: ImeOperation) {
        self.check_ime();
        if revision != self.ime.revision || self.ime.ticket != Some(op.ticket) {
            return;
        }
        if self.ime.accepted.checked_add(1) != Some(op.serial) {
            self.invalidate_ime("invalid_serial");
            return;
        }
        if op.key.is_some()
            && (!self.keyboard.installed()
                || !self.controller.effective_capabilities().physical_key_input)
        {
            self.invalidate_ime("physical_unavailable");
            return;
        }
        if self.ime.pending.len() >= 32 || self.ime.pending_bytes + op.text_bytes() > 16384 {
            self.invalidate_ime("pending_full");
            return;
        }
        self.ime.accepted = op.serial;
        self.ime.pending_bytes += op.text_bytes();
        self.ime.pending.push_back(op);
        self.drain_ime();
    }
    fn send_ime_event(&mut self, ticket: ImeTicket, event: InputEvent, pressed: bool) -> bool {
        let command = self.input.route(
            &self.controller,
            ticket.session,
            ticket.generation,
            self.input.revision,
            event,
            pressed,
        );
        command.is_some_and(|command| self.host.send_command(command).is_ok())
    }
    fn drain_ime(&mut self) {
        self.check_ime();
        if self.ime.ticket.is_none() || !self.input_ready() {
            return;
        }
        while let Some(op) = self.ime.pending.pop_front() {
            self.ime.pending_bytes -= op.text_bytes();
            // 与 detach/overflow producer 的锁也是发送的线性化点，已发布屏障先获胜。
            let ingress = self.ime_ingress.clone();
            let Ok(guard) = ingress.lock() else {
                self.invalidate_ime("internal_failure");
                return;
            };
            if guard.revision != self.ime.revision || guard.active != Some(op.ticket) {
                drop(guard);
                self.check_ime();
                return;
            }
            // 测试专用单次接缝：在真实 ingress 检查后、真实 atomic admission 前插入 producer 步骤。
            #[cfg(test)]
            if let Some(hook) = self.ime_admission_hook.take() {
                hook();
            }
            // 与通用 control 的 CAS 是发送 admission 点；之前发布的后台/取消/窗口失效先清队列。
            loop {
                let control = self.control.load(Ordering::Acquire);
                if control & (STOP | CANCEL | FOCUS_LOST | DISPLAY_INVALID) != 0 {
                    drop(guard);
                    self.apply_control();
                    return;
                }
                if self
                    .control
                    .compare_exchange(control, control, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    break;
                }
            }
            let success = if let Some(text) = op.text.as_ref().and_then(SecretBuffer::expose_text) {
                self.send_ime_event(
                    op.ticket,
                    InputEvent::Text {
                        utf8: text.to_owned(),
                    },
                    false,
                )
            } else if let Some(key) = op.key {
                if !self.input.ime_pair_available(key.hid()) {
                    drop(guard);
                    self.invalidate_ime("physical_source_busy");
                    return;
                }
                let event = |state| InputEvent::PhysicalKey {
                    code: frd_core::PhysicalKeyCode::from_usb_hid_usage(key.hid()),
                    state,
                    modifiers: frd_core::Modifiers::default(),
                };
                let down = self.send_ime_event(op.ticket, event(frd_core::KeyState::Pressed), true);
                let up = down
                    && self.send_ime_event(op.ticket, event(frd_core::KeyState::Released), false);
                if !up {
                    self.release();
                    self.input.consume_quarantined_release(
                        op.ticket.session,
                        op.ticket.generation,
                        &event(frd_core::KeyState::Released),
                    );
                }
                up
            } else {
                false
            };
            drop(guard);
            if !success {
                self.invalidate_ime("dispatch_failed");
                return;
            }
            self.ime.dispatched = op.serial;
        }
    }
    fn apply_control(&mut self) {
        self.ime_barrier();
        let state = self.control.fetch_and(!CONTROL_MASK, Ordering::AcqRel);
        let flags = state & CONTROL_MASK;
        let from = PageCode::from(self.controller.page());
        if flags & DISPLAY_INVALID != 0 {
            self.display_invalid_before = state / CONTROL_REVISION;
            self.local_display.invalidate();
            self.invalidate_ime("window_invalidated");
        }
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
        if flags & (STOP | CANCEL) != 0 {
            let mut record = self.diagnostic_record(DiagnosticEvent::ApplyControl, from);
            record.flags = flags;
            record.revision = state / CONTROL_REVISION;
            self.diagnostic.emit(record);
        }
    }
    fn send(&self, c: Option<SessionCommand>) {
        if let Some(c) = c {
            #[cfg(test)]
            if let SessionCommand::Input(input) = &c {
                self.sent_inputs.borrow_mut().push(input.clone());
            }
            let _ = self.host.send_command(c);
        }
    }
    fn release(&mut self) {
        let c = self.input.release(&self.controller);
        self.send(c)
    }
    fn focus(&mut self, foreground: bool, focused: bool, remote: bool) {
        if !foreground || !focused || !remote {
            self.invalidate_ime("focus_lost");
        }
        let c = self
            .input
            .focus(&self.controller, foreground, focused, remote);
        self.send(c);
        self.check_ime()
    }
    fn cancel(&mut self) {
        let from = PageCode::from(self.controller.page());
        self.release();
        self.clear_remote();
        if self.host.launch_is_pending() {
            self.host.cancel_pending_launch();
            self.cancelled_launch = true;
            self.diagnostic
                .emit(self.diagnostic_record(DiagnosticEvent::OwnerCancel, from));
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
        self.diagnostic
            .emit(self.diagnostic_record(DiagnosticEvent::OwnerCancel, from));
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
        let diagnostic_return = matches!(intent, AppIntent::ReturnToConnection);
        let from = PageCode::from(self.controller.page());
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
            Ok(Some(AppAction::StartSession(mut request, permit))) => {
                self.diagnostic_session = Some((request.session_id, 0));
                self.local_display.freeze(&mut request.display_intent);
                let planned =
                    frd_core::DisplayPlanner::plan(request.display_intent, Default::default()).ok();
                self.frozen_display = Some(json!({"session":request.session_id.get().to_string(),
                    "localSnapshot": self.local_display.evidence(),
                    "mode":format!("{:?}",request.display_intent.mode),
                    "orientation":format!("{:?}",request.display_intent.orientation),
                    "projectedSize":planned.and_then(|plan|plan.requested_size).map(|s| json!({"width":s.width,"height":s.height})),
                    "note":"投影意图；协议仍执行自身约束，非服务端确认"}));
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
        if diagnostic_return {
            self.diagnostic
                .emit(self.diagnostic_record(DiagnosticEvent::Return, from));
        }
    }
    // 已入队的真实 Up 即使被 control 屏障作废，也只清匹配来源的物理账本。
    fn discard_input_release(&mut self, command: &Command) {
        if let Command::Input(epoch, event, _) = command {
            let release = self.input.discard_released(
                &self.controller,
                epoch.session,
                epoch.generation,
                event,
            );
            self.send(release);
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
            let display = matches!(*command, Command::LocalDisplay(_));
            if (display && revision < self.display_invalid_before)
                || (form_edit && revision < self.form_invalid_before)
                || (!form_edit
                    && !display
                    && (revision != control / CONTROL_REVISION || control & CONTROL_MASK != 0))
            {
                self.discard_input_release(&command);
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
            self.discard_input_release(&c);
            return;
        }
        match c {
            Command::Queued(..) => {}
            Command::ImeSource(revision, source) => self.source_ime(revision, source),
            Command::ImeOperation(revision, op) => self.operation_ime(revision, op),
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
            Command::LocalDisplay(message) => {
                self.local_display.accept(message);
                self.check_ime();
            }
            Command::Geometry(g) => {
                let c = self.input.set_geometry(&self.controller, g);
                self.send(c)
            }
            Command::Input(e, event, pressed) => {
                if self.ime.installation.installed() && matches!(event, InputEvent::Text { .. }) {
                    return;
                }
                if self.keyboard.validate_new_down(&event).is_err() {
                    return;
                }
                if self
                    .input
                    .consume_quarantined_release(e.session, e.generation, &event)
                {
                    return;
                }
                // 收窄后的真实 held Up 以 ReleaseAll 清理；孤立/未知 Up 不进入 consumer。
                if let InputEvent::PhysicalKey {
                    code,
                    state: frd_core::KeyState::Released,
                    ..
                } = &event
                {
                    if !self.keyboard.installed() || !qualified_physical_hid(code.usb_hid_usage()) {
                        let release = self.input.discard_released(
                            &self.controller,
                            e.session,
                            e.generation,
                            &event,
                        );
                        self.send(release);
                        return;
                    }
                }
                if !self.input_ready() {
                    let release = self.input.discard_released(
                        &self.controller,
                        e.session,
                        e.generation,
                        &event,
                    );
                    self.send(release);
                    // 未呈现时也不能让旧来源的 Down 污染当前隔离账本。
                    if self.remote_epoch.map(|(s, g)| (s.get(), g))
                        == Some((e.session, e.generation))
                        && e.revision == self.input.revision
                    {
                        self.input.observe_blocked(e.session, e.generation, &event);
                    }
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
            Command::Cleanup(r) => {
                let from = PageCode::from(self.controller.page());
                let session = match &r {
                    BackgroundCleanupOutcome::Complete { completion, .. } => {
                        Some(completion.session_id())
                    }
                    BackgroundCleanupOutcome::Fatal(_) => None,
                };
                let mut code = DiagnosticCode::None;
                match self.host.accept_cleanup_outcome(r) {
                    Ok(c) => {
                        let result = self
                            .controller
                            .finish_session_cleanup_with_stores(c, self.stores.borrow());
                        if result.is_err() {
                            code = DiagnosticCode::CleanupFailed;
                        }
                    }
                    Err(_) => {
                        self.notice = Some("会话清理失败，请重新启动应用");
                        code = DiagnosticCode::CleanupFailed;
                    }
                }
                let mut record = self.diagnostic_record(DiagnosticEvent::Cleanup, from);
                record.current = session.is_some_and(|s| {
                    self.diagnostic_session
                        .is_some_and(|(current, _)| s == current)
                });
                record.session = session.map_or(0, |s| s.get());
                record.code = code;
                self.diagnostic.emit(record);
                if code == DiagnosticCode::None
                    && session.is_some_and(|s| {
                        self.diagnostic_session
                            .is_some_and(|(current, _)| s == current)
                    })
                {
                    self.diagnostic_session = None;
                }
            }
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
        self.invalidate_ime("session_ended");
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
        self.publish_ime_window();
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
        self.check_ime();
        let Some(link) = self.renderer.lock().ok().and_then(|r| r.clone()) else {
            return;
        };
        let (feedback, eligible, failed, local, lifecycle) = {
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
            (
                feedback,
                state.ready_epoch(),
                state.failure,
                state.surface.local,
                state.surface.lifecycle,
            )
        };
        if self.ready.is_some() && self.ready != eligible {
            self.release();
            self.ready = None;
        }
        if let Some(code) = failed {
            // Link::fail 是不可恢复的首次失败锁存；窗口 lifecycle 变化不产生新失败。
            if !self.diagnostic_failure {
                self.diagnostic_failure = true;
                let mut record = self.diagnostic_record(
                    DiagnosticEvent::RendererFailed,
                    self.controller.page().into(),
                );
                record.local = local;
                record.lifecycle = lifecycle;
                record.abi = if (-11..=0).contains(&code) { code } else { 0 };
                record.code = if (-11..=0).contains(&code) {
                    DiagnosticCode::NativePresentation
                } else {
                    DiagnosticCode::Unknown
                };
                self.diagnostic.emit(record);
            }
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
                        let from = PageCode::from(self.controller.page());
                        self.controller.handle_presentation(event);
                        if self.controller.interactive_input_epoch()
                            == Some((epoch.session, epoch.remote))
                        {
                            self.ready = Some(epoch);
                            if self.diagnostic_full != Some(epoch) {
                                self.diagnostic_full = Some(epoch);
                                let mut record =
                                    self.diagnostic_record(DiagnosticEvent::FirstFull, from);
                                record.session = epoch.session.get();
                                record.generation = epoch.remote;
                                record.local = epoch.local;
                                record.lifecycle = epoch.lifecycle;
                                self.diagnostic.emit(record);
                            }
                        }
                    }
                }
            }
        }
        self.drain_ime();
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
        let from = PageCode::from(self.controller.page());
        let diagnostic_event = match &e {
            SessionEvent::Closed(frd_protocol_api::ProtocolExit::Closed) => {
                Some((DiagnosticEvent::RuntimeClosed, DiagnosticCode::None))
            }
            SessionEvent::Closed(frd_protocol_api::ProtocolExit::Failed(error)) => {
                Some((DiagnosticEvent::RuntimeFailed, DiagnosticCode::error(error)))
            }
            SessionEvent::Error(error) => {
                Some((DiagnosticEvent::RuntimeError, DiagnosticCode::error(error)))
            }
            SessionEvent::StageChanged(ConnectionStage::Disconnecting) => {
                Some((DiagnosticEvent::RuntimeDisconnecting, DiagnosticCode::None))
            }
            SessionEvent::SurfaceGenerationChanged { .. } => {
                Some((DiagnosticEvent::SurfaceGeneration, DiagnosticCode::None))
            }
            _ => None,
        };
        let current = self
            .diagnostic_session
            .is_some_and(|(session, _)| session == s);
        let active = self.host.active_session_id() == Some(s);
        let event_session = match &e {
            SessionEvent::SurfaceGenerationChanged { session_id, .. } => session_id.get(),
            _ => s.get(),
        };
        let event_generation = match &e {
            SessionEvent::SurfaceGenerationChanged { generation, .. } => Some(*generation),
            _ => None,
        };

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
                self.invalidate_ime("generation_changed");
                self.ready = None;
                self.remote_epoch = Some((s, *generation));
                self.diagnostic_session = Some((s, *generation));
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
        self.check_ime();
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
        if let Some((event, code)) = diagnostic_event {
            let mut record = self.diagnostic_record(event, from);
            record.session = s.get();
            record.event_session = event_session;
            record.current = current;
            record.active = active;
            record.code = code;
            if let Some(generation) = event_generation {
                record.generation = generation;
            } else if !current {
                record.generation = 0;
            }
            self.diagnostic.emit(record);
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
    fn take_display_log(&mut self, snapshot: &Value) -> Option<Value> {
        // 与发布给 UI 的同次 Owner snapshot 共用对象；明确 allowlist，不复制另一套状态。
        let evidence = json!({
            "localDisplayEvidence": snapshot["localDisplayEvidence"],
            "frozenDisplayIntent": snapshot["frozenDisplayIntent"],
            "acceptedSurface": snapshot["acceptedSurface"],
        });
        if self.last_display_log.as_ref() == Some(&evidence) {
            return None;
        }
        self.last_display_log = Some(evidence.clone());
        Some(evidence)
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
        let protocol = match &d.protocol {
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
        let mut value = json!({"localDisplaySource":self.local_display.source,"localDisplayReason":if self.local_display.reason.is_empty() && self.local_display.geometry.is_none() { "尚无有效本地显示快照" } else { self.local_display.reason },"localDisplay":self.local_display.geometry.map(|g| json!({"width":g.physical_size.width,"height":g.physical_size.height,"densityMilli":g.scale_factor_milli})),"abiVersion":1,"initialized":true,"page":page,"target":match d.target_system{Some(TargetSystem::Windows)=>1,Some(TargetSystem::MacOs)=>2,Some(TargetSystem::Linux)=>3,_=>0},"address":d.address,"port":d.port.unwrap_or(0),"username":d.username,"protocol":protocol,"available":available,"session":epoch.0,"generation":epoch.1,"geometryRevision":self.input.revision.to_string(),"notice":self.notice.unwrap_or(""),"probe":self.probe});
        value["localDisplayEvidence"] = self.local_display.evidence();
        value["frozenDisplayIntent"] = json!(self.frozen_display);
        value["acceptedSurface"] = json!(self.remote_epoch.zip(self.input.remote).map(|((session,generation),size)|
            json!({"session":session.get().to_string(),"generation":generation.to_string(),"width":size.width,"height":size.height,"source":"protocol_SurfaceGenerationChanged"})));
        value["protocolOptions"] = json!(mode_options(&self.catalog, &d));
        value["modeNotice"] = json!(mode_notice(&self.catalog, &d));
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
        let effective = self.controller.effective_capabilities();
        value["physicalKeyInput"] = json!(effective.physical_key_input);
        value["textInput"] = json!(effective.text_input);
        value["ime"] = self
            .ime
            .snapshot(self.controller.negotiated_capabilities().text_input);
        value["ime"]["sourceAvailable"] = json!(self
            .ime_ingress
            .lock()
            .ok()
            .is_some_and(|s| s.window_available && s.revision != u64::MAX));
        value["keyboardBackend"] = json!(self.keyboard.name());
        value["keyboardBackendReason"] = json!(self.keyboard.reason());
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
            value["profiles"] = json!(f.profiles.iter().map(|p| json!({
                "address":p.key.address(), "username":p.key.username(), "port":p.key.port(),
                "protocol":p.key.protocol().as_str(), "target":target_number(p.target_system),
                "modeLabel":mode_labels(p.key.protocol()).0, "targetLabel":target_label(p.target_system)
            })).collect::<Vec<_>>());
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
            diagnostic: Arc::new(Diagnostic::default()),
            diagnostic_session: None,
            diagnostic_full: None,
            diagnostic_failure: false,
            ime: ImeState::new(ImeInstallation::Unavailable),
            ime_ingress: Arc::new(Mutex::new(ImeIngress::default())),
            keyboard: OhosKeyboardQualification::Unavailable,
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
            local_display: crate::local_display::LocalDisplay::default(),
            display_invalid_before: 0,
            frozen_display: None,
            last_display_log: None,
            sent_inputs: std::cell::RefCell::new(Vec::new()),
            ime_admission_hook: None,
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
    #[test]
    fn ime_owner_uninstalled_projection_is_explicit_and_cannot_advertise_text() {
        let o = owner();
        let value = o.snapshot();
        assert_eq!(value["ime"]["installed"], false);
        assert_eq!(value["ime"]["state"], "unavailable");
        assert_eq!(value["ime"]["protocolTextInput"], false);
        assert_eq!(value["textInput"], false);
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
            diagnostic: Arc::new(Diagnostic::default()),
            ime_installation: ImeInstallation::Unavailable,
            ime_ingress: Arc::new(Mutex::new(ImeIngress::default())),
            keyboard: OhosKeyboardQualification::Unavailable,
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
                diagnostic: Arc::new(Diagnostic::default()),
                ime_installation: ImeInstallation::Unavailable,
                ime_ingress: Arc::new(Mutex::new(ImeIngress::default())),
                keyboard: OhosKeyboardQualification::Unavailable,
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
            diagnostic: Arc::new(Diagnostic::default()),
            ime_installation: ImeInstallation::Unavailable,
            ime_ingress: Arc::new(Mutex::new(ImeIngress::default())),
            keyboard: OhosKeyboardQualification::Unavailable,
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
        o.diagnostic_session = Some((request.session_id, 0));
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
    fn unqualified_ohos_keyboard_stays_disabled_after_session_advertises_input() {
        let (mut o, session, link) = presentation_owner();
        o.controller.handle_session_event_with_stores(
            session,
            SessionEvent::CapabilitiesChanged(frd_protocol_api::SessionCapabilities {
                physical_key_input: true,
                text_input: true,
                ..Default::default()
            }),
            o.stores.borrow(),
        );
        let epoch = link.0.lock().unwrap().epoch().unwrap();
        feedback(&link, epoch, frd_frame::FrameCompleteness::FullBaseline);
        o.sync_presentation(false);
        let capabilities = o.controller.effective_capabilities();
        assert!(!capabilities.physical_key_input);
        assert!(!capabilities.text_input);
        assert!(o.controller.route_input(key_event(4, true)).is_none());
        assert!(o
            .controller
            .route_input(InputEvent::Text {
                utf8: "输入".into()
            })
            .is_none());
        assert!(o.controller.route_input(InputEvent::ReleaseAll).is_some());
    }

    #[test]
    fn limited_physical_profile_owner_rejects_unqualified_without_ending_session() {
        let (mut o, epoch, _) = key_owner();
        for hid in [0x32, 0x48, 0x64, 0x66, 0xe3, 0xe7] {
            o.command(key_command(epoch, hid, true));
            o.command(key_command(epoch, hid, false));
        }
        assert!(
            o.sent_inputs.borrow().is_empty(),
            "未资格键不得到达 SessionCommand"
        );
        assert!(matches!(o.controller.page(), Page::RemoteSession { .. }));
        o.command(key_command(epoch, 4, true));
        o.command(key_command(epoch, 4, false));
        assert_key_sent(&o, 4, true);
        assert_key_sent(&o, 4, false);
    }

    #[test]
    fn limited_physical_profile_up_cleanup_precedes_backend_and_hid_filter() {
        // 已持有的旧键以 ReleaseAll 清理，不把未资格 Up 交协议，也不留下隔离。
        for old_hid in [0x48, 0x64] {
            let (mut o, epoch, _) = key_owner();
            let held = o.input.route(
                &o.controller,
                epoch.session,
                epoch.generation,
                epoch.revision,
                key_event(old_hid, true),
                true,
            );
            assert!(held.is_some(), "模拟收窄前真实来源 ledger");
            o.sent_inputs.borrow_mut().clear();
            o.keyboard = OhosKeyboardQualification::Unavailable;
            o.command(key_command(epoch, old_hid, false));
            assert_eq!(
                o.sent_inputs
                    .borrow()
                    .iter()
                    .map(|i| i.event.clone())
                    .collect::<Vec<_>>(),
                vec![InputEvent::ReleaseAll]
            );
            o.keyboard = OhosKeyboardQualification::ArkUiLimited;
            o.command(key_command(epoch, 4, true));
            assert_key_sent(&o, 4, true);
        }
        // 孤立未知 Up 不得借清理路径释放旁侧合法 held。
        let (mut o, epoch, _) = key_owner();
        o.command(key_command(epoch, 4, true));
        o.sent_inputs.borrow_mut().clear();
        for hid in [0x32, 0x48, 0x64, 0xe3] {
            o.command(key_command(epoch, hid, false));
        }
        assert!(o.sent_inputs.borrow().is_empty());
        o.command(key_command(epoch, 4, false));
        assert_key_sent(&o, 4, false);
    }

    #[test]
    fn limited_physical_profile_intersection_and_snapshot_keep_text_unavailable() {
        let (mut o, epoch, _) = key_owner();
        o.keyboard.configure(&mut o.controller);
        assert_eq!(o.snapshot()["physicalKeyInput"], true);
        assert_eq!(o.snapshot()["textInput"], false);
        assert_eq!(o.snapshot()["keyboardBackend"], "arkui_limited");
        for gate in 0..3 {
            o.keyboard.configure(&mut o.controller);
            o.controller.handle_session_event_with_stores(
                o.remote_epoch.unwrap().0,
                SessionEvent::CapabilitiesChanged(frd_protocol_api::SessionCapabilities {
                    physical_key_input: gate != 0,
                    text_input: true,
                    ..Default::default()
                }),
                o.stores.borrow(),
            );
            if gate == 1 {
                o.controller.set_platform_capabilities(Default::default());
            }
            if gate == 2 {
                o.controller.set_product_policy(Default::default());
            }
            o.sent_inputs.borrow_mut().clear();
            o.command(key_command(epoch, 4, true));
            o.command(key_command(epoch, 4, false));
            assert!(
                o.sent_inputs.borrow().is_empty(),
                "protocol/platform/product 独立拒绝"
            );
            assert_eq!(o.snapshot()["physicalKeyInput"], false);
            assert_eq!(o.snapshot()["textInput"], false);
        }
    }

    #[test]
    fn limited_physical_profile_prequeue_rejection_does_not_poison_full_queue_or_focus() {
        let (mut o, epoch, _) = key_owner();
        let (engine, queue) = key_engine(&o, 1);
        engine
            .send(Command::Focus {
                foreground: true,
                focused: true,
                remote: true,
            })
            .unwrap();
        let control = o.control.load(Ordering::Acquire);
        for hid in [0x32, 0x48, 0x64, 0xe3] {
            assert_eq!(
                engine.send(key_command(epoch, hid, true)),
                Err(crate::INVALID_ARGUMENT)
            );
        }
        assert_eq!(
            o.control.load(Ordering::Acquire),
            control,
            "拒绝未资格 Down 不伪造 QueueFull/失焦"
        );
        o.command(queue.recv().unwrap());
        assert!(queue.try_recv().is_err());
        engine.send(key_command(epoch, 4, true)).unwrap();
        o.command(queue.recv().unwrap());
        engine.send(key_command(epoch, 4, false)).unwrap();
        o.command(queue.recv().unwrap());
        assert_key_sent(&o, 4, true);
        assert_key_sent(&o, 4, false);
    }

    fn key_event(hid: u16, down: bool) -> frd_core::InputEvent {
        frd_core::InputEvent::PhysicalKey {
            code: frd_core::PhysicalKeyCode::from_usb_hid_usage(hid),
            state: if down {
                frd_core::KeyState::Pressed
            } else {
                frd_core::KeyState::Released
            },
            modifiers: Default::default(),
        }
    }
    fn key_command(epoch: InputEpoch, hid: u16, down: bool) -> Command {
        Command::Input(epoch, key_event(hid, down), down)
    }
    fn key_owner() -> (Owner, InputEpoch, Arc<crate::presentation::Link>) {
        let (mut o, session, link) = presentation_owner();
        o.keyboard = OhosKeyboardQualification::ArkUiLimited;
        o.controller
            .set_platform_capabilities(frd_platform_api::PlatformCapabilities {
                physical_key_input: true,
                text_input: true,
                ..Default::default()
            });
        o.controller.set_product_policy(frd_app::ProductPolicy {
            physical_key_input: true,
            text_input: true,
            ..Default::default()
        });
        o.controller.handle_session_event_with_stores(
            session,
            SessionEvent::CapabilitiesChanged(frd_protocol_api::SessionCapabilities {
                physical_key_input: true,
                text_input: true,
                ..Default::default()
            }),
            o.stores.borrow(),
        );
        let presented_epoch = link.0.lock().unwrap().epoch().unwrap();
        feedback(
            &link,
            presented_epoch,
            frd_frame::FrameCompleteness::FullBaseline,
        );
        o.sync_presentation(false);
        assert!(o.input_ready());
        o.focus(true, true, true);
        let epoch = InputEpoch {
            session: session.get(),
            generation: 1,
            revision: o.input.revision,
        };
        (o, epoch, link)
    }
    fn key_engine(o: &Owner, capacity: usize) -> (AppEngine, mpsc::Receiver<Command>) {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        (
            AppEngine {
                diagnostic: o.diagnostic.clone(),
                ime_installation: o.ime.installation,
                ime_ingress: o.ime_ingress.clone(),
                keyboard: o.keyboard,
                sender,
                snapshot: Arc::new(Mutex::new(json!({}))),
                control: o.control.clone(),
                renderer: o.renderer.clone(),
            },
            receiver,
        )
    }
    fn assert_key_sent(o: &Owner, hid: u16, down: bool) {
        assert!(
            o.sent_inputs
                .borrow()
                .iter()
                .any(|input| input.event == key_event(hid, down)),
            "必须实际通过 Owner 发出此按键状态"
        );
    }
    fn present_key_owner(o: &mut Owner, link: &crate::presentation::Link) {
        o.sync_presentation(false);
        let epoch = link.0.lock().unwrap().epoch().unwrap();
        feedback(link, epoch, frd_frame::FrameCompleteness::FullBaseline);
        o.sync_presentation(false);
        assert!(o.input_ready());
    }
    #[test]
    fn key_cleanup_accepted_up_survives_a_later_focus_barrier() {
        let (mut o, epoch, _) = key_owner();
        let (e, rx) = key_engine(&o, 8);
        e.send(key_command(epoch, 4, true)).unwrap();
        o.command(rx.recv().unwrap());
        assert_key_sent(&o, 4, true);
        e.lose_focus();
        e.send(key_command(epoch, 4, false)).unwrap();
        e.lose_focus();
        o.command(rx.recv().unwrap());
        assert!(!o
            .sent_inputs
            .borrow()
            .iter()
            .any(|i| i.event == key_event(4, false)));
        o.focus(true, true, true);
        o.sent_inputs.borrow_mut().clear();
        e.send(key_command(epoch, 4, true)).unwrap();
        o.command(rx.recv().unwrap());
        assert_key_sent(&o, 4, true);
    }
    #[test]
    fn key_cleanup_accepted_up_survives_new_geometry_and_presentation() {
        let (mut o, epoch, link) = key_owner();
        let (e, rx) = key_engine(&o, 8);
        e.send(key_command(epoch, 4, true)).unwrap();
        o.command(rx.recv().unwrap());
        let mut geometry = o.input.geometry.unwrap();
        geometry.scale = 2.0;
        e.send(Command::Geometry(geometry)).unwrap();
        e.send(key_command(epoch, 4, false)).unwrap();
        o.command(rx.recv().unwrap());
        present_key_owner(&mut o, &link);
        o.command(rx.recv().unwrap());
        o.sent_inputs.borrow_mut().clear();
        let current = InputEpoch {
            revision: o.input.revision,
            ..epoch
        };
        e.send(key_command(epoch, 5, true)).unwrap();
        o.command(rx.recv().unwrap());
        assert!(o.sent_inputs.borrow().is_empty(), "旧几何 Down 仍拒绝");
        e.send(key_command(current, 4, true)).unwrap();
        o.command(rx.recv().unwrap());
        assert_key_sent(&o, 4, true);
    }
    #[test]
    fn key_cleanup_stale_epoch_up_does_not_clear_current_quarantine_when_unready() {
        let (mut o, epoch, link) = key_owner();
        let (e, rx) = key_engine(&o, 8);
        e.send(key_command(epoch, 4, true)).unwrap();
        o.command(rx.recv().unwrap());
        o.focus(false, false, false);
        o.ready = None;
        let stale = InputEpoch {
            session: frd_core::SessionId::allocate().get(),
            ..epoch
        };
        e.send(key_command(stale, 4, false)).unwrap();
        o.command(rx.recv().unwrap());
        present_key_owner(&mut o, &link);
        o.focus(true, true, true);
        o.sent_inputs.borrow_mut().clear();
        e.send(key_command(epoch, 4, true)).unwrap();
        o.command(rx.recv().unwrap());
        assert!(
            o.sent_inputs.borrow().is_empty(),
            "旧 epoch Up 不得解封仍按住的新键"
        );
        e.send(key_command(epoch, 4, false)).unwrap();
        o.command(rx.recv().unwrap());
        e.send(key_command(epoch, 4, true)).unwrap();
        o.command(rx.recv().unwrap());
        assert_key_sent(&o, 4, true);
    }
    #[test]
    fn key_cleanup_old_origin_can_clear_only_its_own_cross_session_quarantine() {
        let (mut old, old_epoch, _) = key_owner();
        let (e, rx) = key_engine(&old, 8);
        e.send(key_command(old_epoch, 4, true)).unwrap();
        old.command(rx.recv().unwrap());
        old.focus(false, false, false);
        let (mut current, epoch, link) = key_owner();
        current.input = std::mem::take(&mut old.input);
        current.input.remote = Some(frd_core::PixelSize {
            width: 2,
            height: 2,
        });
        present_key_owner(&mut current, &link);
        current.focus(true, true, true);
        let (e2, rx2) = key_engine(&current, 8);
        e2.send(key_command(old_epoch, 4, false)).unwrap();
        current.command(rx2.recv().unwrap());
        current.sent_inputs.borrow_mut().clear();
        e2.send(key_command(epoch, 4, true)).unwrap();
        current.command(rx2.recv().unwrap());
        assert_key_sent(&current, 4, true);
        current.focus(false, false, false);
        current.ready = None;
        e2.send(key_command(old_epoch, 4, false)).unwrap();
        current.command(rx2.recv().unwrap());
        present_key_owner(&mut current, &link);
        current.focus(true, true, true);
        current.sent_inputs.borrow_mut().clear();
        e2.send(key_command(epoch, 4, true)).unwrap();
        current.command(rx2.recv().unwrap());
        assert!(
            current.sent_inputs.borrow().is_empty(),
            "旧来源不得清除新来源的隔离"
        );
    }
    #[test]
    fn key_cleanup_queue_full_is_not_accepted_and_explicit_retry_survives_barrier() {
        let (mut o, epoch, _) = key_owner();
        let (e, rx) = key_engine(&o, 1);
        e.send(key_command(epoch, 4, true)).unwrap();
        o.command(rx.recv().unwrap());
        e.send(Command::Focus {
            foreground: true,
            focused: true,
            remote: true,
        })
        .unwrap();
        assert_eq!(e.send(key_command(epoch, 4, false)), Err(crate::QUEUE_FULL));
        o.command(rx.recv().unwrap());
        assert!(rx.try_recv().is_err(), "失败 Up 不得伪装已排入队列");
        o.focus(true, true, true);
        o.sent_inputs.borrow_mut().clear();
        e.send(key_command(epoch, 4, true)).unwrap();
        o.command(rx.recv().unwrap());
        assert!(o.sent_inputs.borrow().is_empty(), "真实 Up 未重试前仍隔离");
        e.send(key_command(epoch, 4, false)).unwrap();
        e.lose_focus();
        o.command(rx.recv().unwrap());
        o.focus(true, true, true);
        o.sent_inputs.borrow_mut().clear();
        e.send(key_command(epoch, 4, true)).unwrap();
        o.command(rx.recv().unwrap());
        assert_key_sent(&o, 4, true);
    }
    #[test]
    fn key_cleanup_form_barrier_releases_matching_held_before_discarding_up() {
        let (mut o, epoch, _) = key_owner();
        let (e, rx) = key_engine(&o, 8);
        e.send(key_command(epoch, 4, true)).unwrap();
        o.command(rx.recv().unwrap());
        o.sent_inputs.borrow_mut().clear();
        e.send(key_command(epoch, 4, false)).unwrap();
        // FORM_INVALID 增加 control 代际但本身不转移焦点，覆盖尚 held 的清理分支。
        signal(&o.control, FORM_INVALID);
        o.command(rx.recv().unwrap());
        assert_eq!(o.sent_inputs.borrow().len(), 1);
        assert_eq!(
            o.sent_inputs.borrow()[0].event,
            frd_core::InputEvent::ReleaseAll
        );
        o.sent_inputs.borrow_mut().clear();
        e.send(key_command(epoch, 4, true)).unwrap();
        o.command(rx.recv().unwrap());
        assert_key_sent(&o, 4, true);
    }
    #[test]
    fn key_cleanup_unready_old_down_cannot_quarantine_a_new_epoch_key() {
        for old_generation in [false, true] {
            let (mut o, epoch, link) = key_owner();
            let (e, rx) = key_engine(&o, 8);
            o.ready = None;
            let stale = if old_generation {
                InputEpoch {
                    generation: epoch.generation + 1,
                    ..epoch
                }
            } else {
                InputEpoch {
                    session: frd_core::SessionId::allocate().get(),
                    ..epoch
                }
            };
            e.send(key_command(stale, 4, true)).unwrap();
            o.command(rx.recv().unwrap());
            present_key_owner(&mut o, &link);
            o.sent_inputs.borrow_mut().clear();
            e.send(key_command(epoch, 4, true)).unwrap();
            o.command(rx.recv().unwrap());
            assert_key_sent(&o, 4, true);
        }
    }
    #[test]
    fn key_cleanup_normal_remote_down_up_and_local_new_down_keep_their_gates() {
        let (mut o, epoch, _) = key_owner();
        let (e, rx) = key_engine(&o, 8);
        for down in [true, false] {
            e.send(key_command(epoch, 4, down)).unwrap();
            o.command(rx.recv().unwrap());
        }
        let sent = o.sent_inputs.borrow();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].event, key_event(4, true));
        assert_eq!(sent[1].event, key_event(4, false));
        drop(sent);
        o.focus(false, false, false);
        o.sent_inputs.borrow_mut().clear();
        for down in [true, false] {
            e.send(key_command(epoch, 5, down)).unwrap();
            o.command(rx.recv().unwrap());
        }
        assert!(
            o.sent_inputs.borrow().is_empty(),
            "本地新 Down/Up 不进入远端"
        );
        o.focus(true, true, true);
        e.send(key_command(epoch, 5, true)).unwrap();
        o.command(rx.recv().unwrap());
        assert_key_sent(&o, 5, true);
    }

    #[test]
    fn emergency_barrier_rejects_queued_submit_focus_but_preserves_accepted_edit_and_new_geometry()
    {
        let mut o = owner();
        let (tx, rx) = mpsc::sync_channel(4);
        let e = AppEngine {
            diagnostic: Arc::new(Diagnostic::default()),
            ime_installation: ImeInstallation::Unavailable,
            ime_ingress: Arc::new(Mutex::new(ImeIngress::default())),
            keyboard: OhosKeyboardQualification::Unavailable,
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

    // 模式元数据必须反映真实 owner catalog；这些 fixture 不证明目标平台注册。
    fn mode_edit(target: u32, protocol: &str) -> Command {
        Command::Edit(Edit {
            target,
            address: "mode-fixture.invalid".into(),
            port: 5900,
            username: "mode-fixture".into(),
            protocol: protocol.into(),
            remember: false,
        })
    }
    fn mode<'a>(snapshot: &'a Value, id: &str) -> &'a Value {
        snapshot["protocolOptions"]
            .as_array()
            .expect("模式选项投影")
            .iter()
            .find(|option| option["id"] == id)
            .expect("当前模式必须可见")
    }
    #[test]
    fn mode_catalog_distinguishes_explicit_standard_from_unavailable_mac_auto() {
        let mut o = owner();
        o.catalog = ProtocolCatalog::new([ProtocolId::rdp(), ProtocolId::apple_hpss_mvs()]);
        o.command(mode_edit(2, "auto"));
        let snapshot = o.snapshot();
        assert_eq!(snapshot["available"], false);
        assert_eq!(mode(&snapshot, "auto")["available"], false);
        assert_eq!(mode(&snapshot, "apple-hpss-mvs")["available"], true);
        assert_eq!(mode(&snapshot, "apple-hpss-mvs")["targets"], json!([2]));
        assert_eq!(
            mode(&snapshot, "apple-high-performance")["available"],
            false
        );
        assert!(!snapshot["modeNotice"].as_str().unwrap().is_empty());
        assert_eq!(snapshot["protocol"], "auto");
        o.command(mode_edit(2, "apple-hpss-mvs"));
        let snapshot = o.snapshot();
        assert_eq!(snapshot["protocol"], "apple-hpss-mvs");
        assert_eq!(snapshot["available"], true);
        assert!(!snapshot["modeNotice"].as_str().unwrap().is_empty());
        o.catalog = ProtocolCatalog::new([]);
        assert_eq!(mode(&o.snapshot(), "apple-hpss-mvs")["available"], false);
    }
    #[test]
    fn mode_projection_keeps_wrong_target_and_unknown_profile_choices_unavailable() {
        let mut o = owner();
        o.catalog = ProtocolCatalog::new([ProtocolId::rdp(), ProtocolId::apple_hpss_mvs()]);
        o.command(mode_edit(1, "apple-hpss-mvs"));
        let snapshot = o.snapshot();
        assert_eq!(snapshot["protocol"], "apple-hpss-mvs");
        assert_eq!(snapshot["available"], false);
        assert_eq!(mode(&snapshot, "apple-hpss-mvs")["available"], false);
        assert_eq!(mode(&snapshot, "apple-hpss-mvs")["targets"], json!([2]));
        assert!(!snapshot["modeNotice"].as_str().unwrap().is_empty());
        o.command(mode_edit(2, "unknown-profile-mode"));
        let snapshot = o.snapshot();
        assert_eq!(snapshot["protocol"], "unknown-profile-mode");
        assert_eq!(mode(&snapshot, "unknown-profile-mode")["available"], false);
        assert_eq!(
            mode(&snapshot, "unknown-profile-mode")["label"],
            "保存的连接模式"
        );
        assert!(!snapshot["modeNotice"]
            .as_str()
            .unwrap()
            .contains("unknown-profile-mode"));
    }
    fn mode_profiles() -> Vec<SavedConnectionProfile> {
        [
            ProtocolId::apple_hpss_mvs(),
            ProtocolId::apple_high_performance(),
            ProtocolId::rdp(),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, protocol)| SavedConnectionProfile {
            key: ConnectionProfileKey::new(protocol, "same-fixture.invalid", 5900, "same-fixture")
                .unwrap(),
            target_system: if i == 2 {
                TargetSystem::Windows
            } else {
                TargetSystem::MacOs
            },
            last_success_order: i as u64,
        })
        .collect()
    }
    #[test]
    fn mode_recent_profiles_show_protocol_identity_and_load_the_exact_selected_key() {
        let mut o = owner();
        let profiles = mode_profiles();
        assert_ne!(profiles[0].key, profiles[1].key);
        o.controller
            .connection_form_mut()
            .unwrap()
            .set_profiles(profiles.clone());
        let snapshot = o.snapshot();
        for (i, (protocol, target, label)) in [
            ("apple-hpss-mvs", 2, "标准（MVS）"),
            ("apple-high-performance", 2, "高性能"),
            ("rdp", 1, "RDP"),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(snapshot["profiles"][i]["protocol"], protocol);
            assert_eq!(snapshot["profiles"][i]["target"], target);
            assert_eq!(snapshot["profiles"][i]["modeLabel"], label);
        }
        o.command(Command::Select(0));
        let old = *o.pending.keys().next().unwrap();
        o.command(Command::Select(1));
        let current = *o.pending.keys().find(|id| **id != old).unwrap();
        o.command(Command::Loaded(
            old,
            Ok(Some(SecretBuffer::from_text("old-mode-secret".into()))),
        ));
        assert!(o
            .controller
            .connection_form_mut()
            .unwrap()
            .password_is_empty());
        assert_eq!(
            o.controller
                .connection_form_mut()
                .unwrap()
                .selected_profile
                .as_ref(),
            Some(&profiles[1].key)
        );
        o.command(Command::Loaded(
            current,
            Ok(Some(SecretBuffer::from_text("new-mode-secret".into()))),
        ));
        let snapshot = o.snapshot();
        assert_eq!(snapshot["selectedIndex"], 1);
        assert_eq!(snapshot["protocol"], "apple-high-performance");
        assert_eq!(snapshot["target"], 2);
        assert_eq!(snapshot["credentialLoaded"], true);
        // 同一目标/同一模式的回传与状态读取不能清空已载入凭据。
        o.command(Command::Edit(Edit {
            target: 2,
            address: profiles[1].key.address().into(),
            port: 5900,
            username: profiles[1].key.username().into(),
            protocol: "apple-high-performance".into(),
            remember: true,
        }));
        assert_eq!(o.snapshot()["credentialLoaded"], true);
        assert_eq!(o.snapshot()["selectedIndex"], 1);
        // 相同 endpoint/user 只改模式，也必须使原凭据身份失效。
        o.command(Command::Edit(Edit {
            target: 2,
            address: profiles[1].key.address().into(),
            port: 5900,
            username: profiles[1].key.username().into(),
            protocol: "apple-hpss-mvs".into(),
            remember: true,
        }));
        assert_eq!(o.snapshot()["credentialLoaded"], false);
        assert_eq!(o.snapshot()["selectedIndex"], Value::Null);
        assert!(o
            .controller
            .connection_form_mut()
            .unwrap()
            .password_is_empty());
        let text = snapshot.to_string();
        assert!(!text.contains("old-mode-secret"));
        assert!(!text.contains("new-mode-secret"));
        assert!(!text.contains("passwordLength"));
    }
    #[test]
    fn mode_target_edit_uses_projected_compatibility_and_invalidates_loaded_identity() {
        let mut o = owner();
        o.catalog = ProtocolCatalog::new([ProtocolId::rdp(), ProtocolId::apple_hpss_mvs()]);
        let profiles = mode_profiles();
        o.controller
            .connection_form_mut()
            .unwrap()
            .set_profiles(profiles);
        o.command(Command::Select(0));
        let old = *o.pending.keys().next().unwrap();
        let snapshot = o.snapshot();
        // 与正常选择控件相同：目标不在此模式允许列表中，发起原 edit(auto)。
        assert!(!mode(&snapshot, "apple-hpss-mvs")["targets"]
            .as_array()
            .unwrap()
            .contains(&json!(1)));
        o.command(Command::Edit(Edit {
            target: 1,
            address: "same-fixture.invalid".into(),
            port: 5900,
            username: "same-fixture".into(),
            protocol: "auto".into(),
            remember: true,
        }));
        o.command(Command::Loaded(
            old,
            Ok(Some(SecretBuffer::from_text("late-target-secret".into()))),
        ));
        assert_eq!(o.snapshot()["protocol"], "auto");
        assert_eq!(o.snapshot()["available"], true);
        assert_eq!(o.snapshot()["credentialLoaded"], false);
        assert_eq!(o.snapshot()["selectedIndex"], Value::Null);
        assert!(o
            .controller
            .connection_form_mut()
            .unwrap()
            .password_is_empty());
    }
    #[test]
    fn mode_rdp_auto_and_explicit_keep_original_submission_resolution() {
        for protocol in ["auto", "rdp"] {
            let mut o = owner();
            o.catalog = ProtocolCatalog::new([ProtocolId::rdp(), ProtocolId::apple_hpss_mvs()]);
            o.command(mode_edit(1, protocol));
            o.command(Command::Password(SecretBuffer::from_text(
                "rdp-mode-fixture".into(),
            )));
            let snapshot = o.snapshot();
            assert_eq!(mode(&snapshot, "rdp")["available"], true);
            assert_eq!(snapshot["canConnect"], true);
            let submission = o
                .controller
                .connection_form_mut()
                .unwrap()
                .take_submission(&o.catalog)
                .unwrap();
            assert_eq!(submission.resolved_protocol, ProtocolId::rdp());
            assert_eq!(submission.password.expose_text(), Some("rdp-mode-fixture"));
        }
    }
    #[test]
    fn mode_c_abi_ordinary_snapshot_reports_actual_platform_catalog_without_connecting() {
        use crate::app_abi::*;
        let root = std::env::temp_dir().join(format!(
            "frd-mode-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = root.to_str().unwrap().as_bytes();
        let mut id = 0;
        assert_eq!(
            unsafe { frd_ohos_app_create(path.as_ptr(), path.len(), &mut id) },
            crate::OK
        );
        struct App(u64);
        impl Drop for App {
            fn drop(&mut self) {
                let code = frd_ohos_app_destroy(self.0);
                if !std::thread::panicking() {
                    assert_eq!(code, crate::OK);
                }
            }
        }
        let _app = App(id);
        let edit = br#"{"target":2,"address":"mode-fixture.invalid","port":5900,"username":"fixture","protocol":"apple-hpss-mvs","remember":false}"#;
        assert_eq!(
            unsafe { frd_ohos_app_edit(id, edit.as_ptr(), edit.len()) },
            crate::OK
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        let snapshot = loop {
            let mut p = std::ptr::null_mut();
            let mut n = 0;
            assert_eq!(
                unsafe { frd_ohos_app_snapshot(id, &mut p, &mut n) },
                crate::OK
            );
            let value: Value =
                serde_json::from_slice(unsafe { std::slice::from_raw_parts(p, n) }).unwrap();
            unsafe { frd_ohos_app_snapshot_free(p, n) };
            if value["initialized"] == true && value["protocol"] == "apple-hpss-mvs" {
                break value;
            }
            assert!(Instant::now() < deadline, "owner 未处理模式编辑");
            std::thread::sleep(Duration::from_millis(5));
        };
        let actual_standard = cfg!(all(
            feature = "normal-hap-apple-mvs-validation",
            target_env = "ohos",
            target_arch = "aarch64"
        ));
        assert_eq!(
            mode(&snapshot, "apple-hpss-mvs")["available"],
            actual_standard
        );
        assert_eq!(mode(&snapshot, "auto")["available"], false);
        assert_eq!(snapshot["page"], 0);
        assert_eq!(snapshot["session"], "");
        assert_eq!(snapshot["canConnect"], false);
        assert!(snapshot.get("syntheticFixture").is_none());
    }
    struct CaptureDisplay(mpsc::Sender<frd_core::DisplayIntent>);
    impl ProtocolFactory for CaptureDisplay {
        fn descriptor(&self) -> frd_protocol_api::ProtocolDescriptor {
            frd_protocol_api::ProtocolDescriptor {
                id: ProtocolId::rdp(),
                display_name: "测试 RDP".into(),
                default_port: 3389,
                credential_requirements:
                    frd_protocol_api::CredentialRequirements::username_password(),
            }
        }
        fn create(
            &self,
            request: frd_protocol_api::ConnectRequest,
            _runtime: frd_protocol_api::ProtocolRuntime,
        ) -> Result<Box<dyn frd_protocol_api::ProtocolSession>, ProtocolError> {
            self.0.send(request.display_intent).unwrap();
            Err(ProtocolError::Terminal)
        }
    }
    fn capture_display_launch(
        mode: frd_core::ResolutionMode,
        messages: Vec<crate::local_display::Message>,
        focus_barrier: bool,
    ) -> frd_core::DisplayIntent {
        capture_display_owner(mode, messages, focus_barrier).0
    }
    fn capture_display_owner(
        mode: frd_core::ResolutionMode,
        messages: Vec<crate::local_display::Message>,
        focus_barrier: bool,
    ) -> (frd_core::DisplayIntent, Owner, AppEngine) {
        let mut o = owner();
        let (captured, rx) = mpsc::channel();
        o.host = SessionHost::new(
            vec![Arc::new(CaptureDisplay(captured)) as Arc<dyn ProtocolFactory>],
            Arc::new(Wake),
            Arc::new(NoAudio),
            Arc::new(|_| {
                Err(frd_media_api::VideoDecodeError::new(
                    frd_media_api::VideoDecodeErrorCode::BackendUnavailable,
                ))
            }),
        );
        o.catalog = ProtocolCatalog::new([ProtocolId::rdp()]);
        o.controller
            .connection_form_mut()
            .unwrap()
            .draft
            .resolution_mode = mode;
        let (engine, queue) = key_engine(&o, 16);
        engine
            .send(Command::Edit(Edit {
                target: 1,
                address: "fixture.invalid".into(),
                port: 3389,
                username: "synthetic".into(),
                protocol: "auto".into(),
                remember: false,
            }))
            .unwrap();
        engine
            .send(Command::Password(SecretBuffer::from_text(
                "in-memory-display-fixture".into(),
            )))
            .unwrap();
        let count = messages.len();
        for message in messages {
            engine.send(Command::LocalDisplay(message)).unwrap();
        }
        if focus_barrier {
            engine.lose_focus();
        }
        // 真实队列依 FIFO 消费，焦点屏障不丢失 local-display；Submit 在屏障后排入。
        for _ in 0..count + 2 {
            o.command(queue.recv().unwrap());
        }
        engine
            .send(Command::Submit {
                event: 1,
                composition: false,
                repeat: false,
            })
            .unwrap();
        o.command(queue.recv().unwrap());
        let intent = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(
            o.snapshot()["frozenDisplayIntent"]["projectedSize"].is_object()
                || mode == frd_core::ResolutionMode::ServerManaged
                || intent.geometry.is_none()
        );
        (intent, o, engine)
    }
    fn display_message(value: Value) -> crate::local_display::Message {
        crate::local_display::Message::parse(&serde_json::to_vec(&value).unwrap()).unwrap()
    }
    #[test]
    fn local_display_owner_fifo_freezes_actual_connect_request_and_phone_projection() {
        let intent = capture_display_launch(
            frd_core::ResolutionMode::NativeDisplay,
            vec![display_message(crate::local_display::sample_message(
                1, 1, 11, true,
            ))],
            true,
        );
        assert_eq!(intent.mode, frd_core::ResolutionMode::NativeDisplay);
        assert_eq!(
            intent.geometry.unwrap().physical_size,
            frd_core::PixelSize::new(1260, 2720).unwrap()
        );
        assert_eq!(
            intent.orientation,
            frd_core::DisplayOrientationPreference::Landscape
        );
        assert_eq!(
            frd_core::DisplayPlanner::plan(intent, Default::default())
                .unwrap()
                .remote_size,
            frd_core::PixelSize::new(2720, 1260)
        );
        let desktop = capture_display_launch(
            frd_core::ResolutionMode::NativeDisplay,
            vec![display_message(crate::local_display::sample_message(
                1, 1, 11, false,
            ))],
            false,
        );
        assert_eq!(
            desktop.orientation,
            frd_core::DisplayOrientationPreference::Preserve
        );
    }
    #[test]
    fn local_display_owner_keeps_fixed_server_managed_and_rejects_late_destroyed_window() {
        for mode in [
            frd_core::ResolutionMode::Fixed(frd_core::PixelSize::new(900, 1600).unwrap()),
            frd_core::ResolutionMode::ServerManaged,
        ] {
            let intent = capture_display_launch(
                mode,
                vec![display_message(crate::local_display::sample_message(
                    1, 1, 11, true,
                ))],
                false,
            );
            assert_eq!(intent.mode, mode);
            assert!(intent.geometry.is_none());
            assert_eq!(
                intent.orientation,
                frd_core::DisplayOrientationPreference::Preserve
            );
        }
        let intent = capture_display_launch(
            frd_core::ResolutionMode::NativeDisplay,
            vec![
                display_message(crate::local_display::sample_message(1, 1, 11, true)),
                display_message(
                    json!({"abiVersion":1,"stage":2,"revision":2,"windowId":0,"phone":true,"geometry":null,"reason":"window_destroyed"}),
                ),
                display_message(crate::local_display::sample_message(1, 3, 11, true)),
            ],
            false,
        );
        assert!(intent.geometry.is_none());
    }
    #[test]
    fn local_display_queue_overflow_invalidates_cache_and_older_queued_snapshot() {
        let mut o = owner();
        o.command(Command::LocalDisplay(display_message(
            crate::local_display::sample_message(1, 1, 11, true),
        )));
        assert!(o.local_display.geometry.is_some());
        let (engine, queue) = key_engine(&o, 1);
        engine
            .send(Command::LocalDisplay(display_message(
                crate::local_display::sample_message(1, 2, 11, true),
            )))
            .unwrap();
        assert_eq!(
            engine.send(Command::LocalDisplay(display_message(
                crate::local_display::sample_message(1, 3, 11, true)
            ))),
            Err(crate::QUEUE_FULL)
        );
        o.command(queue.recv().unwrap());
        assert!(o.local_display.geometry.is_none());
        assert_eq!(o.local_display.reason, "本地显示消息队列已满");
        engine
            .send(Command::LocalDisplay(display_message(
                crate::local_display::sample_message(1, 4, 11, true),
            )))
            .unwrap();
        o.command(queue.recv().unwrap());
        assert!(o.local_display.geometry.is_some());
    }
    #[test]
    fn local_display_fix1_owner_rejects_malformed_system_area_without_polluting_snapshot() {
        let mut o = owner();
        let (engine, queue) = key_engine(&o, 8);
        engine
            .send(Command::LocalDisplay(display_message(
                crate::local_display::sample_message(1, 1, 11, true),
            )))
            .unwrap();
        o.command(queue.recv().unwrap());
        let original = o.local_display.evidence();
        for (index, area) in [
            json!({"left":-1,"top":0,"width":1260,"height":2720}),
            json!({"left":0,"top":-1,"width":1260,"height":2720}),
            json!({"left":0,"top":0,"width":1261,"height":2720}),
            json!({"left":0,"top":0,"width":1260,"height":2721}),
        ]
        .into_iter()
        .enumerate()
        {
            let mut raw = crate::local_display::sample_message(1, index as u64 + 2, 11, true);
            raw["geometry"]["workArea"] = area;
            // 防御性覆盖真实Owner消费者，绕开ABI parse以证明缓存本身也拒绝畸形更新。
            let message = serde_json::from_value(raw).unwrap();
            engine.send(Command::LocalDisplay(message)).unwrap();
            o.command(queue.recv().unwrap());
            assert_eq!(o.local_display.evidence(), original);
        }
        let mut full = crate::local_display::sample_message(1, 6, 11, true);
        full["geometry"]["workArea"] = json!({"left":0,"top":0,"width":1260,"height":2720});
        engine
            .send(Command::LocalDisplay(display_message(full)))
            .unwrap();
        o.command(queue.recv().unwrap());
        assert_eq!(o.local_display.source, "system_available_area");
        let mut fallback = crate::local_display::sample_message(1, 7, 11, true);
        fallback["reason"] = "invalid_work_area".into();
        engine
            .send(Command::LocalDisplay(display_message(fallback)))
            .unwrap();
        o.command(queue.recv().unwrap());
        assert_eq!(o.local_display.source, "drawable_conservative_fallback");
        assert_eq!(o.local_display.reason, "系统工作区越界，使用可绘制内容区");
    }
    #[test]
    fn local_display_fix1_owner_log_uses_same_snapshot_allowlist_and_only_changes() {
        let (_, mut owner, _engine) = capture_display_owner(
            frd_core::ResolutionMode::NativeDisplay,
            vec![display_message(crate::local_display::sample_message(
                1, 1, 11, true,
            ))],
            false,
        );
        owner.notice = Some("private-profile-notice");
        let snapshot = owner.snapshot();
        assert_eq!(snapshot["address"], "fixture.invalid");
        assert_eq!(snapshot["username"], "synthetic");
        let payload = owner.take_display_log(&snapshot).unwrap();
        let fields = payload.as_object().unwrap();
        assert_eq!(fields.len(), 3);
        for field in [
            "localDisplayEvidence",
            "frozenDisplayIntent",
            "acceptedSurface",
        ] {
            assert_eq!(payload[field], snapshot[field]);
        }
        let text = payload.to_string();
        for secret in [
            "fixture.invalid",
            "synthetic",
            "in-memory-display-fixture",
            "private-profile-notice",
            "credentials",
            "username",
            "address",
            "profile",
        ] {
            assert!(!text.contains(secret), "日志不得含表单身份/秘密字段");
        }
        assert!(owner.take_display_log(&snapshot).is_none());
        owner.notice = Some("another-private-notice");
        assert!(owner.take_display_log(&owner.snapshot()).is_none());
        owner.command(Command::LocalDisplay(display_message(
            crate::local_display::sample_message(1, 2, 11, true),
        )));
        let changed = owner.snapshot();
        assert_eq!(
            owner.take_display_log(&changed).unwrap()["localDisplayEvidence"],
            changed["localDisplayEvidence"]
        );
        assert!(owner.take_display_log(&changed).is_none());
    }

    #[test]
    fn local_display_fix1_owner_log_surface_evidence_is_not_viewport_geometry() {
        let (mut owner, session, _link) = presentation_owner();
        let snapshot = owner.snapshot();
        let evidence = owner.take_display_log(&snapshot).unwrap();
        assert_eq!(evidence["acceptedSurface"], snapshot["acceptedSurface"]);
        assert_eq!(
            evidence["acceptedSurface"]["session"],
            session.get().to_string()
        );
        assert_eq!(evidence["acceptedSurface"]["width"], 2);
        assert_eq!(
            evidence["acceptedSurface"]["source"],
            "protocol_SurfaceGenerationChanged"
        );
        assert_eq!(owner.input.geometry.unwrap().drawable.width, 100);
        assert!(owner.take_display_log(&owner.snapshot()).is_none());
    }
    include!("ime_owner_tests.rs");
    #[test]
    fn terminal_diagnostic_actual_owner_cancel_and_runtime_paths_are_distinct_and_safe() {
        use crate::session_diagnostic::{Event, PageCode};
        let (mut o, session, _) = staged_owner();
        signal(&o.control, CANCEL | FOCUS_LOST);
        o.apply_control();
        let records = o.diagnostic.records();
        assert_eq!(
            records.iter().map(|r| r.event).collect::<Vec<_>>(),
            [Event::OwnerCancel, Event::ApplyControl]
        );
        assert_eq!(records[1].from, PageCode::Connecting);
        assert_eq!(records[1].to, PageCode::Disconnecting);
        assert_eq!(records[1].flags, CANCEL | FOCUS_LOST);
        assert_eq!(records[1].revision, 1);
        assert_eq!(records[1].session, session.get());
        let encoded = records
            .iter()
            .map(|r| r.encode())
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            "fixture.invalid",
            "synthetic",
            "in-memory-auth-fixture",
            "password",
            "username",
        ] {
            assert!(!encoded.contains(forbidden));
        }
        let (mut closed, session, _) = staged_owner();
        closed.runtime_event(
            session,
            SessionEvent::Closed(frd_protocol_api::ProtocolExit::Closed),
        );
        let records = closed.diagnostic.records();
        assert_eq!(records[0].event, Event::RuntimeClosed);
        assert_eq!(records[0].from, PageCode::Connecting);
        assert_eq!(records[0].to, PageCode::Disconnecting);
        assert!(records.iter().all(|r| r.event != Event::OwnerCancel));
        let (mut failed, session, _) = staged_owner();
        failed.runtime_event(
            session,
            SessionEvent::Closed(frd_protocol_api::ProtocolExit::Failed(
                ProtocolError::adapter(ProtocolId::rdp(), "secret-raw-server-error"),
            )),
        );
        let records = failed.diagnostic.records();
        assert_eq!(records[0].event, Event::RuntimeFailed);
        assert_eq!(records[0].to, PageCode::Failed);
        assert!(!records[0].encode().contains("secret-raw-server-error"));
        failed.intent(AppIntent::ReturnToConnection);
        let records = failed.diagnostic.records();
        assert_eq!(records.last().unwrap().event, Event::Return);
        assert_eq!(records.last().unwrap().to, PageCode::Form);
    }
    #[test]
    fn terminal_diagnostic_stale_session_and_non_cancel_controls_preserve_attribution() {
        use crate::session_diagnostic::{Event, PageCode};
        let (mut o, current, _) = staged_owner();
        let old = frd_core::SessionId::allocate();
        o.runtime_event(
            old,
            SessionEvent::Closed(frd_protocol_api::ProtocolExit::Closed),
        );
        let records = o.diagnostic.records();
        assert_eq!(records[0].session, old.get());
        assert!(!records[0].current);
        assert_eq!(records[0].from, PageCode::Connecting);
        assert_eq!(records[0].to, PageCode::Connecting);
        signal(&o.control, FOCUS_LOST | DISPLAY_INVALID);
        o.apply_control();
        assert!(o
            .diagnostic
            .records()
            .iter()
            .all(|r| r.event != Event::OwnerCancel));
        assert_eq!(o.diagnostic_session, Some((current, 0)));
    }
    #[test]
    fn terminal_diagnostic_full_receipt_and_renderer_failure_are_deduplicated() {
        use crate::session_diagnostic::Event;
        let (mut o, _, link) = presentation_owner();
        let epoch = link.0.lock().unwrap().epoch().unwrap();
        feedback(&link, epoch, frd_frame::FrameCompleteness::Incremental);
        o.sync_presentation(false);
        assert!(o.diagnostic.records().is_empty());
        for _ in 0..2 {
            feedback(&link, epoch, frd_frame::FrameCompleteness::FullBaseline);
            o.sync_presentation(false);
        }
        assert_eq!(
            o.diagnostic
                .records()
                .iter()
                .filter(|r| r.event == Event::FirstFull)
                .count(),
            1
        );
        link.fail(crate::GPU_FAILED);
        o.sync_presentation(false);
        o.sync_presentation(false);
        link.surface(crate::PocStatus {
            surface_generation: 2,
            width: 100,
            height: 100,
            attached: 1,
            foreground: 1,
            ..Default::default()
        });
        o.sync_presentation(false);
        assert_eq!(
            o.diagnostic
                .records()
                .iter()
                .filter(|r| r.event == Event::RendererFailed)
                .count(),
            1
        );
        let events = o
            .diagnostic
            .records()
            .iter()
            .map(|r| r.event)
            .collect::<Vec<_>>();
        assert_eq!(
            events,
            [Event::FirstFull, Event::RendererFailed, Event::RuntimeError]
        );
    }

    struct TerminalFactory;
    struct TerminalSession;
    impl ProtocolFactory for TerminalFactory {
        fn descriptor(&self) -> frd_protocol_api::ProtocolDescriptor {
            frd_protocol_api::ProtocolDescriptor {
                id: ProtocolId::rdp(),
                display_name: "终态控制".into(),
                default_port: 3389,
                credential_requirements:
                    frd_protocol_api::CredentialRequirements::username_password(),
            }
        }
        fn create(
            &self,
            _: frd_protocol_api::ConnectRequest,
            _: frd_protocol_api::ProtocolRuntime,
        ) -> Result<Box<dyn frd_protocol_api::ProtocolSession>, ProtocolError> {
            Ok(Box::new(TerminalSession))
        }
    }
    impl frd_protocol_api::ProtocolSession for TerminalSession {
        fn run(self: Box<Self>) -> frd_protocol_api::ProtocolExit {
            frd_protocol_api::ProtocolExit::Closed
        }
    }
    #[test]
    fn terminal_diagnostic_actual_runtime_and_cleanup_complete_to_form_without_network() {
        use crate::session_diagnostic::{Event, PageCode};
        let mut o = owner();
        o.host = SessionHost::new(
            vec![Arc::new(TerminalFactory) as Arc<dyn ProtocolFactory>],
            Arc::new(Wake),
            Arc::new(NoAudio),
            Arc::new(|_| {
                Err(frd_media_api::VideoDecodeError::new(
                    frd_media_api::VideoDecodeErrorCode::BackendUnavailable,
                ))
            }),
        );
        o.catalog = ProtocolCatalog::new([ProtocolId::rdp()]);
        let (tx, rx) = mpsc::sync_channel(64);
        o.tx = tx;
        o.command(mode_edit(1, "auto"));
        o.command(Command::Password(SecretBuffer::from_text(
            "terminal-secret-input".into(),
        )));
        o.command(Command::Submit {
            event: 1,
            composition: false,
            repeat: false,
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        while !matches!(o.controller.page(), Page::ConnectionForm(_)) {
            if let Ok(command) = rx.recv_timeout(Duration::from_millis(5)) {
                o.command(command);
            }
            o.poll();
            assert!(Instant::now() < deadline, "无网络终态控制未完成");
        }
        let records = o.diagnostic.records();
        assert_eq!(
            records.iter().map(|r| r.event).collect::<Vec<_>>(),
            [Event::RuntimeClosed, Event::Cleanup]
        );
        assert_eq!(records[0].from, PageCode::Connecting);
        assert_eq!(records[0].to, PageCode::Disconnecting);
        assert!(records[0].current && records[0].active);
        assert_eq!(records[1].from, PageCode::Disconnecting);
        assert_eq!(records[1].to, PageCode::Form);
        assert_eq!(records[0].session, records[1].session);
        assert!(records[1].current);
        assert_eq!(o.diagnostic_session, None);
        assert!(records
            .iter()
            .all(|r| !r.encode().contains("terminal-secret-input")));
    }
    #[test]
    fn terminal_diagnostic_engine_signals_have_exact_revision_and_distinct_drop_origin() {
        use crate::session_diagnostic::Event;
        let o = owner();
        let (engine, _rx) = key_engine(&o, 2);
        engine.diagnostic_bind(123);
        engine.cancel();
        engine.stop();
        drop(engine);
        let records = o.diagnostic.records();
        assert_eq!(
            records.iter().map(|r| r.event).collect::<Vec<_>>(),
            [
                Event::Bound,
                Event::EngineCancel,
                Event::EngineStop,
                Event::EngineDrop,
                Event::EngineStop
            ]
        );
        assert_eq!((records[1].revision, records[1].flags), (1, CANCEL));
        assert_eq!((records[2].revision, records[2].flags), (2, CANCEL | STOP));
        assert_eq!((records[4].revision, records[4].flags), (3, CANCEL | STOP));
    }

    #[test]
    fn terminal_diagnostic_surface_mismatch_stage_stop_and_error_are_explicit() {
        use crate::session_diagnostic::{Event, PageCode};
        let (mut o, session, _) = staged_owner();
        let wrong = frd_core::SessionId::allocate();
        o.runtime_event(
            session,
            SessionEvent::SurfaceGenerationChanged {
                session_id: wrong,
                generation: 9,
                size: frd_core::PixelSize {
                    width: 2,
                    height: 2,
                },
            },
        );
        let record = o.diagnostic.records()[0];
        assert_eq!(record.event, Event::SurfaceGeneration);
        assert_eq!(
            (record.session, record.event_session, record.generation),
            (session.get(), wrong.get(), 9)
        );
        assert!(record.current);
        assert!(!record.active);
        assert!(o.remote_epoch.is_none());
        o.runtime_event(
            session,
            SessionEvent::StageChanged(ConnectionStage::Disconnecting),
        );
        let record = *o.diagnostic.records().last().unwrap();
        assert_eq!(record.event, Event::RuntimeDisconnecting);
        assert_eq!(record.to, PageCode::Disconnecting);
        let (mut stopped, _, _) = staged_owner();
        signal(&stopped.control, STOP);
        stopped.apply_control();
        assert!(stopped.stopping);
        let records = stopped.diagnostic.records();
        assert_eq!(
            records.iter().map(|r| r.event).collect::<Vec<_>>(),
            [Event::OwnerCancel, Event::ApplyControl]
        );
        assert_eq!(records[1].flags, STOP);
        let (mut failed, session, _) = staged_owner();
        failed.runtime_event(
            session,
            SessionEvent::Error(ProtocolError::adapter(
                ProtocolId::rdp(),
                "secret-input-not-an-allowlisted-code",
            )),
        );
        let record = failed.diagnostic.records()[0];
        assert_eq!(record.event, Event::RuntimeError);
        assert_eq!(record.to, PageCode::Failed);
        assert!(!record
            .encode()
            .contains("secret-input-not-an-allowlisted-code"));
    }
    #[test]
    fn terminal_diagnostic_allowlist_with_populated_secret_form_and_repeated_focus_is_sparse() {
        use crate::session_diagnostic::Event;
        let mut o = owner();
        let form = o.controller.connection_form_mut().unwrap();
        form.draft.address = "private-host-input".into();
        form.draft.username = "private-user-input".into();
        form.set_password(SecretBuffer::from_text("private-password-input".into()));
        for _ in 0..3 {
            signal(&o.control, FOCUS_LOST | DISPLAY_INVALID);
            o.apply_control();
        }
        assert!(o.diagnostic.records().is_empty());
        signal(&o.control, CANCEL);
        o.apply_control();
        let records = o.diagnostic.records();
        assert_eq!(
            records.iter().map(|r| r.event).collect::<Vec<_>>(),
            [Event::OwnerCancel, Event::ApplyControl]
        );
        for record in records {
            let text = record.encode();
            for forbidden in [
                "private-host-input",
                "private-user-input",
                "private-password-input",
                "password",
                "username",
                "address",
            ] {
                assert!(!text.contains(forbidden));
            }
            let value = serde_json::to_value(record).unwrap();
            let keys = value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                keys,
                [
                    "event",
                    "session",
                    "event_session",
                    "active",
                    "generation",
                    "local",
                    "lifecycle",
                    "current",
                    "from",
                    "to",
                    "revision",
                    "flags",
                    "code",
                    "abi"
                ]
                .into_iter()
                .collect()
            );
        }
    }
}
