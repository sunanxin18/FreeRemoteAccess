//! 可选 FFmpeg 客户端解码插件的产品侧边界。

pub mod abi;
mod loader;

pub use loader::FfmpegBackend;
