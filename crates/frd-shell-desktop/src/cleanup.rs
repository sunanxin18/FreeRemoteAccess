use std::time::Duration;

use frd_session::{CleanupComplete, CleanupError, SessionCleanupHandle, SessionCoordinator};

#[derive(Clone, Copy)]
pub(crate) struct CleanupPolicy {
    max_attempts: usize,
    retry_delay: Duration,
}

impl CleanupPolicy {
    pub(crate) fn new(max_attempts: usize, retry_delay: Duration) -> Self {
        assert!(max_attempts != 0, "cleanup policy requires one attempt");
        Self {
            max_attempts,
            retry_delay,
        }
    }
}

pub(crate) struct PendingCleanup {
    coordinator: SessionCoordinator,
    handle: SessionCleanupHandle,
}

impl PendingCleanup {
    pub(crate) fn new(coordinator: SessionCoordinator, handle: SessionCleanupHandle) -> Self {
        Self {
            coordinator,
            handle,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackgroundCleanupFailure {
    WorkerSpawn,
    Exhausted {
        last_error: CleanupError,
        attempts: usize,
    },
}

pub enum BackgroundCleanupOutcome {
    Complete {
        coordinator: SessionCoordinator,
        completion: CleanupComplete,
    },
    Fatal(BackgroundCleanupFailure),
}

pub(crate) fn spawn_cleanup(
    pending: PendingCleanup,
    policy: CleanupPolicy,
    notify: impl FnOnce(BackgroundCleanupOutcome) + Send + 'static,
) -> Result<(), BackgroundCleanupFailure> {
    std::thread::Builder::new()
        .name("frd-session-cleanup".to_owned())
        .spawn(move || notify(run_cleanup(pending, policy)))
        .map(|_| ())
        .map_err(|_| BackgroundCleanupFailure::WorkerSpawn)
}

fn run_cleanup(mut pending: PendingCleanup, policy: CleanupPolicy) -> BackgroundCleanupOutcome {
    for attempt in 1..=policy.max_attempts {
        match pending.coordinator.complete_cleanup(&pending.handle) {
            Ok(completion) => {
                return BackgroundCleanupOutcome::Complete {
                    coordinator: pending.coordinator,
                    completion,
                };
            }
            Err(last_error) if attempt == policy.max_attempts => {
                return BackgroundCleanupOutcome::Fatal(BackgroundCleanupFailure::Exhausted {
                    last_error,
                    attempts: attempt,
                });
            }
            Err(_) => std::thread::sleep(policy.retry_delay),
        }
    }
    unreachable!("cleanup policy always attempts at least once")
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    use frd_core::{Endpoint, ProtocolId, SessionId, TargetSystem};
    use frd_protocol_api::{ConnectRequest, ProtocolCatalog};
    use frd_session::{
        reserve_session_start, CleanupError, CleanupOperations, SessionCoordinator,
        SessionStartOutcome,
    };

    use super::{
        spawn_cleanup, BackgroundCleanupFailure, BackgroundCleanupOutcome, CleanupPolicy,
        PendingCleanup,
    };

    #[test]
    fn background_cleanup_retries_only_the_incomplete_step_then_returns_the_coordinator() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let session_id = SessionId::allocate();
        let pending = pending_cleanup(
            session_id,
            RecordingCleanup::fail_once(calls.clone(), CleanupError::ShutdownWriter),
        );
        let (outcome_tx, outcome_rx) = mpsc::channel();

        spawn_cleanup(
            pending,
            CleanupPolicy::new(3, Duration::ZERO),
            move |outcome| outcome_tx.send(outcome).unwrap(),
        )
        .expect("background cleanup worker starts");

        let outcome = outcome_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("bounded cleanup reports a typed outcome");
        let BackgroundCleanupOutcome::Complete {
            coordinator,
            completion,
        } = outcome
        else {
            panic!("one transient cleanup failure must recover");
        };
        assert_eq!(completion.session_id(), session_id);
        assert_eq!(coordinator.successful_launches(), 1);
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                CleanupError::Cancel,
                CleanupError::ShutdownWriter,
                CleanupError::ShutdownWriter,
                CleanupError::JoinWorkersAndAudio,
                CleanupError::DisposeMailbox,
            ]
        );
    }

    #[test]
    fn background_cleanup_reports_fatal_after_the_bounded_retry_policy() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let pending = pending_cleanup(
            SessionId::allocate(),
            RecordingCleanup::always_fail(calls.clone(), CleanupError::JoinWorkersAndAudio),
        );
        let (outcome_tx, outcome_rx) = mpsc::channel();

        spawn_cleanup(
            pending,
            CleanupPolicy::new(3, Duration::ZERO),
            move |outcome| outcome_tx.send(outcome).unwrap(),
        )
        .expect("background cleanup worker starts");

        let outcome = outcome_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("bounded cleanup cannot strand the caller");
        assert!(matches!(
            outcome,
            BackgroundCleanupOutcome::Fatal(BackgroundCleanupFailure::Exhausted {
                last_error: CleanupError::JoinWorkersAndAudio,
                attempts: 3,
            })
        ));
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                CleanupError::Cancel,
                CleanupError::ShutdownWriter,
                CleanupError::JoinWorkersAndAudio,
                CleanupError::JoinWorkersAndAudio,
                CleanupError::JoinWorkersAndAudio,
            ]
        );
    }

    fn pending_cleanup(session_id: SessionId, cleanup: RecordingCleanup) -> PendingCleanup {
        let mut coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let (_owner, permit) = reserve_session_start(session_id);
        let request = ConnectRequest {
            session_id,
            endpoint: Endpoint::new("test.invalid", 5900).unwrap(),
            protocol_id: ProtocolId::apple_hpss_mvs(),
            credentials: None,
            saved_server_pin: None,
        };
        let SessionStartOutcome::Started(handle) =
            coordinator.start(permit, TargetSystem::MacOs, request, move |_| {
                Ok(Box::new(cleanup) as Box<dyn CleanupOperations>)
            })
        else {
            panic!("test cleanup resource starts");
        };
        PendingCleanup::new(coordinator, handle)
    }

    struct RecordingCleanup {
        calls: Arc<Mutex<Vec<CleanupError>>>,
        failure: CleanupError,
        remaining_failures: Option<usize>,
    }

    impl RecordingCleanup {
        fn fail_once(calls: Arc<Mutex<Vec<CleanupError>>>, failure: CleanupError) -> Self {
            Self {
                calls,
                failure,
                remaining_failures: Some(1),
            }
        }

        fn always_fail(calls: Arc<Mutex<Vec<CleanupError>>>, failure: CleanupError) -> Self {
            Self {
                calls,
                failure,
                remaining_failures: None,
            }
        }

        fn run(&mut self, step: CleanupError) -> Result<(), CleanupError> {
            self.calls.lock().unwrap().push(step);
            if step != self.failure {
                return Ok(());
            }
            match self.remaining_failures.as_mut() {
                Some(remaining) if *remaining > 0 => {
                    *remaining -= 1;
                    Err(step)
                }
                Some(_) => Ok(()),
                None => Err(step),
            }
        }
    }

    impl CleanupOperations for RecordingCleanup {
        fn cancel(&mut self) -> Result<(), CleanupError> {
            self.run(CleanupError::Cancel)
        }

        fn shutdown_writer(&mut self) -> Result<(), CleanupError> {
            self.run(CleanupError::ShutdownWriter)
        }

        fn join_workers_and_audio(&mut self) -> Result<(), CleanupError> {
            self.run(CleanupError::JoinWorkersAndAudio)
        }

        fn dispose_mailbox(&mut self) -> Result<(), CleanupError> {
            self.run(CleanupError::DisposeMailbox)
        }
    }
}
