fn copy_cursor_data(
    from: &[u8],
    from_pos: (usize, usize),
    from_stride: usize,
    to: &mut [u8],
    to_stride: usize,
    to_pos: (usize, usize),
    size: (usize, usize),
    dst_size: (usize, usize),
    composite: bool,
) {
    const PIXEL_SIZE: usize = 4;

    if to_pos.0 + size.0 > dst_size.0 || to_pos.1 + size.1 > dst_size.1 {
        // Perform clipping
        return;
    }

    let (from_x, from_y) = from_pos;
    let (to_x, to_y) = to_pos;
    let (width, height) = size;

    for y in 0..height {
        let from_start = (from_y + y) * from_stride + from_x * PIXEL_SIZE;
        let to_start = (to_y + y) * to_stride + to_x * PIXEL_SIZE;

        if composite {
            for pixel in 0..width {
                let dest_r = to[to_start + pixel * PIXEL_SIZE];
                let dest_g = to[to_start + pixel * PIXEL_SIZE + 1];
                let dest_b = to[to_start + pixel * PIXEL_SIZE + 2];

                let src_r = from[from_start + pixel * PIXEL_SIZE];
                let src_g = from[from_start + pixel * PIXEL_SIZE + 1];
                let src_b = from[from_start + pixel * PIXEL_SIZE + 2];
                let src_a = from[from_start + pixel * PIXEL_SIZE + 3];

                // Inverted pixel, this color has a special meaning when encoded by ironrdp-graphics
                if src_a == 0 && src_r == 255 && src_g == 255 && src_b == 255 {
                    to[to_start + pixel * PIXEL_SIZE] = 255 - dest_r;
                    to[to_start + pixel * PIXEL_SIZE + 1] = 255 - dest_g;
                    to[to_start + pixel * PIXEL_SIZE + 2] = 255 - dest_b;
                    to[to_start + pixel * PIXEL_SIZE + 3] = 255;
                    continue;
                }

                // Skip 100% transparent pixels
                if src_a == 0 {
                    continue;
                }

                #[expect(clippy::as_conversions, reason = "(u16 >> 8) fits into u8 + hot loop")]
                {
                    // Integer alpha blending, source represented as premultiplied alpha color, calculation in floating point
                    to[to_start + pixel * PIXEL_SIZE] =
                        src_r + ((u16::from(dest_r) * u16::from(255 - src_a)) >> 8) as u8;
                    to[to_start + pixel * PIXEL_SIZE + 1] =
                        src_g + ((u16::from(dest_g) * u16::from(255 - src_a)) >> 8) as u8;
                    to[to_start + pixel * PIXEL_SIZE + 2] =
                        src_b + ((u16::from(dest_b) * u16::from(255 - src_a)) >> 8) as u8;
                    // Framebuffer is always opaque, so we can skip alpha channel change
                }
            }
        } else {
            to[to_start..to_start + width * PIXEL_SIZE]
                .copy_from_slice(&from[from_start..from_start + width * PIXEL_SIZE]);
        }
    }
}
