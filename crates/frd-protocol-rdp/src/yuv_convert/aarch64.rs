#[target_feature(enable = "neon")]
pub(super) unsafe fn yuv420_to_rgba(
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
    yuv_to_rgba_kernel(
        width,
        height,
        y_plane,
        y_stride,
        u_plane,
        u_stride,
        v_plane,
        v_stride,
        destination,
        true,
    );
}

#[target_feature(enable = "neon")]
pub(super) unsafe fn yuv444_to_rgba(
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
    yuv_to_rgba_kernel(
        width,
        height,
        y_plane,
        y_stride,
        u_plane,
        u_stride,
        v_plane,
        v_stride,
        destination,
        false,
    );
}

#[target_feature(enable = "neon")]
unsafe fn yuv_to_rgba_kernel(
    width: usize,
    height: usize,
    y_plane: &[u8],
    y_stride: usize,
    u_plane: &[u8],
    u_stride: usize,
    v_plane: &[u8],
    v_stride: usize,
    destination: &mut [u8],
    subsampled: bool,
) {
    use std::arch::aarch64::{
        int16x4_t, int32x4_t, uint16x8_t, vaddq_s32, vdup_n_s16, vdupq_n_s32, vget_high_u16,
        vget_low_u16, vld1_u8, vmaxq_s32, vminq_s32, vmovl_s16, vmovl_u8, vmulq_s32,
        vreinterpret_s16_u16, vshrq_n_s32, vst1q_s32, vsub_s16, vsubq_s32,
    };

    for row in 0..height {
        let y_row = &y_plane[row * y_stride..];
        let u_row = &u_plane[if subsampled { row / 2 } else { row } * u_stride..];
        let v_row = &v_plane[if subsampled { row / 2 } else { row } * v_stride..];
        let output_row = &mut destination[row * width * 4..];
        let mut column = 0;
        while column + 8 <= width {
            let mut y_bytes = [0_u8; 8];
            let mut u_bytes = [0_u8; 8];
            let mut v_bytes = [0_u8; 8];
            y_bytes.copy_from_slice(&y_row[column..column + 8]);
            if subsampled {
                for pair in 0..4 {
                    u_bytes[pair * 2] = u_row[column / 2 + pair];
                    u_bytes[pair * 2 + 1] = u_bytes[pair * 2];
                    v_bytes[pair * 2] = v_row[column / 2 + pair];
                    v_bytes[pair * 2 + 1] = v_bytes[pair * 2];
                }
            } else {
                u_bytes.copy_from_slice(&u_row[column..column + 8]);
                v_bytes.copy_from_slice(&v_row[column..column + 8]);
            }
            let y16: uint16x8_t = vmovl_u8(vld1_u8(y_bytes.as_ptr()));
            let u16: uint16x8_t = vmovl_u8(vld1_u8(u_bytes.as_ptr()));
            let v16: uint16x8_t = vmovl_u8(vld1_u8(v_bytes.as_ptr()));
            write_four(
                &mut output_row[column * 4..],
                process_four(
                    vreinterpret_s16_u16(vget_low_u16(y16)),
                    vreinterpret_s16_u16(vget_low_u16(u16)),
                    vreinterpret_s16_u16(vget_low_u16(v16)),
                ),
            );
            write_four(
                &mut output_row[(column + 4) * 4..],
                process_four(
                    vreinterpret_s16_u16(vget_high_u16(y16)),
                    vreinterpret_s16_u16(vget_high_u16(u16)),
                    vreinterpret_s16_u16(vget_high_u16(v16)),
                ),
            );
            column += 8;
        }
        if subsampled {
            super::yuv420_to_rgba_scalar(
                width - column,
                1,
                &y_row[column..],
                y_stride,
                &u_row[column / 2..],
                u_stride,
                &v_row[column / 2..],
                v_stride,
                &mut output_row[column * 4..],
            );
        } else {
            super::yuv444_to_rgba_scalar(
                width - column,
                1,
                &y_row[column..],
                y_stride,
                &u_row[column..],
                u_stride,
                &v_row[column..],
                v_stride,
                &mut output_row[column * 4..],
            );
        }
    }

    #[inline]
    unsafe fn process_four(
        y: int16x4_t,
        u: int16x4_t,
        v: int16x4_t,
    ) -> ([i32; 4], [i32; 4], [i32; 4]) {
        let y: int32x4_t = vmulq_s32(
            vmaxq_s32(vdupq_n_s32(0), vsubq_s32(vmovl_s16(y), vdupq_n_s32(16))),
            vdupq_n_s32(298),
        );
        let u = vmovl_s16(vsub_s16(u, vdup_n_s16(128)));
        let v = vmovl_s16(vsub_s16(v, vdup_n_s16(128)));
        let red = clamp(vshrq_n_s32::<8>(vaddq_s32(
            vaddq_s32(y, vmulq_s32(v, vdupq_n_s32(459))),
            vdupq_n_s32(128),
        )));
        let green = clamp(vshrq_n_s32::<8>(vaddq_s32(
            vaddq_s32(
                vaddq_s32(y, vmulq_s32(u, vdupq_n_s32(-55))),
                vmulq_s32(v, vdupq_n_s32(-136)),
            ),
            vdupq_n_s32(128),
        )));
        let blue = clamp(vshrq_n_s32::<8>(vaddq_s32(
            vaddq_s32(y, vmulq_s32(u, vdupq_n_s32(541))),
            vdupq_n_s32(128),
        )));
        (to_array(red), to_array(green), to_array(blue))
    }

    #[inline]
    unsafe fn clamp(value: int32x4_t) -> int32x4_t {
        vmaxq_s32(vdupq_n_s32(0), vminq_s32(vdupq_n_s32(255), value))
    }

    #[inline]
    unsafe fn to_array(value: int32x4_t) -> [i32; 4] {
        let mut output = [0_i32; 4];
        vst1q_s32(output.as_mut_ptr(), value);
        output
    }

    #[inline]
    fn write_four(destination: &mut [u8], rgb: ([i32; 4], [i32; 4], [i32; 4])) {
        for index in 0..4 {
            let offset = index * 4;
            destination[offset..offset + 4].copy_from_slice(&[
                rgb.0[index] as u8,
                rgb.1[index] as u8,
                rgb.2[index] as u8,
                0xff,
            ]);
        }
    }
}
