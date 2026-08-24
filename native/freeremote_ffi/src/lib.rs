use freeremotedesk::app::connection::{
    validate_connection, ConnectionRequest, ProtocolKind, ServiceKind,
};
use std::ffi::{c_char, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};

pub const FRD_ABI_VERSION: u32 = 1;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrdStatus {
    Ok = 0,
    NullPointer = 1,
    InvalidUtf8 = 2,
    ServiceInvalid = 100,
    HostRequired = 101,
    PortInvalid = 102,
    UsernameRequired = 103,
    PasswordRequired = 104,
    DomainNotSupported = 105,
    Internal = 255,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrdValidationOutput {
    pub abi_version: u32,
    pub status: u32,
    pub protocol: u8,
    pub reserved: u8,
    pub port: u16,
}

impl Default for FrdValidationOutput {
    fn default() -> Self {
        Self {
            abi_version: FRD_ABI_VERSION,
            status: FrdStatus::Internal as u32,
            protocol: FfiProtocol::Auto as u8,
            reserved: 0,
            port: 0,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiService {
    Auto = 0,
    Windows = 1,
    MacOs = 2,
    LinuxVnc = 3,
}

impl TryFrom<u8> for FfiService {
    type Error = FfiValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Auto),
            1 => Ok(Self::Windows),
            2 => Ok(Self::MacOs),
            3 => Ok(Self::LinuxVnc),
            _ => Err(FfiValidationError::new("service", "service_invalid")),
        }
    }
}

impl From<FfiService> for ServiceKind {
    fn from(value: FfiService) -> Self {
        match value {
            FfiService::Auto => Self::Auto,
            FfiService::Windows => Self::WindowsRdp,
            FfiService::MacOs => Self::MacOsArd,
            FfiService::LinuxVnc => Self::LinuxVnc,
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiProtocol {
    Auto = 0,
    Rdp = 1,
    AppleRfb = 2,
    StandardRfb = 3,
}

impl From<ProtocolKind> for FfiProtocol {
    fn from(value: ProtocolKind) -> Self {
        match value {
            ProtocolKind::Auto => Self::Auto,
            ProtocolKind::Rdp => Self::Rdp,
            ProtocolKind::AppleRfb => Self::AppleRfb,
            ProtocolKind::StandardRfb => Self::StandardRfb,
        }
    }
}

#[derive(Debug)]
pub struct FfiConnectionRequest {
    pub service: u8,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiValidationResult {
    pub protocol: FfiProtocol,
    pub port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiValidationError {
    pub field: &'static str,
    pub code: &'static str,
}

impl FfiValidationError {
    fn new(field: &'static str, code: &'static str) -> Self {
        Self { field, code }
    }
}

pub fn validate_ffi_request(
    request: FfiConnectionRequest,
) -> Result<FfiValidationResult, FfiValidationError> {
    let service = FfiService::try_from(request.service)?;
    let validated = validate_connection(ConnectionRequest {
        service: service.into(),
        host: request.host,
        port: (request.port != 0).then_some(request.port),
        username: request.username,
        password: request.password,
        domain: request.domain,
    })
    .map_err(|error| FfiValidationError::new(error.field(), error.code()))?;

    Ok(FfiValidationResult {
        protocol: validated.protocol.into(),
        port: validated.endpoint.port(),
    })
}

fn status_for_validation(error: FfiValidationError) -> FrdStatus {
    match error.code {
        "service_invalid" => FrdStatus::ServiceInvalid,
        "host_required" => FrdStatus::HostRequired,
        "port_invalid" => FrdStatus::PortInvalid,
        "username_required" => FrdStatus::UsernameRequired,
        "password_required" => FrdStatus::PasswordRequired,
        "domain_not_supported" => FrdStatus::DomainNotSupported,
        _ => FrdStatus::Internal,
    }
}

unsafe fn read_required_utf8(pointer: *const c_char) -> Result<String, FrdStatus> {
    if pointer.is_null() {
        return Err(FrdStatus::NullPointer);
    }
    unsafe { CStr::from_ptr(pointer) }
        .to_str()
        .map(str::to_owned)
        .map_err(|_| FrdStatus::InvalidUtf8)
}

unsafe fn read_optional_utf8(pointer: *const c_char) -> Result<Option<String>, FrdStatus> {
    if pointer.is_null() {
        return Ok(None);
    }
    unsafe { read_required_utf8(pointer) }.map(Some)
}

#[no_mangle]
pub unsafe extern "C" fn frd_validate_connection(
    service: u8,
    host: *const c_char,
    port: u16,
    username: *const c_char,
    password: *const c_char,
    domain: *const c_char,
    output: *mut FrdValidationOutput,
) -> u32 {
    if output.is_null() {
        return FrdStatus::NullPointer as u32;
    }

    let output = unsafe { &mut *output };
    *output = FrdValidationOutput::default();
    let result = catch_unwind(AssertUnwindSafe(|| {
        let request = FfiConnectionRequest {
            service,
            host: unsafe { read_required_utf8(host) }?,
            port,
            username: unsafe { read_required_utf8(username) }?,
            password: unsafe { read_required_utf8(password) }?,
            domain: unsafe { read_optional_utf8(domain) }?,
        };
        validate_ffi_request(request).map_err(status_for_validation)
    }));

    let status = match result {
        Ok(Ok(validated)) => {
            output.protocol = validated.protocol as u8;
            output.port = validated.port;
            FrdStatus::Ok
        }
        Ok(Err(status)) => status,
        Err(_) => FrdStatus::Internal,
    };
    output.status = status as u32;
    status as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn request(service: u8) -> FfiConnectionRequest {
        FfiConnectionRequest {
            service,
            host: "mac.local".to_owned(),
            port: 0,
            username: "sun".to_owned(),
            password: "secret-value".to_owned(),
            domain: None,
        }
    }

    #[test]
    fn maps_mac_os_to_apple_rfb_without_returning_password() {
        let result = validate_ffi_request(request(FfiService::MacOs as u8)).unwrap();

        assert_eq!(result.protocol, FfiProtocol::AppleRfb);
        assert_eq!(result.port, 5900);
        assert!(!format!("{result:?}").contains("secret-value"));
    }

    #[test]
    fn rejects_unknown_service_code_before_core_validation() {
        let error = validate_ffi_request(request(255)).unwrap_err();

        assert_eq!(error.field, "service");
        assert_eq!(error.code, "service_invalid");
    }

    #[test]
    fn maps_zero_port_to_core_default_and_explicit_port_verbatim() {
        let defaulted = validate_ffi_request(request(FfiService::Windows as u8)).unwrap();
        let mut explicit_request = request(FfiService::LinuxVnc as u8);
        explicit_request.port = 5999;
        let explicit = validate_ffi_request(explicit_request).unwrap();

        assert_eq!(defaulted.port, 3389);
        assert_eq!(explicit.port, 5999);
    }

    #[test]
    fn c_abi_returns_numeric_result_without_secret_or_owned_pointer() {
        let host = CString::new("mac.local").unwrap();
        let username = CString::new("sun").unwrap();
        let password = CString::new("secret-value").unwrap();
        let mut output = FrdValidationOutput::default();

        let status = unsafe {
            frd_validate_connection(
                FfiService::MacOs as u8,
                host.as_ptr(),
                0,
                username.as_ptr(),
                password.as_ptr(),
                std::ptr::null(),
                &mut output,
            )
        };

        assert_eq!(status, FrdStatus::Ok as u32);
        assert_eq!(output.abi_version, FRD_ABI_VERSION);
        assert_eq!(output.protocol, FfiProtocol::AppleRfb as u8);
        assert_eq!(output.port, 5900);
        assert_eq!(size_of::<FrdValidationOutput>(), 12);
    }

    #[test]
    fn c_abi_rejects_null_required_pointer_without_unwinding() {
        let mut output = FrdValidationOutput::default();

        let status = unsafe {
            frd_validate_connection(
                FfiService::MacOs as u8,
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                &mut output,
            )
        };

        assert_eq!(status, FrdStatus::NullPointer as u32);
        assert_eq!(output.status, FrdStatus::NullPointer as u32);
    }
}
