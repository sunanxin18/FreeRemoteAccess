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

    use super::{ConnectPlan, SessionCoordinator, WriterGate};

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
}
