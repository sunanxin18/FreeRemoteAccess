//! 单协议会话的选择、运行与输入世代门禁。

mod coordinator;

pub use coordinator::{
    CleanupComplete, CleanupError, CleanupOperations, ConnectPlan, SessionCleanupHandle,
    SessionCoordinator, WriterGate,
};
pub use frd_protocol_api::{
    PresentationEvent, ServerIdentityChallenge, ServerIdentityDecision, SessionCommand,
    SessionEvent,
};
