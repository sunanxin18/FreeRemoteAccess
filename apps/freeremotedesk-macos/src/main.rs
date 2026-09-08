mod cli;

use std::{process::ExitCode, sync::Arc};

use std::io::Write;

use clap::{error::ErrorKind, Parser};
use frd_app::{AppLaunch, ProductPolicy};
use frd_media_api::{AudioOutput, AudioOutputError};
use frd_platform_api::{
    ConnectionProfileStore, PlatformCapabilities, SecureCredentialStore, ServerIdentityStore,
};
use frd_platform_macos::{
    EnvironmentCredentialProvider, MacAudioOutput, MacConnectionProfileStore, MacCredentialStore,
    MacServerIdentityStore, MacSingleInstanceError, MacSingleInstanceGuard,
};
use frd_protocol_api::{ProtocolCatalog, ProtocolFactory};
use frd_protocol_apple::{AppleHighPerformanceProtocolFactory, AppleProtocolFactory};
use frd_protocol_rdp::{Avc420DecoderProvider, RdpClientPlatformIdentity, RdpProtocolFactory};
use frd_shell_desktop::{
    AudioOutputFactory, DesktopApplication, DesktopPlatformStores, DesktopUserEvent,
    DesktopWindowConfiguration, FatalComponent, FatalOperation, FatalReason, FatalReport,
};
use winit::event_loop::{ControlFlow, EventLoop};

use crate::cli::{Cli, RdpEgfxExperiment};

struct MacAudioFactory;

impl AudioOutputFactory for MacAudioFactory {
    fn open(&self) -> std::result::Result<Box<dyn AudioOutput>, AudioOutputError> {
        MacAudioOutput::open_default().map(|output| Box::new(output) as Box<dyn AudioOutput>)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RunnerOutcome {
    Success,
    Fatal(FatalReport),
    ExperimentUnavailable(&'static str),
}

impl RunnerOutcome {
    fn from_failure(failure: RunnerFailure) -> Self {
        let (operation, reason) = match failure {
            RunnerFailure::SingleInstanceAlreadyRunning => (
                FatalOperation::SingleInstance,
                FatalReason::InstanceAlreadyRunning,
            ),
            RunnerFailure::SingleInstanceUnavailable => (
                FatalOperation::SingleInstance,
                FatalReason::SingleInstanceUnavailable,
            ),
            RunnerFailure::EventLoopCreate => (
                FatalOperation::EventLoopCreate,
                FatalReason::EventLoopCreateFailed,
            ),
            RunnerFailure::CommandLineOptions => {
                (FatalOperation::CliValidation, FatalReason::InvalidArguments)
            }
            RunnerFailure::CommandLineOutput => (
                FatalOperation::CliValidation,
                FatalReason::CommandLineOutputFailed,
            ),
            RunnerFailure::IdentityStore => (
                FatalOperation::IdentityStore,
                FatalReason::IdentityStoreUnavailable,
            ),
            RunnerFailure::CredentialStore => (
                FatalOperation::CredentialStore,
                FatalReason::CredentialStoreUnavailable,
            ),
            RunnerFailure::EventLoopRun => (
                FatalOperation::EventLoopRun,
                FatalReason::EventLoopRunFailed,
            ),
        };
        Self::Fatal(FatalReport::internal(
            FatalComponent::Application,
            operation,
            reason,
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunnerFailure {
    SingleInstanceAlreadyRunning,
    SingleInstanceUnavailable,
    EventLoopCreate,
    CommandLineOptions,
    CommandLineOutput,
    IdentityStore,
    CredentialStore,
    EventLoopRun,
}

fn product_window_configuration() -> Result<DesktopWindowConfiguration, RunnerFailure> {
    // macOS 应用与 Dock 图标由应用包的 ICNS 提供，保留原生窗口约定。
    Ok(DesktopWindowConfiguration { icon: None })
}

#[derive(Clone, Debug, PartialEq)]
struct RunnerDecision {
    exit_code: ExitCode,
    stderr: Option<String>,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            return if error.print().is_ok() {
                ExitCode::SUCCESS
            } else {
                emit_runner_outcome(RunnerOutcome::from_failure(
                    RunnerFailure::CommandLineOutput,
                ))
            };
        }
        Err(_) => {
            return emit_runner_outcome(RunnerOutcome::from_failure(
                RunnerFailure::CommandLineOptions,
            ))
        }
    };

    if cli.verify_codec_bundle {
        return match frd_video_ffmpeg::FfmpegBackend::load() {
            Ok(_) => ExitCode::SUCCESS,
            Err(_) => {
                emit_fatal_report("macos_codec_bundle_unavailable\n");
                ExitCode::FAILURE
            }
        };
    }
    emit_runner_outcome(run(cli))
}

fn run(cli: Cli) -> RunnerOutcome {
    let _single_instance =
        match MacSingleInstanceGuard::acquire_for_product("freeremotedesk-macos-product") {
            Ok(guard) => guard,
            Err(MacSingleInstanceError::AlreadyRunning) => {
                return RunnerOutcome::from_failure(RunnerFailure::SingleInstanceAlreadyRunning)
            }
            Err(MacSingleInstanceError::Unavailable) => {
                return RunnerOutcome::from_failure(RunnerFailure::SingleInstanceUnavailable)
            }
        };
    let rdp_factory = match rdp_factory(cli.rdp_egfx_experiment) {
        Ok(factory) => factory,
        Err(code) => return RunnerOutcome::ExperimentUnavailable(code),
    };
    let event_loop = match EventLoop::<DesktopUserEvent>::with_user_event().build() {
        Ok(event_loop) => event_loop,
        Err(_) => return RunnerOutcome::from_failure(RunnerFailure::EventLoopCreate),
    };
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();

    if let Some(options) = cli.test_texture_options() {
        let mut application = DesktopApplication::new_test_texture(proxy, options);
        let configuration = match product_window_configuration() {
            Ok(configuration) => configuration,
            Err(failure) => return RunnerOutcome::from_failure(failure),
        };
        application.set_window_configuration(configuration);
        let run_result = event_loop.run_app(&mut application);
        return finish_event_loop(run_result, application.runner_result());
    }

    let apple_factory = Arc::new(AppleProtocolFactory) as Arc<dyn ProtocolFactory>;
    let apple_high_performance_factory =
        Arc::new(AppleHighPerformanceProtocolFactory) as Arc<dyn ProtocolFactory>;
    let factories = [apple_high_performance_factory, apple_factory, rdp_factory];
    let catalog = ProtocolCatalog::new(factories.iter().map(|factory| factory.descriptor().id));
    let provider = EnvironmentCredentialProvider;
    let launch_options = match cli.launch_options() {
        Ok(options) => options,
        Err(_) => return RunnerOutcome::from_failure(RunnerFailure::CommandLineOptions),
    };
    let server_identities = match MacServerIdentityStore::current_user_default() {
        Ok(store) => Arc::new(store) as Arc<dyn ServerIdentityStore>,
        Err(_) => return RunnerOutcome::from_failure(RunnerFailure::IdentityStore),
    };
    let profiles = match MacConnectionProfileStore::current_user_default() {
        Ok(store) => Arc::new(store) as Arc<dyn ConnectionProfileStore>,
        Err(_) => return RunnerOutcome::from_failure(RunnerFailure::IdentityStore),
    };
    let credentials = Arc::new(MacCredentialStore::new());
    if let Err(failure) = purge_pending_credentials(credentials.as_ref()) {
        return RunnerOutcome::from_failure(failure);
    }
    let credentials = credentials as Arc<dyn SecureCredentialStore>;
    let stores = DesktopPlatformStores::new(server_identities, profiles, credentials);
    let mut launch =
        AppLaunch::new_with_stores(launch_options, &provider, &catalog, stores.as_app_stores());
    launch
        .controller_mut()
        .set_platform_capabilities(PlatformCapabilities {
            dynamic_resolution: true,
            clipboard_read: false,
            clipboard_write: false,
            remote_audio: true,
            text_input: true,
        });
    launch.controller_mut().set_product_policy(ProductPolicy {
        dynamic_resolution: true,
        clipboard_read: false,
        clipboard_write: false,
        remote_audio: true,
        text_input: true,
    });
    let mut application = DesktopApplication::new_product(
        launch,
        factories,
        stores,
        Arc::new(MacAudioFactory),
        proxy,
    );
    let configuration = match product_window_configuration() {
        Ok(configuration) => configuration,
        Err(failure) => return RunnerOutcome::from_failure(failure),
    };
    application.set_window_configuration(configuration);
    let run_result = event_loop.run_app(&mut application);
    finish_event_loop(run_result, application.runner_result())
}

fn rdp_factory(
    experiment: Option<RdpEgfxExperiment>,
) -> Result<Arc<dyn ProtocolFactory>, &'static str> {
    rdp_factory_with_backend(experiment, frd_video_ffmpeg::FfmpegBackend::load().ok())
}

fn rdp_factory_with_backend(
    experiment: Option<RdpEgfxExperiment>,
    backend: Option<frd_video_ffmpeg::FfmpegBackend>,
) -> Result<Arc<dyn ProtocolFactory>, &'static str> {
    let platform = RdpClientPlatformIdentity::Macintosh;
    if let Some(mode) = experiment {
        let backend = backend.ok_or("macos_rdp_egfx_experiment_signed_backend_unavailable")?;
        validate_experiment_support(mode, backend.supports_avc420(), backend.supports_avc444())?;
        let backend = Arc::new(backend);
        let provider: Arc<dyn frd_protocol_rdp::EgfxDecoderProvider> = match mode {
            RdpEgfxExperiment::Avc420 => Arc::new(Avc420DecoderProvider::from_factory(backend)),
            RdpEgfxExperiment::Avc444 => {
                Arc::new(frd_protocol_rdp::Avc444DecoderProvider::new(backend))
            }
        };
        let factory = RdpProtocolFactory::with_egfx_decoder_provider_and_gate(
            platform,
            provider,
            frd_protocol_rdp::RdpGraphicsAdvertisementGate::ValidationOnly,
        )
        .with_graphics_observer(Arc::new(emit_experiment_diagnostics));
        return Ok(Arc::new(factory));
    }
    Ok(match backend {
        Some(backend) if backend.supports_avc420() => {
            Arc::new(RdpProtocolFactory::with_egfx_decoder_provider(
                platform,
                Arc::new(Avc420DecoderProvider::from_factory(Arc::new(backend))),
            ))
        }
        None | Some(_) => legacy_rdp_factory(platform),
    })
}

fn emit_experiment_diagnostics(caps: frd_protocol_rdp::RdpGraphicsCapabilities) {
    let line = format_experiment_diagnostics(caps.egfx_confirmed, caps.egfx_diagnostics);
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}

fn format_experiment_diagnostics(
    egfx_confirmed: bool,
    d: frd_protocol_rdp::RdpEgfxDiagnostics,
) -> String {
    // 仅输出类型化计数和静态失败标签；runtime 接受帧不代表窗口已经呈现该帧。
    format!(
        "RDP EGFX 验证 egfx_confirmed={} avc420_decoded_pictures_total={} avc444_decoded_updates_total={} clearcodec_decoded_bitmaps_total={} progressive_decoded_updates_total={} frames_queued_total={} frames_runtime_accepted_total={} failure_count={} first_failure={:?} progressive_failure_detail={:?} progressive_coverage_failure={:?} publisher_failure_detail={:?} publisher_failure_operation={:?}\n",
        egfx_confirmed,
        d.avc420_decoded_pictures_total,
        d.avc444_decoded_updates_total,
        d.clearcodec_decoded_bitmaps_total,
        d.progressive_decoded_updates_total,
        d.frames_queued_total,
        d.frames_runtime_accepted_total,
        d.failure_count,
        d.first_failure,
        d.progressive_failure_detail,
        d.progressive_coverage_failure,
        d.publisher_failure_detail,
        d.publisher_failure_operation,
    )
}

fn validate_experiment_support(
    mode: RdpEgfxExperiment,
    avc420: bool,
    avc444: bool,
) -> Result<(), &'static str> {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Err("macos_rdp_egfx_experiment_architecture_unsupported");
    }
    if !avc420 {
        return Err("macos_rdp_egfx_experiment_avc420_unavailable");
    }
    if mode == RdpEgfxExperiment::Avc444 && !avc444 {
        return Err("macos_rdp_egfx_experiment_avc444_unavailable");
    }
    Ok(())
}

fn legacy_rdp_factory(platform: RdpClientPlatformIdentity) -> Arc<dyn ProtocolFactory> {
    Arc::new(RdpProtocolFactory::new(platform))
}

fn purge_pending_credentials(credentials: &dyn SecureCredentialStore) -> Result<(), RunnerFailure> {
    credentials
        .purge_pending()
        .map_err(|_| RunnerFailure::CredentialStore)
}

fn finish_event_loop<E>(
    event_loop_result: std::result::Result<(), E>,
    application_result: std::result::Result<(), FatalReport>,
) -> RunnerOutcome {
    match (application_result, event_loop_result) {
        (Err(report), _) => RunnerOutcome::Fatal(report),
        (Ok(()), Err(_)) => RunnerOutcome::from_failure(RunnerFailure::EventLoopRun),
        (Ok(()), Ok(())) => RunnerOutcome::Success,
    }
}

fn runner_decision(outcome: RunnerOutcome) -> RunnerDecision {
    match outcome {
        RunnerOutcome::Success => RunnerDecision {
            exit_code: ExitCode::SUCCESS,
            stderr: None,
        },
        RunnerOutcome::ExperimentUnavailable(code) => RunnerDecision {
            exit_code: ExitCode::FAILURE,
            stderr: Some(format!("{code}\n")),
        },
        RunnerOutcome::Fatal(report) => RunnerDecision {
            exit_code: ExitCode::FAILURE,
            stderr: Some(format!("{report}\n")),
        },
    }
}

fn emit_runner_outcome(outcome: RunnerOutcome) -> ExitCode {
    let decision = runner_decision(outcome);
    if let Some(line) = decision.stderr {
        emit_fatal_report(&line);
    }
    decision.exit_code
}

fn emit_fatal_report(line: &str) {
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicBool, Ordering};

    use frd_platform_api::{ConnectionProfileKey, PlatformError, SecureCredentialStore};
    use frd_shell_desktop::{FatalComponent, FatalOperation, FatalReason, FatalReport};

    use super::{
        finish_event_loop, format_experiment_diagnostics, product_window_configuration,
        purge_pending_credentials, runner_decision, RunnerFailure, RunnerOutcome,
    };

    #[test]
    fn experiment_diagnostics_identifies_publisher_failure_and_codec_attribution() {
        let diagnostics = frd_protocol_rdp::RdpEgfxDiagnostics {
            clearcodec_decoded_bitmaps_total: 2,
            progressive_decoded_updates_total: 3,
            failure_count: 1,
            first_failure: Some(frd_protocol_rdp::RdpEgfxFailure::PublisherOutputBounds),
            publisher_failure_detail: Some("output bounds"),
            publisher_failure_operation: Some("clearcodec"),
            ..Default::default()
        };
        assert_eq!(
            format_experiment_diagnostics(true, diagnostics),
            "RDP EGFX 验证 egfx_confirmed=true avc420_decoded_pictures_total=0 avc444_decoded_updates_total=0 clearcodec_decoded_bitmaps_total=2 progressive_decoded_updates_total=3 frames_queued_total=0 frames_runtime_accepted_total=0 failure_count=1 first_failure=Some(PublisherOutputBounds) progressive_failure_detail=None progressive_coverage_failure=None publisher_failure_detail=Some(\"output bounds\") publisher_failure_operation=Some(\"clearcodec\")\n"
        );
    }

    #[test]
    fn experiment_diagnostics_retains_progressive_static_failure_detail() {
        let diagnostics = frd_protocol_rdp::RdpEgfxDiagnostics {
            failure_count: 1,
            first_failure: Some(frd_protocol_rdp::RdpEgfxFailure::ProgressiveEntropy),
            progressive_failure_detail: Some("entropy short input"),
            progressive_coverage_failure: Some(frd_protocol_rdp::RdpProgressiveCoverageFailure {
                outer_frame_id: 47,
                surface_id: 1,
                codec_context_id: 2,
                rectangle: (0, 0, 128, 64),
                missing_tile: (1, 0),
                frame_tile_count: 1,
                region_tile_count: 1,
            }),
            ..Default::default()
        };
        let line = format_experiment_diagnostics(false, diagnostics);
        assert!(line.contains("first_failure=Some(ProgressiveEntropy)"));
        assert!(line.contains("progressive_failure_detail=Some(\"entropy short input\")"));
        assert!(line.contains("progressive_coverage_failure=Some(CoverageFailure { outer_frame_id: 47, surface_id: 1, codec_context_id: 2, rectangle: (0, 0, 128, 64), missing_tile: (1, 0), frame_tile_count: 1, region_tile_count: 1 })"));
        assert!(line.ends_with("publisher_failure_detail=None publisher_failure_operation=None\n"));
        assert_eq!(line.lines().count(), 1);
        assert_eq!(line, format_experiment_diagnostics(false, diagnostics));
    }

    #[test]
    fn missing_backend_falls_back_only_when_experiment_is_disabled() {
        assert!(super::rdp_factory_with_backend(None, None).is_ok());
        for mode in [
            super::RdpEgfxExperiment::Avc420,
            super::RdpEgfxExperiment::Avc444,
        ] {
            assert_eq!(
                super::rdp_factory_with_backend(Some(mode), None).err(),
                Some("macos_rdp_egfx_experiment_signed_backend_unavailable")
            );
        }
    }

    #[test]
    fn experiment_requires_exact_backend_profiles_and_visible_error() {
        use super::{validate_experiment_support, RdpEgfxExperiment};
        for mode in [RdpEgfxExperiment::Avc420, RdpEgfxExperiment::Avc444] {
            assert!(validate_experiment_support(mode, false, true).is_err());
            assert!(validate_experiment_support(mode, false, false).is_err());
        }
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            assert!(validate_experiment_support(RdpEgfxExperiment::Avc420, true, false).is_ok());
            assert!(validate_experiment_support(RdpEgfxExperiment::Avc444, true, false).is_err());
            assert!(validate_experiment_support(RdpEgfxExperiment::Avc444, true, true).is_ok());
        } else {
            assert_eq!(
                validate_experiment_support(RdpEgfxExperiment::Avc420, true, true),
                Err("macos_rdp_egfx_experiment_architecture_unsupported")
            );
        }
        let decision = runner_decision(RunnerOutcome::ExperimentUnavailable(
            "macos_rdp_egfx_experiment_signed_backend_unavailable",
        ));
        assert_eq!(decision.exit_code, ExitCode::FAILURE);
        assert_eq!(
            decision.stderr.as_deref(),
            Some("macos_rdp_egfx_experiment_signed_backend_unavailable\n")
        );
    }

    struct TestCredentialStore {
        fail_purge: bool,
        purged: AtomicBool,
    }

    impl TestCredentialStore {
        fn successful() -> Self {
            Self {
                fail_purge: false,
                purged: AtomicBool::new(false),
            }
        }

        fn unavailable() -> Self {
            Self {
                fail_purge: true,
                purged: AtomicBool::new(false),
            }
        }
    }

    impl SecureCredentialStore for TestCredentialStore {
        fn load(
            &self,
            _key: &ConnectionProfileKey,
        ) -> Result<Option<frd_core::SecretBuffer>, PlatformError> {
            unreachable!("startup purge does not load a profile credential")
        }

        fn stage(
            &self,
            _session: frd_core::SessionId,
            _key: &ConnectionProfileKey,
            _password: &frd_core::SecretBuffer,
        ) -> Result<(), PlatformError> {
            unreachable!("startup purge does not stage a profile credential")
        }

        fn commit(
            &self,
            _session: frd_core::SessionId,
            _key: &ConnectionProfileKey,
        ) -> Result<(), PlatformError> {
            unreachable!("startup purge does not commit a profile credential")
        }

        fn discard(&self, _session: frd_core::SessionId) -> Result<(), PlatformError> {
            unreachable!("startup purge does not discard one session")
        }

        fn delete(&self, _key: &ConnectionProfileKey) -> Result<(), PlatformError> {
            unreachable!("startup purge does not delete a committed credential")
        }

        fn purge_pending(&self) -> Result<(), PlatformError> {
            self.purged.store(true, Ordering::SeqCst);
            if self.fail_purge {
                Err(PlatformError::Unavailable)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn startup_purges_pending_credentials_before_app_launch_construction() {
        let credentials = TestCredentialStore::successful();

        assert_eq!(purge_pending_credentials(&credentials), Ok(()));
        assert!(credentials.purged.load(Ordering::SeqCst));
    }

    #[test]
    fn startup_pending_purge_failure_uses_the_closed_runner_taxonomy() {
        let credentials = TestCredentialStore::unavailable();

        assert_eq!(
            purge_pending_credentials(&credentials),
            Err(RunnerFailure::CredentialStore)
        );
        let decision = runner_decision(RunnerOutcome::from_failure(RunnerFailure::CredentialStore));
        assert_eq!(
            decision.stderr.as_deref(),
            Some(
                "FRD-WIN-FATAL-001 component=application operation=credential_store reason=credential_store_unavailable details=none\n"
            )
        );
    }

    #[test]
    fn window_icon_is_owned_by_native_app_bundle() {
        assert!(product_window_configuration().unwrap().icon.is_none());
    }

    #[test]
    fn fatal_runner_decision_is_one_exact_display_line_and_a_failure_exit() {
        let fatal = FatalReport::internal(
            FatalComponent::Application,
            FatalOperation::Launch,
            FatalReason::InvalidState,
        );

        let outcome = finish_event_loop(Ok::<(), std::io::Error>(()), Err(fatal));
        let decision = runner_decision(outcome);

        assert_eq!(decision.exit_code, ExitCode::FAILURE);
        assert_eq!(
            decision.stderr,
            Some(
                "FRD-WIN-FATAL-001 component=application operation=launch reason=invalid_state details=none\n"
                    .to_owned()
            )
        );
        assert_eq!(decision.stderr.as_deref().unwrap().lines().count(), 1);
    }

    #[test]
    fn backtrace_environment_cannot_change_the_pure_fatal_output_mapping() {
        let expected = "FRD-WIN-FATAL-001 component=application operation=event_loop_run reason=event_loop_run_failed details=none\n";
        let outcome = finish_event_loop(
            Err(std::io::Error::other(
                "secret diagnostic at C:\\private\\source.rs",
            )),
            Ok(()),
        );

        let first = runner_decision(outcome.clone());
        let second = runner_decision(outcome);

        assert_eq!(first.stderr.as_deref(), Some(expected));
        assert_eq!(second, first);
        assert!(!expected.contains("secret"));
        assert!(!expected.contains("private"));
        assert!(!expected.contains("backtrace"));
    }

    #[test]
    fn event_loop_construction_failure_uses_the_closed_runner_taxonomy() {
        let decision = runner_decision(RunnerOutcome::from_failure(RunnerFailure::EventLoopCreate));

        assert_eq!(decision.exit_code, ExitCode::FAILURE);
        assert_eq!(
            decision.stderr.as_deref(),
            Some(
                "FRD-WIN-FATAL-001 component=application operation=event_loop_create reason=event_loop_create_failed details=none\n"
            )
        );
    }
}
