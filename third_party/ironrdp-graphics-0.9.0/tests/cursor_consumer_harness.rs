//! 直接编译实际 pointer.rs 的 focused harness；无需改变 vendored Cargo/dev-dependencies。
#![allow(dead_code)]
pub use ironrdp_graphics::rle;
mod legacy_native {
    pub use frd_rdp_legacy_kernels::{CursorBitmapMode, CursorXorFormat, KernelError, NativeKernels};
}
mod color_conversion {
    include!("../../../crates/frd-rdp-legacy-kernels/tests/cursor_original/rgb565_original.rs");
}
#[path = "../src/pointer.rs"]
mod pointer;
