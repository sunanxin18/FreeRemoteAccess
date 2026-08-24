use std::error::Error;
use std::fmt;

use secrecy::{ExposeSecret, SecretString};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    Auto,
    WindowsRdp,
    MacOsArd,
    LinuxVnc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolKind {
    Auto,
    Rdp,
    AppleRfb,
    StandardRfb,
}

#[derive(Debug)]
pub struct ConnectionRequest {
    pub service: ServiceKind,
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    pub password: SecretString,
    pub domain: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    host: String,
    port: u16,
    port_was_explicit: bool,
}

impl Endpoint {
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub const fn port_was_explicit(&self) -> bool {
        self.port_was_explicit
    }

    pub(crate) const fn explicit_port(&self) -> Option<u16> {
        if self.port_was_explicit {
            Some(self.port)
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub struct ValidatedConnection {
    pub protocol: ProtocolKind,
    pub endpoint: Endpoint,
    pub username: String,
    pub password: SecretString,
    pub domain: Option<String>,
}

impl ValidatedConnection {
    pub(crate) fn select_auto_protocol(
        mut self,
        protocol: ProtocolKind,
        port: u16,
    ) -> Option<Self> {
        if self.protocol != ProtocolKind::Auto
            || !matches!(protocol, ProtocolKind::Rdp | ProtocolKind::AppleRfb)
            || port == 0
        {
            return None;
        }
        self.protocol = protocol;
        self.endpoint.port = port;
        Some(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionValidationError {
    field: &'static str,
    code: &'static str,
}

impl ConnectionValidationError {
    fn new(field: &'static str, code: &'static str) -> Self {
        Self { field, code }
    }

    pub fn field(&self) -> &'static str {
        self.field
    }

    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for ConnectionValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "连接字段验证失败：{} ({})",
            self.field, self.code
        )
    }
}

impl Error for ConnectionValidationError {}

pub fn validate_connection(
    request: ConnectionRequest,
) -> Result<ValidatedConnection, ConnectionValidationError> {
    let host = request.host.trim();
    if host.is_empty() {
        return Err(ConnectionValidationError::new("host", "host_required"));
    }
    if request.port == Some(0) {
        return Err(ConnectionValidationError::new("port", "port_invalid"));
    }

    let username = request.username.trim();
    if username.is_empty() {
        return Err(ConnectionValidationError::new(
            "username",
            "username_required",
        ));
    }
    if request.password.expose_secret().is_empty() {
        return Err(ConnectionValidationError::new(
            "password",
            "password_required",
        ));
    }

    let domain = request
        .domain
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if !matches!(request.service, ServiceKind::WindowsRdp) && domain.is_some() {
        return Err(ConnectionValidationError::new(
            "domain",
            "domain_not_supported",
        ));
    }

    let port_was_explicit = request.port.is_some();
    let port = request.port.unwrap_or(match request.service {
        ServiceKind::WindowsRdp => 3389,
        ServiceKind::Auto | ServiceKind::MacOsArd | ServiceKind::LinuxVnc => 5900,
    });
    let protocol = match request.service {
        ServiceKind::WindowsRdp => ProtocolKind::Rdp,
        ServiceKind::MacOsArd => ProtocolKind::AppleRfb,
        ServiceKind::LinuxVnc => ProtocolKind::StandardRfb,
        ServiceKind::Auto => ProtocolKind::Auto,
    };

    Ok(ValidatedConnection {
        protocol,
        endpoint: Endpoint {
            host: host.to_owned(),
            port,
            port_was_explicit,
        },
        username: username.to_owned(),
        password: request.password,
        domain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(service: ServiceKind, host: &str, port: Option<u16>) -> ConnectionRequest {
        ConnectionRequest {
            service,
            host: host.to_owned(),
            port,
            username: "sun".to_owned(),
            password: SecretString::from("secret-value".to_owned()),
            domain: None,
        }
    }

    #[test]
    fn mac_os_defaults_to_rfb_port_without_exposing_password() {
        let validated =
            validate_connection(request(ServiceKind::MacOsArd, "mac.local", None)).unwrap();

        assert_eq!(validated.endpoint.port(), 5900);
        assert_eq!(validated.protocol, ProtocolKind::AppleRfb);
        assert!(!format!("{validated:?}").contains("secret-value"));
    }

    #[test]
    fn windows_defaults_to_rdp_port_and_preserves_domain() {
        let mut value = request(ServiceKind::WindowsRdp, "windows.local", None);
        value.domain = Some("WORKGROUP".to_owned());

        let validated = validate_connection(value).unwrap();

        assert_eq!(validated.endpoint.port(), 3389);
        assert_eq!(validated.protocol, ProtocolKind::Rdp);
        assert_eq!(validated.domain.as_deref(), Some("WORKGROUP"));
    }

    #[test]
    fn windows_domain_is_rejected_for_vnc() {
        let mut value = request(ServiceKind::LinuxVnc, "linux.local", Some(5900));
        value.domain = Some("WORKGROUP".to_owned());

        assert_eq!(
            validate_connection(value).unwrap_err().code(),
            "domain_not_supported"
        );
    }

    #[test]
    fn empty_required_fields_are_rejected_before_network_selection() {
        for (field, code, value) in [
            (
                "host",
                "host_required",
                request(ServiceKind::MacOsArd, "  ", None),
            ),
            (
                "username",
                "username_required",
                ConnectionRequest {
                    username: " ".to_owned(),
                    ..request(ServiceKind::MacOsArd, "mac.local", None)
                },
            ),
            (
                "password",
                "password_required",
                ConnectionRequest {
                    password: SecretString::from(String::new()),
                    ..request(ServiceKind::MacOsArd, "mac.local", None)
                },
            ),
        ] {
            let error = validate_connection(value).unwrap_err();
            assert_eq!(error.field(), field);
            assert_eq!(error.code(), code);
        }
    }

    #[test]
    fn explicit_zero_port_is_rejected() {
        let error =
            validate_connection(request(ServiceKind::MacOsArd, "mac.local", Some(0))).unwrap_err();

        assert_eq!(error.field(), "port");
        assert_eq!(error.code(), "port_invalid");
    }

    #[test]
    fn auto_preserves_detection_until_the_protocol_worker() {
        let rdp = validate_connection(request(ServiceKind::Auto, "host", Some(3389))).unwrap();
        let rfb = validate_connection(request(ServiceKind::Auto, "host", Some(5900))).unwrap();
        let no_port = validate_connection(request(ServiceKind::Auto, "host", None)).unwrap();

        assert_eq!(rdp.protocol, ProtocolKind::Auto);
        assert_eq!(rfb.protocol, ProtocolKind::Auto);
        assert_eq!(no_port.protocol, ProtocolKind::Auto);
        assert!(rdp.endpoint.port_was_explicit());
        assert!(rfb.endpoint.port_was_explicit());
        assert!(!no_port.endpoint.port_was_explicit());
    }

    #[test]
    fn explicit_services_bypass_auto_and_preserve_the_validated_port() {
        for (service, port, protocol) in [
            (ServiceKind::WindowsRdp, 13389, ProtocolKind::Rdp),
            (ServiceKind::MacOsArd, 15900, ProtocolKind::AppleRfb),
            (ServiceKind::LinuxVnc, 15901, ProtocolKind::StandardRfb),
        ] {
            let connection = validate_connection(request(service, "host", Some(port))).unwrap();
            assert_eq!(connection.protocol, protocol);
            assert_eq!(connection.endpoint.port(), port);
            assert!(connection.endpoint.port_was_explicit());
        }
    }
}
