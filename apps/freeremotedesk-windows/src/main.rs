mod cli;

use std::{io::Write, process::ExitCode, sync::Arc};

use clap::{error::ErrorKind, Parser};
use frd_app::{AppLaunch, ProductPolicy};
use frd_media_api::{AudioOutput, AudioOutputError};
use frd_platform_api::PlatformCapabilities;
use frd_platform_windows::{
    DpapiServerIdentityStore, EnvironmentCredentialProvider, WindowsAudioOutput,
    WindowsSingleInstanceError, WindowsSingleInstanceGuard,
};
use frd_protocol_api::{ProtocolCatalog, ProtocolFactory};
use frd_protocol_apple::AppleProtocolFactory;
use frd_shell_desktop::{
    AudioOutputFactory, DesktopApplication, DesktopUserEvent, FatalComponent, FatalOperation,
    FatalReason, FatalReport,
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
    EventLoopRun,
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
        let run_result = event_loop.run_app(&mut application);
        return finish_event_loop(run_result, application.runner_result());
    }

    let factory = Arc::new(AppleProtocolFactory) as Arc<dyn ProtocolFactory>;
    let catalog = ProtocolCatalog::new([factory.descriptor().id]);
    let provider = EnvironmentCredentialProvider;
    let launch_options = match cli.launch_options() {
        Ok(options) => options,
        Err(_) => return RunnerOutcome::from_failure(RunnerFailure::CommandLineOptions),
    };
    let mut launch = AppLaunch::new(launch_options, &provider, &catalog);
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
    let store = match DpapiServerIdentityStore::current_user_default() {
        Ok(store) => Arc::new(store),
        Err(_) => return RunnerOutcome::from_failure(RunnerFailure::IdentityStore),
    };
    let mut application = DesktopApplication::new_product(
        launch,
        [factory],
        store,
        Arc::new(WindowsAudioFactory),
        proxy,
    );
    let run_result = event_loop.run_app(&mut application);
    finish_event_loop(run_result, application.runner_result())
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
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }
    decision.exit_code
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;

    use frd_shell_desktop::{FatalComponent, FatalOperation, FatalReason, FatalReport};

    use super::{finish_event_loop, runner_decision, RunnerFailure, RunnerOutcome};

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
