use crate::app::connection::{
    validate_connection, ConnectionRequest, ConnectionValidationError, ServiceKind,
    ValidatedConnection,
};
use secrecy::SecretString;

use super::SecretBuffer;

#[derive(Debug)]
pub struct ConnectionFormState {
    service: ServiceKind,
    host: String,
    port: String,
    username: String,
    password: SecretBuffer,
    domain: String,
}

impl Default for ConnectionFormState {
    fn default() -> Self {
        Self {
            service: ServiceKind::Auto,
            host: String::new(),
            port: String::new(),
            username: String::new(),
            password: SecretBuffer::default(),
            domain: String::new(),
        }
    }
}

impl ConnectionFormState {
    pub fn fixture() -> Self {
        Self {
            service: ServiceKind::MacOsArd,
            host: "mac.local".to_owned(),
            username: "sun".to_owned(),
            ..Self::default()
        }
    }

    pub fn with_password(password: &str) -> Self {
        Self {
            password: SecretBuffer::new(password),
            ..Self::default()
        }
    }

    pub const fn service(&self) -> ServiceKind {
        self.service
    }

    pub fn set_service(&mut self, service: ServiceKind) {
        self.service = service;
        if !self.domain_visible() {
            self.domain.clear();
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut String {
        &mut self.host
    }

    pub fn port(&self) -> &str {
        &self.port
    }

    pub fn port_mut(&mut self) -> &mut String {
        &mut self.port
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn username_mut(&mut self) -> &mut String {
        &mut self.username
    }

    pub fn password(&self) -> &str {
        self.password.expose()
    }

    pub fn password_mut(&mut self) -> &mut SecretBuffer {
        &mut self.password
    }

    pub fn domain(&self) -> &str {
        &self.domain
    }

    pub fn domain_mut(&mut self) -> &mut String {
        &mut self.domain
    }

    pub fn domain_visible(&self) -> bool {
        self.service == ServiceKind::WindowsRdp
    }

    pub fn finish_submission(&mut self, _outcome: SubmissionOutcome) {
        self.password.clear();
    }

    fn validate(&mut self) -> Result<ValidatedConnection, ConnectionValidationError> {
        let password = SecretString::from(self.password.expose().to_owned());
        self.password.clear();
        let port = if self.port.trim().is_empty() {
            None
        } else {
            match self.port.trim().parse::<u16>() {
                Ok(port) => Some(port),
                Err(_) => {
                    return validate_connection(ConnectionRequest {
                        service: self.service,
                        host: self.host.clone(),
                        port: Some(0),
                        username: self.username.clone(),
                        password,
                        domain: self.domain_value(),
                    });
                }
            }
        };
        validate_connection(ConnectionRequest {
            service: self.service,
            host: self.host.clone(),
            port,
            username: self.username.clone(),
            password,
            domain: self.domain_value(),
        })
    }

    fn domain_value(&self) -> Option<String> {
        self.domain_visible().then(|| self.domain.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionOutcome {
    Accepted,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiPage {
    Connection,
    Connecting,
    Session,
}

pub enum UiAction {
    Connect(ValidatedConnection),
    Disconnect,
    None,
}

impl std::fmt::Debug for UiAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(connection) => {
                formatter.debug_tuple("Connect").field(connection).finish()
            }
            Self::Disconnect => formatter.write_str("Disconnect"),
            Self::None => formatter.write_str("None"),
        }
    }
}

#[derive(Debug)]
pub struct FreeRemoteApplication {
    page: UiPage,
    connection_form: ConnectionFormState,
}

impl Default for FreeRemoteApplication {
    fn default() -> Self {
        Self {
            page: UiPage::Connection,
            connection_form: ConnectionFormState::default(),
        }
    }
}

impl FreeRemoteApplication {
    pub fn fixture() -> Self {
        Self {
            page: UiPage::Connection,
            connection_form: ConnectionFormState::fixture(),
        }
    }

    pub const fn page(&self) -> UiPage {
        self.page
    }

    pub fn connection_form(&self) -> &ConnectionFormState {
        &self.connection_form
    }

    pub fn connection_form_mut(&mut self) -> &mut ConnectionFormState {
        &mut self.connection_form
    }

    pub fn submit_connection(&mut self) -> Result<UiAction, ConnectionValidationError> {
        match self.connection_form.validate() {
            Ok(connection) => {
                self.page = UiPage::Connecting;
                Ok(UiAction::Connect(connection))
            }
            Err(error) => {
                self.page = UiPage::Connection;
                Err(error)
            }
        }
    }

    pub fn show_session(&mut self) {
        self.page = UiPage::Session;
    }

    pub fn show_connection(&mut self) {
        self.page = UiPage::Connection;
    }
}
