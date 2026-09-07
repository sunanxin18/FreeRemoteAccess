pub(super) fn sse41_available() -> bool {
    is_x86_feature_detected!("sse4.1")
}

#[cfg(target_arch = "x86")]
#[target_feature(enable = "sse4.1")]
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
    kernel(
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

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
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
    kernel(
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

#[cfg(target_arch = "x86")]
#[target_feature(enable = "sse4.1")]
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
    kernel(
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

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
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
    kernel(
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

#[target_feature(enable = "sse4.1")]
unsafe fn kernel(
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
    #[cfg(target_arch = "x86")]
    use std::arch::x86::{
        __m128i, _mm_add_epi32, _mm_cvtepu8_epi16, _mm_loadl_epi64, _mm_madd_epi16, _mm_max_epi16,
        _mm_max_epi32, _mm_min_epi32, _mm_set1_epi16, _mm_set1_epi32, _mm_setzero_si128,
        _mm_srai_epi16, _mm_srai_epi32, _mm_srli_si128, _mm_storeu_si128, _mm_sub_epi16,
        _mm_unpacklo_epi16,
    };
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::{
        __m128i, _mm_add_epi32, _mm_cvtepu8_epi16, _mm_loadl_epi64, _mm_madd_epi16, _mm_max_epi16,
        _mm_max_epi32, _mm_min_epi32, _mm_set1_epi16, _mm_set1_epi32, _mm_setzero_si128,
        _mm_srai_epi16, _mm_srai_epi32, _mm_srli_si128, _mm_storeu_si128, _mm_sub_epi16,
        _mm_unpacklo_epi16,
    };

    let zero = _mm_setzero_si128();
    let y_coeff = _mm_set1_epi16(298);
    let r_v_coeff = _mm_set1_epi16(459);
    let g_u_coeff = _mm_set1_epi16(-55);
    let g_v_coeff = _mm_set1_epi16(-136);
    let b_u_coeff = _mm_set1_epi16(541);
    let clamp_min = _mm_set1_epi32(0);
    let clamp_max = _mm_set1_epi32(255);
    let offset = _mm_set1_epi32(128);
    let shift = |value: __m128i| _mm_srai_epi32(_mm_add_epi32(value, offset), 8);

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
            let y16 = _mm_cvtepu8_epi16(_mm_loadl_epi64(y_bytes.as_ptr().cast()));
            let u16 = _mm_cvtepu8_epi16(_mm_loadl_epi64(u_bytes.as_ptr().cast()));
            let v16 = _mm_cvtepu8_epi16(_mm_loadl_epi64(v_bytes.as_ptr().cast()));
            write_four(
                &mut output_row[column * 4..],
                process_four(
                    y16, u16, v16, zero, y_coeff, r_v_coeff, g_u_coeff, g_v_coeff, b_u_coeff,
                    clamp_min, clamp_max, shift,
                ),
            );
            write_four(
                &mut output_row[(column + 4) * 4..],
                process_four(
                    _mm_srli_si128(y16, 8),
                    _mm_srli_si128(u16, 8),
                    _mm_srli_si128(v16, 8),
                    zero,
                    y_coeff,
                    r_v_coeff,
                    g_u_coeff,
                    g_v_coeff,
                    b_u_coeff,
                    clamp_min,
                    clamp_max,
                    shift,
                ),
            );
            column += 8;
        }
        let tail_len = width - column;
        if tail_len != 0 {
            // Stage up to eight remaining pixels into a vector-sized sample
            // block. Arithmetic and clamping stay in process_four; only the
            // final packed bytes for the live pixels are copied out.
            let mut y_bytes = [0_u8; 8];
            let mut u_bytes = [0_u8; 8];
            let mut v_bytes = [0_u8; 8];
            y_bytes[..tail_len].copy_from_slice(&y_row[column..column + tail_len]);
            for index in 0..tail_len {
                let sample = if subsampled {
                    (column + index) / 2
                } else {
                    column + index
                };
                u_bytes[index] = u_row[sample];
                v_bytes[index] = v_row[sample];
            }
            let y16 = _mm_cvtepu8_epi16(_mm_loadl_epi64(y_bytes.as_ptr().cast()));
            let u16 = _mm_cvtepu8_epi16(_mm_loadl_epi64(u_bytes.as_ptr().cast()));
            let v16 = _mm_cvtepu8_epi16(_mm_loadl_epi64(v_bytes.as_ptr().cast()));
            let mut packed = [0_u8; 32];
            write_four(
                &mut packed[..16],
                process_four(
                    y16, u16, v16, zero, y_coeff, r_v_coeff, g_u_coeff, g_v_coeff, b_u_coeff,
                    clamp_min, clamp_max, shift,
                ),
            );
            if tail_len > 4 {
                write_four(
                    &mut packed[16..],
                    process_four(
                        _mm_srli_si128(y16, 8),
                        _mm_srli_si128(u16, 8),
                        _mm_srli_si128(v16, 8),
                        zero,
                        y_coeff,
                        r_v_coeff,
                        g_u_coeff,
                        g_v_coeff,
                        b_u_coeff,
                        clamp_min,
                        clamp_max,
                        shift,
                    ),
                );
            }
            output_row[column * 4..(column + tail_len) * 4]
                .copy_from_slice(&packed[..tail_len * 4]);
        }
    }

    #[inline]
    #[target_feature(enable = "sse4.1")]
    unsafe fn process_four(
        y16: __m128i,
        u16: __m128i,
        v16: __m128i,
        zero: __m128i,
        y_coeff: __m128i,
        r_v_coeff: __m128i,
        g_u_coeff: __m128i,
        g_v_coeff: __m128i,
        b_u_coeff: __m128i,
        clamp_min: __m128i,
        clamp_max: __m128i,
        shift: impl Fn(__m128i) -> __m128i,
    ) -> ([i32; 4], [i32; 4], [i32; 4]) {
        let y_center = _mm_max_epi16(_mm_setzero_si128(), _mm_sub_epi16(y16, _mm_set1_epi16(16)));
        let u_center = _mm_sub_epi16(u16, _mm_set1_epi16(128));
        let v_center = _mm_sub_epi16(v16, _mm_set1_epi16(128));
        let y_pairs = _mm_unpacklo_epi16(y_center, zero);
        let u_sign = _mm_srai_epi16(u_center, 15);
        let v_sign = _mm_srai_epi16(v_center, 15);
        let u_pairs = _mm_unpacklo_epi16(u_center, u_sign);
        let v_pairs = _mm_unpacklo_epi16(v_center, v_sign);
        let y_term = _mm_madd_epi16(y_pairs, y_coeff);
        let red = _mm_max_epi32(
            clamp_min,
            _mm_min_epi32(
                clamp_max,
                shift(_mm_add_epi32(y_term, _mm_madd_epi16(v_pairs, r_v_coeff))),
            ),
        );
        let green = _mm_max_epi32(
            clamp_min,
            _mm_min_epi32(
                clamp_max,
                shift(_mm_add_epi32(
                    _mm_add_epi32(y_term, _mm_madd_epi16(u_pairs, g_u_coeff)),
                    _mm_madd_epi16(v_pairs, g_v_coeff),
                )),
            ),
        );
        let blue = _mm_max_epi32(
            clamp_min,
            _mm_min_epi32(
                clamp_max,
                shift(_mm_add_epi32(y_term, _mm_madd_epi16(u_pairs, b_u_coeff))),
            ),
        );
        let mut red_out = [0_i32; 4];
        let mut green_out = [0_i32; 4];
        let mut blue_out = [0_i32; 4];
        _mm_storeu_si128(red_out.as_mut_ptr().cast(), red);
        _mm_storeu_si128(green_out.as_mut_ptr().cast(), green);
        _mm_storeu_si128(blue_out.as_mut_ptr().cast(), blue);
        (red_out, green_out, blue_out)
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
