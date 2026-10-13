//! 固定 lane 的 NEON primitive；不内联，使完整块与短尾具有相同 ISA 数值路径。
use core::arch::aarch64::*;
#[inline(never)]
#[target_feature(enable = "neon")]
pub(super) unsafe fn delta16(previous: &[u8; 16], current: &mut [u8; 16]) {
    let d = vld1q_u8(current.as_ptr());
    let half = vshrq_n_u8::<1>(d);
    let odd = vtstq_u8(d, vdupq_n_u8(1));
    vst1q_u8(
        current.as_mut_ptr(),
        vaddq_u8(vld1q_u8(previous.as_ptr()), veorq_u8(half, odd)),
    );
}
#[inline(never)]
#[target_feature(enable = "neon")]
pub(super) unsafe fn duplicate4(src: &[u8; 4]) -> [u8; 8] {
    let mut stage = [0; 8];
    stage[..4].copy_from_slice(src);
    let v = vld1_u8(stage.as_ptr());
    let mut out = [0; 8];
    vst1_u8(out.as_mut_ptr(), vzip1_u8(v, v));
    out
}
#[inline(never)]
#[target_feature(enable = "neon")]
pub(super) unsafe fn pack8(r: &[u8; 8], g: &[u8; 8], b: &[u8; 8], out: &mut [u8; 24]) {
    vst3_u8(
        out.as_mut_ptr(),
        uint8x8x3_t(
            vld1_u8(r.as_ptr()),
            vld1_u8(g.as_ptr()),
            vld1_u8(b.as_ptr()),
        ),
    );
}
#[inline(never)]
#[target_feature(enable = "neon")]
pub(super) unsafe fn color8(
    y: &[u8; 8],
    co: &[u8; 8],
    cg: &[u8; 8],
    shift: u8,
    alpha: bool,
    out: &mut [u8; 24],
) {
    let sh = vdup_n_s8(shift as i8);
    // 左移在八 bit lane 中截断，之后才 signed widen；不能先 widen 再 shift。
    let co = vmovl_s8(vreinterpret_s8_u8(vshl_u8(vld1_u8(co.as_ptr()), sh)));
    let cg = vmovl_s8(vreinterpret_s8_u8(vshl_u8(vld1_u8(cg.as_ptr()), sh)));
    let y = vreinterpretq_s16_u16(vmovl_u8(vld1_u8(y.as_ptr())));
    let t = vsubq_s16(y, cg);
    let r = vqmovun_s16(vaddq_s16(t, co));
    let g = vqmovun_s16(vaddq_s16(y, cg));
    let b = vqmovun_s16(vsubq_s16(t, co));
    let (first, last) = if alpha { (r, b) } else { (b, r) };
    vst3_u8(out.as_mut_ptr(), uint8x8x3_t(first, g, last));
}
