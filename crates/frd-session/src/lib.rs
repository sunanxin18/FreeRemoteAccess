//! 单协议会话的选择、运行与输入世代门禁。

mod coordinator;

pub use coordinator::{
    reserve_session_start, CleanupComplete, CleanupError, CleanupOperations, SessionCleanupHandle,
    SessionCoordinator, SessionLauncher, SessionStartAbort, SessionStartFailure,
    SessionStartOutcome, SessionStartOwner, SessionStartPermit, WriterGate,
};
pub use frd_protocol_api::{
    PresentationEvent, ServerIdentityChallenge, ServerIdentityDecision, SessionCommand,
    SessionEvent,
};
