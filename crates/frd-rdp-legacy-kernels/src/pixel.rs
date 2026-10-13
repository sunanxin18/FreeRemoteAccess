//! 共用的四像素布局循环；数值、swizzle、copy 与 palette lookup 都进入 selected ISA。
use crate::{KernelError, Pixel32Format, PixelRowSource};
#[cfg(target_arch = "aarch64")]
#[path = "pixel_neon.rs"]
mod arch;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[path = "pixel_x86.rs"]
mod arch;
pub(super) unsafe fn row(
    format: PixelRowSource,
    dst: Pixel32Format,
    source: &[u8],
    palette: Option<&[[u8; 3]; 256]>,
    destination: &mut [u8],
) -> Result<(), KernelError> {
    let width = format.bytes();
    let count = source.len() / width;
    let full = count / 4 * 4;
    // SAFETY: 公有入口已检查ISA和完整row长度；每次只读恰四像素，写恰16byte。
    for start in (0..full).step_by(4) {
        let value = arch::block(format, dst, source.as_ptr().add(start * width), palette);
        arch::store(value, destination.as_mut_ptr().add(start * 4));
    }
    if count != full {
        let n = count - full;
        let mut input = [0; 16];
        let mut output = [0; 16];
        input[..n * width].copy_from_slice(&source[full * width..]);
        // 短尾与bulk调用同一ISAcore；palette dummy index0合法，只复制有效输出。
        let value = arch::block(format, dst, input.as_ptr(), palette);
        arch::store(value, output.as_mut_ptr());
        destination[full * 4..].copy_from_slice(&output[..n * 4]);
    }
    Ok(())
}

#[cfg(all(test, target_arch = "aarch64", target_os = "macos"))]
#[path = "../tests/support/pixel_abi_private.rs"]
mod abi;
