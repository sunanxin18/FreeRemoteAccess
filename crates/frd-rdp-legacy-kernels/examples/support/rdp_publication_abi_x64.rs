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
    #[link_name = "frd_publication_abi_x64_entry"]
    static ENTRY: u8;
    #[link_name = "frd_publication_abi_x64_bad_rbx_low"]
    static BAD_RBX_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_rbx_high"]
    static BAD_RBX_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_rbp_low"]
    static BAD_RBP_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_rbp_high"]
    static BAD_RBP_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_r12_low"]
    static BAD_R12_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_r12_high"]
    static BAD_R12_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_r13_low"]
    static BAD_R13_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_r13_high"]
    static BAD_R13_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_r14_low"]
    static BAD_R14_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_r14_high"]
    static BAD_R14_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_r15_low"]
    static BAD_R15_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_r15_high"]
    static BAD_R15_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_rsi_low"]
    static BAD_RSI_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_rsi_high"]
    static BAD_RSI_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_rdi_low"]
    static BAD_RDI_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_rdi_high"]
    static BAD_RDI_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm6_low"]
    static BAD_XMM6_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm6_high"]
    static BAD_XMM6_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm7_low"]
    static BAD_XMM7_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm7_high"]
    static BAD_XMM7_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm8_low"]
    static BAD_XMM8_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm8_high"]
    static BAD_XMM8_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm9_low"]
    static BAD_XMM9_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm9_high"]
    static BAD_XMM9_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm10_low"]
    static BAD_XMM10_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm10_high"]
    static BAD_XMM10_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm11_low"]
    static BAD_XMM11_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm11_high"]
    static BAD_XMM11_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm12_low"]
    static BAD_XMM12_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm12_high"]
    static BAD_XMM12_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm13_low"]
    static BAD_XMM13_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm13_high"]
    static BAD_XMM13_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm14_low"]
    static BAD_XMM14_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm14_high"]
    static BAD_XMM14_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm15_low"]
    static BAD_XMM15_LOW: u8;
    #[link_name = "frd_publication_abi_x64_bad_xmm15_high"]
    static BAD_XMM15_HIGH: u8;
    #[link_name = "frd_publication_abi_x64_bad_sp_ret8"]
    static BAD_SP_RET8: u8;
    #[link_name = "frd_publication_abi_x64_bad_df"]
    static BAD_DF: u8;
    #[link_name = "frd_publication_abi_x64_bad_mxcsr"]
    static BAD_MXCSR: u8;
    #[link_name = "frd_publication_abi_x64_bad_x87cw"]
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
                name: "rbx_low",
                code: core::ptr::addr_of!(BAD_RBX_LOW),
                mask: 1 << 0,
            },
            Control {
                name: "rbx_high",
                code: core::ptr::addr_of!(BAD_RBX_HIGH),
                mask: 1 << 0,
            },
            Control {
                name: "rbp_low",
                code: core::ptr::addr_of!(BAD_RBP_LOW),
                mask: 1 << 1,
            },
            Control {
                name: "rbp_high",
                code: core::ptr::addr_of!(BAD_RBP_HIGH),
                mask: 1 << 1,
            },
            Control {
                name: "r12_low",
                code: core::ptr::addr_of!(BAD_R12_LOW),
                mask: 1 << 2,
            },
            Control {
                name: "r12_high",
                code: core::ptr::addr_of!(BAD_R12_HIGH),
                mask: 1 << 2,
            },
            Control {
                name: "r13_low",
                code: core::ptr::addr_of!(BAD_R13_LOW),
                mask: 1 << 3,
            },
            Control {
                name: "r13_high",
                code: core::ptr::addr_of!(BAD_R13_HIGH),
                mask: 1 << 3,
            },
            Control {
                name: "r14_low",
                code: core::ptr::addr_of!(BAD_R14_LOW),
                mask: 1 << 4,
            },
            Control {
                name: "r14_high",
                code: core::ptr::addr_of!(BAD_R14_HIGH),
                mask: 1 << 4,
            },
            Control {
                name: "r15_low",
                code: core::ptr::addr_of!(BAD_R15_LOW),
                mask: 1 << 5,
            },
            Control {
                name: "r15_high",
                code: core::ptr::addr_of!(BAD_R15_HIGH),
                mask: 1 << 5,
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
        #[cfg(windows)]
        cases.extend([
            Control {
                name: "rsi_low",
                code: core::ptr::addr_of!(BAD_RSI_LOW),
                mask: 1 << 6,
            },
            Control {
                name: "rsi_high",
                code: core::ptr::addr_of!(BAD_RSI_HIGH),
                mask: 1 << 6,
            },
            Control {
                name: "rdi_low",
                code: core::ptr::addr_of!(BAD_RDI_LOW),
                mask: 1 << 7,
            },
            Control {
                name: "rdi_high",
                code: core::ptr::addr_of!(BAD_RDI_HIGH),
                mask: 1 << 7,
            },
            Control {
                name: "xmm6_low",
                code: core::ptr::addr_of!(BAD_XMM6_LOW),
                mask: 1 << 8,
            },
            Control {
                name: "xmm6_high",
                code: core::ptr::addr_of!(BAD_XMM6_HIGH),
                mask: 1 << 8,
            },
            Control {
                name: "xmm7_low",
                code: core::ptr::addr_of!(BAD_XMM7_LOW),
                mask: 1 << 9,
            },
            Control {
                name: "xmm7_high",
                code: core::ptr::addr_of!(BAD_XMM7_HIGH),
                mask: 1 << 9,
            },
            Control {
                name: "xmm8_low",
                code: core::ptr::addr_of!(BAD_XMM8_LOW),
                mask: 1 << 10,
            },
            Control {
                name: "xmm8_high",
                code: core::ptr::addr_of!(BAD_XMM8_HIGH),
                mask: 1 << 10,
            },
            Control {
                name: "xmm9_low",
                code: core::ptr::addr_of!(BAD_XMM9_LOW),
                mask: 1 << 11,
            },
            Control {
                name: "xmm9_high",
                code: core::ptr::addr_of!(BAD_XMM9_HIGH),
                mask: 1 << 11,
            },
            Control {
                name: "xmm10_low",
                code: core::ptr::addr_of!(BAD_XMM10_LOW),
                mask: 1 << 12,
            },
            Control {
                name: "xmm10_high",
                code: core::ptr::addr_of!(BAD_XMM10_HIGH),
                mask: 1 << 12,
            },
            Control {
                name: "xmm11_low",
                code: core::ptr::addr_of!(BAD_XMM11_LOW),
                mask: 1 << 13,
            },
            Control {
                name: "xmm11_high",
                code: core::ptr::addr_of!(BAD_XMM11_HIGH),
                mask: 1 << 13,
            },
            Control {
                name: "xmm12_low",
                code: core::ptr::addr_of!(BAD_XMM12_LOW),
                mask: 1 << 14,
            },
            Control {
                name: "xmm12_high",
                code: core::ptr::addr_of!(BAD_XMM12_HIGH),
                mask: 1 << 14,
            },
            Control {
                name: "xmm13_low",
                code: core::ptr::addr_of!(BAD_XMM13_LOW),
                mask: 1 << 15,
            },
            Control {
                name: "xmm13_high",
                code: core::ptr::addr_of!(BAD_XMM13_HIGH),
                mask: 1 << 15,
            },
            Control {
                name: "xmm14_low",
                code: core::ptr::addr_of!(BAD_XMM14_LOW),
                mask: 1 << 16,
            },
            Control {
                name: "xmm14_high",
                code: core::ptr::addr_of!(BAD_XMM14_HIGH),
                mask: 1 << 16,
            },
            Control {
                name: "xmm15_low",
                code: core::ptr::addr_of!(BAD_XMM15_LOW),
                mask: 1 << 17,
            },
            Control {
                name: "xmm15_high",
                code: core::ptr::addr_of!(BAD_XMM15_HIGH),
                mask: 1 << 17,
            },
        ]);
        cases
    }
}

// frame/bit表见实现brief。ENDBR字节只是landing候选；最终CET/IBT/CFG配置/编码另门准入。
// 不关闭保护；正常return之外的SEH/异步walk/任意坏SP恢复均未授资格。
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
.if {windows}
.seh_proc {entry}
.else
.cfi_startproc
.endif
.byte 0xf3, 0x0f, 0x1e, 0xfa
subq $408, %rsp
.if {windows}
.seh_stackalloc 408
.else
.cfi_def_cfa_offset 416
.endif
movq %rbx, 64(%rsp)
.if {windows}
.seh_savereg %rbx, 64
.else
.cfi_offset %rbx, -352
.endif
movq %rbp, 72(%rsp)
.if {windows}
.seh_savereg %rbp, 72
.else
.cfi_offset %rbp, -344
.endif
movq %r12, 80(%rsp)
.if {windows}
.seh_savereg %r12, 80
.else
.cfi_offset %r12, -336
.endif
movq %r13, 88(%rsp)
.if {windows}
.seh_savereg %r13, 88
.else
.cfi_offset %r13, -328
.endif
movq %r14, 96(%rsp)
.if {windows}
.seh_savereg %r14, 96
.else
.cfi_offset %r14, -320
.endif
movq %r15, 104(%rsp)
.if {windows}
.seh_savereg %r15, 104
.else
.cfi_offset %r15, -312
.endif
movq %rsi, 112(%rsp)
.if {windows}
.seh_savereg %rsi, 112
.else
.endif
movq %rdi, 120(%rsp)
.if {windows}
.seh_savereg %rdi, 120
.else
.endif
.if {windows}
movdqa %xmm6, 128(%rsp)
.seh_savexmm %xmm6, 128
movdqa %xmm7, 144(%rsp)
.seh_savexmm %xmm7, 144
movdqa %xmm8, 160(%rsp)
.seh_savexmm %xmm8, 160
movdqa %xmm9, 176(%rsp)
.seh_savexmm %xmm9, 176
movdqa %xmm10, 192(%rsp)
.seh_savexmm %xmm10, 192
movdqa %xmm11, 208(%rsp)
.seh_savexmm %xmm11, 208
movdqa %xmm12, 224(%rsp)
.seh_savexmm %xmm12, 224
movdqa %xmm13, 240(%rsp)
.seh_savexmm %xmm13, 240
movdqa %xmm14, 256(%rsp)
.seh_savexmm %xmm14, 256
movdqa %xmm15, 272(%rsp)
.seh_savexmm %xmm15, 272
.seh_endprologue
.endif
stmxcsr 32(%rsp)
fnstcw 40(%rsp)
movb $0, 49(%rsp)
cld
movabsq $0x1122334455667788, %rbx
movabsq $0x2233445566778899, %rbp
movabsq $0x33445566778899aa, %r12
movabsq $0x445566778899aabb, %r13
movabsq $0x5566778899aabbcc, %r14
movabsq $0x66778899aabbccdd, %r15
.if {windows}
movabsq $0x778899aabbccddee, %rsi
.endif
.if {windows}
movabsq $0x8899aabbccddeeff, %rdi
.endif
.if {windows}
movdqa .Lfrd_x64_xmm6(%rip), %xmm6
movdqa .Lfrd_x64_xmm7(%rip), %xmm7
movdqa .Lfrd_x64_xmm8(%rip), %xmm8
movdqa .Lfrd_x64_xmm9(%rip), %xmm9
movdqa .Lfrd_x64_xmm10(%rip), %xmm10
movdqa .Lfrd_x64_xmm11(%rip), %xmm11
movdqa .Lfrd_x64_xmm12(%rip), %xmm12
movdqa .Lfrd_x64_xmm13(%rip), %xmm13
movdqa .Lfrd_x64_xmm14(%rip), %xmm14
movdqa .Lfrd_x64_xmm15(%rip), %xmm15
.endif
movq %rsp, {expected}(%rip)
callq *{target}(%rip)
# 返回后先捕获SP；不先读frame，不信任待测RBP/其他nonvolatile。
movq %rsp, %r11
movq {expected}(%rip), %rax
movq %rax, %rsp
xorl %r10d, %r10d
cmpq %rax, %r11
je .Lfrd_x64_sp_ok
orl $0x40000, %r10d
.Lfrd_x64_sp_ok:
movabsq $0x1122334455667788, %rax
cmpq %rax, %rbx
je .Lfrd_x64_rbx_ok
orl $1, %r10d
.Lfrd_x64_rbx_ok:
movabsq $0x2233445566778899, %rax
cmpq %rax, %rbp
je .Lfrd_x64_rbp_ok
orl $2, %r10d
.Lfrd_x64_rbp_ok:
movabsq $0x33445566778899aa, %rax
cmpq %rax, %r12
je .Lfrd_x64_r12_ok
orl $4, %r10d
.Lfrd_x64_r12_ok:
movabsq $0x445566778899aabb, %rax
cmpq %rax, %r13
je .Lfrd_x64_r13_ok
orl $8, %r10d
.Lfrd_x64_r13_ok:
movabsq $0x5566778899aabbcc, %rax
cmpq %rax, %r14
je .Lfrd_x64_r14_ok
orl $16, %r10d
.Lfrd_x64_r14_ok:
movabsq $0x66778899aabbccdd, %rax
cmpq %rax, %r15
je .Lfrd_x64_r15_ok
orl $32, %r10d
.Lfrd_x64_r15_ok:
.if {windows}
movabsq $0x778899aabbccddee, %rax
cmpq %rax, %rsi
je .Lfrd_x64_rsi_ok
orl $64, %r10d
.Lfrd_x64_rsi_ok:
.endif
.if {windows}
movabsq $0x8899aabbccddeeff, %rax
cmpq %rax, %rdi
je .Lfrd_x64_rdi_ok
orl $128, %r10d
.Lfrd_x64_rdi_ok:
.endif
.if {windows}
movdqa %xmm6, %xmm0
pcmpeqb .Lfrd_x64_xmm6(%rip), %xmm0
pmovmskb %xmm0, %eax
cmpl $65535, %eax
je .Lfrd_x64_xmm6_ok
orl $256, %r10d
.Lfrd_x64_xmm6_ok:
movdqa %xmm7, %xmm0
pcmpeqb .Lfrd_x64_xmm7(%rip), %xmm0
pmovmskb %xmm0, %eax
cmpl $65535, %eax
je .Lfrd_x64_xmm7_ok
orl $512, %r10d
.Lfrd_x64_xmm7_ok:
movdqa %xmm8, %xmm0
pcmpeqb .Lfrd_x64_xmm8(%rip), %xmm0
pmovmskb %xmm0, %eax
cmpl $65535, %eax
je .Lfrd_x64_xmm8_ok
orl $1024, %r10d
.Lfrd_x64_xmm8_ok:
movdqa %xmm9, %xmm0
pcmpeqb .Lfrd_x64_xmm9(%rip), %xmm0
pmovmskb %xmm0, %eax
cmpl $65535, %eax
je .Lfrd_x64_xmm9_ok
orl $2048, %r10d
.Lfrd_x64_xmm9_ok:
movdqa %xmm10, %xmm0
pcmpeqb .Lfrd_x64_xmm10(%rip), %xmm0
pmovmskb %xmm0, %eax
cmpl $65535, %eax
je .Lfrd_x64_xmm10_ok
orl $4096, %r10d
.Lfrd_x64_xmm10_ok:
movdqa %xmm11, %xmm0
pcmpeqb .Lfrd_x64_xmm11(%rip), %xmm0
pmovmskb %xmm0, %eax
cmpl $65535, %eax
je .Lfrd_x64_xmm11_ok
orl $8192, %r10d
.Lfrd_x64_xmm11_ok:
movdqa %xmm12, %xmm0
pcmpeqb .Lfrd_x64_xmm12(%rip), %xmm0
pmovmskb %xmm0, %eax
cmpl $65535, %eax
je .Lfrd_x64_xmm12_ok
orl $16384, %r10d
.Lfrd_x64_xmm12_ok:
movdqa %xmm13, %xmm0
pcmpeqb .Lfrd_x64_xmm13(%rip), %xmm0
pmovmskb %xmm0, %eax
cmpl $65535, %eax
je .Lfrd_x64_xmm13_ok
orl $32768, %r10d
.Lfrd_x64_xmm13_ok:
movdqa %xmm14, %xmm0
pcmpeqb .Lfrd_x64_xmm14(%rip), %xmm0
pmovmskb %xmm0, %eax
cmpl $65535, %eax
je .Lfrd_x64_xmm14_ok
orl $65536, %r10d
.Lfrd_x64_xmm14_ok:
movdqa %xmm15, %xmm0
pcmpeqb .Lfrd_x64_xmm15(%rip), %xmm0
pmovmskb %xmm0, %eax
cmpl $65535, %eax
je .Lfrd_x64_xmm15_ok
orl $131072, %r10d
.Lfrd_x64_xmm15_ok:
.endif
# 全部GPR已比较，才借RSI做安全单字节DF观察；不改变body RSP。
leaq 49(%rsp), %rsi
lodsb
leaq 50(%rsp), %rax
cmpq %rax, %rsi
je .Lfrd_x64_df_ok
orl $0x80000, %r10d
.Lfrd_x64_df_ok:
cld
stmxcsr 36(%rsp)
fnstcw 42(%rsp)
movl 36(%rsp), %eax
xorl 32(%rsp), %eax
andl $0xffc0, %eax
je .Lfrd_x64_mxcsr_ok
orl $0x100000, %r10d
.Lfrd_x64_mxcsr_ok:
movzwl 42(%rsp), %eax
xorw 40(%rsp), %ax
andl $0x1f3f, %eax
je .Lfrd_x64_x87_ok
orl $0x200000, %r10d
.Lfrd_x64_x87_ok:
movl %r10d, {status}(%rip)
# 记录后恢复原完整控制字、caller SIMD/GPR/SP，之后才回Rust。
ldmxcsr 32(%rsp)
fldcw 40(%rsp)
.if {windows}
movdqa 128(%rsp), %xmm6
movdqa 144(%rsp), %xmm7
movdqa 160(%rsp), %xmm8
movdqa 176(%rsp), %xmm9
movdqa 192(%rsp), %xmm10
movdqa 208(%rsp), %xmm11
movdqa 224(%rsp), %xmm12
movdqa 240(%rsp), %xmm13
movdqa 256(%rsp), %xmm14
movdqa 272(%rsp), %xmm15
.endif
movq 64(%rsp), %rbx
.if !{windows}
.cfi_restore %rbx
.endif
movq 72(%rsp), %rbp
.if !{windows}
.cfi_restore %rbp
.endif
movq 80(%rsp), %r12
.if !{windows}
.cfi_restore %r12
.endif
movq 88(%rsp), %r13
.if !{windows}
.cfi_restore %r13
.endif
movq 96(%rsp), %r14
.if !{windows}
.cfi_restore %r14
.endif
movq 104(%rsp), %r15
.if !{windows}
.cfi_restore %r15
.endif
movq 112(%rsp), %rsi
movq 120(%rsp), %rdi
.if {windows}
.seh_startepilogue
.endif
addq $408, %rsp
.if {windows}
.seh_endepilogue
.else
.cfi_def_cfa_offset 8
.endif
retq
.if {windows}
.seh_endproc
.else
.cfi_endproc
.size {entry},.-{entry}
.endif
.macro frd_publication_x64_bad name, mode, reg
.text
.p2align 4
.globl \name
.if {windows}
.def \name; .scl 2; .type 32; .endef
.else
.type \name,@function
.endif
\name:
.if {windows}
.seh_proc \name
.else
.cfi_startproc
.endif
.byte 0xf3, 0x0f, 0x1e, 0xfa
subq $40, %rsp
.if {windows}
.seh_stackalloc 40
.seh_endprologue
.else
.cfi_def_cfa_offset 48
.endif
callq *{real}(%rip)
.if \mode == 0
xorq $1, \reg
.elseif \mode == 1
movabsq $0x100000000, %rax
xorq %rax, \reg
.elseif \mode == 2
pxor .Lfrd_x64_flip_low(%rip), \reg
.elseif \mode == 3
pxor .Lfrd_x64_flip_high(%rip), \reg
.elseif \mode == 5
std
.elseif \mode == 6
stmxcsr 32(%rsp)
xorl $0x2000, 32(%rsp)
ldmxcsr 32(%rsp)
.elseif \mode == 7
fnstcw 32(%rsp)
xorw $0x0400, 32(%rsp)
fldcw 32(%rsp)
.endif
.if {windows}
.seh_startepilogue
.endif
addq $40, %rsp
.if {windows}
.seh_endepilogue
.else
.cfi_def_cfa_offset 8
.endif
.if \mode == 4
retq $8
.else
retq
.endif
.if {windows}
.seh_endproc
.else
.cfi_endproc
.size \name,.-\name
.endif
.endm
frd_publication_x64_bad {bad_rbx_low}, 0, %rbx
frd_publication_x64_bad {bad_rbx_high}, 1, %rbx
frd_publication_x64_bad {bad_rbp_low}, 0, %rbp
frd_publication_x64_bad {bad_rbp_high}, 1, %rbp
frd_publication_x64_bad {bad_r12_low}, 0, %r12
frd_publication_x64_bad {bad_r12_high}, 1, %r12
frd_publication_x64_bad {bad_r13_low}, 0, %r13
frd_publication_x64_bad {bad_r13_high}, 1, %r13
frd_publication_x64_bad {bad_r14_low}, 0, %r14
frd_publication_x64_bad {bad_r14_high}, 1, %r14
frd_publication_x64_bad {bad_r15_low}, 0, %r15
frd_publication_x64_bad {bad_r15_high}, 1, %r15
.if {windows}
frd_publication_x64_bad {bad_rsi_low}, 0, %rsi
.endif
.if {windows}
frd_publication_x64_bad {bad_rsi_high}, 1, %rsi
.endif
.if {windows}
frd_publication_x64_bad {bad_rdi_low}, 0, %rdi
.endif
.if {windows}
frd_publication_x64_bad {bad_rdi_high}, 1, %rdi
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm6_low}, 2, %xmm6
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm6_high}, 3, %xmm6
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm7_low}, 2, %xmm7
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm7_high}, 3, %xmm7
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm8_low}, 2, %xmm8
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm8_high}, 3, %xmm8
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm9_low}, 2, %xmm9
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm9_high}, 3, %xmm9
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm10_low}, 2, %xmm10
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm10_high}, 3, %xmm10
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm11_low}, 2, %xmm11
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm11_high}, 3, %xmm11
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm12_low}, 2, %xmm12
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm12_high}, 3, %xmm12
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm13_low}, 2, %xmm13
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm13_high}, 3, %xmm13
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm14_low}, 2, %xmm14
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm14_high}, 3, %xmm14
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm15_low}, 2, %xmm15
.endif
.if {windows}
frd_publication_x64_bad {bad_xmm15_high}, 3, %xmm15
.endif
frd_publication_x64_bad {bad_sp_ret8}, 4, %rax
frd_publication_x64_bad {bad_df}, 5, %rax
frd_publication_x64_bad {bad_mxcsr}, 6, %rax
frd_publication_x64_bad {bad_x87cw}, 7, %rax
.if {windows}
.section .rdata,"dr"
.else
.section .rodata
.endif
.p2align 4
.Lfrd_x64_flip_low:
.quad 1, 0
.Lfrd_x64_flip_high:
.quad 0, 1
.Lfrd_x64_xmm6:
.quad 0x13121110a3a2a1a0, 0x9b9a9998c3c2c1c0
.Lfrd_x64_xmm7:
.quad 0x14131211a4a3a2a1, 0x9c9b9a99c4c3c2c1
.Lfrd_x64_xmm8:
.quad 0x15141312a5a4a3a2, 0x9d9c9b9ac5c4c3c2
.Lfrd_x64_xmm9:
.quad 0x16151413a6a5a4a3, 0x9e9d9c9bc6c5c4c3
.Lfrd_x64_xmm10:
.quad 0x17161514a7a6a5a4, 0x9f9e9d9cc7c6c5c4
.Lfrd_x64_xmm11:
.quad 0x18171615a8a7a6a5, 0xa09f9e9dc8c7c6c5
.Lfrd_x64_xmm12:
.quad 0x19181716a9a8a7a6, 0xa1a09f9ec9c8c7c6
.Lfrd_x64_xmm13:
.quad 0x1a191817aaa9a8a7, 0xa2a1a09fcac9c8c7
.Lfrd_x64_xmm14:
.quad 0x1b1a1918abaaa9a8, 0xa3a2a1a0cbcac9c8
.Lfrd_x64_xmm15:
.quad 0x1c1b1a19acabaaa9, 0xa4a3a2a1cccbcac9
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
    bad_rbx_low = sym BAD_RBX_LOW,
    bad_rbx_high = sym BAD_RBX_HIGH,
    bad_rbp_low = sym BAD_RBP_LOW,
    bad_rbp_high = sym BAD_RBP_HIGH,
    bad_r12_low = sym BAD_R12_LOW,
    bad_r12_high = sym BAD_R12_HIGH,
    bad_r13_low = sym BAD_R13_LOW,
    bad_r13_high = sym BAD_R13_HIGH,
    bad_r14_low = sym BAD_R14_LOW,
    bad_r14_high = sym BAD_R14_HIGH,
    bad_r15_low = sym BAD_R15_LOW,
    bad_r15_high = sym BAD_R15_HIGH,
    bad_rsi_low = sym BAD_RSI_LOW,
    bad_rsi_high = sym BAD_RSI_HIGH,
    bad_rdi_low = sym BAD_RDI_LOW,
    bad_rdi_high = sym BAD_RDI_HIGH,
    bad_xmm6_low = sym BAD_XMM6_LOW,
    bad_xmm6_high = sym BAD_XMM6_HIGH,
    bad_xmm7_low = sym BAD_XMM7_LOW,
    bad_xmm7_high = sym BAD_XMM7_HIGH,
    bad_xmm8_low = sym BAD_XMM8_LOW,
    bad_xmm8_high = sym BAD_XMM8_HIGH,
    bad_xmm9_low = sym BAD_XMM9_LOW,
    bad_xmm9_high = sym BAD_XMM9_HIGH,
    bad_xmm10_low = sym BAD_XMM10_LOW,
    bad_xmm10_high = sym BAD_XMM10_HIGH,
    bad_xmm11_low = sym BAD_XMM11_LOW,
    bad_xmm11_high = sym BAD_XMM11_HIGH,
    bad_xmm12_low = sym BAD_XMM12_LOW,
    bad_xmm12_high = sym BAD_XMM12_HIGH,
    bad_xmm13_low = sym BAD_XMM13_LOW,
    bad_xmm13_high = sym BAD_XMM13_HIGH,
    bad_xmm14_low = sym BAD_XMM14_LOW,
    bad_xmm14_high = sym BAD_XMM14_HIGH,
    bad_xmm15_low = sym BAD_XMM15_LOW,
    bad_xmm15_high = sym BAD_XMM15_HIGH,
    bad_sp_ret8 = sym BAD_SP_RET8,
    bad_df = sym BAD_DF,
    bad_mxcsr = sym BAD_MXCSR,
    bad_x87cw = sym BAD_X87CW,
    options(att_syntax),
);
