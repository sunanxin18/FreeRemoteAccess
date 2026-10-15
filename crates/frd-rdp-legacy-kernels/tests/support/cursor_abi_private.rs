//! macOS ARM64 精确 Rust fnpointer：真实 target 返回后立即检查，尚未编译/运行。
//! 执行前必须反汇编核对各签名的寄存器和 stack args，其他目标 ABI 仍 OPEN。
use super::arch;
use crate::{CursorBitmapMode as M, CursorXorFormat as F, NativeKernels};
use core::arch::global_asm;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
type Decode4 = unsafe fn(F, M, *const u8, u8, bool) -> arch::V;
type Composite4 = unsafe fn(*const u8, *const u8) -> arch::V;
type DecodeRow = crate::CursorDecodeKernel;
type CompositeRow = crate::CursorCompositeKernel;
static D4_TARGET: AtomicUsize = AtomicUsize::new(0);
static C4_TARGET: AtomicUsize = AtomicUsize::new(0);
static DR_TARGET: AtomicUsize = AtomicUsize::new(0);
static CR_TARGET: AtomicUsize = AtomicUsize::new(0);
static D4_STATUS: AtomicU64 = AtomicU64::new(0);
static C4_STATUS: AtomicU64 = AtomicU64::new(0);
static DR_STATUS: AtomicU64 = AtomicU64::new(0);
static CR_STATUS: AtomicU64 = AtomicU64::new(0);
macro_rules! cursor_probe_body {
    ($entry:ident,$target:ident,$status:ident)=>{
        global_asm!(
            ".text", ".p2align 2", ".globl {entry}", "{entry}:",
            "sub sp, sp, #192",
            // 将 incoming stack words 原样转发；执行前核对实际 Rust ABI lowering。
            "ldp x9, x10, [sp, #192]", "stp x9, x10, [sp]",
            "ldp x9, x10, [sp, #208]", "stp x9, x10, [sp, #16]",
            "stp x29, x30, [sp, #32]",
            "mov x29, sp",
            "stp x19, x20, [sp, #48]",
            "stp x21, x22, [sp, #64]",
            "stp x23, x24, [sp, #80]",
            "stp x25, x26, [sp, #96]",
            "stp x27, x28, [sp, #112]",
            "stp d8, d9, [sp, #128]",
            "stp d10, d11, [sp, #144]",
            "stp d12, d13, [sp, #160]",
            "stp d14, d15, [sp, #176]",
            "mov x19, #19",
            "mov x20, #20",
            "mov x21, #21",
            "mov x22, #22",
            "mov x23, #23",
            "mov x24, #24",
            "mov x25, #25",
            "mov x26, #26",
            "mov x27, #27",
            "mov x28, #28",
            "fmov d8, x19",
            "fmov d9, x20",
            "fmov d10, x21",
            "fmov d11, x22",
            "fmov d12, x23",
            "fmov d13, x24",
            "fmov d14, x25",
            "fmov d15, x26",
            "adrp x16, {target}@PAGE",
            "ldr x16, [x16, {target}@PAGEOFF]",
            "blr x16",
            "mov x11, #0",
            "cmp x19, #19",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #0",
            "cmp x20, #20",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #1",
            "cmp x21, #21",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #2",
            "cmp x22, #22",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #3",
            "cmp x23, #23",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #4",
            "cmp x24, #24",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #5",
            "cmp x25, #25",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #6",
            "cmp x26, #26",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #7",
            "cmp x27, #27",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #8",
            "cmp x28, #28",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #9",
            "fmov x9, d8",
            "cmp x9, #19",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #10",
            "fmov x9, d9",
            "cmp x9, #20",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #11",
            "fmov x9, d10",
            "cmp x9, #21",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #12",
            "fmov x9, d11",
            "cmp x9, #22",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #13",
            "fmov x9, d12",
            "cmp x9, #23",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #14",
            "fmov x9, d13",
            "cmp x9, #24",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #15",
            "fmov x9, d14",
            "cmp x9, #25",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #16",
            "fmov x9, d15",
            "cmp x9, #26",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #17",
            "mov x9, sp",
            "cmp x29, x9",
            "cset x12, ne",
            "orr x11, x11, x12, lsl #18",
            "adrp x16, {status}@PAGE",
            "str x11, [x16, {status}@PAGEOFF]",
            "ldp d8, d9, [sp, #128]",
            "ldp d10, d11, [sp, #144]",
            "ldp d12, d13, [sp, #160]",
            "ldp d14, d15, [sp, #176]",
            "ldp x19, x20, [sp, #48]",
            "ldp x21, x22, [sp, #64]",
            "ldp x23, x24, [sp, #80]",
            "ldp x25, x26, [sp, #96]",
            "ldp x27, x28, [sp, #112]",
            "ldp x29, x30, [sp, #32]",
            "add sp, sp, #192",
            "ret",
            entry=sym $entry,target=sym $target,status=sym $status,
        );
    }
}
extern "Rust" {
    #[link_name = "frd_cursor_probe_decode4"]
    static D4_ENTRY: u8;
    #[link_name = "frd_cursor_probe_composite4"]
    static C4_ENTRY: u8;
    #[link_name = "frd_cursor_probe_decode_row"]
    static DR_ENTRY: u8;
    #[link_name = "frd_cursor_probe_composite_row"]
    static CR_ENTRY: u8;
    #[link_name = "frd_cursor_broken_x25_decode4"]
    static BAD_D4: u8;
    #[link_name = "frd_cursor_broken_x29_composite_row"]
    static BAD_CR: u8;
}
cursor_probe_body!(D4_ENTRY, D4_TARGET, D4_STATUS);
cursor_probe_body!(C4_ENTRY, C4_TARGET, C4_STATUS);
cursor_probe_body!(DR_ENTRY, DR_TARGET, DR_STATUS);
cursor_probe_body!(CR_ENTRY, CR_TARGET, CR_STATUS);
global_asm!(".text",".p2align 2",".globl {entry}","{entry}:",
    "stp x29, x30, [sp, #-16]!","bl {actual}",
    "ldp x29, x30, [sp], #16","mov x25, #0","ret",
    entry=sym BAD_D4, actual=sym arch::decode4);
global_asm!(".text",".p2align 2",".globl {entry}","{entry}:",
    "stp x29, x30, [sp, #-16]!","bl {actual}",
    "ldp x29, x30, [sp], #16","mov x29, #0","ret",
    entry=sym BAD_CR, actual=sym super::composite);
#[inline(never)]
unsafe fn call_d4(f: F, m: M, s: *const u8, a: u8, low: bool) -> arch::V {
    let entry: Decode4 = core::mem::transmute(core::ptr::addr_of!(D4_ENTRY));
    entry(f, m, s, a, low)
}
#[inline(never)]
unsafe fn call_c4(s: *const u8, b: *const u8) -> arch::V {
    let entry: Composite4 = core::mem::transmute(core::ptr::addr_of!(C4_ENTRY));
    entry(s, b)
}
#[inline(never)]
unsafe fn call_dr(f: F, m: M, s: &[u8], a: Option<&[u8]>, o: &mut [u8]) {
    let entry: DecodeRow = core::mem::transmute(core::ptr::addr_of!(DR_ENTRY));
    entry(f, m, s, a, o)
}
#[inline(never)]
unsafe fn call_cr(s: &[u8], b: &[u8], o: &mut [u8]) {
    let entry: CompositeRow = core::mem::transmute(core::ptr::addr_of!(CR_ENTRY));
    entry(s, b, o)
}
#[test]
#[ignore = "执行前须核对实际 Rust ABI 和调用者栈布局"]
fn direct_actual_cursor_core_table_and_negative_callees() {
    let k = NativeKernels::detect().unwrap();
    assert_eq!(
        k.cursor_decode as usize,
        super::decode as *const () as usize
    );
    assert_eq!(
        k.cursor_composite as usize,
        super::composite as *const () as usize
    );
    let wire = [
        0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 1, 96, 80, 64, 128,
    ];
    let expect = [
        0, 0, 0, 0, 255, 255, 255, 0, 254, 254, 254, 1, 16, 25, 36, 128,
    ];
    let bg = [
        1, 127, 255, 31, 0, 1, 254, 32, 255, 255, 255, 33, 70, 90, 110, 34,
    ];
    let blend = [
        1, 127, 255, 31, 255, 254, 1, 255, 251, 251, 251, 33, 50, 69, 90, 34,
    ];
    let mut out = [0; 16];
    D4_TARGET.store(core::ptr::addr_of!(BAD_D4) as usize, Ordering::SeqCst);
    unsafe {
        arch::store(
            call_d4(F::Bgra32, M::Software, wire.as_ptr(), 0xe0, false),
            out.as_mut_ptr(),
        )
    };
    assert_eq!(out, expect);
    assert_eq!(D4_STATUS.load(Ordering::SeqCst), 1 << 6);
    CR_TARGET.store(core::ptr::addr_of!(BAD_CR) as usize, Ordering::SeqCst);
    unsafe { call_cr(&expect, &bg, &mut out) };
    assert_eq!(out, blend);
    assert_eq!(CR_STATUS.load(Ordering::SeqCst), 1 << 18);
    D4_TARGET.store(arch::decode4 as *const () as usize, Ordering::SeqCst);
    C4_TARGET.store(arch::composite4 as *const () as usize, Ordering::SeqCst);
    DR_TARGET.store(k.cursor_decode as usize, Ordering::SeqCst);
    CR_TARGET.store(k.cursor_composite as usize, Ordering::SeqCst);
    for n in [1usize, 2, 3, 4, 7, 8, 9, 15, 16, 17, 31, 32, 33] {
        let src: Vec<u8> = (0..n)
            .flat_map(|i| wire[(i % 4) * 4..(i % 4 + 1) * 4].iter().copied())
            .collect();
        let mask = vec![0xee; n.div_ceil(8)];
        let expected: Vec<u8> = (0..n)
            .flat_map(|i| expect[(i % 4) * 4..(i % 4 + 1) * 4].iter().copied())
            .collect();
        let background: Vec<u8> = (0..n)
            .flat_map(|i| bg[(i % 4) * 4..(i % 4 + 1) * 4].iter().copied())
            .collect();
        let want: Vec<u8> = (0..n)
            .flat_map(|i| blend[(i % 4) * 4..(i % 4 + 1) * 4].iter().copied())
            .collect();
        let mut row = vec![0; n * 4];
        unsafe { call_dr(F::Bgra32, M::Software, &src, Some(&mask), &mut row) };
        assert_eq!(DR_STATUS.load(Ordering::SeqCst), 0);
        assert_eq!(row, expected);
        let mut result = vec![0; n * 4];
        unsafe { call_cr(&row, &background, &mut result) };
        assert_eq!(CR_STATUS.load(Ordering::SeqCst), 0);
        assert_eq!(result, want);
        for start in (0..n).step_by(4) {
            let count = (n - start).min(4);
            let mut xs = [0; 16];
            let mut bs = [0; 16];
            let mut rs = [0; 16];
            xs[..count * 4].copy_from_slice(&src[start * 4..(start + count) * 4]);
            bs[..count * 4].copy_from_slice(&background[start * 4..(start + count) * 4]);
            unsafe {
                arch::store(
                    call_d4(F::Bgra32, M::Software, xs.as_ptr(), 0xe0, false),
                    rs.as_mut_ptr(),
                )
            };
            assert_eq!(D4_STATUS.load(Ordering::SeqCst), 0);
            assert_eq!(&rs[..count * 4], &expected[start * 4..(start + count) * 4]);
            unsafe { arch::store(call_c4(rs.as_ptr(), bs.as_ptr()), out.as_mut_ptr()) };
            assert_eq!(C4_STATUS.load(Ordering::SeqCst), 0);
            assert_eq!(&out[..count * 4], &want[start * 4..(start + count) * 4]);
        }
    }
    // 四种 layout 与三个 mode 的直接 actual core/table probe；期待来自固定 literal tuples。
    for f in [F::Mono1, F::Rgb565, F::Bgr24, F::Bgra32] {
        let pattern: Vec<u8> = match f {
            F::Mono1 => vec![0xaa],
            F::Rgb565 => vec![0, 0, 255, 255, 0, 248, 224, 7],
            F::Bgr24 => vec![0, 0, 0, 255, 255, 255, 0, 0, 255, 0, 255, 0],
            F::Bgra32 => wire.to_vec(),
        };
        for mode in [
            M::Software,
            M::Accelerated { odd_row: false },
            M::Accelerated { odd_row: true },
        ] {
            let tuple: [[u8; 4]; 4] = match (f, mode) {
                (F::Mono1, M::Software) => [[255, 255, 255, 0], [0; 4], [255, 255, 255, 0], [0; 4]],
                (F::Mono1, M::Accelerated { odd_row: false }) => {
                    [[255; 4], [0; 4], [255; 4], [0; 4]]
                }
                (F::Mono1, M::Accelerated { odd_row: true }) => {
                    [[0, 0, 0, 255], [0; 4], [0, 0, 0, 255], [0; 4]]
                }
                (F::Rgb565 | F::Bgr24, M::Software) => [
                    [0; 4],
                    [255, 255, 255, 0],
                    [254, 0, 0, 255],
                    [0, 254, 0, 255],
                ],
                (F::Rgb565 | F::Bgr24, M::Accelerated { odd_row: false }) => {
                    [[0; 4], [0, 0, 0, 255], [255, 0, 0, 255], [0, 255, 0, 255]]
                }
                (F::Rgb565 | F::Bgr24, M::Accelerated { odd_row: true }) => {
                    [[0; 4], [255; 4], [255, 0, 0, 255], [0, 255, 0, 255]]
                }
                (F::Bgra32, M::Software) => [
                    [0; 4],
                    [255, 255, 255, 0],
                    [254, 254, 254, 1],
                    [16, 25, 36, 128],
                ],
                (F::Bgra32, M::Accelerated { odd_row: false }) => [
                    [0; 4],
                    [0, 0, 0, 255],
                    [255, 255, 255, 1],
                    [64, 80, 96, 128],
                ],
                (F::Bgra32, M::Accelerated { odd_row: true }) => {
                    [[0; 4], [255; 4], [255, 255, 255, 1], [64, 80, 96, 128]]
                }
            };
            for n in [1usize, 2, 3, 4, 7, 8, 9, 15, 16, 17, 31, 32, 33] {
                let size = match f {
                    F::Mono1 => n.div_ceil(8),
                    F::Rgb565 => n * 2,
                    F::Bgr24 => n * 3,
                    F::Bgra32 => n * 4,
                };
                let src: Vec<u8> = (0..size).map(|i| pattern[i % pattern.len()]).collect();
                let expected: Vec<u8> = (0..n).flat_map(|i| tuple[i % 4]).collect();
                let mask = vec![255; n.div_ceil(8)];
                let mut row = vec![0; n * 4];
                unsafe { call_dr(f, mode, &src, Some(&mask), &mut row) };
                assert_eq!(DR_STATUS.load(Ordering::SeqCst), 0);
                assert_eq!(row, expected);
                for start in (0..n).step_by(4) {
                    let count = (n - start).min(4);
                    let mut stage = [0; 16];
                    if f == F::Mono1 {
                        stage[0] = src[start / 8];
                    } else {
                        let width = match f {
                            F::Rgb565 => 2,
                            F::Bgr24 => 3,
                            F::Bgra32 => 4,
                            _ => unreachable!(),
                        };
                        stage[..count * width]
                            .copy_from_slice(&src[start * width..(start + count) * width]);
                    }
                    unsafe {
                        arch::store(
                            call_d4(f, mode, stage.as_ptr(), 255, start % 8 == 4),
                            out.as_mut_ptr(),
                        )
                    };
                    assert_eq!(D4_STATUS.load(Ordering::SeqCst), 0);
                    assert_eq!(&out[..count * 4], &expected[start * 4..(start + count) * 4]);
                }
            }
        }
    }
}
