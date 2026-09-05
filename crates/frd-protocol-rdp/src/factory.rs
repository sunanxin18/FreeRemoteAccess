use frd_protocol_api::{
    ConnectRequest, ProtocolDescriptor, ProtocolError, ProtocolExit, ProtocolFactory, ProtocolId,
    ProtocolRuntime, ProtocolSession,
};

use crate::config::{RdpClientPlatformIdentity, RdpConnectionConfig};
use crate::runtime::run_protocol_session;

pub struct RdpProtocolFactory {
    client_platform: RdpClientPlatformIdentity,
}

impl RdpProtocolFactory {
    pub const fn new(client_platform: RdpClientPlatformIdentity) -> Self {
        Self { client_platform }
    }
}

impl ProtocolFactory for RdpProtocolFactory {
    fn descriptor(&self) -> ProtocolDescriptor {
        ProtocolDescriptor::from(ProtocolId::rdp())
    }

    fn create(
        &self,
        request: ConnectRequest,
        runtime: ProtocolRuntime,
    ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
        let config = RdpConnectionConfig::try_new(request, self.client_platform)?;
        Ok(Box::new(RdpProtocolSession { config, runtime }))
    }
}

pub struct RdpProtocolSession {
    config: RdpConnectionConfig,
    runtime: ProtocolRuntime,
}

impl ProtocolSession for RdpProtocolSession {
    fn run(self: Box<Self>) -> ProtocolExit {
        let Self { config, runtime } = *self;
        run_protocol_session(config, runtime)
    }
}

#[cfg(test)]
mod tests {
    use frd_protocol_api::{ProtocolFactory, ProtocolId};

    use crate::{ParsedUsername, RdpClientPlatformIdentity, RdpProtocolFactory};

    #[test]
    fn factory_exposes_stable_rdp_descriptor() {
        let descriptor = RdpProtocolFactory::new(RdpClientPlatformIdentity::Windows).descriptor();
        assert_eq!(descriptor.id, ProtocolId::rdp());
        assert_eq!(descriptor.default_port, 3389);
        assert!(descriptor.credential_requirements.username);
        assert!(descriptor.credential_requirements.password);
    }

    #[test]
    fn username_parser_accepts_local_domain_and_upn_forms() {
        assert_eq!(ParsedUsername::parse("alice").unwrap().account(), "alice");
        assert_eq!(
            ParsedUsername::parse("ACME\\alice").unwrap().domain(),
            Some("ACME")
        );
        assert_eq!(
            ParsedUsername::parse("alice@acme.test").unwrap().upn(),
            Some("alice@acme.test")
        );
    }
}
