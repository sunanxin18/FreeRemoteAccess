use core::fmt;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CredentialProviderId(String);

impl CredentialProviderId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn environment() -> Self {
        Self("environment".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProtocolId(String);

impl ProtocolId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn apple_hpss_mvs() -> Self {
        Self("apple-hpss-mvs".to_owned())
    }

    pub fn apple_high_performance() -> Self {
        Self("apple-high-performance".to_owned())
    }

    pub fn rdp() -> Self {
        Self("rdp".to_owned())
    }

    pub fn rfb() -> Self {
        Self("rfb".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetSystem {
    MacOs,
    Windows,
    Linux,
    Custom,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoint {
    host: String,
    port: u16,
}

impl Endpoint {
    pub fn new(host: impl Into<String>, port: u16) -> Option<Self> {
        let host = host.into();
        (!host.trim().is_empty() && port != 0).then_some(Self { host, port })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.host, self.port)
    }
}
