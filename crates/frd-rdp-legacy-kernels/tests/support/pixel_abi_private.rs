//! 仅macOS ARM64 test；typed Rust fnpointer assembly body 在真实 target 返回后立即检查，不经Rust bridge。
use super::arch;
use crate::{Backend, NativeKernels, PackedDepth, Pixel32Format, PixelRowSource};
use core::arch::global_asm;
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

type BlockFn =
    unsafe fn(PixelRowSource, Pixel32Format, *const u8, Option<&[[u8; 3]; 256]>) -> arch::V;
type CopyFn = unsafe fn(&[u8], &mut [u8]);
static BLOCK_TARGET: AtomicUsize = AtomicUsize::new(0);
static BLOCK_STATUS: AtomicU64 = AtomicU64::new(0);
static COPY_TARGET: AtomicUsize = AtomicUsize::new(0);
static COPY_STATUS: AtomicU64 = AtomicU64::new(0);
// 只有下面一个串行test访问target/status；两个签名的存储分开，不存在并行换表。
// target只保存精确BlockFn/CopyFn已验证地址。asm对status执行自然对齐64bit store，
// 同thread随后原子load；无并发普通/原子访问，无alias/race。没有Rust call夹在检查前。

macro_rules! direct_probe_body {
    ($entry:ident,$target:ident,$status:ident)=>{
        global_asm!(
            ".text", ".p2align 2", ".globl {entry}", "{entry}:",
            "sub sp, sp, #160",
            "stp x29, x30, [sp]",
            "mov x29, sp",
            "stp x19, x20, [sp, #16]",
            "stp x21, x22, [sp, #32]",
            "stp x23, x24, [sp, #48]",
            "stp x25, x26, [sp, #64]",
            "stp x27, x28, [sp, #80]",
            "stp d8, d9, [sp, #96]",
            "stp d10, d11, [sp, #112]",
            "stp d12, d13, [sp, #128]",
            "stp d14, d15, [sp, #144]",
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
            "ldp d8, d9, [sp, #96]",
            "ldp d10, d11, [sp, #112]",
            "ldp d12, d13, [sp, #128]",
            "ldp d14, d15, [sp, #144]",
            "ldp x19, x20, [sp, #16]",
            "ldp x21, x22, [sp, #32]",
            "ldp x23, x24, [sp, #48]",
            "ldp x25, x26, [sp, #64]",
            "ldp x27, x28, [sp, #80]",
            "ldp x29, x30, [sp]",
            "add sp, sp, #160",
            "ret",
            entry=sym $entry,target=sym $target,status=sym $status,
        );
    }
}
// Marker只取code地址，不读u8数据，也不构造Rust reference；global_asm在.text实现入口。
// Rust1.96既禁止Rust ABI #[naked]，也对extern返回SIMD启用simd_ffi gate。
// typed Rust fnpointer调用保留实际BlockFn/CopyFn lowering；无C ABI aggregate假设。
extern "Rust" {
    #[link_name = "frd_pixel_abi_direct_block"]
    static PROBE_BLOCK_ENTRY: u8;
    #[link_name = "frd_pixel_abi_direct_copy"]
    static PROBE_COPY_ENTRY: u8;
    #[link_name = "frd_pixel_abi_broken_x25_block"]
    static BROKEN_X25_BLOCK_ENTRY: u8;
    #[link_name = "frd_pixel_abi_broken_frame_block"]
    static BROKEN_FRAME_BLOCK_ENTRY: u8;
    #[link_name = "frd_pixel_abi_broken_x25_copy"]
    static BROKEN_X25_COPY_ENTRY: u8;
}
direct_probe_body!(PROBE_BLOCK_ENTRY, BLOCK_TARGET, BLOCK_STATUS);
direct_probe_body!(PROBE_COPY_ENTRY, COPY_TARGET, COPY_STATUS);
macro_rules! broken_callee_body {
    ($entry:ident,$actual:path,$break_register:literal)=>{
        global_asm!(".text", ".p2align 2", ".globl {entry}", "{entry}:",
            "stp x29, x30, [sp, #-16]!","mov x29, sp","bl {actual}",
            "ldp x29, x30, [sp], #16",$break_register,"ret",
            entry=sym $entry,actual=sym $actual);
    }
}
broken_callee_body!(BROKEN_X25_BLOCK_ENTRY, arch::block, "mov x25, #0");
broken_callee_body!(BROKEN_FRAME_BLOCK_ENTRY, arch::block, "mov x29, #0");
broken_callee_body!(
    BROKEN_X25_COPY_ENTRY,
    crate::native::bitmap::copy,
    "mov x25, #0"
);
unsafe fn block_entry(code: *const u8) -> BlockFn {
    // SAFETY: 本macARM test所有传入地址都指向上面4byte对齐.text入口；
    // 其assembly遵循精确Rust BlockFn ABI，经执行前caller/callee反汇编核验。
    // rawpointer和functionpointer在该目标同为64bit；不按marker数据读取/引用。
    core::mem::transmute::<*const u8, BlockFn>(code)
}
unsafe fn copy_entry(code: *const u8) -> CopyFn {
    // SAFETY: 同上，但只接受实现精确Rust CopyFn ABI的两个copy代码入口。
    core::mem::transmute::<*const u8, CopyFn>(code)
}
const FORMATS: [Pixel32Format; 8] = [
    Pixel32Format::ARgb,
    Pixel32Format::XRgb,
    Pixel32Format::ABgr,
    Pixel32Format::XBgr,
    Pixel32Format::BgrA,
    Pixel32Format::BgrX,
    Pixel32Format::RgbA,
    Pixel32Format::RgbX,
];
struct Record {
    source_format: PixelRowSource,
    destination_format: Pixel32Format,
    source: Vec<u8>,
    palette: Option<[[u8; 3]; 256]>,
    expected: Vec<u8>,
}
fn records() -> Vec<Record> {
    let mut raw = Vec::new();
    GzDecoder::new(&include_bytes!("../pixel_original/original-pixel.bin.gz")[..])
        .read_to_end(&mut raw)
        .unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&raw)),
        "ee69627eb9513f5e71a7b310dd60b41ececf12ef72aa2be45562fc7cd0910f26"
    );
    assert_eq!(&raw[..8], b"FRDPIX01");
    let mut p = 8;
    let mut records = Vec::new();
    while p < raw.len() {
        let kind = raw[p];
        let sf = raw[p + 1] as usize;
        let df = raw[p + 2] as usize;
        p += 4;
        let sl = u32::from_le_bytes(raw[p..p + 4].try_into().unwrap()) as usize;
        let dl = u32::from_le_bytes(raw[p + 4..p + 8].try_into().unwrap()) as usize;
        p += 8;
        let palette = if kind == 0 {
            let mut pal = [[0; 3]; 256];
            for pixel in &mut pal {
                pixel.copy_from_slice(&raw[p..p + 3]);
                p += 3;
            }
            Some(pal)
        } else {
            None
        };
        let source_format = match kind {
            0 => PixelRowSource::Packed(PackedDepth::Index8),
            1 => PixelRowSource::Packed(PackedDepth::Rgb555),
            2 => PixelRowSource::Packed(PackedDepth::Rgb565),
            3 => PixelRowSource::Packed(PackedDepth::Bgr24),
            4 => PixelRowSource::Rgb24,
            5 => PixelRowSource::Raw32(FORMATS[sf]),
            6 => PixelRowSource::Region32(FORMATS[sf]),
            _ => panic!("fixture kind"),
        };
        let source = raw[p..p + sl].to_vec();
        p += sl;
        let expected = raw[p..p + dl].to_vec();
        p += dl;
        records.push(Record {
            source_format,
            destination_format: FORMATS[df],
            source,
            palette,
            expected,
        });
    }
    assert_eq!(records.len(), 176);
    records
}
fn set_block(target: BlockFn) {
    BLOCK_TARGET.store(target as usize, Ordering::SeqCst);
    BLOCK_STATUS.store(u64::MAX, Ordering::SeqCst);
}
fn set_copy(target: CopyFn) {
    COPY_TARGET.store(target as usize, Ordering::SeqCst);
    COPY_STATUS.store(u64::MAX, Ordering::SeqCst);
}
// noinline调用点供debug/release ABI反汇编：编译器按精确Rust类型摆放args/接收V。
#[inline(never)]
unsafe fn call_block(
    format: PixelRowSource,
    dst: Pixel32Format,
    source: *const u8,
    palette: Option<&[[u8; 3]; 256]>,
) -> arch::V {
    block_entry(core::ptr::addr_of!(PROBE_BLOCK_ENTRY))(format, dst, source, palette)
}
#[inline(never)]
unsafe fn call_copy(source: &[u8], destination: &mut [u8]) {
    copy_entry(core::ptr::addr_of!(PROBE_COPY_ENTRY))(source, destination)
}
#[test]
fn direct_selected_leaf_abi_detects_bad_callees_and_preserves_all_frozen_rows() {
    let kernels = NativeKernels::detect().unwrap();
    assert_eq!(kernels.backend(), Backend::Neon);
    assert_eq!(kernels.pixel_row as usize, super::row as *const () as usize);
    assert_eq!(
        kernels.copy as usize,
        crate::native::bitmap::copy as *const () as usize
    );
    let records = records();
    let first = &records[0];
    let mut vector_bytes = [0; 16];
    // 真实stockpalette core仍完成相同像素，单独破坏x25必须马上检测bit6。
    set_block(unsafe { block_entry(core::ptr::addr_of!(BROKEN_X25_BLOCK_ENTRY)) });
    let v = unsafe {
        call_block(
            first.source_format,
            first.destination_format,
            first.source.as_ptr(),
            first.palette.as_ref(),
        )
    };
    unsafe { arch::store(v, vector_bytes.as_mut_ptr()) };
    assert_eq!(vector_bytes, &first.expected[..16]);
    assert_eq!(BLOCK_STATUS.load(Ordering::SeqCst), 1 << 6);
    // FP也有实际负控制，不忽略x29；probe只在捕获失败后才恢复原caller frame。
    set_block(unsafe { block_entry(core::ptr::addr_of!(BROKEN_FRAME_BLOCK_ENTRY)) });
    let v = unsafe {
        call_block(
            first.source_format,
            first.destination_format,
            first.source.as_ptr(),
            first.palette.as_ref(),
        )
    };
    unsafe { arch::store(v, vector_bytes.as_mut_ptr()) };
    assert_eq!(vector_bytes, &first.expected[..16]);
    assert_eq!(BLOCK_STATUS.load(Ordering::SeqCst), 1 << 18);
    let copy_input = [0x39; 16];
    let mut copy_output = [0; 16];
    set_copy(unsafe { copy_entry(core::ptr::addr_of!(BROKEN_X25_COPY_ENTRY)) });
    unsafe { call_copy(&copy_input, &mut copy_output) };
    assert_eq!(copy_output, copy_input);
    assert_eq!(COPY_STATUS.load(Ordering::SeqCst), 1 << 6);
    let (mut core_calls, mut copy_calls, mut public_rows) = (0, 0, 0);
    for record in records {
        for n in [1, 3, 4, 7, 8, 65] {
            let width = record.source_format.bytes();
            let input = &record.source[..n * width];
            let expected = &record.expected[..n * 4];
            let mut output = vec![0; n * 4];
            kernels
                .convert_pixel_row(
                    record.source_format,
                    record.destination_format,
                    input,
                    record.palette.as_ref(),
                    &mut output,
                )
                .unwrap();
            assert_eq!(output, expected);
            public_rows += 1;
            let raw_copy = match record.source_format {
                PixelRowSource::Raw32(src) => src == record.destination_format,
                PixelRowSource::Region32(src) => {
                    src.offsets() == record.destination_format.offsets()
                }
                _ => false,
            };
            if raw_copy {
                set_copy(kernels.copy);
                unsafe { call_copy(input, &mut output) };
                assert_eq!(
                    COPY_STATUS.load(Ordering::SeqCst),
                    0,
                    "copy {:?}->{:?} n{n}",
                    record.source_format,
                    record.destination_format
                );
                assert_eq!(output, expected);
                copy_calls += 1;
            }
            // 每个record同时直接包围真实ISA core，完整块直借input，只有短尾使用16byte staging。
            for start in (0..n).step_by(4) {
                let count = (n - start).min(4);
                let mut staging = [0; 16];
                let source = if count == 4 {
                    unsafe { input.as_ptr().add(start * width) }
                } else {
                    staging[..count * width].copy_from_slice(&input[start * width..]);
                    staging.as_ptr()
                };
                set_block(arch::block);
                let value = unsafe {
                    call_block(
                        record.source_format,
                        record.destination_format,
                        source,
                        record.palette.as_ref(),
                    )
                };
                assert_eq!(
                    BLOCK_STATUS.load(Ordering::SeqCst),
                    0,
                    "core {:?}->{:?} n{n} block{start}",
                    record.source_format,
                    record.destination_format
                );
                unsafe { arch::store(value, vector_bytes.as_mut_ptr()) };
                assert_eq!(
                    &vector_bytes[..count * 4],
                    &expected[start * 4..(start + count) * 4]
                );
                core_calls += 1;
            }
        }
    }
    assert_eq!((public_rows, core_calls, copy_calls), (1056, 4224, 144));
    eprintln!("direct ABI: 176 records, 1056 public rows, 4224 core calls, 144 selected copy calls; x25 core/copy negative mask64, x29 negative mask262144");
}
