//! 与协议、平台和渲染器无关的帧传递契约。

mod mailbox;
mod surface;

pub use mailbox::{FrameMailbox, PushOutcome};
pub use surface::{FrameCompleteness, PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate};
