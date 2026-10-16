//! 固定 publication 微基准；源码准备不等于实际运行、shipping 或远程延迟证明。
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
    any(target_arch = "x86", target_arch = "x86_64")
))]
#[path = "../../frd-protocol-rdp/src/pixel_convert.rs"]
mod pixel_convert;

#[cfg(all(
    not(test),
    any(target_os = "linux", target_os = "windows"),
    any(target_arch = "x86", target_arch = "x86_64")
))]
const BYTES_PER_PIXEL: usize = 4;

// 以下函数整段保持冻结旧 publication 的原文；启动时在计时外比较原文与源 SHA。
#[cfg(all(
    not(test),
    any(target_os = "linux", target_os = "windows"),
    any(target_arch = "x86", target_arch = "x86_64")
))]
#[inline(never)]
fn rgba_to_bgrx_scalar(source: &[u8], destination: &mut [u8]) {
    for (rgba, bgrx) in source
        .chunks_exact(BYTES_PER_PIXEL)
        .zip(destination.chunks_exact_mut(BYTES_PER_PIXEL))
    {
        bgrx.copy_from_slice(&[rgba[2], rgba[1], rgba[0], 0xff]);
    }
}

#[cfg(all(
    not(test),
    any(target_os = "linux", target_os = "windows"),
    any(target_arch = "x86", target_arch = "x86_64")
))]
mod native {
    use super::{actual, pixel_convert, rgba_to_bgrx_scalar};
    use flate2::read::GzDecoder;
    use sha2::{Digest, Sha256};
    use std::{
        hint::black_box,
        io::Read,
        time::{Duration, Instant},
    };

    const COUNTS: [usize; 15] = [
        0, 1, 2, 3, 4, 7, 33, 65, 1024, 1920, 1921, 3840, 4096, 2073600, 8294400,
    ];
    const ALIGNMENTS: [(usize, usize); 2] = [(0, 0), (1, 3)];
    const WARMUP: usize = 8;
    const SAMPLES: usize = 31;
    const BUDGET: Duration = Duration::from_secs(120);
    const ARCHIVE: &[u8] = include_bytes!("../tests/pixel_original/original-pixel.bin.gz");
    const OLD_SOURCE: &str =
        include_str!("../../frd-protocol-rdp/tests/fixtures/sse2-publication-red/pixel_convert.rs");
    const THIS_SOURCE: &str = include_str!("rdp_publication_benchmark.rs");
    const ARCHIVE_SHA: &str = "ebfe8d6db6195ae1f45213733c8789fb48425f57e1ae2d106a9decc103592196";
    const RAW_SHA: &str = "ee69627eb9513f5e71a7b310dd60b41ececf12ef72aa2be45562fc7cd0910f26";
    const OLD_SOURCE_SHA: &str = "514cacd540e94cdef57c15482022ea50b8efe894cc512d9b8ae4e1cbc987aa2b";
    const INPUT_SHA: &str = "19f8954d765f4687af7e34ce46875f1680e460c3244ec8e20614bfa84ac7ed08";
    const OUTPUT_SHA: &str = "944256d1f6be362de8dd56fe36eead743046f3bf0fcf46f32091efce5a0b92a8";

    #[derive(Clone, Copy)]
    enum Route {
        Reference,
        Sse2,
        Ssse3,
        CurrentDispatch,
    }
    const ROUTES: [Route; 4] = [
        Route::Reference,
        Route::Sse2,
        Route::Ssse3,
        Route::CurrentDispatch,
    ];
    impl Route {
        fn name(self) -> &'static str {
            match self {
                Self::Reference => "reference",
                Self::Sse2 => "direct-sse2",
                Self::Ssse3 => "direct-ssse3",
                Self::CurrentDispatch => "current-dispatch",
            }
        }
        fn available(self, ssse3: bool) -> bool {
            !matches!(self, Self::Ssse3) || ssse3
        }
    }

    fn require(value: bool, message: &str) -> Result<(), String> {
        if value {
            Ok(())
        } else {
            Err(message.into())
        }
    }
    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    fn budget(began: Instant) -> Result<(), String> {
        require(began.elapsed() < BUDGET, "120秒总预算耗尽；记录为partial")
    }

    fn scalar_text(source: &str) -> Result<&str, String> {
        let start = source
            .find("#[inline(never)]\nfn rgba_to_bgrx_scalar(")
            .ok_or("冻结scalar原文起点缺失")?;
        let end = source[start..]
            .find("\n}\n")
            .ok_or("冻结scalar原文终点缺失")?
            + start
            + 3;
        Ok(&source[start..end])
    }

    fn independent_output(source: &[u8]) -> Vec<u8> {
        let mut result = vec![0; source.len()];
        for pixel in 0..source.len() / 4 {
            let offset = pixel * 4;
            result[offset] = source[offset + 2];
            result[offset + 1] = source[offset + 1];
            result[offset + 2] = source[offset];
            result[offset + 3] = 255;
        }
        result
    }

    fn take<'a>(raw: &'a [u8], position: &mut usize, count: usize) -> Result<&'a [u8], String> {
        let end = position.checked_add(count).ok_or("oracle长度溢出")?;
        let bytes = raw.get(*position..end).ok_or("oracle截断")?;
        *position = end;
        Ok(bytes)
    }

    fn oracle() -> Result<(Vec<u8>, Vec<u8>), String> {
        require(digest(ARCHIVE) == ARCHIVE_SHA, "冻结gzip SHA不符")?;
        require(
            digest(OLD_SOURCE.as_bytes()) == OLD_SOURCE_SHA,
            "冻结旧源SHA不符",
        )?;
        require(
            scalar_text(OLD_SOURCE)? == scalar_text(THIS_SOURCE)?,
            "scalar原文已变化",
        )?;
        // 固定gzip仅作数据；先有界解压，再核完整原SHA，不运行generator或原程序。
        let mut raw = Vec::new();
        GzDecoder::new(ARCHIVE)
            .take(6_453_433)
            .read_to_end(&mut raw)
            .map_err(|error| format!("冻结gzip读取失败：{error}"))?;
        require(
            raw.len() == 6_453_432 && digest(&raw) == RAW_SHA,
            "完整解压SHA或长度不符",
        )?;
        let mut position = 0;
        require(
            take(&raw, &mut position, 8)? == b"FRDPIX01",
            "oracle magic不符",
        )?;
        let mut record = 0;
        let mut selected = None;
        while position < raw.len() {
            let header = take(&raw, &mut position, 4)?;
            let lengths = take(&raw, &mut position, 8)?;
            let source_len =
                u32::from_le_bytes(lengths[..4].try_into().map_err(|_| "oracle source长度")?)
                    as usize;
            let output_len =
                u32::from_le_bytes(lengths[4..].try_into().map_err(|_| "oracle output长度")?)
                    as usize;
            if header[0] == 0 {
                take(&raw, &mut position, 256 * 3)?;
            }
            let source = take(&raw, &mut position, source_len)?;
            let output = take(&raw, &mut position, output_len)?;
            if record == 165 {
                require(
                    header[..3] == [6, 6, 5],
                    "仅接受record165 Region32/RgbA/BgrX",
                )?;
                require(
                    source.len() == 384 && output.len() == 384,
                    "record165必须96pixel",
                )?;
                require(
                    digest(source) == INPUT_SHA && digest(output) == OUTPUT_SHA,
                    "record165双SHA不符",
                )?;
                require(
                    independent_output(source) == output,
                    "record165与独立逐index oracle不符",
                )?;
                selected = Some((source.to_vec(), output.to_vec()));
            }
            record += 1;
        }
        require(
            record == 176 && position == raw.len(),
            "oracle记录总数或终点不符",
        )?;
        let literal = [1, 2, 3, 0, 255, 0, 16, 1, 0, 254, 1, 127, 9, 8, 7, 255];
        require(
            independent_output(&literal)
                == [3, 2, 1, 255, 16, 0, 255, 255, 1, 254, 0, 255, 7, 8, 9, 255],
            "独立RGB/opaque-X锚点不符",
        )?;
        selected.ok_or("record165缺失".into())
    }

    struct Case {
        source: Vec<u8>,
        source_before: Vec<u8>,
        destination: Vec<u8>,
        expected: Vec<u8>,
        source_start: usize,
        destination_start: usize,
        bytes: usize,
    }
    impl Case {
        fn new(
            pixels: usize,
            alignment: (usize, usize),
            input: &[u8],
            output: &[u8],
        ) -> Result<Self, String> {
            let bytes = pixels.checked_mul(4).ok_or("pixel长度溢出")?;
            // 两端各16byte canary，加31byte对齐余量；不用Vec offset0对齐假设。
            let mut source = vec![0x5a; bytes + 63];
            let destination = vec![0xa7; bytes + 63];
            let source_start =
                16 + (alignment.0 + 16 - ((source.as_ptr() as usize + 16) & 15)) % 16;
            let destination_start =
                16 + (alignment.1 + 16 - ((destination.as_ptr() as usize + 16) & 15)) % 16;
            let mut expected = vec![0; bytes];
            for index in 0..bytes {
                source[source_start + index] = input[index % input.len()];
                expected[index] = output[index % output.len()];
            }
            require(
                independent_output(&source[source_start..source_start + bytes]) == expected,
                "扩展record165 oracle不符",
            )?;
            require(
                (source.as_ptr() as usize + source_start) & 15 == alignment.0,
                "实际source对齐不符",
            )?;
            require(
                (destination.as_ptr() as usize + destination_start) & 15 == alignment.1,
                "实际destination对齐不符",
            )?;
            let source_before = source.clone();
            Ok(Self {
                source,
                source_before,
                destination,
                expected,
                source_start,
                destination_start,
                bytes,
            })
        }
        fn reset(&mut self) {
            self.destination.fill(0xa7);
        }
        fn verify(&self) -> Result<(), String> {
            require(
                self.source == self.source_before,
                "source或source canary被修改",
            )?;
            let end = self.destination_start + self.bytes;
            require(
                self.destination[self.destination_start..end] == self.expected,
                "输出逐byte不符",
            )?;
            require(
                self.destination[..self.destination_start]
                    .iter()
                    .all(|&v| v == 0xa7),
                "目标前canary被修改",
            )?;
            require(
                self.destination[end..].iter().all(|&v| v == 0xa7),
                "目标后canary被修改",
            )
        }
    }

    // route选择、输入准备、验证与输出均在计时外；每个batch分支只静态直接调用。
    fn batch(route: Route, case: &mut Case, calls: usize) -> u128 {
        let source = &case.source[case.source_start..case.source_start + case.bytes];
        let destination =
            &mut case.destination[case.destination_start..case.destination_start + case.bytes];
        match route {
            Route::Reference => {
                let start = Instant::now();
                for _ in 0..calls {
                    rgba_to_bgrx_scalar(black_box(source), black_box(&mut *destination));
                    black_box(&*destination);
                }
                start.elapsed().as_nanos()
            }
            Route::Sse2 => {
                let start = Instant::now();
                for _ in 0..calls {
                    // SAFETY: run已检查真实SSE2；本case完整pixel、独立且目标长度充分。
                    unsafe {
                        actual::rgba_to_bgrx_sse2(black_box(source), black_box(&mut *destination))
                    };
                    black_box(&*destination);
                }
                start.elapsed().as_nanos()
            }
            Route::Ssse3 => {
                let start = Instant::now();
                for _ in 0..calls {
                    // SAFETY: 只有真实SSSE3可用才进入此route；切片合同同上。
                    unsafe {
                        actual::rgba_to_bgrx(black_box(source), black_box(&mut *destination))
                    };
                    black_box(&*destination);
                }
                start.elapsed().as_nanos()
            }
            Route::CurrentDispatch => {
                let start = Instant::now();
                for _ in 0..calls {
                    pixel_convert::convert_rgba_to_bgrx(
                        black_box(source),
                        black_box(&mut *destination),
                    )
                    .expect("有效固定case不应被真实API拒绝");
                    black_box(&*destination);
                }
                start.elapsed().as_nanos()
            }
        }
    }

    fn calls(pixels: usize) -> usize {
        if pixels <= 65 {
            4096
        } else if pixels <= 4096 {
            64
        } else {
            1
        }
    }
    fn literal_routes(ssse3: bool) -> Result<(), String> {
        let source = [1, 2, 3, 0, 255, 0, 16, 1, 0, 254, 1, 127, 9, 8, 7, 255];
        let expected = [3, 2, 1, 255, 16, 0, 255, 255, 1, 254, 0, 255, 7, 8, 9, 255];
        for route in ROUTES {
            if !route.available(ssse3) {
                continue;
            }
            let mut output = [0xa7; 23];
            let destination = &mut output[3..19];
            // 此固定literal锚点全部在计时外；真实ISA与后续batch同一准入。
            match route {
                Route::Reference => rgba_to_bgrx_scalar(&source, destination),
                Route::Sse2 => unsafe { actual::rgba_to_bgrx_sse2(&source, destination) },
                Route::Ssse3 => unsafe { actual::rgba_to_bgrx(&source, destination) },
                Route::CurrentDispatch => pixel_convert::convert_rgba_to_bgrx(&source, destination)
                    .map_err(|_| "真实API拒绝literal锚点")?,
            }
            require(output[3..19] == expected, "真实route的RGB/opaque-X锚点不符")?;
            require(
                output[..3] == [0xa7; 3] && output[19..] == [0xa7; 4],
                "literal目标canary不符",
            )?;
        }
        Ok(())
    }
    fn ordered_route(round: usize, order: usize) -> usize {
        (round + order) % ROUTES.len()
    }
    fn quantiles(samples: &[u128; SAMPLES]) -> (u128, u128) {
        let mut sorted = *samples;
        sorted.sort_unstable();
        (sorted[15], sorted[29])
    }
    fn ratio(numerator: u128, denominator: u128) -> String {
        if denominator == 0 {
            "UNDEFINED_ZERO_ELAPSED".into()
        } else {
            format!("{:.6}", numerator as f64 / denominator as f64)
        }
    }

    fn cpu(ssse3: bool) {
        #[cfg(target_arch = "x86")]
        use std::arch::x86::__cpuid;
        #[cfg(target_arch = "x86_64")]
        use std::arch::x86_64::__cpuid;
        // SAFETY: 此函数在实际SSE2检查之后调用；只读取真实CPU标识，不改变能力或策略。
        unsafe {
            let vendor = __cpuid(0);
            let mut vendor_bytes = Vec::new();
            for word in [vendor.ebx, vendor.edx, vendor.ecx] {
                vendor_bytes.extend_from_slice(&word.to_le_bytes());
            }
            let max = __cpuid(0x8000_0000).eax;
            let mut brand = Vec::new();
            if max >= 0x8000_0004 {
                for leaf in 0x8000_0002..=0x8000_0004 {
                    let part = __cpuid(leaf);
                    for word in [part.eax, part.ebx, part.ecx, part.edx] {
                        brand.extend_from_slice(&word.to_le_bytes());
                    }
                }
            }
            let features = __cpuid(1);
            println!("BENCH_CPU process_arch={} os={} pointer_bits={} vendor={:?} brand={:?} leaf1_eax=0x{:08x} leaf1_ecx=0x{:08x} leaf1_edx=0x{:08x} actual_sse2=true actual_ssse3={ssse3} host_scope=external-runner-record",
                std::env::consts::ARCH, std::env::consts::OS, usize::BITS,
                String::from_utf8_lossy(&vendor_bytes), String::from_utf8_lossy(&brand).trim_end_matches('\0'), features.eax, features.ecx, features.edx);
        }
    }

    pub fn run() -> Result<i32, String> {
        let began = Instant::now();
        require(
            std::env::args_os().len() == 1,
            "固定benchmark不接受路径、次数、CPU、阈值或其他参数",
        )?;
        require(!cfg!(debug_assertions), "私有微基准只接受release图像")?;
        // 独立固定总预算；无重试。正常结束不等待watchdog，进程退出时线程一并结束。
        std::thread::spawn(move || {
            std::thread::sleep(BUDGET.saturating_sub(began.elapsed()));
            eprintln!("BENCH_PARTIAL reason=120-second-total-budget");
            std::process::exit(124);
        });
        if !actual::sse2_available() {
            eprintln!("UNAVAILABLE reason=actual-sse2-required");
            return Ok(77);
        }
        let ssse3 = actual::ssse3_available();
        let (input, output) = oracle()?;
        literal_routes(ssse3)?;
        budget(began)?;
        println!("BENCH_META scope=private-publication-warm-microbenchmark cases=30 routes=4 warmup_rounds=8 samples=31 total_budget_seconds=120 timed_allocation=false payload_mib_definition=one-source-payload");
        println!("BENCH_ORACLE record=165 pixels=96 archive_sha256={ARCHIVE_SHA} uncompressed_sha256={RAW_SHA} source_sha256={INPUT_SHA} output_sha256={OUTPUT_SHA} frozen_scalar_source_sha256={OLD_SOURCE_SHA}");
        println!("BENCH_SCOPE direct_leaf_and_current_dispatch=separate-compilation-contexts frozen_rust_reference_auto_vectorization=not-excluded first_operation=per-case-after-isa-and-input-preparation cache_cold=not-claimed frame_rate=not-claimed end_to_end_latency=not-measured speed_threshold=none");
        cpu(ssse3);
        if !ssse3 {
            println!("BENCH_UNAVAILABLE route=direct-ssse3 reason=actual-cpu-feature-absent");
        }
        let mut completed = 0;
        for pixels in COUNTS {
            for alignment in ALIGNMENTS {
                budget(began)?;
                let case_id = completed;
                let mut case = Case::new(pixels, alignment, &input, &output)?;
                let batch_calls = calls(pixels);
                println!("BENCH_CASE case={case_id} pixels={pixels} src_mod16={} dst_mod16={} calls={batch_calls} input_sha256={} expected_sha256={}", alignment.0, alignment.1,
                    digest(&case.source[case.source_start..case.source_start + case.bytes]), digest(&case.expected));
                for order in 0..4 {
                    let route = ROUTES[ordered_route(case_id, order)];
                    if !route.available(ssse3) {
                        continue;
                    }
                    budget(began)?;
                    case.reset();
                    let elapsed = batch(route, &mut case, 1);
                    case.verify()?;
                    budget(began)?;
                    println!(
                        "BENCH_FIRST case={case_id} route={} calls=1 elapsed_ns={elapsed}",
                        route.name()
                    );
                }
                let mut samples = [[0_u128; SAMPLES]; 4];
                for round in 0..WARMUP + SAMPLES {
                    for order in 0..4 {
                        let index = ordered_route(case_id + 1 + round, order);
                        let route = ROUTES[index];
                        if !route.available(ssse3) {
                            continue;
                        }
                        budget(began)?;
                        case.reset();
                        let elapsed = batch(route, &mut case, batch_calls);
                        case.verify()?;
                        budget(began)?;
                        if round >= WARMUP {
                            let sample = round - WARMUP;
                            samples[index][sample] = elapsed;
                            println!("BENCH_SAMPLE case={case_id} route={} sample={sample} order={order} calls={batch_calls} elapsed_ns={elapsed}", route.name());
                        }
                    }
                }
                for (index, route) in ROUTES.iter().copied().enumerate() {
                    if !route.available(ssse3) {
                        continue;
                    }
                    let (median, p95) = quantiles(&samples[index]);
                    let mib_s = if case.bytes == 0 || median == 0 {
                        "NA".into()
                    } else {
                        format!(
                            "{:.6}",
                            case.bytes as f64 * batch_calls as f64 * 1_000_000_000.0
                                / (median as f64 * 1_048_576.0)
                        )
                    };
                    println!("BENCH_SUMMARY case={case_id} route={} calls={batch_calls} samples=31 median_batch_ns={median} p95_batch_ns={p95} median_ns_per_call={:.6} p95_ns_per_call={:.6} payload_mib_s={mib_s}",
                        route.name(), median as f64 / batch_calls as f64, p95 as f64 / batch_calls as f64);
                }
                let reference = quantiles(&samples[0]).0;
                let ssse3_ratio = if ssse3 {
                    ratio(reference, quantiles(&samples[2]).0)
                } else {
                    "UNAVAILABLE".into()
                };
                println!("BENCH_COMPARE case={case_id} reference_over_sse2={} reference_over_ssse3={ssse3_ratio} current_dispatch=reported-separately", ratio(reference, quantiles(&samples[1]).0));
                println!("BENCH_CASE_DONE case={case_id} output_sha256={} source=unchanged canaries=unchanged", digest(&case.destination[case.destination_start..case.destination_start + case.bytes]));
                completed += 1;
            }
        }
        budget(began)?;
        if !ssse3 {
            println!("BENCH_PARTIAL cases={completed} complete_routes=3 unavailable=direct-ssse3");
            return Ok(77);
        }
        println!("BENCH_OK cases={completed} routes=4 samples_per_route_case=31 scope=private-warm-only shipping=open full_unwind=open protocol_latency=open shared_runner_noise=not-excluded");
        Ok(0)
    }
}

#[cfg(all(
    not(test),
    any(target_os = "linux", target_os = "windows"),
    any(target_arch = "x86", target_arch = "x86_64")
))]
fn main() {
    match native::run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("BENCH_PARTIAL reason={error}");
            std::process::exit(1);
        }
    }
}
#[cfg(not(all(
    not(test),
    any(target_os = "linux", target_os = "windows"),
    any(target_arch = "x86", target_arch = "x86_64")
)))]
fn main() {
    eprintln!("UNAVAILABLE requires=non-test-linux-or-windows-x86-or-x64 macos=unsupported");
    std::process::exit(77);
}
