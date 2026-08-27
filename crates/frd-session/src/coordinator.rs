use frd_core::{Endpoint, SessionId, TargetSystem};
use frd_protocol_api::{ProtocolCatalog, ProtocolError, ProtocolId, ProtocolSelection};

/// 连接选择在调用 factory 前完成，避免错误组合创建 worker。
pub struct ConnectPlan {
    target: TargetSystem,
    protocol_id: ProtocolId,
    endpoint: Endpoint,
}

impl ConnectPlan {
    pub fn for_target(target: TargetSystem, protocol_id: ProtocolId, endpoint: Endpoint) -> Self {
        Self {
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
}

/// 协调器执行资源回收的失败步骤；不携带协议或平台实现细节。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupError {
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

/// 仅由 `SessionCoordinator` 在完整资源回收后签发的会话释放能力。
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
        }
    }

    /// 当前最小协调器只进行无副作用预检；后续 worker 创建必须先经过此门。
    pub fn start(&mut self, plan: ConnectPlan) -> Result<(), ProtocolError> {
        let _ = plan.endpoint();
        self.catalog
            .select(plan.target, ProtocolSelection::Explicit(plan.protocol_id))?;
        self.factory_creations = self
            .factory_creations
            .checked_add(1)
            .expect("factory 创建计数溢出");
        Ok(())
    }

    pub fn factory_creations(&self) -> usize {
        self.factory_creations
    }

    /// 依序尝试所有回收步骤。只有每一步都成功时才签发不可伪造的完成能力。
    pub fn complete_cleanup(
        &mut self,
        session_id: SessionId,
        cleanup_ops: &mut dyn CleanupOperations,
    ) -> Result<CleanupComplete, CleanupError> {
        let cancel = cleanup_ops.cancel();
        let shutdown_writer = cleanup_ops.shutdown_writer();
        let join_workers_and_audio = cleanup_ops.join_workers_and_audio();
        let dispose_mailbox = cleanup_ops.dispose_mailbox();

        [
            cancel,
            shutdown_writer,
            join_workers_and_audio,
            dispose_mailbox,
        ]
        .into_iter()
        .find_map(Result::err)
        .map_or_else(|| Ok(CleanupComplete::new(session_id)), Err)
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
    use frd_core::SessionId;
    use frd_protocol_api::{Endpoint, ProtocolCatalog, ProtocolId, TargetSystem};

    use super::{CleanupError, CleanupOperations, ConnectPlan, SessionCoordinator, WriterGate};

    #[test]
    fn invalid_target_protocol_is_rejected_before_factory_creation() {
        let mut coordinator =
            SessionCoordinator::new(ProtocolCatalog::new([ProtocolId::apple_hpss_mvs()]));
        let request = ConnectPlan::for_target(
            TargetSystem::Windows,
            ProtocolId::apple_hpss_mvs(),
            Endpoint::new("host.example", 3389).expect("valid endpoint"),
        );

        assert!(coordinator.start(request).is_err());
        assert_eq!(coordinator.factory_creations(), 0);
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
        let mut coordinator = SessionCoordinator::new(ProtocolCatalog::new([]));
        let mut cleanup = RecordingCleanup::default();

        let completion = coordinator
            .complete_cleanup(session_id, &mut cleanup)
            .expect("all cleanup steps succeed");

        assert_eq!(completion.session_id(), session_id);
        assert_eq!(
            cleanup.calls,
            vec![
                "cancel",
                "shutdown_writer",
                "join_workers_and_audio",
                "dispose_mailbox"
            ]
        );
    }

    #[test]
    fn failed_cleanup_runs_remaining_steps_without_issuing_capability() {
        let session_id = SessionId::allocate();
        let mut coordinator = SessionCoordinator::new(ProtocolCatalog::new([]));
        let mut cleanup = RecordingCleanup::failing_at(CleanupError::JoinWorkersAndAudio);

        assert!(matches!(
            coordinator.complete_cleanup(session_id, &mut cleanup),
            Err(CleanupError::JoinWorkersAndAudio)
        ));
        assert_eq!(
            cleanup.calls,
            vec![
                "cancel",
                "shutdown_writer",
                "join_workers_and_audio",
                "dispose_mailbox"
            ]
        );
    }

    #[derive(Default)]
    struct RecordingCleanup {
        calls: Vec<&'static str>,
        failure: Option<CleanupError>,
    }

    impl RecordingCleanup {
        fn failing_at(failure: CleanupError) -> Self {
            Self {
                calls: Vec::new(),
                failure: Some(failure),
            }
        }

        fn result(&self, error: CleanupError) -> Result<(), CleanupError> {
            (self.failure == Some(error))
                .then_some(error)
                .map_or(Ok(()), Err)
        }
    }

    impl CleanupOperations for RecordingCleanup {
        fn cancel(&mut self) -> Result<(), CleanupError> {
            self.calls.push("cancel");
            self.result(CleanupError::Cancel)
        }

        fn shutdown_writer(&mut self) -> Result<(), CleanupError> {
            self.calls.push("shutdown_writer");
            self.result(CleanupError::ShutdownWriter)
        }

        fn join_workers_and_audio(&mut self) -> Result<(), CleanupError> {
            self.calls.push("join_workers_and_audio");
            self.result(CleanupError::JoinWorkersAndAudio)
        }

        fn dispose_mailbox(&mut self) -> Result<(), CleanupError> {
            self.calls.push("dispose_mailbox");
            self.result(CleanupError::DisposeMailbox)
        }
    }
}
