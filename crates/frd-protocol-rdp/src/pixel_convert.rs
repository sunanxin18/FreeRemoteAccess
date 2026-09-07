//! Architecture-specific leaf kernels for RDP pixel publication.
//!
//! The protocol and surface state machines stay in portable Rust. This module
//! owns only the RGBA8888 -> BGRX8888 byte operation used by the legacy and
//! EGFX paths. Supported CPUs select their target kernel at runtime; the scalar
//! implementation is retained as the correctness oracle and explicit fallback.

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
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
    use super::{active_kernel_name, convert_rgba_to_bgrx, rgba_to_bgrx_scalar, PixelConvertError};

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
    fn conversion_reports_only_the_kernel_for_the_current_architecture() {
        let kernel = active_kernel_name();
        #[cfg(target_arch = "aarch64")]
        assert_eq!(kernel, "aarch64-neon");
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        assert!(matches!(kernel, "x86-ssse3" | "scalar-unsupported-x86"));
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
