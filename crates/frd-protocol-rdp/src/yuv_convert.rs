//! Architecture-dispatched YUV420P8 to RGBA8888 conversion.
//!
//! RDP EGFX decoders return planar YUV. The protocol state machine remains
//! portable Rust, while this leaf operation uses an architecture kernel when
//! the target supports it. The scalar loop is retained as the byte-accurate
//! oracle and as the explicit unsupported-CPU fallback.

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod x86;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum YuvConvertError {
    InvalidPlane,
    DestinationTooSmall,
}

/// Return the minimum byte length needed for a strided plane without allowing
/// attacker-controlled dimensions or strides to wrap `usize` arithmetic.
fn required_plane_len(height: usize, stride: usize, row_width: usize) -> Option<usize> {
    if height == 0 {
        // Preserve the existing validation contract for a zero-row input:
        // callers still need to provide one row's worth of storage when the
        // declared plane width is non-zero.
        return Some(row_width);
    }
    height
        .checked_sub(1)?
        .checked_mul(stride)?
        .checked_add(row_width)
}

pub(crate) fn convert_yuv420_to_rgba(
    width: usize,
    height: usize,
    y_plane: &[u8],
    y_stride: usize,
    u_plane: &[u8],
    u_stride: usize,
    v_plane: &[u8],
    v_stride: usize,
    destination: &mut [u8],
) -> Result<(), YuvConvertError> {
    let chroma_width = width.div_ceil(2);
    let chroma_height = height.div_ceil(2);
    let output_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(YuvConvertError::DestinationTooSmall)?;
    if destination.len() < output_len {
        return Err(YuvConvertError::DestinationTooSmall);
    }
    let Some(y_required) = required_plane_len(height, y_stride, width) else {
        return Err(YuvConvertError::InvalidPlane);
    };
    let Some(u_required) = required_plane_len(chroma_height, u_stride, chroma_width) else {
        return Err(YuvConvertError::InvalidPlane);
    };
    let Some(v_required) = required_plane_len(chroma_height, v_stride, chroma_width) else {
        return Err(YuvConvertError::InvalidPlane);
    };
    if y_stride < width
        || u_stride < chroma_width
        || v_stride < chroma_width
        || y_plane.len() < y_required
        || u_plane.len() < u_required
        || v_plane.len() < v_required
    {
        return Err(YuvConvertError::InvalidPlane);
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if x86::sse41_available() {
            // SAFETY: the runtime feature check matches the leaf kernel and
            // all plane/destination lengths were validated above.
            unsafe {
                x86::yuv420_to_rgba(
                    width,
                    height,
                    y_plane,
                    y_stride,
                    u_plane,
                    u_stride,
                    v_plane,
                    v_stride,
                    &mut destination[..output_len],
                )
            };
            return Ok(());
        }
        yuv420_to_rgba_scalar(
            width,
            height,
            y_plane,
            y_stride,
            u_plane,
            u_stride,
            v_plane,
            v_stride,
            &mut destination[..output_len],
        );
        return Ok(());
    }

    #[cfg(target_arch = "aarch64")]
    {
        // AArch64 mandates NEON in the supported ABI.
        // SAFETY: all plane/destination lengths were validated above.
        unsafe {
            aarch64::yuv420_to_rgba(
                width,
                height,
                y_plane,
                y_stride,
                u_plane,
                u_stride,
                v_plane,
                v_stride,
                &mut destination[..output_len],
            )
        };
        return Ok(());
    }

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        yuv420_to_rgba_scalar(
            width,
            height,
            y_plane,
            y_stride,
            u_plane,
            u_stride,
            v_plane,
            v_stride,
            &mut destination[..output_len],
        );
        Ok(())
    }
}

/// Convert full-resolution YUV444P8 planes to RGBA8888 using the same
/// architecture-dispatched arithmetic as the YUV420 path.
pub(crate) fn convert_yuv444_to_rgba(
    width: usize,
    height: usize,
    y_plane: &[u8],
    y_stride: usize,
    u_plane: &[u8],
    u_stride: usize,
    v_plane: &[u8],
    v_stride: usize,
    destination: &mut [u8],
) -> Result<(), YuvConvertError> {
    let output_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(YuvConvertError::DestinationTooSmall)?;
    if destination.len() < output_len {
        return Err(YuvConvertError::DestinationTooSmall);
    }
    let Some(y_required) = required_plane_len(height, y_stride, width) else {
        return Err(YuvConvertError::InvalidPlane);
    };
    let Some(u_required) = required_plane_len(height, u_stride, width) else {
        return Err(YuvConvertError::InvalidPlane);
    };
    let Some(v_required) = required_plane_len(height, v_stride, width) else {
        return Err(YuvConvertError::InvalidPlane);
    };
    if y_stride < width
        || u_stride < width
        || v_stride < width
        || y_plane.len() < y_required
        || u_plane.len() < u_required
        || v_plane.len() < v_required
    {
        return Err(YuvConvertError::InvalidPlane);
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if x86::sse41_available() {
            // SAFETY: the feature check and plane validation match the leaf kernel.
            unsafe {
                x86::yuv444_to_rgba(
                    width,
                    height,
                    y_plane,
                    y_stride,
                    u_plane,
                    u_stride,
                    v_plane,
                    v_stride,
                    &mut destination[..output_len],
                )
            };
            return Ok(());
        }
        yuv444_to_rgba_scalar(
            width,
            height,
            y_plane,
            y_stride,
            u_plane,
            u_stride,
            v_plane,
            v_stride,
            &mut destination[..output_len],
        );
        return Ok(());
    }

    #[cfg(target_arch = "aarch64")]
    {
        // AArch64 mandates NEON in the supported ABI.
        unsafe {
            aarch64::yuv444_to_rgba(
                width,
                height,
                y_plane,
                y_stride,
                u_plane,
                u_stride,
                v_plane,
                v_stride,
                &mut destination[..output_len],
            )
        };
        return Ok(());
    }

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        yuv444_to_rgba_scalar(
            width,
            height,
            y_plane,
            y_stride,
            u_plane,
            u_stride,
            v_plane,
            v_stride,
            &mut destination[..output_len],
        );
        Ok(())
    }
}

#[inline]
pub(crate) fn yuv_to_rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    let luma = (i32::from(y) - 16).max(0);
    let u = i32::from(u) - 128;
    let v = i32::from(v) - 128;
    let red = (298 * luma + 459 * v + 128) >> 8;
    let green = (298 * luma - 55 * u - 136 * v + 128) >> 8;
    let blue = (298 * luma + 541 * u + 128) >> 8;
    [
        red.clamp(0, 255) as u8,
        green.clamp(0, 255) as u8,
        blue.clamp(0, 255) as u8,
    ]
}

#[inline(never)]
pub(super) fn yuv420_to_rgba_scalar(
    width: usize,
    height: usize,
    y_plane: &[u8],
    y_stride: usize,
    u_plane: &[u8],
    u_stride: usize,
    v_plane: &[u8],
    v_stride: usize,
    destination: &mut [u8],
) {
    for row in 0..height {
        let y_row = &y_plane[row * y_stride..];
        let u_row = &u_plane[(row / 2) * u_stride..];
        let v_row = &v_plane[(row / 2) * v_stride..];
        let output_row = &mut destination[row * width * 4..];
        for column in 0..width {
            let [red, green, blue] =
                yuv_to_rgb(y_row[column], u_row[column / 2], v_row[column / 2]);
            let offset = column * 4;
            output_row[offset..offset + 4].copy_from_slice(&[red, green, blue, 0xff]);
        }
    }
}

#[inline(never)]
pub(super) fn yuv444_to_rgba_scalar(
    width: usize,
    height: usize,
    y_plane: &[u8],
    y_stride: usize,
    u_plane: &[u8],
    u_stride: usize,
    v_plane: &[u8],
    v_stride: usize,
    destination: &mut [u8],
) {
    for row in 0..height {
        let y_row = &y_plane[row * y_stride..];
        let u_row = &u_plane[row * u_stride..];
        let v_row = &v_plane[row * v_stride..];
        let output_row = &mut destination[row * width * 4..];
        for column in 0..width {
            let [red, green, blue] = yuv_to_rgb(y_row[column], u_row[column], v_row[column]);
            let offset = column * 4;
            output_row[offset..offset + 4].copy_from_slice(&[red, green, blue, 0xff]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        convert_yuv420_to_rgba, convert_yuv444_to_rgba, yuv420_to_rgba_scalar,
        yuv444_to_rgba_scalar, YuvConvertError,
    };

    #[test]
    fn conversion_rejects_strides_that_overflow_plane_length() {
        let overflowing_stride = usize::MAX / 2 + 1;
        let mut destination = [0_u8; 12];

        assert_eq!(
            convert_yuv420_to_rgba(
                1,
                3,
                &[],
                overflowing_stride,
                &[],
                1,
                &[],
                1,
                &mut destination,
            ),
            Err(YuvConvertError::InvalidPlane)
        );
        assert_eq!(
            convert_yuv444_to_rgba(
                1,
                3,
                &[],
                overflowing_stride,
                &[],
                1,
                &[],
                1,
                &mut destination,
            ),
            Err(YuvConvertError::InvalidPlane)
        );
    }

    #[test]
    fn conversion_keeps_zero_height_plane_validation_contract() {
        let mut destination = [];

        assert_eq!(
            convert_yuv420_to_rgba(1, 0, &[], 1, &[], 1, &[], 1, &mut destination),
            Err(YuvConvertError::InvalidPlane)
        );
        assert_eq!(
            convert_yuv444_to_rgba(1, 0, &[], 1, &[], 1, &[], 1, &mut destination),
            Err(YuvConvertError::InvalidPlane)
        );
    }

    #[test]
    fn dispatched_yuv420_conversion_matches_reference_with_strides_and_odd_edges() {
        let width: usize = 11;
        let height: usize = 5;
        let y_stride = 13;
        let chroma_width = width.div_ceil(2);
        let chroma_height = height.div_ceil(2);
        let u_stride = 7;
        let v_stride = 8;
        let y = (0..y_stride * height)
            .map(|index| (index as u8).wrapping_mul(19).wrapping_add(3))
            .collect::<Vec<_>>();
        let u = (0..u_stride * chroma_height)
            .map(|index| (index as u8).wrapping_mul(7).wrapping_add(80))
            .collect::<Vec<_>>();
        let v = (0..v_stride * chroma_height)
            .map(|index| (index as u8).wrapping_mul(11).wrapping_add(120))
            .collect::<Vec<_>>();
        let mut expected = vec![0; width * height * 4];
        yuv420_to_rgba_scalar(
            width,
            height,
            &y,
            y_stride,
            &u,
            u_stride,
            &v,
            v_stride,
            &mut expected,
        );
        let mut actual = vec![0; expected.len()];
        convert_yuv420_to_rgba(
            width,
            height,
            &y,
            y_stride,
            &u,
            u_stride,
            &v,
            v_stride,
            &mut actual,
        )
        .expect("valid YUV420 planes");
        assert_eq!(actual, expected);

        assert_eq!(chroma_width, 6);
        assert_eq!(chroma_height, 3);
    }

    #[test]
    fn conversion_rejects_short_plane_or_destination() {
        let mut destination = [0_u8; 16];
        assert!(convert_yuv420_to_rgba(
            2,
            2,
            &[16, 16],
            2,
            &[128; 4],
            2,
            &[128; 4],
            2,
            &mut destination,
        )
        .is_err());
        assert!(convert_yuv420_to_rgba(
            2,
            2,
            &[16; 4],
            2,
            &[128; 4],
            2,
            &[128; 4],
            2,
            &mut [0_u8; 15],
        )
        .is_err());
    }

    #[test]
    fn dispatched_yuv444_conversion_matches_reference_with_strides() {
        let width = 9;
        let height = 3;
        let y_stride = 11;
        let u_stride = 12;
        let v_stride = 13;
        let y = (0..y_stride * height)
            .map(|index| (index as u8).wrapping_mul(3).wrapping_add(16))
            .collect::<Vec<_>>();
        let u = (0..u_stride * height)
            .map(|index| (index as u8).wrapping_mul(5).wrapping_add(90))
            .collect::<Vec<_>>();
        let v = (0..v_stride * height)
            .map(|index| (index as u8).wrapping_mul(7).wrapping_add(110))
            .collect::<Vec<_>>();
        let mut expected = vec![0; width * height * 4];
        yuv444_to_rgba_scalar(
            width,
            height,
            &y,
            y_stride,
            &u,
            u_stride,
            &v,
            v_stride,
            &mut expected,
        );
        let mut actual = vec![0; expected.len()];
        convert_yuv444_to_rgba(
            width,
            height,
            &y,
            y_stride,
            &u,
            u_stride,
            &v,
            v_stride,
            &mut actual,
        )
        .expect("valid YUV444 planes");
        assert_eq!(actual, expected);
    }

    #[test]
    fn dispatched_yuv_kernels_match_reference_for_every_short_width_tail() {
        let height: usize = 3;
        for width in 1_usize..=15 {
            let y_stride = width + 3;
            let chroma_width = width.div_ceil(2);
            let chroma_height = height.div_ceil(2);
            let u420_stride = chroma_width + 2;
            let v420_stride = chroma_width + 3;
            let y = (0..y_stride * height)
                .map(|index| (index as u8).wrapping_mul(13).wrapping_add(5))
                .collect::<Vec<_>>();
            let u420 = (0..u420_stride * chroma_height)
                .map(|index| (index as u8).wrapping_mul(17).wrapping_add(71))
                .collect::<Vec<_>>();
            let v420 = (0..v420_stride * chroma_height)
                .map(|index| (index as u8).wrapping_mul(23).wrapping_add(109))
                .collect::<Vec<_>>();
            let mut expected420 = vec![0_u8; width * height * 4];
            let mut actual420 = vec![0_u8; expected420.len()];
            yuv420_to_rgba_scalar(
                width,
                height,
                &y,
                y_stride,
                &u420,
                u420_stride,
                &v420,
                v420_stride,
                &mut expected420,
            );
            convert_yuv420_to_rgba(
                width,
                height,
                &y,
                y_stride,
                &u420,
                u420_stride,
                &v420,
                v420_stride,
                &mut actual420,
            )
            .expect("valid YUV420 planes");
            assert_eq!(actual420, expected420, "YUV420 width={width}");

            let u444_stride = width + 2;
            let v444_stride = width + 3;
            let u444 = (0..u444_stride * height)
                .map(|index| (index as u8).wrapping_mul(19).wrapping_add(83))
                .collect::<Vec<_>>();
            let v444 = (0..v444_stride * height)
                .map(|index| (index as u8).wrapping_mul(31).wrapping_add(127))
                .collect::<Vec<_>>();
            let mut expected444 = vec![0_u8; width * height * 4];
            let mut actual444 = vec![0_u8; expected444.len()];
            yuv444_to_rgba_scalar(
                width,
                height,
                &y,
                y_stride,
                &u444,
                u444_stride,
                &v444,
                v444_stride,
                &mut expected444,
            );
            convert_yuv444_to_rgba(
                width,
                height,
                &y,
                y_stride,
                &u444,
                u444_stride,
                &v444,
                v444_stride,
                &mut actual444,
            )
            .expect("valid YUV444 planes");
            assert_eq!(actual444, expected444, "YUV444 width={width}");
        }
    }
}
