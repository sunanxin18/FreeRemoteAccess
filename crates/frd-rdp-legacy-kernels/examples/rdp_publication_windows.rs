//! 私有 Windows publication leaf 正确性/保护页入口；不是产品分派、ABI 或性能资格。
//! 普通 cargo build --example 编译真实源，cfg(test) 的 ISA 否决和 TLS 计数不参与。

#[cfg(all(
    not(test),
    target_os = "windows",
    any(target_arch = "x86", target_arch = "x86_64")
))]
#[path = "../../frd-protocol-rdp/src/pixel_convert/x86.rs"]
mod actual;

#[cfg(all(
    not(test),
    target_os = "windows",
    any(target_arch = "x86", target_arch = "x86_64")
))]
#[path = "support/rdp_publication_windows_guard.rs"]
mod guard;

#[cfg(all(
    not(test),
    target_os = "windows",
    any(target_arch = "x86", target_arch = "x86_64")
))]
mod native {
    use super::{actual, guard};

    pub const COUNTS: [usize; 40] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31, 32, 33, 63, 64, 65, 127, 128, 129,
    ];

    #[derive(Clone, Copy)]
    pub enum Kernel {
        Sse2,
        Ssse3,
    }

    impl Kernel {
        pub fn name(self) -> &'static str {
            match self {
                Self::Sse2 => "sse2",
                Self::Ssse3 => "ssse3",
            }
        }

        pub fn convert(self, source: &[u8], destination: &mut [u8]) {
            assert_eq!(source.len() % 4, 0);
            assert!(destination.len() >= source.len());
            let destination = &mut destination[..source.len()];
            match self {
                Self::Sse2 => {
                    assert!(actual::sse2_available(), "需要真实 SSE2 CPU");
                    // SAFETY: 实际 ISA、完整 pixel、目标长度先检查；借用互不重叠。
                    unsafe { actual::rgba_to_bgrx_sse2(source, destination) };
                }
                Self::Ssse3 => {
                    assert!(actual::ssse3_available(), "需要真实 SSSE3 CPU");
                    // SAFETY: 同上，真实 SSSE3 检查与原叶子要求一致。
                    unsafe { actual::rgba_to_bgrx(source, destination) };
                }
            }
        }
    }

    pub fn kernels() -> [Kernel; 2] {
        // run() 对两个实际 ISA 统一准入；不把单核 partial 记成双核 GREEN。
        [Kernel::Sse2, Kernel::Ssse3]
    }

    pub fn input(pixels: usize) -> Vec<u8> {
        let mut source = Vec::with_capacity(pixels * 4);
        for pixel in 0..pixels {
            source.extend_from_slice(&[
                (pixel as u8).wrapping_mul(29).wrapping_add(1),
                (pixel as u8).wrapping_mul(43).wrapping_add(7),
                (pixel as u8).wrapping_mul(67).wrapping_add(13),
                [0, 1, 127, 255][pixel % 4],
            ]);
        }
        source
    }

    // 独立逐 pixel 字节 oracle，不调用生产 scalar、SIMD 或输入生成器。
    pub fn expected(source: &[u8]) -> Vec<u8> {
        assert_eq!(source.len() % 4, 0);
        let mut output = Vec::with_capacity(source.len());
        for pixel in 0..source.len() / 4 {
            output.push(source[pixel * 4 + 2]);
            output.push(source[pixel * 4 + 1]);
            output.push(source[pixel * 4]);
            output.push(255);
        }
        output
    }

    pub fn literal(kernel: Kernel) {
        let source = [1, 2, 3, 0, 255, 0, 16, 1, 0, 254, 1, 127, 9, 8, 7, 255];
        let want = [3, 2, 1, 255, 16, 0, 255, 255, 1, 254, 0, 255, 7, 8, 9, 255];
        assert_eq!(expected(&source), want);
        let mut destination = [0xa7; 21];
        kernel.convert(&source, &mut destination);
        assert_eq!(&destination[..16], want);
        assert_eq!(&destination[16..], [0xa7; 5]);
    }

    fn parity() {
        for kernel in kernels() {
            literal(kernel);
            let mut cases = 0;
            for pixels in COUNTS {
                let source = input(pixels);
                let want = expected(&source);
                for source_offset in 0..16 {
                    for destination_offset in 0..16 {
                        let mut storage = vec![0x5a; source_offset + source.len() + 16];
                        storage[source_offset..source_offset + source.len()]
                            .copy_from_slice(&source);
                        let before = storage.clone();
                        let mut destination = vec![0xa7; destination_offset + source.len() + 19];
                        kernel.convert(
                            &storage[source_offset..source_offset + source.len()],
                            &mut destination[destination_offset..],
                        );
                        assert_eq!(
                            &destination[destination_offset..destination_offset + source.len()],
                            want,
                            "kernel={} pixels={pixels} source_offset={source_offset} destination_offset={destination_offset}",
                            kernel.name()
                        );
                        assert!(destination[..destination_offset].iter().all(|&v| v == 0xa7));
                        assert!(destination[destination_offset + source.len()..]
                            .iter()
                            .all(|&v| v == 0xa7));
                        assert_eq!(storage, before);
                        cases += 1;
                    }
                }
            }
            assert_eq!(cases, 10_240);
            println!(
                "PARITY_OK kernel={} cases={cases} counts=40 input_offsets=16 output_offsets=16",
                kernel.name()
            );
        }
    }

    pub fn run() -> Result<(), String> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if !matches!(args.as_slice(), [mode] if mode == "parity" || mode == "guards")
            && !matches!(args.as_slice(), [mode, case] if mode == "guard-child" && guard::CASES.contains(&case.as_str()))
        {
            return Err("只接受 parity、guards 或固定 guard-child case；无可调数量或路径".into());
        }
        if !actual::sse2_available() || !actual::ssse3_available() {
            eprintln!(
                "UNAVAILABLE arch={} os=windows reason=actual-sse2-and-ssse3-required",
                std::env::consts::ARCH
            );
            std::process::exit(77);
        }
        guard::suppress_error_dialogs();
        match args[0].as_str() {
            "parity" => parity(),
            "guards" => guard::parent()?,
            "guard-child" => guard::child(&args[1])?,
            _ => unreachable!(),
        }
        Ok(())
    }
}

#[cfg(all(
    not(test),
    target_os = "windows",
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
    target_os = "windows",
    any(target_arch = "x86", target_arch = "x86_64")
)))]
fn main() {
    eprintln!(
        "UNAVAILABLE arch={} os={} requires=non-test-windows-x86-or-x86_64",
        std::env::consts::ARCH,
        std::env::consts::OS
    );
    std::process::exit(77);
}

// Cargo 的 test harness 不执行 main；显式标出该模式不授本门资格。
#[cfg(test)]
#[test]
#[ignore = "UNAVAILABLE: 只能运行非 test 的 Windows x64/i686 example binary"]
fn native_publication_qualification_is_unavailable_in_test_harness() {
    panic!("UNAVAILABLE: 需要普通 binary 的 parity 与 guards 调用");
}
