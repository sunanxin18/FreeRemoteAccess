//! 只负责 packed 字节展开；所有 repeated materialization、XOR 与短尾经过 SIMD。
//! pattern 准备至多 48 bytes，mask 准备至多 24 bytes；scalar 仅搬运 staging/布局。
#[cfg(target_arch = "aarch64")]
#[path = "bitmap_neon.rs"]
mod arch;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[path = "bitmap_x86.rs"]
mod arch;

// width<=3，period<=6，均整除48；三种16-byte相位覆盖packed24跨 lane。
fn pattern(a: &[u8], b: &[u8]) -> [[u8; 16]; 3] {
    std::array::from_fn(|block| {
        std::array::from_fn(|lane| {
            let i = (block * 16 + lane) % (a.len() + b.len());
            if i < a.len() {
                a[i]
            } else {
                b[i - a.len()]
            }
        })
    })
}

// 所有调用都提供完整已检查数组；assembly 不可被优化为 scalar/memcpy。
unsafe fn block(previous: Option<&[u8; 16]>, pattern: &[u8; 16], dst: &mut [u8; 16]) {
    if let Some(previous) = previous {
        arch::xor16(previous, pattern, dst);
    } else {
        arch::copy16(pattern, dst);
    }
}

unsafe fn repeat(pattern: &[[u8; 16]; 3], previous: Option<&[u8]>, dst: &mut [u8]) {
    let full = dst.len() / 16 * 16;
    for start in (0..full).step_by(16) {
        block(
            previous.map(|p| p[start..start + 16].try_into().unwrap()),
            &pattern[(start / 16) % 3],
            (&mut dst[start..start + 16]).try_into().unwrap(),
        );
    }
    let n = dst.len() - full;
    if n != 0 {
        let mut staged_previous = [0; 16];
        if let Some(p) = previous {
            staged_previous[..n].copy_from_slice(&p[full..]);
        }
        let mut staged_out = [0; 16];
        block(
            previous.map(|_| &staged_previous),
            &pattern[(full / 16) % 3],
            &mut staged_out,
        );
        dst[full..].copy_from_slice(&staged_out[..n]);
    }
}

pub(super) unsafe fn copy(src: &[u8], dst: &mut [u8]) {
    let full = dst.len() / 16 * 16;
    for start in (0..full).step_by(16) {
        arch::copy16(
            src[start..start + 16].try_into().unwrap(),
            (&mut dst[start..start + 16]).try_into().unwrap(),
        );
    }
    let n = dst.len() - full;
    if n != 0 {
        let mut staged_src = [0; 16];
        staged_src[..n].copy_from_slice(&src[full..]);
        let mut staged_out = [0; 16];
        arch::copy16(&staged_src, &mut staged_out);
        dst[full..].copy_from_slice(&staged_out[..n]);
    }
}
pub(super) unsafe fn fill(pixel: &[u8], dst: &mut [u8]) {
    if !dst.is_empty() {
        repeat(&pattern(pixel, &[]), None, dst);
    }
}
pub(super) unsafe fn xor_previous(previous: &[u8], fg: &[u8], dst: &mut [u8]) {
    if !dst.is_empty() {
        repeat(&pattern(fg, &[]), Some(previous), dst);
    }
}
pub(super) unsafe fn dither(a: &[u8], b: &[u8], starts_with_b: bool, dst: &mut [u8]) {
    if !dst.is_empty() {
        let (a, b) = if starts_with_b { (b, a) } else { (a, b) };
        repeat(&pattern(a, b), None, dst);
    }
}
pub(super) unsafe fn mask_previous(
    previous: Option<&[u8]>,
    fg: &[u8],
    mask: u8,
    bit_offset: u8,
    dst: &mut [u8],
) {
    let mut selected = [[0; 16]; 3];
    // 仅准备这个 mask 的至多8个pixel；XOR与输出必须经完整vector/staged尾。
    for i in 0..dst.len() {
        if mask & (1 << (usize::from(bit_offset) + i / fg.len())) != 0 {
            selected[i / 16][i % 16] = fg[i % fg.len()];
        }
    }
    repeat(&selected, previous, dst);
}
