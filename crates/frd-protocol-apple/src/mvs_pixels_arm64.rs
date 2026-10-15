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

/// # Safety
/// rgb 至少 24 可写字节；颜色按值传入，无输入内存或重叠约束。
#[inline(never)]
pub(super) unsafe fn bitmap_rgb8(mask: u8, one: [u8; 3], zero: [u8; 3], rgb: *mut u8) {
    // SAFETY: 固定 8 像素，st3 只写 24 字节；无标量逐像素颜色选择。
    unsafe {
        let bits = [0x80, 0x40, 0x20, 0x10, 8, 4, 2, 1];
        let selected = vtst_u8(vdup_n_u8(mask), vld1_u8(bits.as_ptr()));
        vst3_u8(
            rgb,
            uint8x8x3_t(
                vbsl_u8(selected, vdup_n_u8(one[0]), vdup_n_u8(zero[0])),
                vbsl_u8(selected, vdup_n_u8(one[1]), vdup_n_u8(zero[1])),
                vbsl_u8(selected, vdup_n_u8(one[2]), vdup_n_u8(zero[2])),
            ),
        );
    }
}

/// # Safety
/// pixels 为 1..=8；src/dst 分别有 pixels*3 可读/可写字节，范围不重叠。
/// 允许任意字节对齐。精确分解 16/8/4/2/1 字节，绝不触碰尾后字节。
#[inline(never)]
pub(super) unsafe fn copy_rgb_nonoverlap(src: *const u8, dst: *mut u8, pixels: usize) {
    // SAFETY: 调用者保证完整跨度。q/d 搬运使用 NEON；小尾用 ARM64
    // 标量寄存器 ldr/str，仍是显式原生汇编，不回退 Rust 逐像素循环。
    // 未使用 nomem/readonly/pure：编译器可见其读写内存；显式声明向量、
    // 临时寄存器及指针/长度修改，默认条件码 clobber；不操作栈或手写 ABI。
    unsafe {
        core::arch::asm!(
            "cmp {bytes}, #16",
            "b.lo 2f",
            "ldr q0, [{src}], #16",
            "str q0, [{dst}], #16",
            "sub {bytes}, {bytes}, #16",
            "2:",
            "cmp {bytes}, #8",
            "b.lo 3f",
            "ldr d0, [{src}], #8",
            "str d0, [{dst}], #8",
            "sub {bytes}, {bytes}, #8",
            "3:",
            "tbz {bytes}, #2, 4f",
            "ldr {temp:w}, [{src}], #4",
            "str {temp:w}, [{dst}], #4",
            "4:",
            "tbz {bytes}, #1, 5f",
            "ldrh {temp:w}, [{src}], #2",
            "strh {temp:w}, [{dst}], #2",
            "5:",
            "tbz {bytes}, #0, 6f",
            "ldrb {temp:w}, [{src}]",
            "strb {temp:w}, [{dst}]",
            "6:",
            src = inout(reg) src => _,
            dst = inout(reg) dst => _,
            bytes = inout(reg) (pixels * 3) => _,
            temp = out(reg) _,
            out("v0") _,
            options(nostack),
        );
    }
}
