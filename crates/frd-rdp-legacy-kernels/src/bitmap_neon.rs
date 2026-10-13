//! NEON packed primitive；显式指令保证短尾数值/搬运不会被scalar化。
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn copy16(src: &[u8; 16], dst: &mut [u8; 16]) {
    // SAFETY: 各数组恰16bytes且无重叠；能力对象已验证NEON。
    core::arch::asm!(
        "ldr q0, [{src}]", "str q0, [{dst}]",
        src = in(reg) src.as_ptr(), dst = in(reg) dst.as_mut_ptr(),
        out("v0") _, options(nostack, preserves_flags)
    );
}
#[inline]
#[target_feature(enable = "neon")]
pub(super) unsafe fn xor16(previous: &[u8; 16], fg: &[u8; 16], dst: &mut [u8; 16]) {
    // SAFETY: 各数组恰16bytes且无重叠；只有明确NEON vector XOR与store。
    core::arch::asm!(
        "ldr q0, [{previous}]", "ldr q1, [{fg}]", "eor v0.16b, v0.16b, v1.16b", "str q0, [{dst}]",
        previous = in(reg) previous.as_ptr(), fg = in(reg) fg.as_ptr(), dst = in(reg) dst.as_mut_ptr(),
        out("v0") _, out("v1") _, options(nostack, preserves_flags)
    );
}
