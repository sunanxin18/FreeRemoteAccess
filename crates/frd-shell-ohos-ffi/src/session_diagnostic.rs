//! 会话终态稀疏诊断：只接受封闭枚举和内部数字，不能接收快照、输入或错误文本。
use frd_ui_model::Page;
use serde::Serialize;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock,
};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Event {
    Bound,
    ActionCancel,
    ActionReturn,
    AppDestroy,
    EngineCancel,
    EngineStop,
    EngineDrop,
    ChannelClosed,
    ApplyControl,
    OwnerCancel,
    Return,
    Cleanup,
    RuntimeClosed,
    RuntimeFailed,
    RuntimeError,
    RuntimeDisconnecting,
    SurfaceGeneration,
    RendererFailed,
    FirstFull,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[repr(u8)]
pub(crate) enum PageCode {
    Form = 0,
    Connecting,
    Awaiting,
    Remote,
    Disconnecting,
    Failed,
    Unbound,
}
impl From<&Page> for PageCode {
    fn from(page: &Page) -> Self {
        match page {
            Page::ConnectionForm(_) => Self::Form,
            Page::Connecting { .. } => Self::Connecting,
            Page::AwaitingFirstFrame { .. } => Self::Awaiting,
            Page::RemoteSession { .. } => Self::Remote,
            Page::Disconnecting { .. } => Self::Disconnecting,
            Page::Failed { .. } => Self::Failed,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Code {
    #[default]
    None,
    Unknown,
    NativePresentation,
    RuntimeAdapter,
    RuntimeCore,
    CleanupFailed,
}
impl Code {
    pub(crate) fn error(error: &frd_protocol_api::ProtocolError) -> Self {
        match error {
            frd_protocol_api::ProtocolError::Adapter {
                code: "native_presentation_failed",
                ..
            } => Self::NativePresentation,
            frd_protocol_api::ProtocolError::Adapter { .. } => Self::RuntimeAdapter,
            _ => Self::RuntimeCore,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct Record {
    pub event: Event,
    pub session: u64,
    pub event_session: u64,
    pub active: bool,
    pub generation: u64,
    pub local: u64,
    pub lifecycle: u64,
    pub current: bool,
    pub from: PageCode,
    pub to: PageCode,
    pub revision: u64,
    pub flags: u64,
    pub code: Code,
    pub abi: i32,
}
impl Record {
    pub(crate) fn new(event: Event, from: PageCode, to: PageCode) -> Self {
        Self {
            event,
            from,
            to,
            session: 0,
            event_session: 0,
            active: false,
            generation: 0,
            local: 0,
            lifecycle: 0,
            current: false,
            revision: 0,
            flags: 0,
            code: Code::None,
            abi: 0,
        }
    }
    #[cfg(test)]
    pub(crate) fn encode(&self) -> String {
        serde_json::to_string(self).unwrap()
    }
}
#[derive(Default)]
pub(crate) struct Diagnostic {
    app: AtomicU64,
    #[cfg(test)]
    records: std::sync::Mutex<Vec<Record>>,
}
impl Diagnostic {
    pub(crate) fn bind(&self, app: u64) {
        self.app.store(app, Ordering::Release);
        self.ingress(Event::Bound, 0);
    }
    pub(crate) fn ingress(&self, event: Event, control: u64) {
        let mut record = Record::new(event, PageCode::Unbound, PageCode::Unbound);
        record.revision = control >> 32;
        // STOP/CANCEL/FOCUS_LOST/FORM_INVALID/DISPLAY_INVALID；从不读取输入。
        record.flags = control & 31;
        self.emit(record);
    }
    pub(crate) fn emit(&self, record: Record) {
        static START: OnceLock<Instant> = OnceLock::new();
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let micros = START.get_or_init(Instant::now).elapsed().as_micros();
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1;
        let app = self.app.load(Ordering::Acquire);
        #[derive(Serialize)]
        struct Envelope {
            producer: &'static str,
            clock: &'static str,
            sequence: u64,
            monotonic_us: u128,
            app: u64,
            bound: bool,
            #[serde(flatten)]
            record: Record,
        }
        let envelope = Envelope {
            producer: "rust",
            clock: "rust_process_elapsed",
            sequence,
            monotonic_us: micros,
            app,
            bound: app != 0,
            record,
        };
        #[cfg(test)]
        if let Ok(mut records) = self.records.lock() {
            records.push(record);
        }
        if let Ok(text) = serde_json::to_string(&envelope) {
            emit(&text);
        }
    }
    #[cfg(test)]
    pub(crate) fn records(&self) -> Vec<Record> {
        self.records.lock().unwrap().clone()
    }
}
#[cfg(target_env = "ohos")]
fn emit(text: &str) {
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
    if let Ok(text) = std::ffi::CString::new(text) {
        unsafe {
            OH_LOG_Print(
                0,
                4,
                0xFF00,
                c"FrdSession".as_ptr(),
                c"%{public}s".as_ptr(),
                text.as_ptr(),
            );
        }
    }
}
#[cfg(not(target_env = "ohos"))]
fn emit(_: &str) {}
