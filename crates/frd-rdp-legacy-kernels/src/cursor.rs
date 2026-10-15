//! 只处理有界 row staging；颜色、mask、checker、合成都在 ISA core 中。
use crate::{CursorBitmapMode, CursorXorFormat};
#[cfg(target_arch = "aarch64")]
#[path = "cursor_neon.rs"]
mod arch;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[path = "cursor_x86.rs"]
mod arch;

/// # Safety
/// 同一 NativeKernels 已验证 ISA；所有源与目的必须通过公开叶子的精确长度检查。
pub(super) unsafe fn decode(
    format: CursorXorFormat,
    mode: CursorBitmapMode,
    xor: &[u8],
    and: Option<&[u8]>,
    output: &mut [u8],
) {
    let pixels = output.len() / 4;
    for start in (0..pixels).step_by(4) {
        let count = (pixels - start).min(4);
        let mut source = [0u8; 16];
        // Mono 读取当前一个字节；其他格式只拷贝有效编码字节，绝不读取行 padding。
        match format {
            CursorXorFormat::Mono1 => source[0] = xor[start / 8],
            CursorXorFormat::Rgb565 => {
                source[..count * 2].copy_from_slice(&xor[start * 2..(start + count) * 2])
            }
            CursorXorFormat::Bgr24 => {
                source[..count * 3].copy_from_slice(&xor[start * 3..(start + count) * 3])
            }
            CursorXorFormat::Bgra32 => {
                source[..count * 4].copy_from_slice(&xor[start * 4..(start + count) * 4])
            }
        }
        let mask = and.map_or(0, |a| a[start / 8]);
        let value = arch::decode4(format, mode, source.as_ptr(), mask, start % 8 == 4);
        let mut rgba = [0u8; 16];
        arch::store(value, rgba.as_mut_ptr());
        output[start * 4..(start + count) * 4].copy_from_slice(&rgba[..count * 4]);
    }
}

/// # Safety
/// ISA 已验证；slice 相同长度且4对齐。完全块直读，短尾执行同一个 core。
pub(super) unsafe fn composite(src: &[u8], background: &[u8], output: &mut [u8]) {
    let full = src.len() / 16 * 16;
    for start in (0..full).step_by(16) {
        arch::store(
            arch::composite4(src.as_ptr().add(start), background.as_ptr().add(start)),
            output.as_mut_ptr().add(start),
        );
    }
    if full != src.len() {
        let n = src.len() - full;
        let mut s = [0u8; 16];
        let mut b = [0u8; 16];
        let mut o = [0u8; 16];
        s[..n].copy_from_slice(&src[full..]);
        b[..n].copy_from_slice(&background[full..]);
        arch::store(arch::composite4(s.as_ptr(), b.as_ptr()), o.as_mut_ptr());
        output[full..].copy_from_slice(&o[..n]);
    }
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    target_arch = "aarch64"
))]
#[path = "../tests/support/cursor_abi_private.rs"]
mod abi;
