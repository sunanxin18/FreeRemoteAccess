//! 私有正常return直接状态门；单线程、不可重入，不支持exception/full-unwind或shipping内部实例。
use super::native::{Control, CopyFn, DF_BIT, MXCSR_BIT, SP_BIT, X87_BIT};
use core::arch::global_asm;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

static TARGET: AtomicUsize = AtomicUsize::new(0);
static REAL_TARGET: AtomicUsize = AtomicUsize::new(0);
static EXPECTED_SP: AtomicUsize = AtomicUsize::new(0);
static STATUS: AtomicU32 = AtomicU32::new(u32::MAX);
// 只有main的串行闭集访问这些slot；asm同thread自然对齐load/store，禁止信号handler换表。
#[allow(dead_code)]
unsafe extern "Rust" {
    #[link_name = "frd_publication_abi_i686_entry"]
    static ENTRY: u8;
    #[link_name = "frd_publication_abi_i686_bad_ebx_low"]
    static BAD_EBX_LOW: u8;
    #[link_name = "frd_publication_abi_i686_bad_ebx_high"]
    static BAD_EBX_HIGH: u8;
    #[link_name = "frd_publication_abi_i686_bad_ebp_low"]
    static BAD_EBP_LOW: u8;
    #[link_name = "frd_publication_abi_i686_bad_ebp_high"]
    static BAD_EBP_HIGH: u8;
    #[link_name = "frd_publication_abi_i686_bad_esi_low"]
    static BAD_ESI_LOW: u8;
    #[link_name = "frd_publication_abi_i686_bad_esi_high"]
    static BAD_ESI_HIGH: u8;
    #[link_name = "frd_publication_abi_i686_bad_edi_low"]
    static BAD_EDI_LOW: u8;
    #[link_name = "frd_publication_abi_i686_bad_edi_high"]
    static BAD_EDI_HIGH: u8;
    #[link_name = "frd_publication_abi_i686_bad_sp_ret8"]
    static BAD_SP_RET8: u8;
    #[link_name = "frd_publication_abi_i686_bad_df"]
    static BAD_DF: u8;
    #[link_name = "frd_publication_abi_i686_bad_mxcsr"]
    static BAD_MXCSR: u8;
    #[link_name = "frd_publication_abi_i686_bad_x87cw"]
    static BAD_X87CW: u8;
}

#[inline(never)]
unsafe fn invoke(source: &[u8], destination: &mut [u8]) {
    // 只取.text marker地址；精确Rust CopyFn lowering须在最终同exe复核，不作C fat-slice假设。
    let entry: CopyFn = unsafe { core::mem::transmute(core::ptr::addr_of!(ENTRY)) };
    unsafe { entry(source, destination) };
}

pub(super) unsafe fn probe(
    target: CopyFn,
    real: CopyFn,
    source: &[u8],
    destination: &mut [u8],
) -> u32 {
    assert_eq!(source.len() % 4, 0);
    assert_eq!(source.len(), destination.len());
    REAL_TARGET.store(real as usize, Ordering::SeqCst);
    TARGET.store(target as usize, Ordering::SeqCst);
    STATUS.store(u32::MAX, Ordering::SeqCst);
    // 正向target就是真实leaf，return后的检查/恢复完全在同一个asm body内。
    unsafe { invoke(source, destination) };
    STATUS.load(Ordering::SeqCst)
}

pub(super) fn controls() -> Vec<Control> {
    // 全部地址来自本文件固定代码marker，不接用户地址或闭包。
    unsafe {
        let mut cases = vec![
            Control {
                name: "ebx_low",
                code: core::ptr::addr_of!(BAD_EBX_LOW),
                mask: 1 << 0,
            },
            Control {
                name: "ebx_high",
                code: core::ptr::addr_of!(BAD_EBX_HIGH),
                mask: 1 << 0,
            },
            Control {
                name: "ebp_low",
                code: core::ptr::addr_of!(BAD_EBP_LOW),
                mask: 1 << 1,
            },
            Control {
                name: "ebp_high",
                code: core::ptr::addr_of!(BAD_EBP_HIGH),
                mask: 1 << 1,
            },
            Control {
                name: "esi_low",
                code: core::ptr::addr_of!(BAD_ESI_LOW),
                mask: 1 << 2,
            },
            Control {
                name: "esi_high",
                code: core::ptr::addr_of!(BAD_ESI_HIGH),
                mask: 1 << 2,
            },
            Control {
                name: "edi_low",
                code: core::ptr::addr_of!(BAD_EDI_LOW),
                mask: 1 << 3,
            },
            Control {
                name: "edi_high",
                code: core::ptr::addr_of!(BAD_EDI_HIGH),
                mask: 1 << 3,
            },
            Control {
                name: "sp_ret8",
                code: core::ptr::addr_of!(BAD_SP_RET8),
                mask: SP_BIT,
            },
            Control {
                name: "df",
                code: core::ptr::addr_of!(BAD_DF),
                mask: DF_BIT,
            },
            Control {
                name: "mxcsr",
                code: core::ptr::addr_of!(BAD_MXCSR),
                mask: MXCSR_BIT,
            },
            Control {
                name: "x87cw",
                code: core::ptr::addr_of!(BAD_X87CW),
                mask: X87_BIT,
            },
        ];
        cases
    }
}

// P=S-140；outgoing P0..15，saved state>=P32，EBP链P136/return P140。
// PC thunk临时return槽普通P-4、ret8后P+4；均不碰saved state，非通用坏SP恢复。
// ENDBR编码/PE relocation/ELF GOT与实际CET/IBT/CFG须后继准入，绝不关闭保护。
global_asm!(r#"
.text
.p2align 4
.globl {entry}
.if {windows}
.def {entry}; .scl 2; .type 32; .endef
.else
.type {entry},@function
.endif
{entry}:
.if !{windows}
.cfi_startproc
.endif
.byte 0xf3, 0x0f, 0x1e, 0xfb
pushl %ebp
.if !{windows}
.cfi_def_cfa_offset 8
.cfi_offset %ebp,-8
.endif
movl %esp, %ebp
subl $136, %esp
.if !{windows}
.cfi_def_cfa_offset 144
.endif
movl %ebx, 32(%esp)
.if !{windows}
.cfi_offset %ebx, -112
.endif
movl %esi, 40(%esp)
.if !{windows}
.cfi_offset %esi, -104
.endif
movl %edi, 44(%esp)
.if !{windows}
.cfi_offset %edi, -100
.endif
movl 144(%esp), %eax
movl %eax, 0(%esp)
movl 148(%esp), %eax
movl %eax, 4(%esp)
movl 152(%esp), %eax
movl %eax, 8(%esp)
movl 156(%esp), %eax
movl %eax, 12(%esp)
stmxcsr 52(%esp)
fnstcw 60(%esp)
movb $0, 49(%esp)
cld
.if {windows}
movl %esp, {expected}
movl {target}, %eax
.else
calll .Lfrd_i686_pc_thunk
.Lfrd_i686_setup_pc:
.Lfrd_i686_setup_got:
addl $_GLOBAL_OFFSET_TABLE_+(.Lfrd_i686_setup_got-.Lfrd_i686_setup_pc), %eax
movl %esp, {expected}@GOTOFF(%eax)
movl {target}@GOTOFF(%eax), %eax
.endif
movl $0x11223344, %ebx
movl $0x22334455, %ebp
movl $0x33445566, %esi
movl $0x44556677, %edi
calll *%eax
# 第一条保存actualESP；本地PC thunk只改volatile EAX且正常CALL/RET配对。
movl %esp, %ecx
.if {windows}
movl {expected}, %eax
.else
calll .Lfrd_i686_pc_thunk
.Lfrd_i686_return_pc:
.Lfrd_i686_return_got:
addl $_GLOBAL_OFFSET_TABLE_+(.Lfrd_i686_return_got-.Lfrd_i686_return_pc), %eax
movl {expected}@GOTOFF(%eax), %eax
.endif
movl %eax, %esp
xorl %edx, %edx
cmpl %eax, %ecx
je .Lfrd_i686_sp_ok
orl $0x40000, %edx
.Lfrd_i686_sp_ok:
cmpl $0x11223344, %ebx
je .Lfrd_i686_ebx_ok
orl $1, %edx
.Lfrd_i686_ebx_ok:
cmpl $0x22334455, %ebp
je .Lfrd_i686_ebp_ok
orl $2, %edx
.Lfrd_i686_ebp_ok:
cmpl $0x33445566, %esi
je .Lfrd_i686_esi_ok
orl $4, %edx
.Lfrd_i686_esi_ok:
cmpl $0x44556677, %edi
je .Lfrd_i686_edi_ok
orl $8, %edx
.Lfrd_i686_edi_ok:
# 全部GPR已捕获才用ESI观察DF；LODSB只读frame+49，更新指针到+48/+50。
leal 49(%esp), %esi
lodsb
leal 50(%esp), %eax
cmpl %eax, %esi
je .Lfrd_i686_df_ok
orl $0x80000, %edx
.Lfrd_i686_df_ok:
cld
stmxcsr 56(%esp)
fnstcw 62(%esp)
movl 56(%esp), %eax
xorl 52(%esp), %eax
andl $0xffc0, %eax
je .Lfrd_i686_mxcsr_ok
orl $0x100000, %edx
.Lfrd_i686_mxcsr_ok:
movzwl 62(%esp), %eax
xorw 60(%esp), %ax
andl $0x1f3f, %eax
je .Lfrd_i686_x87_ok
orl $0x200000, %edx
.Lfrd_i686_x87_ok:
.if {windows}
movl %edx, {status}
.else
calll .Lfrd_i686_pc_thunk
.Lfrd_i686_status_pc:
.Lfrd_i686_status_got:
addl $_GLOBAL_OFFSET_TABLE_+(.Lfrd_i686_status_got-.Lfrd_i686_status_pc), %eax
movl %edx, {status}@GOTOFF(%eax)
.endif
ldmxcsr 52(%esp)
fldcw 60(%esp)
movl 32(%esp), %ebx
.if !{windows}
.cfi_restore %ebx
.endif
movl 40(%esp), %esi
.if !{windows}
.cfi_restore %esi
.endif
movl 44(%esp), %edi
.if !{windows}
.cfi_restore %edi
.endif
addl $136, %esp
.if !{windows}
.cfi_def_cfa_offset 8
.endif
popl %ebp
.if !{windows}
.cfi_restore %ebp
.cfi_def_cfa_offset 4
.endif
retl
.if !{windows}
.cfi_endproc
.size {entry},.-{entry}
.endif
.text
.p2align 2
.if !{windows}
.type .Lfrd_i686_pc_thunk,@function
.endif
.Lfrd_i686_pc_thunk:
.if !{windows}
.cfi_startproc
.cfi_def_cfa_offset 4
.endif
# 本地直接CALL/RET；返回地址从实际栈读到EAX，不依赖任何nonvolatile。
movl (%esp), %eax
retl
.if !{windows}
.cfi_endproc
.size .Lfrd_i686_pc_thunk,.-.Lfrd_i686_pc_thunk
.endif
.macro frd_publication_i686_bad name, mode, reg
.text
.p2align 4
.globl \name
.if {windows}
.def \name; .scl 2; .type 32; .endef
.else
.type \name,@function
.endif
\name:
.if !{windows}
.cfi_startproc
.endif
.byte 0xf3, 0x0f, 0x1e, 0xfb
subl $28, %esp
.if !{windows}
.cfi_def_cfa_offset 32
.endif
movl 32(%esp), %eax
movl %eax, 0(%esp)
movl 36(%esp), %eax
movl %eax, 4(%esp)
movl 40(%esp), %eax
movl %eax, 8(%esp)
movl 44(%esp), %eax
movl %eax, 12(%esp)
.if {windows}
movl {real}, %eax
.else
calll .Lfrd_i686_pc_thunk
.Lfrd_i686_bad_pc\@:
.Lfrd_i686_bad_got\@:
addl $_GLOBAL_OFFSET_TABLE_+(.Lfrd_i686_bad_got\@-.Lfrd_i686_bad_pc\@), %eax
movl {real}@GOTOFF(%eax), %eax
.endif
calll *%eax
.if \mode == 0
xorl $1, \reg
.elseif \mode == 1
xorl $65536, \reg
.elseif \mode == 3
std
.elseif \mode == 4
stmxcsr 16(%esp)
xorl $0x2000, 16(%esp)
ldmxcsr 16(%esp)
.elseif \mode == 5
fnstcw 16(%esp)
xorw $0x0400, 16(%esp)
fldcw 16(%esp)
.endif
addl $28, %esp
.if !{windows}
.cfi_def_cfa_offset 4
.endif
.if \mode == 2
retl $8
.else
retl
.endif
.if !{windows}
.cfi_endproc
.size \name,.-\name
.endif
.endm
frd_publication_i686_bad {bad_ebx_low}, 0, %ebx
frd_publication_i686_bad {bad_ebx_high}, 1, %ebx
frd_publication_i686_bad {bad_ebp_low}, 0, %ebp
frd_publication_i686_bad {bad_ebp_high}, 1, %ebp
frd_publication_i686_bad {bad_esi_low}, 0, %esi
frd_publication_i686_bad {bad_esi_high}, 1, %esi
frd_publication_i686_bad {bad_edi_low}, 0, %edi
frd_publication_i686_bad {bad_edi_high}, 1, %edi
frd_publication_i686_bad {bad_sp_ret8}, 2, %eax
frd_publication_i686_bad {bad_df}, 3, %eax
frd_publication_i686_bad {bad_mxcsr}, 4, %eax
frd_publication_i686_bad {bad_x87cw}, 5, %eax
.if !{windows}
.section .note.GNU-stack,"",@progbits
.endif
"#,
    entry = sym ENTRY,
    target = sym TARGET,
    real = sym REAL_TARGET,
    expected = sym EXPECTED_SP,
    status = sym STATUS,
    windows = const cfg!(windows) as u8,
    bad_ebx_low = sym BAD_EBX_LOW,
    bad_ebx_high = sym BAD_EBX_HIGH,
    bad_ebp_low = sym BAD_EBP_LOW,
    bad_ebp_high = sym BAD_EBP_HIGH,
    bad_esi_low = sym BAD_ESI_LOW,
    bad_esi_high = sym BAD_ESI_HIGH,
    bad_edi_low = sym BAD_EDI_LOW,
    bad_edi_high = sym BAD_EDI_HIGH,
    bad_sp_ret8 = sym BAD_SP_RET8,
    bad_df = sym BAD_DF,
    bad_mxcsr = sym BAD_MXCSR,
    bad_x87cw = sym BAD_X87CW,
    options(att_syntax),
);
