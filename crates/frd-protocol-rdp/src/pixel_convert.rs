//! Architecture-specific leaf kernels for RDP pixel publication.
//!
//! The protocol and surface state machines stay in portable Rust. This module
//! owns only the RGBA8888 -> BGRX8888 byte operation used by the legacy and
//! EGFX paths. Supported CPUs select their target kernel at runtime; the scalar
//! implementation is retained as the correctness oracle and explicit fallback.

#[cfg(target_arch = "aarch64")]
#[path = "pixel_convert/aarch64.rs"]
mod aarch64;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[path = "pixel_convert/x86.rs"]
mod x86;

const BYTES_PER_PIXEL: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PixelConvertError {
    InvalidLength,
    DestinationTooSmall,
}

pub(crate) fn convert_rgba_to_bgrx(
    source: &[u8],
    destination: &mut [u8],
) -> Result<(), PixelConvertError> {
    if source.len() % BYTES_PER_PIXEL != 0 {
        return Err(PixelConvertError::InvalidLength);
    }
    if destination.len() < source.len() {
        return Err(PixelConvertError::DestinationTooSmall);
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if x86::ssse3_available() {
            // SAFETY: the runtime check matches the target feature required by
            // the leaf kernel, and both buffers are valid for their lengths.
            unsafe { x86::rgba_to_bgrx(source, &mut destination[..source.len()]) };
            return Ok(());
        }
        if x86::sse2_available() {
            // SAFETY: 实际 SSE2 检查与叶子一致，完整 pixel 和目标长度已先检查。
            unsafe { x86::rgba_to_bgrx_sse2(source, &mut destination[..source.len()]) };
            return Ok(());
        }
        // 未通过真实 SSE2 检查的 CPU 才可使用显式 reference 后备。
        rgba_to_bgrx_scalar(source, &mut destination[..source.len()]);
        return Ok(());
    }

    #[cfg(target_arch = "aarch64")]
    {
        // AArch64 mandates NEON in the supported ABI.
        // SAFETY: the target architecture guarantees the enabled instructions.
        unsafe { aarch64::rgba_to_bgrx(source, &mut destination[..source.len()]) };
        return Ok(());
    }

    // Unsupported CPUs and deterministic tests use the explicit reference
    // implementation. Supported x86/AArch64 CPUs return above.
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        rgba_to_bgrx_scalar(source, &mut destination[..source.len()]);
        Ok(())
    }
}

#[inline(never)]
fn rgba_to_bgrx_scalar(source: &[u8], destination: &mut [u8]) {
    for (rgba, bgrx) in source
        .chunks_exact(BYTES_PER_PIXEL)
        .zip(destination.chunks_exact_mut(BYTES_PER_PIXEL))
    {
        bgrx.copy_from_slice(&[rgba[2], rgba[1], rgba[0], 0xff]);
    }
}

#[cfg(any(test, doctest))]
fn active_kernel_name() -> &'static str {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        return if x86::ssse3_available() {
            "x86-ssse3"
        } else if x86::sse2_available() {
            "x86-sse2"
        } else {
            "scalar-unsupported-x86"
        };
    }

    #[cfg(target_arch = "aarch64")]
    {
        return "aarch64-neon";
    }

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        "scalar-unsupported-cpu"
    }
}

#[cfg(test)]
mod tests {
    use super::{
        active_kernel_name, convert_rgba_to_bgrx, rgba_to_bgrx_scalar, PixelConvertError,
        BYTES_PER_PIXEL,
    };

    // 独立逐 pixel 字节 oracle，不调用生产 scalar 或任何 ISA 叶子。
    fn expected_bgrx(source: &[u8]) -> Vec<u8> {
        assert_eq!(source.len() % 4, 0);
        let mut expected = Vec::with_capacity(source.len());
        for pixel in 0..source.len() / 4 {
            expected.push(source[pixel * 4 + 2]);
            expected.push(source[pixel * 4 + 1]);
            expected.push(source[pixel * 4]);
            expected.push(255);
        }
        expected
    }

    fn publication_matrix(mut convert: impl FnMut(&[u8], &mut [u8])) {
        for pixels in (0usize..=33).chain([63, 64, 65, 127, 128, 129]) {
            for source_offset in 0..16 {
                for destination_offset in 0..16 {
                    let bytes = pixels * 4;
                    let mut source = vec![0x5a; source_offset + bytes + 16];
                    for pixel in 0..pixels {
                        let offset = source_offset + pixel * 4;
                        source[offset..offset + 4].copy_from_slice(&[
                            (pixel as u8).wrapping_mul(29).wrapping_add(1),
                            (pixel as u8).wrapping_mul(43).wrapping_add(7),
                            (pixel as u8).wrapping_mul(67).wrapping_add(13),
                            [0, 1, 127, 255][pixel % 4],
                        ]);
                    }
                    let source_before = source.clone();
                    let input = &source[source_offset..source_offset + bytes];
                    let expected = expected_bgrx(input);
                    let mut destination = vec![0xa7; destination_offset + bytes + 19];
                    convert(input, &mut destination[destination_offset..]);
                    assert_eq!(&destination[destination_offset..destination_offset + bytes],
                        expected, "pixels={pixels} source_offset={source_offset} destination_offset={destination_offset}");
                    assert!(destination[..destination_offset]
                        .iter()
                        .all(|&byte| byte == 0xa7));
                    assert!(destination[destination_offset + bytes..]
                        .iter()
                        .all(|&byte| byte == 0xa7));
                    assert_eq!(source, source_before);
                }
            }
        }
    }

    #[test]
    fn publication_literal_rgb_and_opaque_x_anchors() {
        let source = [1, 2, 3, 0, 255, 0, 16, 1, 0, 254, 1, 127, 9, 8, 7, 255];
        let expected = [3, 2, 1, 255, 16, 0, 255, 255, 1, 254, 0, 255, 7, 8, 9, 255];
        assert_eq!(expected_bgrx(&source), expected);
        let mut destination = [0xa7; 21];
        convert_rgba_to_bgrx(&source, &mut destination).unwrap();
        assert_eq!(&destination[..16], expected);
        assert_eq!(&destination[16..], [0xa7; 5]);
    }

    #[test]
    fn publication_dispatch_matches_independent_oracle_unaligned_and_tail_matrix() {
        publication_matrix(|source, destination| {
            convert_rgba_to_bgrx(source, destination).unwrap();
        });
    }

    #[test]
    fn publication_invalid_inputs_preserve_entire_destination() {
        for source_len in 0..=33 {
            for destination_len in 0..=40 {
                let expected_error = if source_len % 4 != 0 {
                    Some(PixelConvertError::InvalidLength)
                } else if destination_len < source_len {
                    Some(PixelConvertError::DestinationTooSmall)
                } else {
                    None
                };
                if let Some(expected_error) = expected_error {
                    let source = vec![0x3c; source_len];
                    let mut destination = vec![0xa7; destination_len];
                    assert_eq!(
                        convert_rgba_to_bgrx(&source, &mut destination),
                        Err(expected_error)
                    );
                    assert_eq!(destination, vec![0xa7; destination_len]);
                }
            }
        }
    }

    /// 原体可编译此反例：只否决 SSSE3，实际 SSE2 CPU 上必须进入 native publication。
    #[cfg(all(
        not(target_os = "macos"),
        any(target_arch = "x86", target_arch = "x86_64")
    ))]
    #[test]
    fn publication_actual_sse2_dispatch_when_ssse3_denied() {
        use super::x86;
        assert!(
            is_x86_feature_detected!("sse2"),
            "此资格反例需要实际 SSE2 CPU，不得模拟"
        );
        let actual_ssse3 = x86::ssse3_available();
        x86::with_ssse3_denied(|| {
            assert!(!x86::ssse3_available());
            let before = x86::sse2_entries();
            let source = [1, 2, 3, 0, 9, 8, 7, 127];
            let mut destination = [0xa7; 13];
            convert_rgba_to_bgrx(&source, &mut destination).unwrap();
            assert_eq!(&destination[..8], [3, 2, 1, 255, 7, 8, 9, 255]);
            assert_eq!(&destination[8..], [0xa7; 5]);
            assert_eq!(
                x86::sse2_entries(),
                before + 1,
                "像素相同仍不得让 SSE2-only publication 静默调用 scalar"
            );
            assert_eq!(active_kernel_name(), "x86-sse2");
        });
        assert_eq!(x86::ssse3_available(), actual_ssse3);
    }

    #[cfg(all(
        not(target_os = "macos"),
        any(target_arch = "x86", target_arch = "x86_64")
    ))]
    #[test]
    fn publication_actual_sse2_direct_and_denied_dispatch_matrix() {
        use super::x86;
        assert!(x86::sse2_available(), "需要实际 SSE2 CPU，不得模拟");
        publication_matrix(|source, destination| {
            // SAFETY: 实际 CPU 已检查；matrix 只传完整 pixel 与足够大的目标。
            unsafe { x86::rgba_to_bgrx_sse2(source, destination) };
        });
        x86::with_ssse3_denied(|| {
            let before = x86::sse2_entries();
            publication_matrix(|source, destination| {
                convert_rgba_to_bgrx(source, destination).unwrap();
            });
            assert_eq!(x86::sse2_entries() - before, 40 * 16 * 16);
        });
    }

    #[cfg(all(
        not(target_os = "macos"),
        any(target_arch = "x86", target_arch = "x86_64")
    ))]
    #[test]
    fn publication_actual_ssse3_priority_and_denial_restoration() {
        use super::x86;
        let actual_ssse3 = is_x86_feature_detected!("ssse3");
        assert_eq!(x86::ssse3_available(), actual_ssse3);
        if actual_ssse3 {
            let before = x86::sse2_entries();
            publication_matrix(|source, destination| {
                convert_rgba_to_bgrx(source, destination).unwrap();
            });
            assert_eq!(x86::sse2_entries(), before, "真实 SSSE3 必须优先");
            assert_eq!(active_kernel_name(), "x86-ssse3");
        }
        let unwound = std::panic::catch_unwind(|| {
            x86::with_ssse3_denied(|| {
                assert!(!x86::ssse3_available());
                x86::with_ssse3_denied(|| assert!(!x86::ssse3_available()));
                assert!(!x86::ssse3_available());
                panic!("测试 deny-only guard 的 unwind 恢复");
            })
        });
        assert!(unwound.is_err());
        assert_eq!(x86::ssse3_available(), actual_ssse3);
    }

    #[test]
    fn conversion_preserves_rgb_order_and_forces_opaque_alpha() {
        let source = [1, 2, 3, 4, 9, 8, 7, 6];
        let mut destination = [0; 8];

        convert_rgba_to_bgrx(&source, &mut destination).expect("valid pixel buffer");

        assert_eq!(destination, [3, 2, 1, 0xff, 7, 8, 9, 0xff]);
    }

    #[test]
    fn conversion_rejects_partial_pixels_and_short_destinations() {
        let mut destination = [0; 4];
        assert_eq!(
            convert_rgba_to_bgrx(&[1, 2, 3], &mut destination),
            Err(PixelConvertError::InvalidLength)
        );
        assert_eq!(
            convert_rgba_to_bgrx(&[1, 2, 3, 4], &mut [0; 3]),
            Err(PixelConvertError::DestinationTooSmall)
        );
    }

    #[test]
    fn conversion_handles_kernel_tail_after_aligned_blocks() {
        let source = (0..(4 * 5)).map(|value| value as u8).collect::<Vec<_>>();
        let mut destination = vec![0; source.len()];

        convert_rgba_to_bgrx(&source, &mut destination).expect("valid pixel buffer");

        for (rgba, bgrx) in source.chunks_exact(4).zip(destination.chunks_exact(4)) {
            assert_eq!(bgrx, &[rgba[2], rgba[1], rgba[0], 0xff]);
        }
    }

    #[test]
    fn conversion_matches_reference_for_every_short_pixel_tail() {
        for pixel_count in 1..=15 {
            let source = (0..pixel_count * BYTES_PER_PIXEL)
                .map(|index| (index as u8).wrapping_mul(29).wrapping_add(7))
                .collect::<Vec<_>>();
            let mut expected = vec![0_u8; source.len()];
            let mut actual = vec![0_u8; source.len()];
            rgba_to_bgrx_scalar(&source, &mut expected);
            convert_rgba_to_bgrx(&source, &mut actual).expect("valid pixel buffer");
            assert_eq!(actual, expected, "pixel_count={pixel_count}");
        }
    }

    #[test]
    fn conversion_reports_only_the_kernel_for_the_current_architecture() {
        let kernel = active_kernel_name();
        #[cfg(target_arch = "aarch64")]
        assert_eq!(kernel, "aarch64-neon");
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        assert!(matches!(
            kernel,
            "x86-ssse3" | "x86-sse2" | "scalar-unsupported-x86"
        ));
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
        assert_eq!(kernel, "scalar-unsupported-cpu");
    }

    #[test]
    fn conversion_dispatch_benchmark_records_kernel_and_matches_reference() {
        use std::hint::black_box;
        use std::time::Instant;

        const WIDTH: usize = 1_920;
        const HEIGHT: usize = 1_080;
        const ITERATIONS: usize = 4;
        let source = (0..WIDTH * HEIGHT * 4)
            .map(|index| (index as u8).wrapping_mul(17).wrapping_add(3))
            .collect::<Vec<_>>();
        let mut dispatched = vec![0_u8; source.len()];
        let mut reference = vec![0_u8; source.len()];

        let reference_start = Instant::now();
        for _ in 0..ITERATIONS {
            rgba_to_bgrx_scalar(black_box(&source), black_box(&mut reference));
        }
        let reference_elapsed = reference_start.elapsed();

        let dispatched_start = Instant::now();
        for _ in 0..ITERATIONS {
            convert_rgba_to_bgrx(black_box(&source), black_box(&mut dispatched))
                .expect("benchmark input is valid");
        }
        let dispatched_elapsed = dispatched_start.elapsed();

        assert_eq!(dispatched, reference);
        let bytes = (source.len() * ITERATIONS) as f64;
        let reference_mib_s = bytes / reference_elapsed.as_secs_f64() / (1024.0 * 1024.0);
        let dispatched_mib_s = bytes / dispatched_elapsed.as_secs_f64() / (1024.0 * 1024.0);
        let speedup = reference_elapsed.as_secs_f64() / dispatched_elapsed.as_secs_f64();
        eprintln!(
            "pixel_convert kernel={} size={}x{} iterations={} reference_mib_s={:.1} dispatched_mib_s={:.1} speedup={:.2}x",
            active_kernel_name(),
            WIDTH,
            HEIGHT,
            ITERATIONS,
            reference_mib_s,
            dispatched_mib_s,
            speedup,
        );
        if let Some(raw_threshold) = std::env::var_os("FRD_PIXEL_CONVERT_MIN_SPEEDUP") {
            let threshold = raw_threshold
                .to_str()
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value > 0.0)
                .expect("FRD_PIXEL_CONVERT_MIN_SPEEDUP 必须是正的有限数字");
            assert!(
                speedup >= threshold,
                "pixel_convert speedup {speedup:.2}x 低于显式门槛 {threshold:.2}x"
            );
        }
    }
}
