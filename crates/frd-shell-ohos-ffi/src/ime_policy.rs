//! 有来源的 IME 意图；只保存零化文本，不实现 ArkUI editor 或能力自动安装。
use frd_core::SecretBuffer;
use serde::{Deserialize, Deserializer};
use std::collections::VecDeque;

const JS_MAX: u64 = 9_007_199_254_740_991;
fn decimal<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let s = String::deserialize(d)?;
    if s.is_empty() || s.starts_with('0') || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(serde::de::Error::custom("invalid_id"));
    }
    s.parse()
        .map_err(|_| serde::de::Error::custom("invalid_id"))
}
fn safe<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let n = u64::deserialize(d)?;
    if n == 0 || n > JS_MAX {
        return Err(serde::de::Error::custom("invalid_serial"));
    }
    Ok(n)
}
fn secret<'de, D: Deserializer<'de>>(d: D) -> Result<Option<SecretBuffer>, D::Error> {
    // 不先构建 Value 或带内容 Debug 的中间对象；失败和离开作用域均零化。
    String::deserialize(d).map(|s| Some(SecretBuffer::from_text(s)))
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Ticket {
    #[serde(deserialize_with = "safe")]
    pub window_stage: u64,
    #[serde(deserialize_with = "safe")]
    pub window_id: u64,
    #[serde(deserialize_with = "safe")]
    pub editor_serial: u64,
    #[serde(deserialize_with = "decimal")]
    pub session: u64,
    #[serde(deserialize_with = "decimal")]
    pub generation: u64,
}
impl Ticket {
    pub fn snapshot(self) -> serde_json::Value {
        serde_json::json!({"windowStage":self.window_stage,"windowId":self.window_id,
            "editorSerial":self.editor_serial,"session":self.session.to_string(),"generation":self.generation.to_string()})
    }
}
#[derive(Clone, Copy, Deserialize)]
#[serde(try_from = "String")]
pub(crate) enum SourceState {
    Attached,
    Detached,
}
impl TryFrom<String> for SourceState {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "attached" => Ok(Self::Attached),
            "detached" => Ok(Self::Detached),
            _ => Err("invalid_source_state"),
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Source {
    abi_version: u32,
    pub(crate) state: SourceState,
    pub(crate) ticket: Ticket,
}
impl Source {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > 4096 {
            return None;
        }
        let s: Self = serde_json::from_slice(bytes).ok()?;
        (s.abi_version == 1).then_some(s)
    }
}
#[derive(Clone, Copy, Deserialize)]
#[serde(try_from = "String")]
pub(crate) enum ControlKey {
    Enter,
    Tab,
    Backspace,
    Delete,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Home,
    End,
    PageUp,
    PageDown,
}
impl TryFrom<String> for ControlKey {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "Enter" => Ok(Self::Enter),
            "Tab" => Ok(Self::Tab),
            "Backspace" => Ok(Self::Backspace),
            "Delete" => Ok(Self::Delete),
            "ArrowLeft" => Ok(Self::ArrowLeft),
            "ArrowRight" => Ok(Self::ArrowRight),
            "ArrowUp" => Ok(Self::ArrowUp),
            "ArrowDown" => Ok(Self::ArrowDown),
            "Home" => Ok(Self::Home),
            "End" => Ok(Self::End),
            "PageUp" => Ok(Self::PageUp),
            "PageDown" => Ok(Self::PageDown),
            _ => Err("invalid_control_key"),
        }
    }
}
impl ControlKey {
    pub fn hid(self) -> u16 {
        match self {
            Self::Enter => 0x28,
            Self::Tab => 0x2b,
            Self::Backspace => 0x2a,
            Self::Delete => 0x4c,
            Self::ArrowLeft => 0x50,
            Self::ArrowRight => 0x4f,
            Self::ArrowUp => 0x52,
            Self::ArrowDown => 0x51,
            Self::Home => 0x4a,
            Self::End => 0x4d,
            Self::PageUp => 0x4b,
            Self::PageDown => 0x4e,
        }
    }
}
fn control_key<'de, D: Deserializer<'de>>(d: D) -> Result<Option<ControlKey>, D::Error> {
    ControlKey::deserialize(d).map(Some)
}
#[derive(Deserialize)]
#[serde(try_from = "String")]
enum Kind {
    Text,
    KeyPair,
}
impl TryFrom<String> for Kind {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "text" => Ok(Self::Text),
            "keyPair" => Ok(Self::KeyPair),
            _ => Err("invalid_operation_kind"),
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Operation {
    abi_version: u32,
    pub(crate) ticket: Ticket,
    #[serde(deserialize_with = "safe")]
    pub(crate) serial: u64,
    kind: Kind,
    #[serde(default, deserialize_with = "secret")]
    pub(crate) text: Option<SecretBuffer>,
    #[serde(default, deserialize_with = "control_key")]
    pub(crate) key: Option<ControlKey>,
}
impl Operation {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > 32768 {
            return None;
        }
        let op: Self = serde_json::from_slice(bytes).ok()?;
        let valid = op.abi_version == 1
            && match op.kind {
                Kind::Text => {
                    op.key.is_none()
                        && op.text.as_ref().is_some_and(|s| {
                            s.expose_text()
                                .is_some_and(|s| !s.is_empty() && s.len() <= 16384)
                        })
                }
                Kind::KeyPair => op.key.is_some() && op.text.is_none(),
            };
        valid.then_some(op)
    }
    pub fn text_bytes(&self) -> usize {
        self.text
            .as_ref()
            .and_then(SecretBuffer::expose_text)
            .map_or(0, str::len)
    }
}
#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub(crate) enum Installation {
    #[default]
    Unavailable,
    SystemImeV1,
}
impl Installation {
    pub fn installed(self) -> bool {
        self == Self::SystemImeV1
    }
    pub fn name(self) -> &'static str {
        if self.installed() {
            "system_ime_v1"
        } else {
            "unavailable"
        }
    }
}
// 与 Owner command 队列分开，锁内发布屏障并 try_send，保证并发 producer 的先后关系。
#[derive(Default)]
pub(crate) struct Ingress {
    pub revision: u64,
    pub active: Option<Ticket>,
    pub reason: &'static str,
    window: Option<(u64, u64)>,
    pub window_available: bool,
    high_water: u64,
}
impl Ingress {
    pub fn invalidate(&mut self, reason: &'static str) -> Result<(), i32> {
        self.active = None;
        self.reason = reason;
        self.revision = self.revision.checked_add(1).ok_or(crate::EXHAUSTED)?;
        Ok(())
    }
    // 仅 Owner 用 LocalDisplay 已接受的单调窗口发布；producer 不能创建历史项。
    pub fn publish_window(&mut self, identity: Option<(u64, u64)>, available: bool) {
        if let Some(identity) = identity {
            if self.window != Some(identity) {
                let _ = self.invalidate("window_changed");
                self.window = Some(identity);
                self.high_water = 0;
            }
        }
        let available = identity.is_some() && available;
        if self.window_available && !available {
            let _ = self.invalidate("source_unavailable");
        }
        self.window_available = available;
    }
    pub fn source(&mut self, source: &Source) -> Result<bool, i32> {
        if self.revision == u64::MAX {
            return Err(crate::EXHAUSTED);
        }
        match source.state {
            SourceState::Attached => {
                let t = source.ticket;
                if !self.window_available
                    || self.window != Some((t.window_stage, t.window_id))
                    || t.editor_serial <= self.high_water
                {
                    return Err(crate::INVALID_ARGUMENT);
                }
                self.high_water = t.editor_serial;
                self.invalidate("source_replaced")?;
                self.active = Some(t);
                Ok(true)
            }
            SourceState::Detached => {
                if self.active != Some(source.ticket) {
                    return Ok(false);
                }
                self.invalidate("source_detached")?;
                Ok(true)
            }
        }
    }
}
pub(crate) struct State {
    pub installation: Installation,
    pub revision: u64,
    pub ticket: Option<Ticket>,
    pub ack_ticket: Option<Ticket>,
    pub accepted: u64,
    pub dispatched: u64,
    pub reason: &'static str,
    pub pending: VecDeque<Operation>,
    pub pending_bytes: usize,
}
impl State {
    pub fn new(installation: Installation) -> Self {
        Self {
            installation,
            revision: 0,
            ticket: None,
            ack_ticket: None,
            accepted: 0,
            dispatched: 0,
            reason: "no_source",
            pending: VecDeque::new(),
            pending_bytes: 0,
        }
    }
    pub fn clear(&mut self, reason: &'static str) {
        self.ticket = None;
        self.pending.clear();
        self.pending_bytes = 0;
        self.reason = reason;
    }
    pub fn snapshot(&self, protocol_text: bool) -> serde_json::Value {
        serde_json::json!({"installed":self.installation.installed(),"backend":self.installation.name(),
            "state":if !self.installation.installed() { "unavailable" } else if self.ticket.is_some() { "attached" } else { "detached" },
            "reason":self.reason,"ticket":self.ticket.map(Ticket::snapshot),"ackTicket":self.ack_ticket.map(Ticket::snapshot),
            "lastAcceptedSerial":self.accepted,"lastDispatchedSerial":self.dispatched,
            "protocolTextInput":protocol_text,"ackScope":"session_command_channel_only"})
    }
}
