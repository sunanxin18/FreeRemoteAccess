//! 当前 macOS AArch64 的实际调用：填满 AAPCS64 所有非易失 GPR 与 SIMD low64。
//! Windows/Linux/OHOS 对象编译不能替代本机 ABI 执行。
#![cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[path = "common/pixel.rs"]
mod frozen;
use frd_rdp_legacy_kernels::NativeKernels;
use frozen::*;
struct Context {
    kernels: NativeKernels,
    records: Vec<Record>,
    success: bool,
}
extern "C" fn invoke(context: *mut core::ffi::c_void) {
    let context = unsafe { &mut *context.cast::<Context>() };
    context.success = true;
    for record in &context.records {
        for n in [1, 3, 4, 7, 8, 65] {
            let mut out = vec![0; 4 * n];
            let result = context.kernels.convert_pixel_row(
                record.source_format,
                record.destination_format,
                &record.source[..n * record.width()],
                record.palette.as_ref(),
                &mut out,
            );
            context.success &= result.is_ok() && out == record.expected[..n * 4];
        }
    }
}
extern "C" {
    fn frd_pixel_abi_probe(
        callback: extern "C" fn(*mut core::ffi::c_void),
        context: *mut core::ffi::c_void,
    ) -> u64;
}
#[test]
fn selected_row_calls_preserve_aarch64_callee_saved_gpr_and_simd_registers() {
    let mut context = Context {
        kernels: NativeKernels::detect().unwrap(),
        records: records(),
        success: false,
    };
    assert_eq!(
        unsafe { frd_pixel_abi_probe(invoke, (&mut context as *mut Context).cast()) },
        0
    );
    assert!(context.success);
}
core::arch::global_asm!(
    r#"
.text
.p2align 2
.globl _frd_pixel_abi_probe
_frd_pixel_abi_probe:
sub sp, sp, #160
stp x29, x30, [sp]
mov x29, sp
stp x19, x20, [sp, #16]
stp x21, x22, [sp, #32]
stp x23, x24, [sp, #48]
stp x25, x26, [sp, #64]
stp x27, x28, [sp, #80]
stp d8, d9, [sp, #96]
stp d10, d11, [sp, #112]
stp d12, d13, [sp, #128]
stp d14, d15, [sp, #144]
mov x16, x0
mov x0, x1
mov x19, #19
mov x20, #20
mov x21, #21
mov x22, #22
mov x23, #23
mov x24, #24
mov x25, #25
mov x26, #26
mov x27, #27
mov x28, #28
fmov d8, x19
fmov d9, x20
fmov d10, x21
fmov d11, x22
fmov d12, x23
fmov d13, x24
fmov d14, x25
fmov d15, x26
blr x16
mov x0, #0
cmp x19, #19
cset x2, ne
orr x0, x0, x2
cmp x20, #20
cset x2, ne
orr x0, x0, x2
cmp x21, #21
cset x2, ne
orr x0, x0, x2
cmp x22, #22
cset x2, ne
orr x0, x0, x2
cmp x23, #23
cset x2, ne
orr x0, x0, x2
cmp x24, #24
cset x2, ne
orr x0, x0, x2
cmp x25, #25
cset x2, ne
orr x0, x0, x2
cmp x26, #26
cset x2, ne
orr x0, x0, x2
cmp x27, #27
cset x2, ne
orr x0, x0, x2
cmp x28, #28
cset x2, ne
orr x0, x0, x2
fmov x1, d8
cmp x1, #19
cset x2, ne
orr x0, x0, x2
fmov x1, d9
cmp x1, #20
cset x2, ne
orr x0, x0, x2
fmov x1, d10
cmp x1, #21
cset x2, ne
orr x0, x0, x2
fmov x1, d11
cmp x1, #22
cset x2, ne
orr x0, x0, x2
fmov x1, d12
cmp x1, #23
cset x2, ne
orr x0, x0, x2
fmov x1, d13
cmp x1, #24
cset x2, ne
orr x0, x0, x2
fmov x1, d14
cmp x1, #25
cset x2, ne
orr x0, x0, x2
fmov x1, d15
cmp x1, #26
cset x2, ne
orr x0, x0, x2
ldp d8, d9, [sp, #96]
ldp d10, d11, [sp, #112]
ldp d12, d13, [sp, #128]
ldp d14, d15, [sp, #144]
ldp x19, x20, [sp, #16]
ldp x21, x22, [sp, #32]
ldp x23, x24, [sp, #48]
ldp x25, x26, [sp, #64]
ldp x27, x28, [sp, #80]
ldp x29, x30, [sp]
add sp, sp, #160
ret
"#
);
