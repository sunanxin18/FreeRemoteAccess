//! 与协议、平台和渲染器无关的帧传递契约。

mod mailbox;
mod surface;
mod transaction;

pub use mailbox::{EnqueuedSurfaceUpdate, FrameMailbox, PushOutcome};
pub use surface::{FrameCompleteness, PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate};
pub use transaction::{
    FrameReset, FrameRevision, FrameTransaction, FrameTransactionCompiler, FrameTransactionError,
};
