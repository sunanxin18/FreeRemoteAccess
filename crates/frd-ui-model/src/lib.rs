//! 单窗口 UI 可展示状态与低频连接提交 DTO。

use frd_core::{CredentialProviderId, SecretBuffer, TargetSystem};
use frd_protocol_api::{
    ConnectionStage, ProtocolCatalog, ProtocolId, ProtocolSelection, SessionCapabilities,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LaunchOptions {
    pub target_system: Option<TargetSystem>,
    pub address: Option<String>,
    pub port: Option<u16>,
    pub protocol: Option<ProtocolId>,
    pub username_provider: Option<CredentialProviderId>,
    pub password_provider: Option<CredentialProviderId>,
    pub connect_when_complete: bool,
}

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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConnectionFormErrors {
    pub target_system: Option<String>,
    pub address: Option<String>,
    pub port: Option<String>,
    pub protocol: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl ConnectionFormErrors {
    pub fn is_empty(&self) -> bool {
        self.target_system.is_none()
            && self.address.is_none()
            && self.port.is_none()
            && self.protocol.is_none()
            && self.username.is_none()
            && self.password.is_none()
    }
}

/// 可编辑的连接页状态。密码与可复制草稿分离，且本结构不能 Clone 或 Debug。
pub struct ConnectionForm {
    pub draft: ConnectionDraft,
    password: SecretBuffer,
    errors: ConnectionFormErrors,
}

impl ConnectionForm {
    pub fn new(draft: ConnectionDraft) -> Self {
        Self {
            draft,
            password: SecretBuffer::new(Vec::new()),
            errors: ConnectionFormErrors::default(),
        }
    }

    pub fn set_password(&mut self, password: SecretBuffer) {
        self.password = password;
    }

    pub fn password_mut(&mut self) -> &mut SecretBuffer {
        &mut self.password
    }

    pub fn password_is_empty(&self) -> bool {
        self.password.is_empty()
    }

    pub fn errors(&self) -> &ConnectionFormErrors {
        &self.errors
    }

    pub fn set_username_error(&mut self, code: &'static str) {
        self.errors.username = Some(code.to_owned());
    }

    pub fn set_password_error(&mut self, code: &'static str) {
        self.errors.password = Some(code.to_owned());
    }

    pub fn set_validation_error(&mut self, code: &'static str) {
        match code {
            "target_system_required" => self.errors.target_system = Some(code.to_owned()),
            "address_required" => self.errors.address = Some(code.to_owned()),
            "port_required" => self.errors.port = Some(code.to_owned()),
            "username_required" => self.errors.username = Some(code.to_owned()),
            "password_required" => self.errors.password = Some(code.to_owned()),
            _ => self.errors.protocol = Some(code.to_owned()),
        }
    }

    pub fn validate(&mut self, catalog: &ProtocolCatalog) -> bool {
        self.validate_and_resolve(catalog).is_some()
    }

    pub fn take_submission(&mut self, catalog: &ProtocolCatalog) -> Option<ConnectionSubmission> {
        let resolved_protocol = self.validate_and_resolve(catalog)?;
        let password = std::mem::replace(&mut self.password, SecretBuffer::new(Vec::new()));
        Some(ConnectionSubmission {
            draft: self.draft.clone(),
            resolved_protocol,
            password,
        })
    }

    pub fn take_connect_intent(
        &mut self,
        catalog: &ProtocolCatalog,
    ) -> Option<ConnectionSubmission> {
        self.take_submission(catalog)
    }

    fn validate_and_resolve(&mut self, catalog: &ProtocolCatalog) -> Option<ProtocolId> {
        self.errors = ConnectionFormErrors::default();

        let target_system = match self.draft.target_system {
            Some(target_system) => target_system,
            None => {
                self.errors.target_system = Some("target_system_required".to_owned());
                TargetSystem::Custom
            }
        };
        if self.draft.address.trim().is_empty() {
            self.errors.address = Some("address_required".to_owned());
        }
        if self.draft.port.is_none() || self.draft.port == Some(0) {
            self.errors.port = Some("port_required".to_owned());
        }
        if self.draft.username.trim().is_empty() {
            self.errors.username = Some("username_required".to_owned());
        }
        if self.password.is_empty() {
            self.errors.password = Some("password_required".to_owned());
        }

        let resolved_protocol = catalog
            .select(target_system, self.draft.protocol.clone().into())
            .map_err(|error| {
                self.errors.protocol = Some(error.code().to_owned());
            })
            .ok();

        if self.errors.is_empty() {
            resolved_protocol
        } else {
            None
        }
    }
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
    pub resolved_protocol: ProtocolId,
    pub password: SecretBuffer,
}

pub enum Page {
    ConnectionForm(ConnectionForm),
    Connecting {
        draft: ConnectionDraft,
        stage: ConnectionStage,
        diagnostics: Option<String>,
    },
    AwaitingFirstFrame {
        draft: ConnectionDraft,
        stage: ConnectionStage,
        diagnostics: Option<String>,
    },
    Disconnecting {
        draft: ConnectionDraft,
    },
    RemoteSession {
        draft: ConnectionDraft,
        capabilities: SessionCapabilities,
    },
    Failed {
        draft: ConnectionDraft,
        code: String,
    },
}

impl Page {
    pub fn connection_form(draft: ConnectionDraft) -> Self {
        Self::ConnectionForm(ConnectionForm::new(draft))
    }

    pub fn retained_draft(&self) -> ConnectionDraft {
        match self {
            Self::ConnectionForm(form) => form.draft.clone(),
            Self::Connecting { draft, .. }
            | Self::AwaitingFirstFrame { draft, .. }
            | Self::Disconnecting { draft }
            | Self::RemoteSession { draft, .. }
            | Self::Failed { draft, .. } => draft.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use frd_core::{SecretBuffer, TargetSystem};
    use frd_protocol_api::{ProtocolCatalog, ProtocolId};

    use super::{ConnectionDraft, ConnectionForm, Page};

    #[test]
    fn connection_draft_starts_on_the_connection_form() {
        assert!(matches!(
            Page::connection_form(ConnectionDraft::default()),
            Page::ConnectionForm(_)
        ));
    }

    #[test]
    fn submitting_the_connection_form_moves_the_password_out_of_editable_state() {
        let mut form = ConnectionForm::new(ConnectionDraft {
            target_system: Some(TargetSystem::MacOs),
            address: "host.invalid".to_owned(),
            port: Some(5900),
            protocol: super::ProtocolChoice::Automatic,
            username: "test-user".to_owned(),
        });
        form.set_password(SecretBuffer::new(b"test-password".to_vec()));
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);

        let submission = form
            .take_submission(&catalog)
            .expect("complete form is submitted");

        assert!(form.password_is_empty());
        assert!(!submission.password.is_empty());
    }

    #[test]
    fn automatic_protocol_for_mac_is_resolved_before_submission() {
        let mut form = ConnectionForm::new(ConnectionDraft {
            target_system: Some(TargetSystem::MacOs),
            address: "host.invalid".to_owned(),
            port: Some(5900),
            protocol: super::ProtocolChoice::Automatic,
            username: "test-user".to_owned(),
        });
        form.set_password(SecretBuffer::new(b"test-password".to_vec()));
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);

        let submission = form
            .take_submission(&catalog)
            .expect("registered Mac automatic protocol is accepted");

        assert_eq!(submission.resolved_protocol, ProtocolId::apple_hpss_mvs());
    }
}
