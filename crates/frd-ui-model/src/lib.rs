//! 单窗口 UI 可展示状态与低频连接提交 DTO。

mod chrome;

pub use chrome::{
    CapabilityGlyphState, ConnectionGlyph, SessionChromeAction, SessionChromeModel, SessionTiming,
    SessionTimingSource,
};

use frd_core::{CredentialProviderId, SecretBuffer, TargetSystem};
use frd_platform_api::{ConnectionProfileKey, SavedConnectionProfile};
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
    pub profile: Option<String>,
    pub target_system: Option<String>,
    pub address: Option<String>,
    pub port: Option<String>,
    pub protocol: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl ConnectionFormErrors {
    pub fn is_empty(&self) -> bool {
        self.profile.is_none()
            && self.target_system.is_none()
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
    pub profiles: Vec<SavedConnectionProfile>,
    pub selected_profile: Option<ConnectionProfileKey>,
    pub remember_on_this_device: bool,
    pub password_visible: bool,
    password: SecretBuffer,
    errors: Box<ConnectionFormErrors>,
}

impl ConnectionForm {
    pub fn new(draft: ConnectionDraft) -> Self {
        Self {
            draft,
            profiles: Vec::new(),
            selected_profile: None,
            remember_on_this_device: false,
            password_visible: false,
            password: SecretBuffer::new(Vec::new()),
            errors: Box::default(),
        }
    }

    pub fn set_password(&mut self, password: SecretBuffer) {
        self.password = password;
    }

    pub fn set_profiles(&mut self, profiles: Vec<SavedConnectionProfile>) {
        self.profiles = profiles;
    }

    pub fn select_profile_metadata(&mut self, profile: &SavedConnectionProfile) {
        self.draft = ConnectionDraft {
            target_system: Some(profile.target_system),
            address: profile.key.address().to_owned(),
            port: Some(profile.key.port()),
            protocol: ProtocolChoice::Explicit(profile.key.protocol().clone()),
            username: profile.key.username().to_owned(),
        };
        self.password = SecretBuffer::new(Vec::new());
        self.selected_profile = Some(profile.key.clone());
        self.remember_on_this_device = true;
        self.password_visible = false;
        self.errors.password = None;
        self.errors.profile = None;
    }

    pub fn set_loaded_password(&mut self, password: SecretBuffer) {
        self.password = password;
        self.errors.password = None;
    }

    pub fn invalidate_loaded_secret_after_identity_edit(
        &mut self,
        original_identity: &ConnectionDraft,
    ) -> bool {
        if &self.draft == original_identity {
            return false;
        }
        self.selected_profile = None;
        self.password = SecretBuffer::new(Vec::new());
        self.password_visible = false;
        self.errors.password = None;
        true
    }

    pub fn set_profile_storage_error(&mut self, code: &'static str) {
        self.errors.profile = Some(code.to_owned());
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
            "profile_storage_unavailable" | "profile_storage_failed" | "invalid_profile" => {
                self.errors.profile = Some(code.to_owned())
            }
            "credential_storage_failed" | "saved_credential_unavailable" => {
                self.errors.password = Some(code.to_owned())
            }
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
            remember_on_this_device: self.remember_on_this_device,
            selected_profile: self.selected_profile.clone(),
        })
    }

    pub fn take_connect_intent(
        &mut self,
        catalog: &ProtocolCatalog,
    ) -> Option<ConnectionSubmission> {
        self.take_submission(catalog)
    }

    fn validate_and_resolve(&mut self, catalog: &ProtocolCatalog) -> Option<ProtocolId> {
        *self.errors = ConnectionFormErrors::default();

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
    pub remember_on_this_device: bool,
    pub selected_profile: Option<ConnectionProfileKey>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfilePersistenceWarning {
    SaveFailed,
    CredentialDeleteFailed,
    MetadataDeleteFailed,
}

impl ProfilePersistenceWarning {
    pub const fn message(self) -> &'static str {
        match self {
            Self::SaveFailed => "登录信息未能安全保存；本次连接仍可继续，请稍后重试。",
            Self::CredentialDeleteFailed => {
                "无法删除保存在此设备上的登录信息；登录信息仍保留，请稍后重试。"
            }
            Self::MetadataDeleteFailed => {
                "密码已从系统凭据库删除，但最近连接记录清理失败；请稍后重试。"
            }
        }
    }
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
        diagnostics: Option<String>,
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
    use frd_platform_api::{ConnectionProfileKey, SavedConnectionProfile};
    use frd_protocol_api::{ProtocolCatalog, ProtocolId};

    use super::{ConnectionDraft, ConnectionForm, Page, ProfilePersistenceWarning};

    #[test]
    fn profile_persistence_warnings_have_action_specific_safe_chinese_messages() {
        let cases = [
            (
                ProfilePersistenceWarning::SaveFailed,
                "登录信息未能安全保存；本次连接仍可继续，请稍后重试。",
            ),
            (
                ProfilePersistenceWarning::CredentialDeleteFailed,
                "无法删除保存在此设备上的登录信息；登录信息仍保留，请稍后重试。",
            ),
            (
                ProfilePersistenceWarning::MetadataDeleteFailed,
                "密码已从系统凭据库删除，但最近连接记录清理失败；请稍后重试。",
            ),
        ];

        for (warning, expected) in cases {
            assert_eq!(warning.message(), expected);
            assert!(!warning.message().contains("profile_persistence"));
        }
    }

    fn saved_profile() -> SavedConnectionProfile {
        SavedConnectionProfile {
            key: ConnectionProfileKey::new(
                ProtocolId::apple_hpss_mvs(),
                "remembered.invalid",
                5901,
                "remembered-user",
            )
            .expect("test profile key is valid"),
            target_system: TargetSystem::MacOs,
            last_success_order: 7,
        }
    }

    #[test]
    fn selecting_saved_profile_replaces_connection_draft() {
        let profile = saved_profile();
        let mut form = ConnectionForm::new(ConnectionDraft {
            target_system: Some(TargetSystem::Custom),
            address: "draft.invalid".to_owned(),
            port: Some(3389),
            protocol: super::ProtocolChoice::Automatic,
            username: "draft-user".to_owned(),
        });
        form.set_password(SecretBuffer::new(b"stale-password".to_vec()));
        form.password_visible = true;

        form.select_profile_metadata(&profile);

        assert_eq!(form.draft.target_system, Some(TargetSystem::MacOs));
        assert_eq!(form.draft.address, "remembered.invalid");
        assert_eq!(form.draft.port, Some(5901));
        assert_eq!(
            form.draft.protocol,
            super::ProtocolChoice::Explicit(ProtocolId::apple_hpss_mvs())
        );
        assert_eq!(form.draft.username, "remembered-user");
        assert_eq!(form.selected_profile.as_ref(), Some(&profile.key));
        assert!(form.password_is_empty());
        assert!(!form.password_visible);
    }

    #[test]
    fn connection_submission_carries_remember_choice_and_selected_profile() {
        let profile = saved_profile();
        let mut form = ConnectionForm::new(ConnectionDraft::default());
        form.select_profile_metadata(&profile);
        form.set_loaded_password(SecretBuffer::new(b"loaded-password".to_vec()));
        form.remember_on_this_device = true;
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);

        let submission = form
            .take_submission(&catalog)
            .expect("selected saved profile is complete");

        assert!(submission.remember_on_this_device);
        assert_eq!(submission.selected_profile, Some(profile.key));
        assert!(!submission.password.is_empty());
    }

    #[test]
    fn identity_edit_invalidates_loaded_profile_secret_before_submission() {
        let profile = saved_profile();
        let mut form = ConnectionForm::new(ConnectionDraft::default());
        form.select_profile_metadata(&profile);
        form.set_loaded_password(SecretBuffer::new(b"vault-password".to_vec()));
        form.password_visible = true;
        let original_identity = form.draft.clone();

        form.draft.address = "edited.invalid".to_owned();
        assert!(form.invalidate_loaded_secret_after_identity_edit(&original_identity));

        assert!(form.selected_profile.is_none());
        assert!(form.password_is_empty());
        assert!(!form.password_visible);
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]);
        assert!(form.take_submission(&catalog).is_none());
        assert_eq!(form.errors().password.as_deref(), Some("password_required"));

        form.set_password(SecretBuffer::new(b"new-password".to_vec()));
        let submission = form
            .take_submission(&catalog)
            .expect("a newly entered password may submit the edited identity");
        assert_eq!(submission.draft.address, "edited.invalid");
        assert!(submission.selected_profile.is_none());
        assert_eq!(submission.password.expose_text(), Some("new-password"));
    }

    #[test]
    fn every_identity_field_edit_clears_profile_association_and_secret() {
        let profile = saved_profile();
        let base = ConnectionDraft {
            target_system: Some(profile.target_system),
            address: profile.key.address().to_owned(),
            port: Some(profile.key.port()),
            protocol: super::ProtocolChoice::Explicit(profile.key.protocol().clone()),
            username: profile.key.username().to_owned(),
        };
        let mut edits = Vec::new();
        let mut target = base.clone();
        target.target_system = Some(TargetSystem::Custom);
        edits.push(target);
        let mut protocol = base.clone();
        protocol.protocol = super::ProtocolChoice::Automatic;
        edits.push(protocol);
        let mut address = base.clone();
        address.address = "edited.invalid".to_owned();
        edits.push(address);
        let mut port = base.clone();
        port.port = Some(5902);
        edits.push(port);
        let mut username = base;
        username.username = "edited-user".to_owned();
        edits.push(username);

        for edited_identity in edits {
            let mut form = ConnectionForm::new(ConnectionDraft::default());
            form.select_profile_metadata(&profile);
            form.set_loaded_password(SecretBuffer::new(b"vault-password".to_vec()));
            form.password_visible = true;
            let original_identity = form.draft.clone();
            form.draft = edited_identity;

            assert!(form.invalidate_loaded_secret_after_identity_edit(&original_identity));
            assert!(form.selected_profile.is_none());
            assert!(form.password_is_empty());
            assert!(!form.password_visible);
        }
    }

    #[test]
    fn connection_draft_starts_on_the_connection_form() {
        assert!(matches!(
            Page::connection_form(ConnectionDraft::default()),
            Page::ConnectionForm(_)
        ));
    }

    #[test]
    fn page_keeps_connection_form_state_out_of_line() {
        assert!(std::mem::size_of::<Page>() <= 256);
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
    fn automatic_protocol_for_mac_remains_apple_with_rdp_registered() {
        let mut form = ConnectionForm::new(ConnectionDraft {
            target_system: Some(TargetSystem::MacOs),
            address: "host.invalid".to_owned(),
            port: Some(5900),
            protocol: super::ProtocolChoice::Automatic,
            username: "test-user".to_owned(),
        });
        form.set_password(SecretBuffer::new(b"test-password".to_vec()));
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs(), ProtocolId::rdp()]);

        let submission = form
            .take_submission(&catalog)
            .expect("registered Mac automatic protocol is accepted");

        assert_eq!(submission.resolved_protocol, ProtocolId::apple_hpss_mvs());
    }

    #[test]
    fn automatic_protocol_for_windows_is_resolved_before_submission() {
        let mut form = ConnectionForm::new(ConnectionDraft {
            target_system: Some(TargetSystem::Windows),
            address: "host.invalid".to_owned(),
            port: Some(3389),
            protocol: super::ProtocolChoice::Automatic,
            username: "test-user".to_owned(),
        });
        form.set_password(SecretBuffer::new(b"test-password".to_vec()));
        let catalog = ProtocolCatalog::new([ProtocolId::apple_hpss_mvs(), ProtocolId::rdp()]);

        let submission = form
            .take_submission(&catalog)
            .expect("registered Windows automatic protocol is accepted");

        assert_eq!(submission.resolved_protocol, ProtocolId::rdp());
    }
}
