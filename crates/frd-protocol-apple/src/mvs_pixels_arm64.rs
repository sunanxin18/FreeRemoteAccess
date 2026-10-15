//! 只包含 NEON 叶函数。调用者提供固定块大小，编译门保证小端 ARM64 NEON。
use std::arch::aarch64::*;

#[inline]
unsafe fn channels4(
    y: int32x4_t,
    cb: int32x4_t,
    cr: int32x4_t,
) -> (uint16x4_t, uint16x4_t, uint16x4_t) {
    // SAFETY: 输入为已扩展向量，完整 u8 域中全部有符号 i32 中间量均有界。
    unsafe {
        let bias = vdupq_n_s32(32_768);
        let red = vaddq_s32(
            y,
            vshrq_n_s32::<16>(vaddq_s32(vmulq_n_s32(cr, 91_881), bias)),
        );
        let blue = vaddq_s32(
            y,
            vshrq_n_s32::<16>(vaddq_s32(vmulq_n_s32(cb, 116_130), bias)),
        );
        let green = vaddq_s32(
            y,
            vshrq_n_s32::<16>(vsubq_s32(
                vsubq_s32(bias, vmulq_n_s32(cr, 46_802)),
                vmulq_n_s32(cb, 22_554),
            )),
        );
        (vqmovun_s32(red), vqmovun_s32(green), vqmovun_s32(blue))
    }
}

/// # Safety
/// y/cb/cr 各至少 blocks*8 可读字节，rgb 至少 blocks*24 可写字节，互不重叠。
#[inline(never)]
pub(super) unsafe fn ycbcr_blocks8(
    y: *const u8,
    cb: *const u8,
    cr: *const u8,
    rgb: *mut u8,
    blocks: usize,
) {
    // SAFETY: 调用者提供完整批次边界；每步只加载 8 字节、写入 24 字节。
    unsafe {
        for block in 0..blocks {
            let yy = vreinterpretq_s16_u16(vmovl_u8(vld1_u8(y.add(block * 8))));
            let cc = vsubq_s16(
                vreinterpretq_s16_u16(vmovl_u8(vld1_u8(cb.add(block * 8)))),
                vdupq_n_s16(128),
            );
            let rr = vsubq_s16(
                vreinterpretq_s16_u16(vmovl_u8(vld1_u8(cr.add(block * 8)))),
                vdupq_n_s16(128),
            );
            let lo = channels4(
                vmovl_s16(vget_low_s16(yy)),
                vmovl_s16(vget_low_s16(cc)),
                vmovl_s16(vget_low_s16(rr)),
            );
            let hi = channels4(vmovl_high_s16(yy), vmovl_high_s16(cc), vmovl_high_s16(rr));
            vst3_u8(
                rgb.add(block * 24),
                uint8x8x3_t(
                    vqmovn_u16(vcombine_u16(lo.0, hi.0)),
                    vqmovn_u16(vcombine_u16(lo.1, hi.1)),
                    vqmovn_u16(vcombine_u16(lo.2, hi.2)),
                ),
            );
        }
    }
}

/// # Safety
/// rgb 至少 24 可读字节，bgrx 至少 32 可写字节，互不重叠；字节地址可非对齐。
#[inline(never)]
pub(super) unsafe fn rgb_pack8(rgb: *const u8, bgrx: *mut u8) -> bool {
    // SAFETY: 以上固定边界保证 ld3/st4 完全落在有效内存内。
    unsafe {
        let pixels = vld3_u8(rgb);
        vst4_u8(
            bgrx,
            uint8x8x4_t(pixels.2, pixels.1, pixels.0, vdup_n_u8(0)),
        );
        vmaxv_u8(vorr_u8(vorr_u8(pixels.0, pixels.1), pixels.2)) != 0
    }
}

/// # Safety
/// bytes 至少 16 可读字节，words 至少 4 个对齐可写 u32，互不重叠。
#[inline(never)]
pub(super) unsafe fn bytes_words4(bytes: *const u8, words: *mut u32) {
    // SAFETY: 完整向量边界；编译门保证小端，按位重解释保留全部四字节。
    unsafe {
        vst1q_u32(words, vreinterpretq_u32_u8(vld1q_u8(bytes)));
    }
}

/// # Safety
/// words 至少 4 个对齐可读 u32，bytes 至少 16 可写字节，互不重叠。
#[inline(never)]
pub(super) unsafe fn words_bytes4(words: *const u32, bytes: *mut u8) {
    // SAFETY: 完整向量边界；编译门保证小端，高字节与颜色字节完全相同地保留。
    unsafe {
        vst1q_u8(bytes, vreinterpretq_u8_u32(vld1q_u32(words)));
    }
}
