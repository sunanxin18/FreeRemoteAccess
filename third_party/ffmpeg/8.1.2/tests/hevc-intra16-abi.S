/* SPDX-License-Identifier: LGPL-2.1-or-later
 * 私有直接数值入口 ABI sentinel；只准备 Darwin/ELF。
 * mask bits0..10=x19..29, 11..18=d8..15, 19=x18, 20=SP,
 * bit21=LR。错误 SP/LR 若无法返回则进程异常也判 FAIL。
 */
#if defined(_WIN32)
#error Windows ARM64 native probe requires a separately reviewed SEH adapter
#endif
#include "libavutil/aarch64/asm.S"
/* 上游禁止 x18 用作工作寄存器；此处仅 mov 读取，绝不写回。 */
#undef x18
function frd_hevc_intra16_abi, export=1
        AARCH64_SIGN_LINK_REGISTER
        stp x19, x20, [sp, #-192]!
        .cfi_startproc
        .cfi_def_cfa_offset 192
#if GNU_PROPERTY_AARCH64_PAC
        .cfi_negate_ra_state
#endif
        .cfi_offset x19, -192
        .cfi_offset x20, -184
        .cfi_offset x21, -176
        .cfi_offset x22, -168
        .cfi_offset x23, -160
        .cfi_offset x24, -152
        .cfi_offset x25, -144
        .cfi_offset x26, -136
        .cfi_offset x27, -128
        .cfi_offset x28, -120
        .cfi_offset x29, -112
        .cfi_offset x30, -104
        .cfi_offset 72, -96
        .cfi_offset 73, -88
        .cfi_offset 74, -80
        .cfi_offset 75, -72
        .cfi_offset 76, -64
        .cfi_offset 77, -56
        .cfi_offset 78, -48
        .cfi_offset 79, -40
        stp x21, x22, [sp, #16]
        stp x23, x24, [sp, #32]
        stp x25, x26, [sp, #48]
        stp x27, x28, [sp, #64]
        stp x29, x30, [sp, #80]
        stp d8, d9, [sp, #96]
        stp d10, d11, [sp, #112]
        stp d12, d13, [sp, #128]
        stp d14, d15, [sp, #144]
        mov x16, x18
        str x16, [sp, #160]
        mov x16, sp
        str x16, [sp, #168]
        mov x16, #0x5a5a
        lsl x16, x16, #32
        orr x4, x4, x16
        orr x5, x5, x16
        movz x19, #24595
        movk x19, #4660, lsl #16
        movk x19, #22136, lsl #32
        movk x19, #39612, lsl #48
        movz x20, #24596
        movk x20, #4660, lsl #16
        movk x20, #22136, lsl #32
        movk x20, #39612, lsl #48
        movz x21, #24597
        movk x21, #4660, lsl #16
        movk x21, #22136, lsl #32
        movk x21, #39612, lsl #48
        movz x22, #24598
        movk x22, #4660, lsl #16
        movk x22, #22136, lsl #32
        movk x22, #39612, lsl #48
        movz x23, #24599
        movk x23, #4660, lsl #16
        movk x23, #22136, lsl #32
        movk x23, #39612, lsl #48
        movz x24, #24600
        movk x24, #4660, lsl #16
        movk x24, #22136, lsl #32
        movk x24, #39612, lsl #48
        movz x25, #24601
        movk x25, #4660, lsl #16
        movk x25, #22136, lsl #32
        movk x25, #39612, lsl #48
        movz x26, #24602
        movk x26, #4660, lsl #16
        movk x26, #22136, lsl #32
        movk x26, #39612, lsl #48
        movz x27, #24603
        movk x27, #4660, lsl #16
        movk x27, #22136, lsl #32
        movk x27, #39612, lsl #48
        movz x28, #24604
        movk x28, #4660, lsl #16
        movk x28, #22136, lsl #32
        movk x28, #39612, lsl #48
        movz x29, #24605
        movk x29, #4660, lsl #16
        movk x29, #22136, lsl #32
        movk x29, #39612, lsl #48
        movz x16, #28680
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        fmov d8, x16
        movz x16, #28681
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        fmov d9, x16
        movz x16, #28682
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        fmov d10, x16
        movz x16, #28683
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        fmov d11, x16
        movz x16, #28684
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        fmov d12, x16
        movz x16, #28685
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        fmov d13, x16
        movz x16, #28686
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        fmov d14, x16
        movz x16, #28687
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        fmov d15, x16
        cbnz w6, Lprobe_angular
        bl X(ff_hevc_pred_planar_16_8_neon)
Lprobe_planar_return:
        adr x16, Lprobe_planar_return
        b Lprobe_check
Lprobe_angular:
        bl X(ff_hevc_pred_angular_16_8_neon)
Lprobe_angular_return:
        adr x16, Lprobe_angular_return
Lprobe_check:
        cmp x30, x16
        cset x0, ne
        lsl x0, x0, #21
        movz x16, #24595
        movk x16, #4660, lsl #16
        movk x16, #22136, lsl #32
        movk x16, #39612, lsl #48
        cmp x19, x16
        cset x17, ne
        orr x0, x0, x17, lsl #0
        movz x16, #24596
        movk x16, #4660, lsl #16
        movk x16, #22136, lsl #32
        movk x16, #39612, lsl #48
        cmp x20, x16
        cset x17, ne
        orr x0, x0, x17, lsl #1
        movz x16, #24597
        movk x16, #4660, lsl #16
        movk x16, #22136, lsl #32
        movk x16, #39612, lsl #48
        cmp x21, x16
        cset x17, ne
        orr x0, x0, x17, lsl #2
        movz x16, #24598
        movk x16, #4660, lsl #16
        movk x16, #22136, lsl #32
        movk x16, #39612, lsl #48
        cmp x22, x16
        cset x17, ne
        orr x0, x0, x17, lsl #3
        movz x16, #24599
        movk x16, #4660, lsl #16
        movk x16, #22136, lsl #32
        movk x16, #39612, lsl #48
        cmp x23, x16
        cset x17, ne
        orr x0, x0, x17, lsl #4
        movz x16, #24600
        movk x16, #4660, lsl #16
        movk x16, #22136, lsl #32
        movk x16, #39612, lsl #48
        cmp x24, x16
        cset x17, ne
        orr x0, x0, x17, lsl #5
        movz x16, #24601
        movk x16, #4660, lsl #16
        movk x16, #22136, lsl #32
        movk x16, #39612, lsl #48
        cmp x25, x16
        cset x17, ne
        orr x0, x0, x17, lsl #6
        movz x16, #24602
        movk x16, #4660, lsl #16
        movk x16, #22136, lsl #32
        movk x16, #39612, lsl #48
        cmp x26, x16
        cset x17, ne
        orr x0, x0, x17, lsl #7
        movz x16, #24603
        movk x16, #4660, lsl #16
        movk x16, #22136, lsl #32
        movk x16, #39612, lsl #48
        cmp x27, x16
        cset x17, ne
        orr x0, x0, x17, lsl #8
        movz x16, #24604
        movk x16, #4660, lsl #16
        movk x16, #22136, lsl #32
        movk x16, #39612, lsl #48
        cmp x28, x16
        cset x17, ne
        orr x0, x0, x17, lsl #9
        movz x16, #24605
        movk x16, #4660, lsl #16
        movk x16, #22136, lsl #32
        movk x16, #39612, lsl #48
        cmp x29, x16
        cset x17, ne
        orr x0, x0, x17, lsl #10
        fmov x17, d8
        movz x16, #28680
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        cmp x17, x16
        cset x17, ne
        orr x0, x0, x17, lsl #11
        fmov x17, d9
        movz x16, #28681
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        cmp x17, x16
        cset x17, ne
        orr x0, x0, x17, lsl #12
        fmov x17, d10
        movz x16, #28682
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        cmp x17, x16
        cset x17, ne
        orr x0, x0, x17, lsl #13
        fmov x17, d11
        movz x16, #28683
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        cmp x17, x16
        cset x17, ne
        orr x0, x0, x17, lsl #14
        fmov x17, d12
        movz x16, #28684
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        cmp x17, x16
        cset x17, ne
        orr x0, x0, x17, lsl #15
        fmov x17, d13
        movz x16, #28685
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        cmp x17, x16
        cset x17, ne
        orr x0, x0, x17, lsl #16
        fmov x17, d14
        movz x16, #28686
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        cmp x17, x16
        cset x17, ne
        orr x0, x0, x17, lsl #17
        fmov x17, d15
        movz x16, #28687
        movk x16, #30292, lsl #16
        movk x16, #47768, lsl #32
        movk x16, #65244, lsl #48
        cmp x17, x16
        cset x17, ne
        orr x0, x0, x17, lsl #18
        ldr x16, [sp, #160]
        cmp x18, x16
        cset x17, ne
        orr x0, x0, x17, lsl #19
        ldr x16, [sp, #168]
        mov x17, sp
        cmp x17, x16
        cset x17, ne
        orr x0, x0, x17, lsl #20
        ldp d8, d9, [sp, #96]
        ldp d10, d11, [sp, #112]
        ldp d12, d13, [sp, #128]
        ldp d14, d15, [sp, #144]
        ldp x21, x22, [sp, #16]
        ldp x23, x24, [sp, #32]
        ldp x25, x26, [sp, #48]
        ldp x27, x28, [sp, #64]
        ldp x29, x30, [sp, #80]
        ldp x19, x20, [sp], #192
        .cfi_def_cfa_offset 0
        AARCH64_VALIDATE_LINK_REGISTER
#if GNU_PROPERTY_AARCH64_PAC
        .cfi_negate_ra_state
#endif
        .cfi_restore x19
        .cfi_restore x20
        .cfi_restore x21
        .cfi_restore x22
        .cfi_restore x23
        .cfi_restore x24
        .cfi_restore x25
        .cfi_restore x26
        .cfi_restore x27
        .cfi_restore x28
        .cfi_restore x29
        .cfi_restore x30
        .cfi_restore 72
        .cfi_restore 73
        .cfi_restore 74
        .cfi_restore 75
        .cfi_restore 76
        .cfi_restore 77
        .cfi_restore 78
        .cfi_restore 79
        ret
        .cfi_endproc
endfunc
