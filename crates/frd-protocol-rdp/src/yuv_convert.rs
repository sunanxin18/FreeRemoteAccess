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
    if y_stride < width
        || u_stride < chroma_width
        || v_stride < chroma_width
        || y_plane.len() < (height.saturating_sub(1) * y_stride).saturating_add(width)
        || u_plane.len() < (chroma_height.saturating_sub(1) * u_stride).saturating_add(chroma_width)
        || v_plane.len() < (chroma_height.saturating_sub(1) * v_stride).saturating_add(chroma_width)
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
    if y_stride < width
        || u_stride < width
        || v_stride < width
        || y_plane.len() < (height.saturating_sub(1) * y_stride).saturating_add(width)
        || u_plane.len() < (height.saturating_sub(1) * u_stride).saturating_add(width)
        || v_plane.len() < (height.saturating_sub(1) * v_stride).saturating_add(width)
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
        yuv444_to_rgba_scalar,
    };

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
}
