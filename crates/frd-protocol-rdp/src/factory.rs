use std::sync::Arc;

use frd_protocol_api::{
    ConnectRequest, ProtocolDescriptor, ProtocolError, ProtocolExit, ProtocolFactory, ProtocolId,
    ProtocolRuntime, ProtocolSession,
};

use crate::config::{RdpClientPlatformIdentity, RdpConnectionConfig};
use crate::egfx::EgfxDecoderProvider;
use crate::legacy_decode::{RdpLegacyDecodePolicy, ResolvedLegacyDecode};
use crate::runtime::run_protocol_session;

/// Controls whether a decoder provider may advertise the EGFX graphics channel.
///
/// A local decoder or package probe is not sufficient for production
/// advertisement. `LiveInteroperable` is reserved for a platform/server pair
/// whose exact wire profile, first frame, sustained refresh and recovery gates
/// were recorded separately.
/// `ValidationOnly` 允许验证工具采集这些证据，但不代表证据已经成立。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RdpGraphicsAdvertisementGate {
    /// Keep the legacy Bitmap/RemoteFX advertisement.
    LegacyOnly,
    /// 仅供显式选择的有界互操作验证；不声明生产环境或实机验证已通过。
    /// 仍要求 provider 为当前会话创建精确匹配的 decoder。
    ValidationOnly,
    /// Permit EGFX capability advertisement after an exact live gate passed.
    LiveInteroperable,
}

/// 非敏感的 EGFX 停止原因；不保存载荷或上游错误原文。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RdpEgfxFailure {
    Start,
    PayloadProcessing,
    Decoder,
    ProgressiveWire,
    ProgressiveState,
    ProgressiveEntropy,
    ProgressiveKernel,
    Publisher,
    PublisherMissingSurface,
    PublisherUnmappedSurface,
    PublisherInvalidRectangle,
    PublisherMissingOutput,
    PublisherOutputBounds,
    PublisherQueueLimit,
    PublisherRevisionOverflow,
    PublisherCodedSizeMismatch,
    UnsupportedCodec,
    Reactivation,
}

/// 本地阶段证据；排队不等于网络写入成功。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RdpEgfxDiagnostics {
    /// 当前 adapter 生命周期累计值，跨 ResetGraphics 保留；新会话/adapter 从零开始。
    /// 解码次数不代表可见帧，帧次数也不对 codec 归因。
    /// typed WTS1 收到数，包含后续 envelope 或解码失败；AVC444 包含 v2。
    pub avc420_received_pdus_total: u64,
    pub avc444_received_pdus_total: u64,
    /// envelope 校验后的解码器调用数；AVC444 双子流只算一次调用。
    pub avc420_decode_attempts_total: u64,
    pub avc444_decode_attempts_total: u64,
    pub avc420_decoded_pictures_total: u64,
    pub avc444_decoded_updates_total: u64,
    pub clearcodec_decoded_bitmaps_total: u64,
    pub progressive_decoded_updates_total: u64,
    pub progressive_coverage_failure: Option<crate::RdpProgressiveCoverageFailure>,
    pub progressive_failure_detail: Option<&'static str>,
    pub publisher_failure_detail: Option<&'static str>,
    pub publisher_failure_operation: Option<&'static str>,
    pub frames_queued_total: u64,
    /// 当前 active session 成功提交给 runtime 的帧数，不等同 GUI 呈现。
    pub frames_runtime_accepted_total: u64,
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
    legacy_decode_policy: RdpLegacyDecodePolicy,
    #[cfg(test)]
    native_probe: Option<
        fn() -> Result<
            ironrdp::graphics::legacy_native::NativeKernels,
            ironrdp::graphics::legacy_native::KernelError,
        >,
    >,
    egfx_decoder_provider: Option<Arc<dyn EgfxDecoderProvider>>,
    graphics_advertisement_gate: RdpGraphicsAdvertisementGate,
    graphics_observer: Option<Arc<dyn RdpGraphicsObserver>>,
}

impl RdpProtocolFactory {
    pub const fn new(client_platform: RdpClientPlatformIdentity) -> Self {
        Self {
            client_platform,
            legacy_decode_policy: RdpLegacyDecodePolicy::Compatibility,
            #[cfg(test)]
            native_probe: None,
            egfx_decoder_provider: None,
            graphics_advertisement_gate: RdpGraphicsAdvertisementGate::LegacyOnly,
            graphics_observer: None,
        }
    }

    /// Stage an application-owned EGFX decoder provider without advertising it.
    ///
    /// The default remains legacy-only until the caller supplies an explicit
    /// [`RdpGraphicsAdvertisementGate::LiveInteroperable`]（生产证据已通过）或
    /// [`RdpGraphicsAdvertisementGate::ValidationOnly`]（显式采集验证证据），通过
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
    /// platform/server evidence has passed.
    /// 有界验证工具应显式使用 `ValidationOnly`；默认 factory 仍保留 legacy 路径。
    pub fn with_egfx_decoder_provider_and_gate(
        client_platform: RdpClientPlatformIdentity,
        provider: Arc<dyn EgfxDecoderProvider>,
        graphics_advertisement_gate: RdpGraphicsAdvertisementGate,
    ) -> Self {
        Self {
            client_platform,
            legacy_decode_policy: RdpLegacyDecodePolicy::Compatibility,
            #[cfg(test)]
            native_probe: None,
            egfx_decoder_provider: Some(provider),
            graphics_advertisement_gate,
            graphics_observer: None,
        }
    }

    /// 显式要求 checked native legacy 解码；调用方另行负责完整目标资格。
    /// 不改变 EGFX 广告或 wire 平台身份；不可用时 create 返回静态错误。
    pub fn with_legacy_decode_policy(mut self, policy: RdpLegacyDecodePolicy) -> Self {
        self.legacy_decode_policy = policy;
        self
    }

    fn create_session(
        &self,
        request: ConnectRequest,
        runtime: ProtocolRuntime,
    ) -> Result<RdpProtocolSession, ProtocolError> {
        // 在 config/session、执行器和任何网络阶段之前解析一次实际 CPU 后端。
        #[cfg(test)]
        let legacy_decode = match self.native_probe {
            Some(probe) => {
                ResolvedLegacyDecode::resolve_with_probe(self.legacy_decode_policy, probe)
            }
            None => ResolvedLegacyDecode::resolve(self.legacy_decode_policy),
        }?;
        #[cfg(not(test))]
        let legacy_decode = ResolvedLegacyDecode::resolve(self.legacy_decode_policy)?;
        let config = RdpConnectionConfig::try_new(request, self.client_platform)?;
        Ok(RdpProtocolSession {
            config,
            runtime,
            legacy_decode,
            egfx_decoder_provider: self.egfx_decoder_provider.clone(),
            graphics_advertisement_gate: self.graphics_advertisement_gate,
            graphics_observer: self.graphics_observer.clone(),
        })
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
        Ok(Box::new(self.create_session(request, runtime)?))
    }
}

pub struct RdpProtocolSession {
    legacy_decode: ResolvedLegacyDecode,
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
            legacy_decode,
            egfx_decoder_provider,
            graphics_advertisement_gate,
            graphics_observer,
        } = *self;
        run_protocol_session(
            config,
            runtime,
            legacy_decode,
            egfx_decoder_provider,
            graphics_advertisement_gate,
            graphics_observer,
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;
    use std::sync::mpsc;
    use std::sync::Arc;

    use crate::legacy_decode::ResolvedLegacyDecode;
    use crate::RdpLegacyDecodePolicy;
    use frd_core::{PixelSize, SecretBuffer, SessionId};
    use frd_protocol_api::{
        ConnectRequest, Credentials, Endpoint, ProtocolError, ProtocolFactory, ProtocolId,
        ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionEvent, SurfacePublisher,
    };
    use ironrdp::graphics::legacy_native::{KernelError, NativeKernels};
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

    thread_local! {
        static PROBE_CALLS: Cell<usize> = const { Cell::new(0) };
    }

    fn deny_features() -> Result<NativeKernels, KernelError> {
        PROBE_CALLS.with(|calls| calls.set(calls.get() + 1));
        NativeKernels::detect_if(false)
    }

    fn count_actual_probe() -> Result<NativeKernels, KernelError> {
        PROBE_CALLS.with(|calls| calls.set(calls.get() + 1));
        NativeKernels::detect()
    }

    struct NoEffects;
    impl RuntimeEventSink for NoEffects {
        fn publish(&self, _: SessionEvent) -> Result<(), ProtocolError> {
            panic!("factory create 不得发布会话事件");
        }
    }
    impl SurfacePublisher for NoEffects {
        fn publish(&self, _: frd_frame::SurfaceUpdate) -> Result<(), ProtocolError> {
            panic!("factory create 不得发布图像");
        }
    }
    impl RuntimeWake for NoEffects {
        fn wake(&self) -> Result<(), ProtocolError> {
            panic!("factory create 不得唤醒运行时");
        }
    }

    fn request_runtime() -> (ConnectRequest, ProtocolRuntime) {
        let session_id = SessionId::allocate();
        let (_, command_rx) = mpsc::channel();
        let request = ConnectRequest {
            session_id,
            endpoint: Endpoint::new("no-network.invalid", 3389).unwrap(),
            protocol_id: ProtocolId::rdp(),
            credentials: Some(Credentials {
                username: "fixture".to_owned(),
                password: SecretBuffer::new(vec![1]).take(),
            }),
            saved_server_pin: None,
            display_intent: frd_core::DisplayIntent::default(),
        };
        let runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoEffects),
            Box::new(NoEffects),
            None,
            Box::new(NoEffects),
        );
        (request, runtime)
    }

    /// 编码流测试取自真实工厂创建的 session，而非测试手工拼接 NativeK 表。
    pub(crate) fn resolved_session_selection(
        policy: RdpLegacyDecodePolicy,
    ) -> ResolvedLegacyDecode {
        let (request, runtime) = request_runtime();
        RdpProtocolFactory::new(RdpClientPlatformIdentity::Windows)
            .with_legacy_decode_policy(policy)
            .create_session(request, runtime)
            .unwrap()
            .legacy_decode
    }

    #[test]
    fn legacy_native_default_constructors_never_probe_and_keep_all_gates() {
        let provider: Arc<dyn EgfxDecoderProvider> = Arc::new(NoopProvider);
        let observer: Arc<dyn RdpGraphicsObserver> = Arc::new(|_| panic!("create 不观察图形"));
        let factories = [
            RdpProtocolFactory::new(RdpClientPlatformIdentity::Unix),
            RdpProtocolFactory::with_egfx_decoder_provider(
                RdpClientPlatformIdentity::Unix,
                provider.clone(),
            ),
            RdpProtocolFactory::with_egfx_decoder_provider_and_gate(
                RdpClientPlatformIdentity::Unix,
                provider.clone(),
                RdpGraphicsAdvertisementGate::ValidationOnly,
            ),
            RdpProtocolFactory::with_egfx_decoder_provider_and_gate(
                RdpClientPlatformIdentity::Unix,
                provider,
                RdpGraphicsAdvertisementGate::LiveInteroperable,
            ),
        ];
        PROBE_CALLS.with(|calls| calls.set(0));
        for mut factory in factories {
            let gate = factory.graphics_advertisement_gate;
            factory.native_probe = Some(deny_features);
            let factory = factory.with_graphics_observer(observer.clone());
            let (request, runtime) = request_runtime();
            let session = factory.create_session(request, runtime).unwrap();
            assert!(matches!(
                session.legacy_decode,
                ResolvedLegacyDecode::Compatibility
            ));
            assert_eq!(session.graphics_advertisement_gate, gate);
            assert!(session.graphics_observer.is_some());
        }
        assert_eq!(PROBE_CALLS.with(Cell::get), 0);
    }

    #[test]
    fn legacy_native_unavailable_fails_before_config_session_and_runtime_effects() {
        PROBE_CALLS.with(|calls| calls.set(0));
        let mut factory = RdpProtocolFactory::new(RdpClientPlatformIdentity::Unix)
            .with_legacy_decode_policy(RdpLegacyDecodePolicy::RequiredNative);
        factory.native_probe = Some(deny_features);
        for valid_config in [true, false] {
            let (mut request, runtime) = request_runtime();
            if !valid_config {
                request.credentials = None;
            }
            let error = factory
                .create(request, runtime)
                .err()
                .expect("必须拒绝，不能回落兼容 session");
            assert_eq!(error.code(), "rdp_legacy_native_unavailable");
            assert_eq!(error.protocol_id(), Some(&ProtocolId::rdp()));
        }
        assert_eq!(PROBE_CALLS.with(Cell::get), 2);
    }

    #[test]
    fn legacy_native_resolves_once_independent_of_wire_identity_and_builder_order() {
        let actual_backend = NativeKernels::detect().unwrap().backend();
        let provider: Arc<dyn EgfxDecoderProvider> = Arc::new(NoopProvider);
        let observer: Arc<dyn RdpGraphicsObserver> = Arc::new(|_| {});
        for platform in [
            RdpClientPlatformIdentity::Windows,
            RdpClientPlatformIdentity::Macintosh,
            RdpClientPlatformIdentity::Ios,
            RdpClientPlatformIdentity::Unix,
            RdpClientPlatformIdentity::Android,
        ] {
            for policy_first in [false, true] {
                PROBE_CALLS.with(|calls| calls.set(0));
                let factory = RdpProtocolFactory::with_egfx_decoder_provider_and_gate(
                    platform,
                    provider.clone(),
                    RdpGraphicsAdvertisementGate::ValidationOnly,
                );
                let mut factory = if policy_first {
                    factory
                        .with_legacy_decode_policy(RdpLegacyDecodePolicy::RequiredNative)
                        .with_graphics_observer(observer.clone())
                } else {
                    factory
                        .with_graphics_observer(observer.clone())
                        .with_legacy_decode_policy(RdpLegacyDecodePolicy::RequiredNative)
                };
                factory.native_probe = Some(count_actual_probe);
                let (request, runtime) = request_runtime();
                let session = factory.create_session(request, runtime).unwrap();
                let ResolvedLegacyDecode::CheckedNative(kernels) = session.legacy_decode else {
                    panic!("session 丢失原生选择");
                };
                assert_eq!(kernels.backend(), actual_backend);
                assert_eq!(session.config.client_platform, platform);
                assert_eq!(
                    session.graphics_advertisement_gate,
                    RdpGraphicsAdvertisementGate::ValidationOnly
                );
                assert!(session.graphics_observer.is_some());
                assert_eq!(PROBE_CALLS.with(Cell::get), 1);
            }
        }
    }

    #[test]
    fn legacy_native_policy_can_be_explicitly_restored_to_compatibility() {
        let mut factory = RdpProtocolFactory::new(RdpClientPlatformIdentity::Unix)
            .with_legacy_decode_policy(RdpLegacyDecodePolicy::RequiredNative)
            .with_legacy_decode_policy(RdpLegacyDecodePolicy::Compatibility);
        factory.native_probe = Some(deny_features);
        let (request, runtime) = request_runtime();
        assert!(factory.create(request, runtime).is_ok());
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
    fn explicit_validation_only_gate_is_retained_by_factory() {
        let factory = RdpProtocolFactory::with_egfx_decoder_provider_and_gate(
            RdpClientPlatformIdentity::Macintosh,
            Arc::new(NoopProvider),
            RdpGraphicsAdvertisementGate::ValidationOnly,
        );
        assert_eq!(
            factory.graphics_advertisement_gate,
            RdpGraphicsAdvertisementGate::ValidationOnly
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

#[cfg(test)]
#[path = "cursor_consumer_tests.rs"]
mod cursor_consumer_tests;
