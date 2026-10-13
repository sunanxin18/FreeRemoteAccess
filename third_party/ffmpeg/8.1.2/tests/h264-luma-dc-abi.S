/* SPDX-License-Identifier: LGPL-2.1-or-later
 * 私有测试辅助；不得导出到产品库。 */
#include "libavutil/aarch64/asm.S"
/* x18 只读快照；生产核保持 FFmpeg 的禁用宏。 */
#undef x18
function frd_h264_dc_check_abi, export=1
        AARCH64_VALID_CALL_TARGET
        sub sp, sp, #192
        stp x19, x20, [sp, #0]
        stp x21, x22, [sp, #16]
        stp x23, x24, [sp, #32]
        stp x25, x26, [sp, #48]
        stp x27, x28, [sp, #64]
        stp x29, x30, [sp, #80]
        stp d8, d9, [sp, #96]
        stp d10, d11, [sp, #112]
        stp d12, d13, [sp, #128]
        stp d14, d15, [sp, #144]
        str x18, [sp, #160]
        mov x4, sp
        str x4, [sp, #168]
.irp n,19,20,21,22,23,24,25,26,27,28,29
        mov x\n, #(0x510 + \n)
.endr
.irp n,8,9,10,11,12,13,14,15
        movi v\n\().8b, #0xa5
.endr
        blr x3
        mov w0, #0
.irp n,19,20,21,22,23,24,25,26,27,28,29
        cmp x\n, #(0x510 + \n)
        cinc w0, w0, ne
.endr
        movi v0.8b, #0xa5
        fmov x5, d0
.irp n,8,9,10,11,12,13,14,15
        fmov x4, d\n
        cmp x4, x5
        cinc w0, w0, ne
.endr
        ldr x4, [sp, #160]
        cmp x18, x4
        cinc w0, w0, ne
        ldr x4, [sp, #168]
        mov x5, sp
        cmp x4, x5
        cinc w0, w0, ne
        ldp x19, x20, [sp, #0]
        ldp x21, x22, [sp, #16]
        ldp x23, x24, [sp, #32]
        ldp x25, x26, [sp, #48]
        ldp x27, x28, [sp, #64]
        ldp x29, x30, [sp, #80]
        ldp d8, d9, [sp, #96]
        ldp d10, d11, [sp, #112]
        ldp d12, d13, [sp, #128]
        ldp d14, d15, [sp, #144]
        add sp, sp, #192
        ret
endfunc
