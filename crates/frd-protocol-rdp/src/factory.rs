use std::sync::Arc;

use frd_protocol_api::{
    ConnectRequest, ProtocolDescriptor, ProtocolError, ProtocolExit, ProtocolFactory, ProtocolId,
    ProtocolRuntime, ProtocolSession,
};

use crate::config::{RdpClientPlatformIdentity, RdpConnectionConfig};
use crate::egfx::EgfxDecoderProvider;
use crate::runtime::run_protocol_session;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RdpGraphicsCapabilities {
    pub legacy_bitmap: bool,
    pub remotefx: bool,
    pub egfx_advertised: bool,
    pub egfx_confirmed: bool,
    /// At least one generation-bound EGFX frame boundary reached the runtime.
    /// This is separate from `egfx_confirmed`: a capability response alone does
    /// not prove that an EGFX surface was decoded and published.
    pub egfx_frame_confirmed: bool,
    pub avc420: bool,
    pub avc444: bool,
}

pub trait RdpGraphicsObserver: Send + Sync {
    fn observe(&self, capabilities: RdpGraphicsCapabilities);
}

impl<F> RdpGraphicsObserver for F
where
    F: Fn(RdpGraphicsCapabilities) + Send + Sync,
{
    fn observe(&self, capabilities: RdpGraphicsCapabilities) {
        self(capabilities);
    }
}

pub struct RdpProtocolFactory {
    client_platform: RdpClientPlatformIdentity,
    egfx_decoder_provider: Option<Arc<dyn EgfxDecoderProvider>>,
    graphics_observer: Option<Arc<dyn RdpGraphicsObserver>>,
}

impl RdpProtocolFactory {
    pub const fn new(client_platform: RdpClientPlatformIdentity) -> Self {
        Self {
            client_platform,
            egfx_decoder_provider: None,
            graphics_observer: None,
        }
    }

    /// Opt into the EGFX AVC420 transport seam with an application-owned
    /// decoder provider.  The default constructor remains legacy-only.  The
    /// provider is deliberately an IronRDP decoder boundary rather than an
    /// FFmpeg dependency, so each client platform can supply its own exact
    /// backend without leaking platform APIs into this protocol crate.
    pub fn with_egfx_decoder_provider(
        client_platform: RdpClientPlatformIdentity,
        provider: Arc<dyn EgfxDecoderProvider>,
    ) -> Self {
        Self {
            client_platform,
            egfx_decoder_provider: Some(provider),
            graphics_observer: None,
        }
    }

    pub fn with_graphics_observer(mut self, observer: Arc<dyn RdpGraphicsObserver>) -> Self {
        self.graphics_observer = Some(observer);
        self
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
        Ok(Box::new(RdpProtocolSession {
            config,
            runtime,
            egfx_decoder_provider: self.egfx_decoder_provider.clone(),
            graphics_observer: self.graphics_observer.clone(),
        }))
    }
}

pub struct RdpProtocolSession {
    config: RdpConnectionConfig,
    runtime: ProtocolRuntime,
    egfx_decoder_provider: Option<Arc<dyn EgfxDecoderProvider>>,
    graphics_observer: Option<Arc<dyn RdpGraphicsObserver>>,
}

impl ProtocolSession for RdpProtocolSession {
    fn run(self: Box<Self>) -> ProtocolExit {
        let Self {
            config,
            runtime,
            egfx_decoder_provider,
            graphics_observer,
        } = *self;
        run_protocol_session(config, runtime, egfx_decoder_provider, graphics_observer)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use frd_protocol_api::{ProtocolFactory, ProtocolId};

    use crate::{
        ParsedUsername, RdpClientPlatformIdentity, RdpGraphicsObserver, RdpProtocolFactory,
    };

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

    #[test]
    fn graphics_observer_is_optional_and_chainable() {
        let observer: Arc<dyn RdpGraphicsObserver> = Arc::new(|_| {});
        let factory = RdpProtocolFactory::new(RdpClientPlatformIdentity::Macintosh)
            .with_graphics_observer(observer);
        assert!(factory.graphics_observer.is_some());
        assert!(factory.egfx_decoder_provider.is_none());
    }
}
