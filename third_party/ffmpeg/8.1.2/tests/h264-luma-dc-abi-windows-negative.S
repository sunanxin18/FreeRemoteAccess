/* SPDX-License-Identifier: LGPL-2.1-or-later
 * Windows 原生 ARM64 私有 ABI contract 探针；只链接隔离测试程序。
 * 蓄意破坏 ABI 的回调不得被 C 直接调用；由 checker/外层探针保护。
 */
#if !defined(_WIN32) || !(defined(__aarch64__) || defined(_M_ARM64))
#error "此私有探针仅支持 Windows 原生 ARM64"
#endif
#if defined(__arm64ec__) || defined(_M_ARM64EC) || defined(__ARM64EC__)
#error "此私有探针不支持 ARM64EC"
#endif
#if !defined(FRD_DC_WINDOWS_ABI_CONTRACT)
#error "此文件仅允许用于私有 contract，禁止链接 GREEN 数值程序"
#endif

        .text
        .macro frd_probe_leaf name
        .p2align 2
        .globl \name
        .def \name
        .scl 2
        .type 32
        .endef
        .seh_proc \name
\name:
        .endm
        .macro frd_probe_end
        .seh_endproc
        .endm

        frd_probe_leaf frd_h264_dc_preserve
        ret
        frd_probe_end

/* 检查低位和只改高位两类；高位负例能区分仅比较 wN/sN 的坏 checker。
 * 同名 fake_checker 变体返回 0，让外层独立检测漏恢复的 checker。
 */
        .macro frd_probe_gpr n
        frd_probe_leaf frd_h264_dc_clobber_x\n\()_lo
        eor x\n, x\n, #1
        ret
        frd_probe_end
        frd_probe_leaf frd_h264_dc_clobber_x\n\()_hi
        eor x\n, x\n, #0x10000000000
        ret
        frd_probe_end
        frd_probe_leaf frd_h264_dc_fake_checker_x\n
        eor x\n, x\n, #0x10000000000
        mov w0, #0
        ret
        frd_probe_end
        .endm
.irp n,19,20,21,22,23,24,25,26,27,28
        frd_probe_gpr \n
.endr

        .macro frd_probe_simd n
        frd_probe_leaf frd_h264_dc_clobber_d\n\()_lo
        fmov x9, d\n
        eor x9, x9, #1
        fmov d\n, x9
        ret
        frd_probe_end
        frd_probe_leaf frd_h264_dc_clobber_d\n\()_hi
        fmov x9, d\n
        eor x9, x9, #0x10000000000
        fmov d\n, x9
        ret
        frd_probe_end
        frd_probe_leaf frd_h264_dc_fake_checker_d\n
        fmov x9, d\n
        eor x9, x9, #0x10000000000
        fmov d\n, x9
        mov w0, #0
        ret
        frd_probe_end
        .endm
.irp n,8,9,10,11,12,13,14,15
        frd_probe_simd \n
.endr

        frd_probe_leaf frd_h264_dc_simd_upper_only
        movz x9, #0xdead
        movk x9, #0xbeef, lsl #16
        movk x9, #0xcafe, lsl #32
        movk x9, #0x1234, lsl #48
.irp n,8,9,10,11,12,13,14,15
        ins v\n\().d[1], x9            /* 合法：不改变 dN 的低 64 位。 */
.endr
        ret
        frd_probe_end

/* 快照预期值故障注入；真实 x18 永不写。FP/SP 快照注入不改结构。 */
        .macro frd_probe_snapshot name, off
        frd_probe_leaf \name
        ldr x9, [x29, #\off]
        eor x9, x9, #1
        str x9, [x29, #\off]
        ret
        frd_probe_end
        .endm
        frd_probe_snapshot frd_h264_dc_fault_x18_snapshot, 160
        frd_probe_snapshot frd_h264_dc_fault_sp_snapshot, 168
        frd_probe_snapshot frd_h264_dc_fault_fp_snapshot, 176

/* 下列回调只能在先输出/flush ready 的独立 child 中调用。 */
        frd_probe_leaf frd_h264_dc_corrupt_fp
        mov x29, xzr
        ret
        frd_probe_end
        frd_probe_leaf frd_h264_dc_corrupt_sp16
        add sp, sp, #16
        ret
        frd_probe_end
        frd_probe_leaf frd_h264_dc_corrupt_sp8
        add sp, sp, #8
        ret
        frd_probe_end
        frd_probe_leaf frd_h264_dc_wrong_return
        adr x30, .Lfrd_wrong_return_landing
        ret
.Lfrd_wrong_return_landing:
        brk #0xf000                   /* site 身份由实际 PC 区分。 */
        b .Lfrd_wrong_return_landing
        frd_probe_end
        frd_probe_leaf frd_h264_dc_no_return
.Lfrd_no_return:
        b .Lfrd_no_return
        frd_probe_end

/* 两个真实 tail-call wrapper：四参数，x3 为同一个真实 kernel。
 * 只改 int 未使用高位，不借助未规定的 callee 返回值。
 */
        frd_probe_leaf frd_h264_dc_call_qmul_hi_a
        mov w2, w2
        movk x2, #0x7e35, lsl #32
        movk x2, #0xa19c, lsl #48
        br x3
        frd_probe_end
        frd_probe_leaf frd_h264_dc_call_qmul_hi_b
        mov w2, w2
        movk x2, #0x81ca, lsl #32
        movk x2, #0x5e63, lsl #48
        br x3
        frd_probe_end
        frd_probe_leaf frd_h264_dc_call_qmul_bad_full_x2
        mov w2, w2
        movk x2, #0x7e35, lsl #32
        movk x2, #0xa19c, lsl #48
        lsr x9, x2, #32
        eor w2, w2, w9                 /* 故意将高位折入实际 kernel 的 qmul。 */
        br x3
        frd_probe_end

/* uint64_t outer(in,out,q,kernel,checker)：低 32 位是 checker 返回值，
 * 高 32 位是独立 caller-restoration mask。与内层不同的完整常量。
 */
        .macro frd_outer_pattern reg, n
        movz \reg, #(0x1500 + \n)
        movk \reg, #(0x2600 + \n), lsl #16
        movk \reg, #(0x3700 + \n), lsl #32
        movk \reg, #(0x4800 + \n), lsl #48
        .endm
        .p2align 2
        .globl frd_h264_dc_outer_probe
        .def frd_h264_dc_outer_probe
        .scl 2
        .type 32
        .endef
        .seh_proc frd_h264_dc_outer_probe
frd_h264_dc_outer_probe:
        sub sp, sp, #192
        .seh_stackalloc 192
        stp x29, x30, [sp, #0]
        .seh_save_fplr 0
        stp x19, x20, [sp, #16]
        .seh_save_regp x19, 16
        stp x21, x22, [sp, #32]
        .seh_save_regp x21, 32
        stp x23, x24, [sp, #48]
        .seh_save_regp x23, 48
        stp x25, x26, [sp, #64]
        .seh_save_regp x25, 64
        stp x27, x28, [sp, #80]
        .seh_save_regp x27, 80
        stp d8, d9, [sp, #96]
        .seh_save_fregp d8, 96
        stp d10, d11, [sp, #112]
        .seh_save_fregp d10, 112
        stp d12, d13, [sp, #128]
        .seh_save_fregp d12, 128
        stp d14, d15, [sp, #144]
        .seh_save_fregp d14, 144
        mov x29, sp
        .seh_set_fp
        .seh_endprologue
        str x18, [sp, #160]
        mov x9, sp
        str x9, [sp, #168]
        str x29, [sp, #176]
        str xzr, [sp, #184]
.irp n,19,20,21,22,23,24,25,26,27,28
        frd_outer_pattern x\n, \n
.endr
.irp n,8,9,10,11,12,13,14,15
        frd_outer_pattern x9, (0x90+\n)
        fmov d\n, x9
.endr
        blr x4                        /* 可替换 checker，独立证明漏恢复控制。 */
        .globl frd_h264_dc_outer_pc_return
frd_h264_dc_outer_pc_return:
        mov x9, sp
        tst x9, #15
        b.ne .Lfrd_outer_frame_failure
        cmp x29, x9
        b.ne .Lfrd_outer_frame_failure
        str w0, [sp, #184]
        mov w0, #0
        ldr x10, [sp, #168]
        cmp x9, x10
        cset w11, ne
        orr w0, w0, w11, lsl #20
        ldr x10, [sp, #176]
        cmp x29, x10
        cset w11, ne
        orr w0, w0, w11, lsl #19
        ldr x10, [sp, #160]
        cmp x18, x10
        cset w11, ne
        orr w0, w0, w11, lsl #18
.irp n,19,20,21,22,23,24,25,26,27,28
        frd_outer_pattern x10, \n
        cmp x\n, x10
        cset w11, ne
        orr w0, w0, w11, lsl #(\n - 19)
.endr
.irp n,8,9,10,11,12,13,14,15
        frd_outer_pattern x10, (0x90+\n)
        fmov x9, d\n
        cmp x9, x10
        cset w11, ne
        orr w0, w0, w11, lsl #(\n + 2)
.endr
        ldr w8, [sp, #184]
        orr x0, x8, x0, lsl #32
        b .Lfrd_outer_epilogue
.Lfrd_outer_frame_failure:
        brk #0xf000                   /* outer failure 不是合格 child probe。 */
        b .Lfrd_outer_frame_failure
.Lfrd_outer_epilogue:
        .seh_startepilogue
        ldp d14, d15, [sp, #144]
        .seh_save_fregp d14, 144
        ldp d12, d13, [sp, #128]
        .seh_save_fregp d12, 128
        ldp d10, d11, [sp, #112]
        .seh_save_fregp d10, 112
        ldp d8, d9, [sp, #96]
        .seh_save_fregp d8, 96
        ldp x27, x28, [sp, #80]
        .seh_save_regp x27, 80
        ldp x25, x26, [sp, #64]
        .seh_save_regp x25, 64
        ldp x23, x24, [sp, #48]
        .seh_save_regp x23, 48
        ldp x21, x22, [sp, #32]
        .seh_save_regp x21, 32
        ldp x19, x20, [sp, #16]
        .seh_save_regp x19, 16
        ldp x29, x30, [sp, #0]
        .seh_save_fplr 0
        add sp, sp, #192
        .seh_stackalloc 192
        .seh_endepilogue
        ret
        .seh_endproc
