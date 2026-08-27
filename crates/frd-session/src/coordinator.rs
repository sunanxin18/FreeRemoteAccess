use std::sync::atomic::{AtomicU64, Ordering};

use frd_core::{Endpoint, SessionId, TargetSystem};
use frd_protocol_api::{ProtocolCatalog, ProtocolError, ProtocolId, ProtocolSelection};

/// 连接选择在调用 factory 前完成，避免错误组合创建 worker。
pub struct ConnectPlan {
    session_id: SessionId,
    target: TargetSystem,
    protocol_id: ProtocolId,
    endpoint: Endpoint,
}

impl ConnectPlan {
    pub fn for_session(
        session_id: SessionId,
        target: TargetSystem,
        protocol_id: ProtocolId,
        endpoint: Endpoint,
    ) -> Self {
        Self {
            session_id,
            target,
            protocol_id,
            endpoint,
        }
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
}

pub struct SessionCoordinator {
    catalog: ProtocolCatalog,
    factory_creations: usize,
    active: Option<ActiveSessionResources>,
}

/// 协调器执行资源回收的失败步骤；不携带协议或平台实现细节。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupError {
    NoActiveSession,
    WrongSessionHandle,
    Cancel,
    ShutdownWriter,
    JoinWorkersAndAudio,
    DisposeMailbox,
}

/// 会话资源回收的协议中立操作。所有步骤都必须完成才能释放活动会话槽。
pub trait CleanupOperations {
    fn cancel(&mut self) -> Result<(), CleanupError>;
    fn shutdown_writer(&mut self) -> Result<(), CleanupError>;
    fn join_workers_and_audio(&mut self) -> Result<(), CleanupError>;
    fn dispose_mailbox(&mut self) -> Result<(), CleanupError>;
}

static NEXT_CLEANUP_HANDLE_ID: AtomicU64 = AtomicU64::new(1);

/// 仅由一次成功 `start` 签发；不可复制，且不能由调用者构造或改写 session 绑定。
pub struct SessionCleanupHandle {
    session_id: SessionId,
    handle_id: u64,
}

struct ActiveSessionResources {
    session_id: SessionId,
    handle_id: u64,
    cleanup_ops: Box<dyn CleanupOperations>,
    progress: CleanupProgress,
}

#[derive(Default)]
struct CleanupProgress {
    cancel: bool,
    shutdown_writer: bool,
    join_workers_and_audio: bool,
    dispose_mailbox: bool,
}

/// 仅由 `SessionCoordinator` 在完整资源回收后签发的会话释放能力。
#[derive(Debug, Eq, PartialEq)]
pub struct CleanupComplete {
    session_id: SessionId,
}

impl CleanupComplete {
    fn new(session_id: SessionId) -> Self {
        Self { session_id }
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }
}

impl SessionCoordinator {
    pub fn new(catalog: ProtocolCatalog) -> Self {
        Self {
            catalog,
            factory_creations: 0,
            active: None,
        }
    }

    /// 先完成无副作用预检和单会话门禁，再创建并独占该 session 的清理资源。
    pub fn start<F>(
        &mut self,
        plan: ConnectPlan,
        create_resources: F,
    ) -> Result<SessionCleanupHandle, ProtocolError>
    where
        F: FnOnce() -> Box<dyn CleanupOperations>,
    {
        if self.active.is_some() {
            return Err(ProtocolError::Terminal);
        }
        let _ = plan.endpoint();
        self.catalog
            .select(plan.target, ProtocolSelection::Explicit(plan.protocol_id))?;
        let handle_id = NEXT_CLEANUP_HANDLE_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .expect("session cleanup handle ID 已耗尽");
        let cleanup_ops = create_resources();
        self.factory_creations = self
            .factory_creations
            .checked_add(1)
            .expect("factory 创建计数溢出");
        self.active = Some(ActiveSessionResources {
            session_id: plan.session_id,
            handle_id,
            cleanup_ops,
            progress: CleanupProgress::default(),
        });
        Ok(SessionCleanupHandle {
            session_id: plan.session_id,
            handle_id,
        })
    }

    pub fn factory_creations(&self) -> usize {
        self.factory_creations
    }

    /// 只清理与本 coordinator 成功 start 绑定的资源；已成功的步骤不会在重试时重复。
    pub fn complete_cleanup(
        &mut self,
        handle: &SessionCleanupHandle,
    ) -> Result<CleanupComplete, CleanupError> {
        let active = self.active.as_mut().ok_or(CleanupError::NoActiveSession)?;
        if active.session_id != handle.session_id || active.handle_id != handle.handle_id {
            return Err(CleanupError::WrongSessionHandle);
        }

        if !active.progress.cancel {
            active.cleanup_ops.cancel()?;
            active.progress.cancel = true;
        }
        if !active.progress.shutdown_writer {
            active.cleanup_ops.shutdown_writer()?;
            active.progress.shutdown_writer = true;
        }
        if !active.progress.join_workers_and_audio {
            active.cleanup_ops.join_workers_and_audio()?;
            active.progress.join_workers_and_audio = true;
        }
        if !active.progress.dispose_mailbox {
            active.cleanup_ops.dispose_mailbox()?;
            active.progress.dispose_mailbox = true;
        }

        let session_id = active.session_id;
        self.active = None;
        Ok(CleanupComplete::new(session_id))
    }
}

/// 协议 writer 的会话/世代过滤器。generation 前进后旧输入即刻失效。
pub struct WriterGate {
    session_id: SessionId,
    generation: u64,
}

impl WriterGate {
    pub fn new(session_id: SessionId, generation: u64) -> Self {
        assert!(generation != 0, "writer generation 必须大于零");
        Self {
            session_id,
            generation,
        }
    }

    pub fn advance_generation(&mut self, generation: u64) -> bool {
        if generation <= self.generation {
            return false;
        }
        self.generation = generation;
        true
    }

    pub fn accepts(&self, session_id: SessionId, generation: u64) -> bool {
        self.session_id == session_id && self.generation == generation
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use frd_core::SessionId;
    use frd_protocol_api::{Endpoint, ProtocolCatalog, ProtocolId, TargetSystem};

    use super::{CleanupError, CleanupOperations, ConnectPlan, SessionCoordinator, WriterGate};

    #[test]
    fn invalid_target_protocol_is_rejected_before_factory_creation() {
        let mut coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let request = ConnectPlan::for_session(
            SessionId::allocate(),
            TargetSystem::Windows,
            ProtocolId::apple_hpss_mvs(),
            Endpoint::new("host.example", 3389).expect("valid endpoint"),
        );
        let log = Arc::new(Mutex::new(Vec::new()));

        assert!(coordinator
            .start(request, {
                let log = log.clone();
                move || Box::new(SharedCleanup::new(log))
            })
            .is_err());
        assert_eq!(coordinator.factory_creations(), 0);
        assert!(log.lock().expect("log lock").is_empty());
    }

    #[test]
    fn stale_session_input_is_rejected_by_writer_gate() {
        let current = SessionId::allocate();
        let stale = SessionId::allocate();
        let mut gate = WriterGate::new(current, 2);

        assert!(!gate.accepts(stale, 2));
        assert!(!gate.accepts(current, 1));
        assert!(gate.accepts(current, 2));
        assert!(gate.advance_generation(3));
        assert!(!gate.accepts(current, 2));
    }

    #[test]
    fn complete_cleanup_runs_every_resource_step_in_order_before_issuing_capability() {
        let session_id = SessionId::allocate();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let handle = coordinator
            .start(connect_plan(session_id), {
                let log = log.clone();
                move || Box::new(SharedCleanup::new(log))
            })
            .expect("session starts");

        let completion = coordinator
            .complete_cleanup(&handle)
            .expect("all cleanup steps succeed");

        assert_eq!(completion.session_id(), session_id);
        assert_eq!(
            *log.lock().expect("log lock"),
            vec![
                CleanupError::Cancel,
                CleanupError::ShutdownWriter,
                CleanupError::JoinWorkersAndAudio,
                CleanupError::DisposeMailbox,
            ]
        );
    }

    #[test]
    fn failed_cleanup_stops_before_dependent_steps_without_issuing_capability() {
        let session_id = SessionId::allocate();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let handle = coordinator
            .start(connect_plan(session_id), {
                let log = log.clone();
                move || {
                    Box::new(SharedCleanup::failing_once(
                        log,
                        CleanupError::JoinWorkersAndAudio,
                    ))
                }
            })
            .expect("session starts");

        assert!(matches!(
            coordinator.complete_cleanup(&handle),
            Err(CleanupError::JoinWorkersAndAudio)
        ));
        assert_eq!(
            *log.lock().expect("log lock"),
            vec![
                CleanupError::Cancel,
                CleanupError::ShutdownWriter,
                CleanupError::JoinWorkersAndAudio,
            ]
        );
    }

    #[test]
    fn cleanup_before_start_is_rejected_without_touching_bound_resources() {
        let session_id = SessionId::allocate();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut owner =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let handle = owner
            .start(connect_plan(session_id), {
                let log = log.clone();
                move || Box::new(SharedCleanup::new(log))
            })
            .expect("owner starts the session");
        let mut never_started = SessionCoordinator::new(ProtocolCatalog::new([]));

        assert_eq!(
            never_started.complete_cleanup(&handle),
            Err(CleanupError::NoActiveSession)
        );
        assert!(log.lock().expect("log lock").is_empty());
    }

    #[test]
    fn cleanup_handle_from_another_start_or_session_is_rejected() {
        let first = SessionId::allocate();
        let other = SessionId::allocate();
        let first_log = Arc::new(Mutex::new(Vec::new()));
        let other_log = Arc::new(Mutex::new(Vec::new()));
        let same_id_other_start_log = Arc::new(Mutex::new(Vec::new()));
        let mut first_coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let mut other_coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let mut same_id_other_coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let _first_handle = first_coordinator
            .start(connect_plan(first), {
                let first_log = first_log.clone();
                move || Box::new(SharedCleanup::new(first_log))
            })
            .expect("first starts");
        let other_handle = other_coordinator
            .start(connect_plan(other), {
                let other_log = other_log.clone();
                move || Box::new(SharedCleanup::new(other_log))
            })
            .expect("other starts");
        let same_id_other_handle = same_id_other_coordinator
            .start(connect_plan(first), {
                let same_id_other_start_log = same_id_other_start_log.clone();
                move || Box::new(SharedCleanup::new(same_id_other_start_log))
            })
            .expect("same session ID starts under another coordinator");

        assert_eq!(
            first_coordinator.complete_cleanup(&other_handle),
            Err(CleanupError::WrongSessionHandle)
        );
        assert_eq!(
            first_coordinator.complete_cleanup(&same_id_other_handle),
            Err(CleanupError::WrongSessionHandle)
        );
        assert!(first_log.lock().expect("first log").is_empty());
        assert!(other_log.lock().expect("other log").is_empty());
        assert!(same_id_other_start_log
            .lock()
            .expect("same ID other start log")
            .is_empty());
    }

    #[test]
    fn cleanup_completion_is_issued_once_for_the_started_resources() {
        let session_id = SessionId::allocate();
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let handle = coordinator
            .start(connect_plan(session_id), {
                let log = log.clone();
                move || Box::new(SharedCleanup::new(log))
            })
            .expect("session starts");

        let completion = coordinator
            .complete_cleanup(&handle)
            .expect("cleanup succeeds once");
        assert_eq!(completion.session_id(), session_id);
        assert_eq!(
            coordinator.complete_cleanup(&handle),
            Err(CleanupError::NoActiveSession)
        );
        assert_eq!(log.lock().expect("log lock").len(), 4);
    }

    #[test]
    fn failed_cleanup_keeps_session_and_resources_bound_until_retry_succeeds() {
        let first = SessionId::allocate();
        let second = SessionId::allocate();
        let log = Arc::new(Mutex::new(Vec::new()));
        let second_resource_creations = Arc::new(AtomicUsize::new(0));
        let mut coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let handle = coordinator
            .start(connect_plan(first), {
                let log = log.clone();
                move || {
                    Box::new(SharedCleanup::failing_once(
                        log,
                        CleanupError::ShutdownWriter,
                    ))
                }
            })
            .expect("first session starts");

        assert_eq!(
            coordinator.complete_cleanup(&handle),
            Err(CleanupError::ShutdownWriter)
        );
        assert!(coordinator
            .start(connect_plan(second), {
                let creations = second_resource_creations.clone();
                move || {
                    creations.fetch_add(1, Ordering::Relaxed);
                    Box::new(SharedCleanup::new(Arc::new(Mutex::new(Vec::new()))))
                }
            })
            .is_err());
        assert_eq!(second_resource_creations.load(Ordering::Relaxed), 0);
        assert!(coordinator.complete_cleanup(&handle).is_ok());
        assert!(coordinator
            .start(connect_plan(second), {
                let creations = second_resource_creations.clone();
                move || {
                    creations.fetch_add(1, Ordering::Relaxed);
                    Box::new(SharedCleanup::new(Arc::new(Mutex::new(Vec::new()))))
                }
            })
            .is_ok());
        assert_eq!(second_resource_creations.load(Ordering::Relaxed), 1);
    }

    fn connect_plan(session_id: SessionId) -> ConnectPlan {
        ConnectPlan::for_session(
            session_id,
            TargetSystem::MacOs,
            ProtocolId::apple_hpss_mvs(),
            Endpoint::new("host.example", 5900).expect("valid endpoint"),
        )
    }

    struct SharedCleanup {
        calls: Arc<Mutex<Vec<CleanupError>>>,
        fail_once: Option<CleanupError>,
    }

    impl SharedCleanup {
        fn new(calls: Arc<Mutex<Vec<CleanupError>>>) -> Self {
            Self {
                calls,
                fail_once: None,
            }
        }

        fn failing_once(calls: Arc<Mutex<Vec<CleanupError>>>, failure: CleanupError) -> Self {
            Self {
                calls,
                fail_once: Some(failure),
            }
        }

        fn run(&mut self, step: CleanupError) -> Result<(), CleanupError> {
            self.calls.lock().expect("log lock").push(step);
            if self.fail_once == Some(step) {
                self.fail_once = None;
                Err(step)
            } else {
                Ok(())
            }
        }
    }

    impl CleanupOperations for SharedCleanup {
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
