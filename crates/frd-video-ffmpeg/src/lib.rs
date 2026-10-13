//! 可选 FFmpeg 客户端解码插件的产品侧边界。

pub mod abi;
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
mod linux_bundle;
mod loader;
#[cfg(target_os = "macos")]
mod macos_bundle;
#[cfg(any(all(target_env = "ohos", target_arch = "aarch64"), all(test, unix)))]
mod ohos_bundle;
#[cfg(windows)]
mod trusted_path;

pub use loader::FfmpegBackend;
