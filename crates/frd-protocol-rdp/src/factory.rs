use frd_protocol_api::{
    ConnectRequest, ProtocolDescriptor, ProtocolError, ProtocolExit, ProtocolFactory, ProtocolId,
    ProtocolRuntime, ProtocolSession,
};

use crate::config::RdpConnectionConfig;

const RDP_SESSION_NOT_IMPLEMENTED: &str = "rdp_session_not_implemented";

fn rdp_error(code: &'static str) -> ProtocolError {
    ProtocolError::adapter(ProtocolId::rdp(), code)
}

pub struct RdpProtocolFactory;

impl ProtocolFactory for RdpProtocolFactory {
    fn descriptor(&self) -> ProtocolDescriptor {
        ProtocolDescriptor::from(ProtocolId::rdp())
    }

    fn create(
        &self,
        request: ConnectRequest,
        runtime: ProtocolRuntime,
    ) -> Result<Box<dyn ProtocolSession>, ProtocolError> {
        let config = RdpConnectionConfig::try_from(request)?;
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
        let RdpConnectionConfig { request, username } = config;
        let _ = (request, username, runtime);
        ProtocolExit::Failed(rdp_error(RDP_SESSION_NOT_IMPLEMENTED))
    }
}

#[cfg(test)]
mod tests {
    use frd_protocol_api::{ProtocolFactory, ProtocolId};

    use crate::{ParsedUsername, RdpProtocolFactory};

    #[test]
    fn factory_exposes_stable_rdp_descriptor() {
        let descriptor = RdpProtocolFactory.descriptor();
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
