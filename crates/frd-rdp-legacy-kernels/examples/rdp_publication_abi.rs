//! 私有 address-taken 正常 return 状态门；不是 shipping fastcc/inline、异常展开或性能资格。
#[cfg(all(
    not(test),
    any(target_os = "linux", target_os = "windows"),
    any(target_arch = "x86", target_arch = "x86_64")
))]
#[path = "../../frd-protocol-rdp/src/pixel_convert/x86.rs"]
mod actual;
#[cfg(all(
    not(test),
    any(target_os = "linux", target_os = "windows"),
    target_arch = "x86_64"
))]
#[path = "support/rdp_publication_abi_x64.rs"]
mod helper;
#[cfg(all(
    not(test),
    any(target_os = "linux", target_os = "windows"),
    target_arch = "x86"
))]
#[path = "support/rdp_publication_abi_i686.rs"]
mod helper;

#[cfg(all(
    not(test),
    any(target_os = "linux", target_os = "windows"),
    any(target_arch = "x86", target_arch = "x86_64")
))]
mod native {
    use super::{actual, helper};
    pub type CopyFn = unsafe fn(&[u8], &mut [u8]);
    pub struct Control {
        pub name: &'static str,
        pub code: *const u8,
        pub mask: u32,
    }
    pub const SP_BIT: u32 = 1 << 18;
    pub const DF_BIT: u32 = 1 << 19;
    pub const MXCSR_BIT: u32 = 1 << 20;
    pub const X87_BIT: u32 = 1 << 21;

    // 使精确真实leaf的CopyFn地址逃逸；最终对象仍须另审，不能据此假定内部fastcc未变。
    #[no_mangle]
    #[inline(never)]
    pub fn frd_publication_abi_sse2_address() -> CopyFn {
        actual::rgba_to_bgrx_sse2
    }
    #[no_mangle]
    #[inline(never)]
    pub fn frd_publication_abi_ssse3_address() -> CopyFn {
        actual::rgba_to_bgrx
    }

    fn input(pixels: usize) -> Vec<u8> {
        (0..pixels)
            .flat_map(|p| {
                [
                    (p as u8).wrapping_mul(29).wrapping_add(1),
                    (p as u8).wrapping_mul(43).wrapping_add(7),
                    (p as u8).wrapping_mul(67).wrapping_add(13),
                    [0, 1, 127, 255][p % 4],
                ]
            })
            .collect()
    }
    fn expected(source: &[u8]) -> Vec<u8> {
        let mut result = Vec::with_capacity(source.len());
        for pixel in source.chunks_exact(4) {
            result.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
        }
        result
    }

    // TDD：先固定每个真实kernel与逐state坏callee的mask/pixels断言；本轮未授权运行。
    fn check(real: CopyFn, target: CopyFn, pixels: usize, wanted_mask: u32) {
        let payload = input(pixels);
        let want = expected(&payload);
        let mut source = vec![0x5a; 3 + payload.len() + 16];
        source[3..3 + payload.len()].copy_from_slice(&payload);
        let before = source.clone();
        let mut destination = vec![0xa7; 5 + payload.len() + 19];
        // SAFETY: 两个地址只来自上述真实leaf或helper固定闭集；ISA已真实准入，
        // 切片完整pixel且独立有效。helper正常return必须先恢复所有caller状态才回Rust。
        let mask = unsafe {
            helper::probe(
                target,
                real,
                &source[3..3 + payload.len()],
                &mut destination[5..5 + payload.len()],
            )
        };
        assert_ne!(mask, u32::MAX, "helper未写status");
        assert_eq!(mask, wanted_mask, "状态mask必须精确相等");
        assert_eq!(&destination[5..5 + payload.len()], want);
        assert!(destination[..5].iter().all(|&v| v == 0xa7));
        assert!(destination[5 + payload.len()..].iter().all(|&v| v == 0xa7));
        assert_eq!(source, before);
    }

    pub fn run() -> Result<(), String> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if !matches!(args.as_slice(), [mode] if mode == "abi") {
            return Err("只接受固定abi调用；不接受任意callee、路径、数量或超时".into());
        }
        if !actual::sse2_available() || !actual::ssse3_available() {
            eprintln!("UNAVAILABLE reason=actual-sse2-and-ssse3-required");
            std::process::exit(77);
        }
        let literal = [1, 2, 3, 0, 255, 0, 16, 1, 0, 254, 1, 127, 9, 8, 7, 255];
        assert_eq!(
            expected(&literal),
            [3, 2, 1, 255, 16, 0, 255, 255, 1, 254, 0, 255, 7, 8, 9, 255]
        );
        println!("ABI_PRECONDITION protection_configuration=external-admission-required final_copyfn_chain=external-review-required");
        let controls = helper::controls();
        let required_controls = if cfg!(target_arch = "x86") {
            12
        } else if cfg!(windows) {
            40
        } else {
            16
        };
        assert_eq!(controls.len(), required_controls);
        for (kernel, real) in [
            ("sse2", frd_publication_abi_sse2_address()),
            ("ssse3", frd_publication_abi_ssse3_address()),
        ] {
            for control in &controls {
                // SAFETY: 只取本helper固定.text标签的地址，不读marker数据；
                // 最终target caller/callee/marker链必须在root后继对象/同exe中审查。
                let broken: CopyFn = unsafe { core::mem::transmute(control.code) };
                check(real, broken, 7, control.mask);
                println!(
                    "ABI_CALIBRATION kernel={kernel} case={} exact_mask=0x{:08x}",
                    control.name, control.mask
                );
            }
            for pixels in [0, 1, 2, 3, 4, 7, 8, 33, 129] {
                check(real, real, pixels, 0);
            }
            println!("ABI_VALID kernel={kernel} cases=9 exact_mask=0");
        }
        println!("ABI_OK scope=private-address-taken-normal-return controls_per_kernel={required_controls} kernels=2 full_unwind=open shipping_fastcc_inline=open control_flow_protection=not-qualified protection_admission=external-required");
        Ok(())
    }
}

#[cfg(all(
    not(test),
    any(target_os = "linux", target_os = "windows"),
    any(target_arch = "x86", target_arch = "x86_64")
))]
fn main() {
    if let Err(error) = native::run() {
        eprintln!("FAIL {error}");
        std::process::exit(2);
    }
}
#[cfg(not(all(
    not(test),
    any(target_os = "linux", target_os = "windows"),
    any(target_arch = "x86", target_arch = "x86_64")
)))]
fn main() {
    eprintln!("UNAVAILABLE requires=non-test-linux-or-windows-x86-or-x64");
    std::process::exit(77);
}
#[cfg(test)]
#[test]
#[ignore = "UNAVAILABLE: 必须运行普通x86/x64 binary，不能以test harness授ABI资格"]
fn private_abi_is_unavailable_in_test_harness() {
    panic!("UNAVAILABLE");
}
