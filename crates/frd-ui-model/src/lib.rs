//! 单窗口 UI 可展示状态与低频连接提交 DTO。

use frd_core::{SecretBuffer, TargetSystem};
use frd_protocol_api::{ProtocolId, ProtocolSelection, SessionCapabilities};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionDraft {
    pub target_system: Option<TargetSystem>,
    pub address: String,
    pub port: Option<u16>,
    pub protocol: ProtocolChoice,
    pub username: String,
}

impl Default for ConnectionDraft {
    fn default() -> Self {
        Self {
            target_system: None,
            address: String::new(),
            port: None,
            protocol: ProtocolChoice::Automatic,
            username: String::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolChoice {
    Automatic,
    Explicit(ProtocolId),
}

impl From<ProtocolChoice> for ProtocolSelection {
    fn from(choice: ProtocolChoice) -> Self {
        match choice {
            ProtocolChoice::Automatic => Self::Automatic,
            ProtocolChoice::Explicit(protocol_id) => Self::Explicit(protocol_id),
        }
    }
}

/// 秘密缓冲独占所有权，不能 Clone 或 Debug。
pub struct ConnectionSubmission {
    pub draft: ConnectionDraft,
    pub password: SecretBuffer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Page {
    ConnectionForm(ConnectionDraft),
    Connecting,
    AwaitingFirstFrame,
    RemoteSession {
        capabilities: SessionCapabilities,
    },
    Failed {
        draft: ConnectionDraft,
        code: String,
    },
}

impl Page {
    pub fn connection_form(draft: ConnectionDraft) -> Self {
        Self::ConnectionForm(draft)
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectionDraft, Page};

    #[test]
    fn connection_draft_starts_on_the_connection_form() {
        assert!(matches!(
            Page::connection_form(ConnectionDraft::default()),
            Page::ConnectionForm(_)
        ));
    }
}
