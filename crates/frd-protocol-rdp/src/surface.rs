use frd_core::{PixelRect, PixelSize};
use frd_frame::{PixelBuffer, PixelPatch};
use ironrdp::graphics::image_processing::PixelFormat as IronPixelFormat;
use ironrdp::pdu::geometry::InclusiveRectangle;
use ironrdp::session::image::DecodedImage;

const BYTES_PER_PIXEL: usize = 4;
const FRAME_MAILBOX_PIXEL_BUDGET: u64 = 64 * 1024 * 1024;
const RENDERER_TEXTURE_BUDGET: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RdpSurfaceError {
    InvalidSize,
    SurfaceBudgetExceeded,
    UnsupportedPixelFormat,
    InvalidRegion,
    InvalidImageBuffer,
    AllocationFailed,
}

pub(crate) fn validate_negotiated_size(
    width: u16,
    height: u16,
) -> Result<PixelSize, RdpSurfaceError> {
    let size =
        PixelSize::new(u32::from(width), u32::from(height)).ok_or(RdpSurfaceError::InvalidSize)?;
    let surface_bytes = u64::from(size.width)
        .checked_mul(u64::from(size.height))
        .and_then(|pixels| pixels.checked_mul(BYTES_PER_PIXEL as u64))
        .ok_or(RdpSurfaceError::SurfaceBudgetExceeded)?;
    if surface_bytes > FRAME_MAILBOX_PIXEL_BUDGET || surface_bytes > RENDERER_TEXTURE_BUDGET {
        return Err(RdpSurfaceError::SurfaceBudgetExceeded);
    }
    Ok(size)
}

pub(crate) fn extract_bgrx_patch(
    image: &DecodedImage,
    region: InclusiveRectangle,
) -> Result<PixelPatch, RdpSurfaceError> {
    extract_bgrx_patch_from_parts(
        image.pixel_format(),
        image.width(),
        image.height(),
        image.data(),
        region,
    )
}

fn extract_bgrx_patch_from_parts(
    pixel_format: IronPixelFormat,
    image_width: u16,
    image_height: u16,
    image_data: &[u8],
    region: InclusiveRectangle,
) -> Result<PixelPatch, RdpSurfaceError> {
    if !matches!(
        pixel_format,
        IronPixelFormat::RgbA32 | IronPixelFormat::RgbX32
    ) {
        return Err(RdpSurfaceError::UnsupportedPixelFormat);
    }
    if image_width == 0
        || image_height == 0
        || region.left > region.right
        || region.top > region.bottom
        || region.right >= image_width
        || region.bottom >= image_height
    {
        return Err(RdpSurfaceError::InvalidRegion);
    }

    let source_stride = usize::from(image_width)
        .checked_mul(BYTES_PER_PIXEL)
        .ok_or(RdpSurfaceError::InvalidImageBuffer)?;
    let expected_image_bytes = source_stride
        .checked_mul(usize::from(image_height))
        .ok_or(RdpSurfaceError::InvalidImageBuffer)?;
    if image_data.len() != expected_image_bytes {
        return Err(RdpSurfaceError::InvalidImageBuffer);
    }

    let width = u32::from(region.right - region.left) + 1;
    let height = u32::from(region.bottom - region.top) + 1;
    let stride_bytes = width
        .checked_mul(BYTES_PER_PIXEL as u32)
        .ok_or(RdpSurfaceError::InvalidRegion)?;
    let byte_count = usize::try_from(stride_bytes)
        .ok()
        .and_then(|stride| stride.checked_mul(height as usize))
        .ok_or(RdpSurfaceError::InvalidRegion)?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(byte_count)
        .map_err(|_| RdpSurfaceError::AllocationFailed)?;

    let source_x = usize::from(region.left)
        .checked_mul(BYTES_PER_PIXEL)
        .ok_or(RdpSurfaceError::InvalidRegion)?;
    let source_width = usize::try_from(stride_bytes).map_err(|_| RdpSurfaceError::InvalidRegion)?;
    for y in region.top..=region.bottom {
        let row_start = usize::from(y)
            .checked_mul(source_stride)
            .and_then(|offset| offset.checked_add(source_x))
            .ok_or(RdpSurfaceError::InvalidRegion)?;
        let row_end = row_start
            .checked_add(source_width)
            .ok_or(RdpSurfaceError::InvalidRegion)?;
        for source in image_data[row_start..row_end].chunks_exact(BYTES_PER_PIXEL) {
            pixels.extend_from_slice(&[source[2], source[1], source[0], 0xff]);
        }
    }

    debug_assert_eq!(pixels.len(), byte_count);
    Ok(PixelPatch {
        rect: PixelRect {
            x: u32::from(region.left),
            y: u32::from(region.top),
            width,
            height,
        },
        stride_bytes,
        pixels: PixelBuffer::new(pixels),
    })
}

#[cfg(test)]
mod tests {
    use frd_core::{PixelRect, PixelSize};
    use ironrdp::graphics::image_processing::PixelFormat as IronPixelFormat;
    use ironrdp::pdu::geometry::InclusiveRectangle;
    use ironrdp::session::image::DecodedImage;

    use super::{
        extract_bgrx_patch, extract_bgrx_patch_from_parts, validate_negotiated_size,
        RdpSurfaceError,
    };

    fn rgb_fixture(width: u16, height: u16) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(usize::from(width) * usize::from(height) * 4);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    10 + x as u8,
                    40 + y as u8,
                    80 + (y * width + x) as u8,
                    7,
                ]);
            }
        }
        pixels
    }

    #[test]
    fn surface_inner_update_copies_only_changed_rows_as_bgrx() {
        let source = rgb_fixture(4, 3);
        let patch = extract_bgrx_patch_from_parts(
            IronPixelFormat::RgbA32,
            4,
            3,
            &source,
            InclusiveRectangle {
                left: 1,
                top: 1,
                right: 2,
                bottom: 2,
            },
        )
        .expect("valid inner rectangle");

        assert_eq!(
            patch.rect,
            PixelRect {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            }
        );
        assert_eq!(patch.stride_bytes, 8);
        assert_eq!(patch.pixels.len(), 16);
        assert!(patch.pixels.len() < source.len());
        assert_eq!(
            patch.pixels.as_bytes(),
            &[85, 41, 11, 255, 86, 41, 12, 255, 89, 42, 11, 255, 90, 42, 12, 255,]
        );
    }

    #[test]
    fn surface_decoded_image_wrapper_keeps_patch_bounded_and_opaque() {
        let image = DecodedImage::new(IronPixelFormat::RgbA32, 4, 3);
        let patch = extract_bgrx_patch(
            &image,
            InclusiveRectangle {
                left: 1,
                top: 1,
                right: 2,
                bottom: 2,
            },
        )
        .expect("valid decoded image region");

        assert_eq!(patch.pixels.len(), 16);
        assert_eq!(
            patch.pixels.as_bytes(),
            &[0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]
        );
    }

    #[test]
    fn surface_rejects_out_of_bounds_and_inverted_inclusive_rectangles() {
        let source = rgb_fixture(4, 3);
        for region in [
            InclusiveRectangle {
                left: 0,
                top: 0,
                right: 4,
                bottom: 2,
            },
            InclusiveRectangle {
                left: 0,
                top: 0,
                right: 3,
                bottom: 3,
            },
            InclusiveRectangle {
                left: 2,
                top: 0,
                right: 1,
                bottom: 0,
            },
        ] {
            assert_eq!(
                extract_bgrx_patch_from_parts(IronPixelFormat::RgbA32, 4, 3, &source, region,)
                    .expect_err("invalid inclusive rectangle must fail"),
                RdpSurfaceError::InvalidRegion
            );
        }
    }

    #[test]
    fn surface_size_is_nonzero_and_within_mailbox_and_renderer_budgets() {
        assert_eq!(
            validate_negotiated_size(8192, 2048).expect("exact mailbox budget is accepted"),
            PixelSize {
                width: 8192,
                height: 2048,
            }
        );
        assert_eq!(
            validate_negotiated_size(0, 2048),
            Err(RdpSurfaceError::InvalidSize)
        );
        assert_eq!(
            validate_negotiated_size(8192, 2049),
            Err(RdpSurfaceError::SurfaceBudgetExceeded)
        );
    }
}
