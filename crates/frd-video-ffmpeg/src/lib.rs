//! 可选 FFmpeg 客户端解码插件的产品侧边界。

pub mod abi;
#[cfg(target_os = "linux")]
mod linux_bundle;
mod loader;
#[cfg(target_os = "macos")]
mod macos_bundle;
#[cfg(windows)]
mod trusted_path;

pub use loader::FfmpegBackend;
