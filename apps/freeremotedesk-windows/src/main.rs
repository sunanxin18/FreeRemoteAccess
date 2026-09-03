#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cli;

use std::{process::ExitCode, sync::Arc};

#[cfg(not(all(windows, not(debug_assertions))))]
use std::io::Write;

use clap::{error::ErrorKind, Parser};
use frd_app::{AppLaunch, ProductPolicy};
use frd_media_api::{AudioOutput, AudioOutputError};
use frd_platform_api::{
    ConnectionProfileStore, PlatformCapabilities, SecureCredentialStore, ServerIdentityStore,
};
use frd_platform_windows::{
    DpapiServerIdentityStore, EnvironmentCredentialProvider, WindowsAudioOutput,
    WindowsConnectionProfileStore, WindowsCredentialStore, WindowsSingleInstanceError,
    WindowsSingleInstanceGuard,
};
use frd_protocol_api::{ProtocolCatalog, ProtocolFactory};
use frd_protocol_apple::{AppleHighPerformanceProtocolFactory, AppleProtocolFactory};
use frd_protocol_rdp::RdpProtocolFactory;
use frd_shell_desktop::{
    AudioOutputFactory, DesktopApplication, DesktopPlatformStores, DesktopUserEvent,
    DesktopWindowConfiguration, FatalComponent, FatalOperation, FatalReason, FatalReport,
};
#[cfg(all(windows, not(debug_assertions)))]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND,
};
use winit::event_loop::{ControlFlow, EventLoop};

use crate::cli::Cli;

struct WindowsAudioFactory;

impl AudioOutputFactory for WindowsAudioFactory {
    fn open(&self) -> std::result::Result<Box<dyn AudioOutput>, AudioOutputError> {
        WindowsAudioOutput::open_default().map(|output| Box::new(output) as Box<dyn AudioOutput>)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RunnerOutcome {
    Success,
    Fatal(FatalReport),
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
            RunnerFailure::WindowIcon => {
                (FatalOperation::Initialize, FatalReason::WindowChromeFailed)
            }
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
    WindowIcon,
}

const WINDOW_ICON_RGBA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/app-icon/windows/window-icon-64.rgba"
));

fn product_window_configuration() -> Result<DesktopWindowConfiguration, RunnerFailure> {
    let icon = winit::window::Icon::from_rgba(WINDOW_ICON_RGBA.to_vec(), 64, 64)
        .map_err(|_| RunnerFailure::WindowIcon)?;
    Ok(DesktopWindowConfiguration { icon: Some(icon) })
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

    emit_runner_outcome(run(cli))
}

fn run(cli: Cli) -> RunnerOutcome {
    let _single_instance =
        match WindowsSingleInstanceGuard::acquire_for_product("freeremotedesk-windows-product") {
            Ok(guard) => guard,
            Err(WindowsSingleInstanceError::AlreadyRunning) => {
                return RunnerOutcome::from_failure(RunnerFailure::SingleInstanceAlreadyRunning)
            }
            Err(WindowsSingleInstanceError::Unavailable) => {
                return RunnerOutcome::from_failure(RunnerFailure::SingleInstanceUnavailable)
            }
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
    let rdp_factory = Arc::new(RdpProtocolFactory) as Arc<dyn ProtocolFactory>;
    let factories = [apple_high_performance_factory, apple_factory, rdp_factory];
    let catalog = ProtocolCatalog::new(factories.iter().map(|factory| factory.descriptor().id));
    let provider = EnvironmentCredentialProvider;
    let launch_options = match cli.launch_options() {
        Ok(options) => options,
        Err(_) => return RunnerOutcome::from_failure(RunnerFailure::CommandLineOptions),
    };
    let server_identities = match DpapiServerIdentityStore::current_user_default() {
        Ok(store) => Arc::new(store) as Arc<dyn ServerIdentityStore>,
        Err(_) => return RunnerOutcome::from_failure(RunnerFailure::IdentityStore),
    };
    let profiles = match WindowsConnectionProfileStore::current_user_default() {
        Ok(store) => Arc::new(store) as Arc<dyn ConnectionProfileStore>,
        Err(_) => return RunnerOutcome::from_failure(RunnerFailure::IdentityStore),
    };
    let credentials = Arc::new(WindowsCredentialStore::new());
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
        Arc::new(WindowsAudioFactory),
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

#[cfg(all(windows, not(debug_assertions)))]
fn emit_fatal_report(line: &str) {
    let text = line.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let title = "FreeRemoteDesk 启动失败"
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();

    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

#[cfg(not(all(windows, not(debug_assertions))))]
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
        finish_event_loop, product_window_configuration, purge_pending_credentials,
        runner_decision, RunnerFailure, RunnerOutcome, WINDOW_ICON_RGBA,
    };

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
    fn window_icon_uses_exact_64_pixel_rgba_asset() {
        assert_eq!(WINDOW_ICON_RGBA.len(), 64 * 64 * 4);
        assert!(product_window_configuration().unwrap().icon.is_some());
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
