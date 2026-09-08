use std::sync::Arc;

use frd_protocol_api::{
    ConnectRequest, ProtocolDescriptor, ProtocolError, ProtocolExit, ProtocolFactory, ProtocolId,
    ProtocolRuntime, ProtocolSession,
};

use crate::config::{RdpClientPlatformIdentity, RdpConnectionConfig};
use crate::egfx::EgfxDecoderProvider;
use crate::runtime::run_protocol_session;

/// Controls whether a decoder provider may advertise the EGFX graphics channel.
///
/// A local decoder or package probe is not sufficient for production
/// advertisement. `LiveInteroperable` is reserved for a platform/server pair
/// whose exact wire profile, first frame, sustained refresh and recovery gates
/// were recorded separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RdpGraphicsAdvertisementGate {
    /// Keep the legacy Bitmap/RemoteFX advertisement.
    LegacyOnly,
    /// Permit EGFX capability advertisement after an exact live gate passed.
    LiveInteroperable,
}

/// 非敏感的 EGFX 停止原因；不保存载荷或上游错误原文。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RdpEgfxFailure {
    Start,
    PayloadProcessing,
    Decoder,
    Publisher,
    Reactivation,
}

/// 本地阶段证据；排队不等于网络写入成功。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RdpEgfxDiagnostics {
    pub start_calls: u64,
    pub capability_messages_queued: u64,
    /// 曾观察到能够解析的服务器 CapabilitiesConfirm，失败后仍保留。
    pub typed_confirmation_ever: bool,
    pub confirmed_version: Option<u32>,
    /// V10.1 没有 flags 字段，保持 None。
    pub confirmed_flags: Option<u32>,
    pub unhandled_codec_count: u64,
    pub last_unhandled_codec: Option<u16>,
    pub failure_count: u64,
    pub first_failure: Option<RdpEgfxFailure>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RdpGraphicsCapabilities {
    pub legacy_bitmap: bool,
    pub remotefx: bool,
    /// 本地已配置通道和 early capability，不能证明 EGFX 能力报文已发送。
    pub egfx_advertised: bool,
    pub egfx_diagnostics: RdpEgfxDiagnostics,
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
    graphics_advertisement_gate: RdpGraphicsAdvertisementGate,
    graphics_observer: Option<Arc<dyn RdpGraphicsObserver>>,
}

impl RdpProtocolFactory {
    pub const fn new(client_platform: RdpClientPlatformIdentity) -> Self {
        Self {
            client_platform,
            egfx_decoder_provider: None,
            graphics_advertisement_gate: RdpGraphicsAdvertisementGate::LegacyOnly,
            graphics_observer: None,
        }
    }

    /// Stage an application-owned EGFX decoder provider without advertising it.
    ///
    /// The default remains legacy-only until the caller supplies an explicit
    /// [`RdpGraphicsAdvertisementGate::LiveInteroperable`] through
    /// [`Self::with_egfx_decoder_provider_and_gate`]. The provider is
    /// deliberately an IronRDP decoder boundary rather than an FFmpeg
    /// dependency, so each client platform can supply its own exact backend
    /// without leaking platform APIs into this protocol crate.
    pub fn with_egfx_decoder_provider(
        client_platform: RdpClientPlatformIdentity,
        provider: Arc<dyn EgfxDecoderProvider>,
    ) -> Self {
        Self::with_egfx_decoder_provider_and_gate(
            client_platform,
            provider,
            RdpGraphicsAdvertisementGate::LegacyOnly,
        )
    }

    /// Construct a factory with an explicit graphics advertisement gate.
    /// Production callers may use `LiveInteroperable` only after the exact
    /// platform/server evidence has passed; bounded probes use the same
    /// explicit path and do not alter the default factory.
    pub fn with_egfx_decoder_provider_and_gate(
        client_platform: RdpClientPlatformIdentity,
        provider: Arc<dyn EgfxDecoderProvider>,
        graphics_advertisement_gate: RdpGraphicsAdvertisementGate,
    ) -> Self {
        Self {
            client_platform,
            egfx_decoder_provider: Some(provider),
            graphics_advertisement_gate,
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
            graphics_advertisement_gate: self.graphics_advertisement_gate,
            graphics_observer: self.graphics_observer.clone(),
        }))
    }
}

pub struct RdpProtocolSession {
    config: RdpConnectionConfig,
    runtime: ProtocolRuntime,
    egfx_decoder_provider: Option<Arc<dyn EgfxDecoderProvider>>,
    graphics_advertisement_gate: RdpGraphicsAdvertisementGate,
    graphics_observer: Option<Arc<dyn RdpGraphicsObserver>>,
}

impl ProtocolSession for RdpProtocolSession {
    fn run(self: Box<Self>) -> ProtocolExit {
        let Self {
            config,
            runtime,
            egfx_decoder_provider,
            graphics_advertisement_gate,
            graphics_observer,
        } = *self;
        run_protocol_session(
            config,
            runtime,
            egfx_decoder_provider,
            graphics_advertisement_gate,
            graphics_observer,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use frd_core::{PixelSize, SessionId};
    use frd_protocol_api::{ProtocolFactory, ProtocolId};
    use ironrdp_egfx::decode::H264Decoder;

    use crate::{
        EgfxDecoderProvider, ParsedUsername, RdpClientPlatformIdentity,
        RdpGraphicsAdvertisementGate, RdpGraphicsObserver, RdpProtocolFactory,
    };

    struct NoopProvider;

    impl EgfxDecoderProvider for NoopProvider {
        fn create_decoder(
            &self,
            _session_id: SessionId,
            _coded_size: PixelSize,
        ) -> Option<Box<dyn H264Decoder>> {
            None
        }
    }

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
        assert_eq!(
            factory.graphics_advertisement_gate,
            RdpGraphicsAdvertisementGate::LegacyOnly
        );
    }

    #[test]
    fn provider_constructor_defaults_to_legacy_advertisement_gate() {
        let factory = RdpProtocolFactory::with_egfx_decoder_provider(
            RdpClientPlatformIdentity::Macintosh,
            Arc::new(NoopProvider),
        );
        assert_eq!(
            factory.graphics_advertisement_gate,
            RdpGraphicsAdvertisementGate::LegacyOnly
        );
    }

    #[test]
    fn explicit_live_gate_is_retained_by_factory() {
        let factory = RdpProtocolFactory::with_egfx_decoder_provider_and_gate(
            RdpClientPlatformIdentity::Macintosh,
            Arc::new(NoopProvider),
            RdpGraphicsAdvertisementGate::LiveInteroperable,
        );
        assert_eq!(
            factory.graphics_advertisement_gate,
            RdpGraphicsAdvertisementGate::LiveInteroperable
        );
    }
}
