use std::fmt;
use std::sync::Arc;

use frd_core::{SessionId, TargetSystem};
use frd_protocol_api::{ConnectRequest, ProtocolCatalog, ProtocolError, ProtocolSelection};

pub struct SessionCoordinator {
    catalog: ProtocolCatalog,
    successful_launches: usize,
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

struct SessionStartIdentity {
    session_id: SessionId,
}

/// app 活动槽独占持有的 reservation owner；外部不能构造或复制其身份。
pub struct SessionStartOwner {
    identity: Arc<SessionStartIdentity>,
}

/// `AppAction::StartSession` 一次性移交给 Task 11 composition root 的 start capability。
pub struct SessionStartPermit {
    identity: Arc<SessionStartIdentity>,
}

/// 为一个 app 活动槽签发彼此配对、但职责不同的 owner/permit。
///
/// 任意另一轮调用即使复用相同 `SessionId`，也会获得不同的 opaque identity。
pub fn reserve_session_start(session_id: SessionId) -> (SessionStartOwner, SessionStartPermit) {
    let identity = Arc::new(SessionStartIdentity { session_id });
    (
        SessionStartOwner {
            identity: identity.clone(),
        },
        SessionStartPermit { identity },
    )
}

impl SessionStartPermit {
    fn session_id(&self) -> SessionId {
        self.identity.session_id
    }
}

/// Task 11 composition boundary：实现方选择中立 request 对应的 concrete factory/runtime，
/// 启动 worker 并返回其完整 cleanup bundle。返回 `Err` 前必须回收本次构造的任何部分资源。
pub trait SessionLauncher {
    fn launch(self, request: ConnectRequest) -> Result<Box<dyn CleanupOperations>, ProtocolError>;
}

impl<F> SessionLauncher for F
where
    F: FnOnce(ConnectRequest) -> Result<Box<dyn CleanupOperations>, ProtocolError>,
{
    fn launch(self, request: ConnectRequest) -> Result<Box<dyn CleanupOperations>, ProtocolError> {
        self(request)
    }
}

/// 启动失败不会签发 cleanup handle；permit 退还给 composition root 以便安全重试。
pub struct SessionStartFailure {
    error: ProtocolError,
    permit: SessionStartPermit,
}

impl SessionStartFailure {
    pub fn error(&self) -> &ProtocolError {
        &self.error
    }

    pub fn into_permit(self) -> SessionStartPermit {
        self.permit
    }
}

impl fmt::Debug for SessionStartFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionStartFailure")
            .field("error", &self.error)
            .field("session_id", &self.permit.session_id())
            .finish_non_exhaustive()
    }
}

/// 仅由一次成功 `start` 签发；不可复制，且不能由调用者构造或改写 session 绑定。
pub struct SessionCleanupHandle {
    identity: Arc<SessionStartIdentity>,
}

struct ActiveSessionResources {
    identity: Arc<SessionStartIdentity>,
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
pub struct CleanupComplete {
    identity: Arc<SessionStartIdentity>,
}

impl CleanupComplete {
    pub fn session_id(&self) -> SessionId {
        self.identity.session_id
    }

    pub fn matches_owner(&self, owner: &SessionStartOwner) -> bool {
        Arc::ptr_eq(&self.identity, &owner.identity)
    }
}

impl fmt::Debug for CleanupComplete {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CleanupComplete")
            .field("session_id", &self.session_id())
            .finish_non_exhaustive()
    }
}

impl PartialEq for CleanupComplete {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
    }
}

impl Eq for CleanupComplete {}

impl SessionCoordinator {
    pub fn new(catalog: ProtocolCatalog) -> Self {
        Self {
            catalog,
            successful_launches: 0,
            active: None,
        }
    }

    /// 先完成 reservation、catalog 和单会话门禁，再通过 Task 11 launcher 创建资源。
    /// launcher 失败不会占用 coordinator，也不会签发 handle/completion。
    pub fn start<L>(
        &mut self,
        permit: SessionStartPermit,
        target: TargetSystem,
        request: ConnectRequest,
        launcher: L,
    ) -> Result<SessionCleanupHandle, SessionStartFailure>
    where
        L: SessionLauncher,
    {
        if self.active.is_some() {
            return Err(SessionStartFailure {
                error: ProtocolError::Terminal,
                permit,
            });
        }
        if permit.session_id() != request.session_id {
            return Err(SessionStartFailure {
                error: ProtocolError::StaleSession,
                permit,
            });
        }
        if let Err(error) = self.catalog.select(
            target,
            ProtocolSelection::Explicit(request.protocol_id.clone()),
        ) {
            return Err(SessionStartFailure { error, permit });
        }
        let cleanup_ops = match launcher.launch(request) {
            Ok(cleanup_ops) => cleanup_ops,
            Err(error) => return Err(SessionStartFailure { error, permit }),
        };
        self.successful_launches = self
            .successful_launches
            .checked_add(1)
            .expect("会话启动计数溢出");
        let identity = permit.identity;
        self.active = Some(ActiveSessionResources {
            identity: identity.clone(),
            cleanup_ops,
            progress: CleanupProgress::default(),
        });
        Ok(SessionCleanupHandle { identity })
    }

    pub fn successful_launches(&self) -> usize {
        self.successful_launches
    }

    /// 只清理与本 coordinator 成功 start 绑定的资源；已成功的步骤不会在重试时重复。
    pub fn complete_cleanup(
        &mut self,
        handle: &SessionCleanupHandle,
    ) -> Result<CleanupComplete, CleanupError> {
        let active = self.active.as_mut().ok_or(CleanupError::NoActiveSession)?;
        if !Arc::ptr_eq(&active.identity, &handle.identity) {
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

        let identity = active.identity.clone();
        self.active = None;
        Ok(CleanupComplete { identity })
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
    use frd_protocol_api::{
        ConnectRequest, Endpoint, ProtocolCatalog, ProtocolError, ProtocolId, TargetSystem,
    };

    use super::{
        reserve_session_start, CleanupError, CleanupOperations, SessionCoordinator,
        SessionLauncher, SessionStartFailure, WriterGate,
    };

    #[test]
    fn launcher_failure_issues_no_handle_and_leaves_coordinator_reusable() {
        let session_id = SessionId::allocate();
        let (_owner, permit) = reserve_session_start(session_id);
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));

        let failure = match coordinator.start(
            permit,
            TargetSystem::MacOs,
            connect_request(session_id),
            |_| Err(ProtocolError::Terminal),
        ) {
            Ok(_) => panic!("failed launcher must not sign a cleanup handle"),
            Err(failure) => failure,
        };
        assert_eq!(failure.error(), &ProtocolError::Terminal);
        assert_eq!(coordinator.successful_launches(), 0);

        let handle = coordinator
            .start(
                failure.into_permit(),
                TargetSystem::MacOs,
                connect_request(session_id),
                {
                    let log = log.clone();
                    move |_| Ok(Box::new(SharedCleanup::new(log)) as Box<dyn CleanupOperations>)
                },
            )
            .expect("the same reserved start can retry after launcher rollback");
        assert_eq!(coordinator.successful_launches(), 1);
        assert!(coordinator.complete_cleanup(&handle).is_ok());
    }

    #[test]
    fn invalid_target_protocol_is_rejected_before_launcher_invocation() {
        let mut coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let session_id = SessionId::allocate();
        let log = Arc::new(Mutex::new(Vec::new()));

        assert!(
            start_session(&mut coordinator, session_id, TargetSystem::Windows, {
                let log = log.clone();
                move |_| Ok(Box::new(SharedCleanup::new(log)) as Box<dyn CleanupOperations>)
            })
            .is_err()
        );
        assert_eq!(coordinator.successful_launches(), 0);
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
        let handle = start_session(&mut coordinator, session_id, TargetSystem::MacOs, {
            let log = log.clone();
            move |_| Ok(Box::new(SharedCleanup::new(log)) as Box<dyn CleanupOperations>)
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
        let handle = start_session(&mut coordinator, session_id, TargetSystem::MacOs, {
            let log = log.clone();
            move |_| {
                Ok(Box::new(SharedCleanup::failing_once(
                    log,
                    CleanupError::JoinWorkersAndAudio,
                )) as Box<dyn CleanupOperations>)
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
        let handle = start_session(&mut owner, session_id, TargetSystem::MacOs, {
            let log = log.clone();
            move |_| Ok(Box::new(SharedCleanup::new(log)) as Box<dyn CleanupOperations>)
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
        let _first_handle = start_session(&mut first_coordinator, first, TargetSystem::MacOs, {
            let first_log = first_log.clone();
            move |_| Ok(Box::new(SharedCleanup::new(first_log)) as Box<dyn CleanupOperations>)
        })
        .expect("first starts");
        let other_handle = start_session(&mut other_coordinator, other, TargetSystem::MacOs, {
            let other_log = other_log.clone();
            move |_| Ok(Box::new(SharedCleanup::new(other_log)) as Box<dyn CleanupOperations>)
        })
        .expect("other starts");
        let same_id_other_handle = start_session(
            &mut same_id_other_coordinator,
            first,
            TargetSystem::MacOs,
            {
                let same_id_other_start_log = same_id_other_start_log.clone();
                move |_| {
                    Ok(Box::new(SharedCleanup::new(same_id_other_start_log))
                        as Box<dyn CleanupOperations>)
                }
            },
        )
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
        let handle = start_session(&mut coordinator, session_id, TargetSystem::MacOs, {
            let log = log.clone();
            move |_| Ok(Box::new(SharedCleanup::new(log)) as Box<dyn CleanupOperations>)
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
        let handle = start_session(&mut coordinator, first, TargetSystem::MacOs, {
            let log = log.clone();
            move |_| {
                Ok(Box::new(SharedCleanup::failing_once(
                    log,
                    CleanupError::ShutdownWriter,
                )) as Box<dyn CleanupOperations>)
            }
        })
        .expect("first session starts");

        assert_eq!(
            coordinator.complete_cleanup(&handle),
            Err(CleanupError::ShutdownWriter)
        );
        assert!(coordinator
            .start(
                reserve_session_start(second).1,
                TargetSystem::MacOs,
                connect_request(second),
                {
                    let creations = second_resource_creations.clone();
                    move |_| {
                        creations.fetch_add(1, Ordering::Relaxed);
                        Ok(
                            Box::new(SharedCleanup::new(Arc::new(Mutex::new(Vec::new()))))
                                as Box<dyn CleanupOperations>,
                        )
                    }
                },
            )
            .is_err());
        assert_eq!(second_resource_creations.load(Ordering::Relaxed), 0);
        assert!(coordinator.complete_cleanup(&handle).is_ok());
        assert!(coordinator
            .start(
                reserve_session_start(second).1,
                TargetSystem::MacOs,
                connect_request(second),
                {
                    let creations = second_resource_creations.clone();
                    move |_| {
                        creations.fetch_add(1, Ordering::Relaxed);
                        Ok(
                            Box::new(SharedCleanup::new(Arc::new(Mutex::new(Vec::new()))))
                                as Box<dyn CleanupOperations>,
                        )
                    }
                },
            )
            .is_ok());
        assert_eq!(second_resource_creations.load(Ordering::Relaxed), 1);
    }

    fn connect_request(session_id: SessionId) -> ConnectRequest {
        ConnectRequest {
            session_id,
            endpoint: Endpoint::new("host.example", 5900).expect("valid endpoint"),
            protocol_id: ProtocolId::apple_hpss_mvs(),
            credentials: None,
            saved_server_pin: None,
        }
    }

    fn start_session<L>(
        coordinator: &mut SessionCoordinator,
        session_id: SessionId,
        target: TargetSystem,
        launcher: L,
    ) -> Result<super::SessionCleanupHandle, SessionStartFailure>
    where
        L: SessionLauncher,
    {
        coordinator.start(
            reserve_session_start(session_id).1,
            target,
            connect_request(session_id),
            launcher,
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
