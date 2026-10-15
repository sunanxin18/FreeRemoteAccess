/* SPDX-License-Identifier: LGPL-2.1-or-later
 * Windows 原生 ARM64 私有控制台 ABI 检查；不得加入产品库或应用包。
 * 独立 COFF/SEH 实现，不包含 FFmpeg/POSIX 的 function 宏。
 */
#if !defined(_WIN32) || !(defined(__aarch64__) || defined(_M_ARM64))
#error "此私有辅助仅支持 Windows 原生 ARM64"
#endif
#if defined(__arm64ec__) || defined(_M_ARM64EC) || defined(__ARM64EC__)
#error "此私有辅助不支持 ARM64EC"
#endif

/* 仅隔离 contract 副本允许坏元数据：1 缺失，2 栈尺寸，3 保存偏移，
 * 4 漏尾声恢复描述，5 错尾声范围。
 * 实际指令不变，正常调用返回不能区分这些副本。每个副本单独链接。
 */
#ifndef FRD_DC_WINDOWS_ABI_BAD_UNWIND
#define FRD_DC_WINDOWS_ABI_BAD_UNWIND 0
#endif
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND < 0 || FRD_DC_WINDOWS_ABI_BAD_UNWIND > 5
#error "未知私有 unwind 控制"
#endif
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND && !defined(FRD_DC_WINDOWS_ABI_CONTRACT)
#error "坏 unwind 副本仅允许用于私有 contract"
#endif

        .text
        .p2align 2
        .globl frd_h264_dc_check_abi
        .def frd_h264_dc_check_abi
        .scl 2
        .type 32
        .endef

/* 四段常量保证高低 32 位非零且不同；x0/x1/w2/x3 不参与生成。 */
        .macro frd_dc_pattern reg, n
        movz \reg, #(0x5100 + \n)
        movk \reg, #(0x6200 + \n), lsl #16
        movk \reg, #(0x7300 + \n), lsl #32
        movk \reg, #(0x8400 + \n), lsl #48
        .endm

/* 这些宏每次对应一条真实帧指令；缺失元数据控制展开为空。 */
        .macro frd_dc_stack_unwind
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND != 1
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND == 2
        .seh_stackalloc 208
#else
        .seh_stackalloc 192
#endif
#endif
        .endm
        .macro frd_dc_reg_unwind reg, off
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND != 1
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND == 3
        .if \off == 16
        .seh_save_regp \reg, 24
        .else
        .seh_save_regp \reg, \off
        .endif
#else
        .seh_save_regp \reg, \off
#endif
#endif
        .endm
        .macro frd_dc_simd_unwind reg, off
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND != 1
        .seh_save_fregp \reg, \off
#endif
        .endm

/* 返回结构检查前四条在主路径与 C0 fragment 完全相同。 */
        .macro frd_dc_return_guards failure
        mov x9, sp
        tst x9, #15
        b.ne \failure
        cmp x29, x9
        .endm

        .seh_proc frd_h264_dc_check_abi
frd_h264_dc_check_abi:
        sub sp, sp, #192
        frd_dc_stack_unwind
        .globl frd_h264_dc_abi_pc_after_alloc
frd_h264_dc_abi_pc_after_alloc:
        stp x29, x30, [sp, #0]
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND != 1
        .seh_save_fplr 0
#endif
        stp x19, x20, [sp, #16]
        frd_dc_reg_unwind x19, 16
        stp x21, x22, [sp, #32]
        frd_dc_reg_unwind x21, 32
        stp x23, x24, [sp, #48]
        frd_dc_reg_unwind x23, 48
        stp x25, x26, [sp, #64]
        frd_dc_reg_unwind x25, 64
        stp x27, x28, [sp, #80]
        frd_dc_reg_unwind x27, 80
        stp d8, d9, [sp, #96]
        frd_dc_simd_unwind d8, 96
        stp d10, d11, [sp, #112]
        frd_dc_simd_unwind d10, 112
        stp d12, d13, [sp, #128]
        frd_dc_simd_unwind d12, 128
        stp d14, d15, [sp, #144]
        frd_dc_simd_unwind d14, 144
        mov x29, sp
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND != 1
        .seh_set_fp
        .seh_endprologue
#endif
        .globl frd_h264_dc_abi_pc_body
frd_h264_dc_abi_pc_body:
        str x18, [sp, #160]             /* 只读 TEB 快照，无 x18 写入。 */
        mov x9, sp
        str x9, [sp, #168]
        str x29, [sp, #176]
        str xzr, [sp, #184]
        tst x9, #15
        b.ne .Lfrd_dc_frame_failure
        cmp x29, x9
        b.ne .Lfrd_dc_frame_failure

        /* int 的无用高位不确定；保留 signed w2，污染 x2 高 32 位。 */
        mov w2, w2
        movk x2, #0x7e35, lsl #32
        movk x2, #0xa19c, lsl #48
.irp n,19,20,21,22,23,24,25,26,27,28
        frd_dc_pattern x\n, \n
.endr
.irp n,8,9,10,11,12,13,14,15
        frd_dc_pattern x9, (0x80+\n)
        fmov d\n, x9
.endr
#if defined(FRD_DC_WINDOWS_ABI_CONTRACT)
        /* C0：调用前只绑定固定二指令 callback；不从损坏状态猜测 SP。 */
        adrp x9, frd_h264_dc_corrupt_fp
        add x9, x9, :lo12:frd_h264_dc_corrupt_fp
        cmp x3, x9
        b.eq .Lfrd_dc_sp_fragment
#endif
        .globl frd_h264_dc_abi_pc_call
frd_h264_dc_abi_pc_call:
        blr x3
        .globl frd_h264_dc_abi_pc_return
frd_h264_dc_abi_pc_return:
        /* 先验结构检查：在任何返回后的栈读取之前；绝不恢复坏帧。 */
        frd_dc_return_guards .Lfrd_dc_frame_failure
        b.ne .Lfrd_dc_frame_failure
        mov w0, #0                    /* 被调用核是 void，不检查其 x0。 */
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
        frd_dc_pattern x10, \n
        cmp x\n, x10
        cset w11, ne
        orr w0, w0, w11, lsl #(\n - 19)
.endr
.irp n,8,9,10,11,12,13,14,15
        frd_dc_pattern x10, (0x80+\n)
        fmov x9, d\n
        cmp x9, x10
        cset w11, ne
        orr w0, w0, w11, lsl #(\n + 2)
.endr
        b .Lfrd_dc_epilogue

.Lfrd_dc_frame_failure:
        /* 父进程隔离此失败；不调用 C/SEH 恢复，也不读取潜在坏帧。 */
        brk #0xf000                   /* Windows ARM64 documented breakpoint。 */
        b .Lfrd_dc_frame_failure

.Lfrd_dc_epilogue:
        .globl frd_h264_dc_abi_pc_epilogue
frd_h264_dc_abi_pc_epilogue:
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND != 1 && FRD_DC_WINDOWS_ABI_BAD_UNWIND != 5
        .seh_startepilogue
#endif
        ldp d14, d15, [sp, #144]
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND == 5
        /* 错误控制：scope 晚一条指令开始，但仍描述全套恢复。
         * 固定 assembler 可能直接拒绝大小不一致，届时仅记录该 rejection。 */
        .seh_startepilogue
#endif
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND == 4
        /* 错误控制：真实恢复被标为 nop，尾声元数据漏掉 d14/d15。 */
        .seh_nop
#else
        frd_dc_simd_unwind d14, 144
#endif
        .globl frd_h264_dc_abi_pc_epilogue_after_d14
frd_h264_dc_abi_pc_epilogue_after_d14:
        ldp d12, d13, [sp, #128]
        frd_dc_simd_unwind d12, 128
        ldp d10, d11, [sp, #112]
        frd_dc_simd_unwind d10, 112
        ldp d8, d9, [sp, #96]
        frd_dc_simd_unwind d8, 96
        ldp x27, x28, [sp, #80]
        frd_dc_reg_unwind x27, 80
        ldp x25, x26, [sp, #64]
        frd_dc_reg_unwind x25, 64
        ldp x23, x24, [sp, #48]
        frd_dc_reg_unwind x23, 48
        ldp x21, x22, [sp, #32]
        frd_dc_reg_unwind x21, 32
        ldp x19, x20, [sp, #16]
        frd_dc_reg_unwind x19, 16
        ldp x29, x30, [sp, #0]
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND != 1
        .seh_save_fplr 0
#endif
        add sp, sp, #192
        frd_dc_stack_unwind
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND != 1
        .seh_endepilogue
#endif
        .globl frd_h264_dc_abi_pc_ret
frd_h264_dc_abi_pc_ret:
        ret
        .seh_endproc

#if defined(FRD_DC_WINDOWS_ABI_CONTRACT)
/* 仅 C0 真实 callback 保证 SP=H；独占 32B，无真实 prologue/epilogue。 */
        .globl frd_h264_dc_abi_sp_failure_fragment
        .def frd_h264_dc_abi_sp_failure_fragment
        .scl 2
        .type 32
        .endef
frd_h264_dc_abi_sp_failure_fragment:
.Lfrd_dc_sp_fragment:
        blr x3
        .globl frd_h264_dc_abi_sp_failure_return
frd_h264_dc_abi_sp_failure_return:
        frd_dc_return_guards frd_h264_dc_abi_sp_failure_trap
        b.eq frd_h264_dc_abi_pc_return
        .globl frd_h264_dc_abi_sp_failure_trap
frd_h264_dc_abi_sp_failure_trap:
        brk #0xf000
        b frd_h264_dc_abi_sp_failure_trap
        .globl frd_h264_dc_abi_sp_failure_end
frd_h264_dc_abi_sp_failure_end:
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND != 1
        .section .xdata,"dr"
        .p2align 2
.Lfrd_dc_sp_xdata:
        .long 0x28200008
        .byte 0xe5,0xd9,0x92,0xd9,0x10,0xd8,0x8e,0xd8,0x0c
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND == 3
        .byte 0xe6,0xe6,0xe6,0xc8,0x84,0xc8,0x03,0x40,0x0c,0xe4,0xe3
#else
        .byte 0xe6,0xe6,0xe6,0xe6,0xc8,0x02,0x40
#if FRD_DC_WINDOWS_ABI_BAD_UNWIND == 2
        .byte 0x0d
#else
        .byte 0x0c
#endif
        .byte 0xe4,0xe3,0xe3
#endif
        .section .pdata,"dr"
        .p2align 2
        .rva frd_h264_dc_abi_sp_failure_fragment
        .rva .Lfrd_dc_sp_xdata
        .text
#endif
#endif
