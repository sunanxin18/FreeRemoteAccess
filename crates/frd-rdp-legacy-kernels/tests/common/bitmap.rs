#![allow(dead_code)]
use frd_rdp_legacy_kernels::PackedDepth;
pub const DEPTHS: [(PackedDepth, usize); 4] = [
    (PackedDepth::Index8, 1),
    (PackedDepth::Rgb555, 2),
    (PackedDepth::Rgb565, 2),
    (PackedDepth::Bgr24, 3),
];
pub fn bytes(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| (i.wrapping_mul(73).wrapping_add(29)) as u8)
        .collect()
}
// 独立逐 pixel 模型仅用于 tests；不使用生产 pattern 或 SIMD helpers。
pub fn fill(pixel: &[u8], n: usize) -> Vec<u8> {
    (0..n).flat_map(|_| pixel.iter().copied()).collect()
}
pub fn xor(previous: &[u8], fg: &[u8]) -> Vec<u8> {
    previous
        .chunks_exact(fg.len())
        .flat_map(|p| p.iter().zip(fg).map(|(bg, fg)| bg ^ fg))
        .collect()
}
pub fn mask(previous: Option<&[u8]>, fg: &[u8], mask: u8, offset: u8, n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..n {
        for (channel, foreground) in fg.iter().enumerate() {
            let bg = previous.map_or(0, |p| p[i * fg.len() + channel]);
            out.push(if mask & (1 << (usize::from(offset) + i)) != 0 {
                bg ^ foreground
            } else {
                bg
            });
        }
    }
    out
}
pub fn dither(a: &[u8], b: &[u8], starts_with_b: bool, n: usize) -> Vec<u8> {
    (0..n)
        .flat_map(|i| {
            if (i % 2 != 0) ^ starts_with_b { b } else { a }
                .iter()
                .copied()
        })
        .collect()
}
