//! x86与AMD64分别编译；只要求SSE2，使用无对齐要求的16byte访问。
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn copy16(src: &[u8; 16], dst: &mut [u8; 16]) {
    // SAFETY: 各数组恰16bytes且无重叠；能力对象已验证SSE2。
    core::arch::asm!(
        "movdqu xmm0, [{src}]", "movdqu [{dst}], xmm0",
        src = in(reg) src.as_ptr(), dst = in(reg) dst.as_mut_ptr(),
        out("xmm0") _, options(nostack, preserves_flags)
    );
}
#[inline]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn xor16(previous: &[u8; 16], fg: &[u8; 16], dst: &mut [u8; 16]) {
    // SAFETY: 三个无重叠定长数组；fg也用movdqu，不能以pxor内存操作假设对齐。
    core::arch::asm!(
        "movdqu xmm0, [{previous}]", "movdqu xmm1, [{fg}]", "pxor xmm0, xmm1", "movdqu [{dst}], xmm0",
        previous = in(reg) previous.as_ptr(), fg = in(reg) fg.as_ptr(), dst = in(reg) dst.as_mut_ptr(),
        out("xmm0") _, out("xmm1") _, options(nostack, preserves_flags)
    );
}
