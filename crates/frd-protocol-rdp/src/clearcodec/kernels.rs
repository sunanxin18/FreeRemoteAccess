//! ClearCodec 像素叶内核。构造失败明确表示当前 CPU 不支持生产解码。
//! 离散列写没有 SSE/NEON scatter 指令：每次加载四像素，再用四条 lane store。
use super::PixelKernel;

pub struct NativeKernel {
    _private: (),
}

pub fn native_kernel() -> Option<NativeKernel> {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if std::is_x86_feature_detected!("sse2") && std::is_x86_feature_detected!("ssse3") {
        return Some(NativeKernel { _private: () });
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        return Some(NativeKernel { _private: () });
    }
    None
}

impl PixelKernel for NativeKernel {
    fn fill_bgra(&self, dst: &mut [u8], color: [u8; 4]) {
        assert_eq!(dst.len() % 4, 0);
        #[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
        unsafe {
            arch::fill(dst, color)
        }
    }
    fn expand_bgr24(&self, src: &[u8], dst: &mut [u8]) {
        assert_eq!(src.len() % 3, 0);
        assert_eq!(dst.len() / 4, src.len() / 3);
        assert_eq!(dst.len() % 4, 0);
        #[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
        unsafe {
            arch::expand(src, dst)
        }
    }
    fn copy_bgra(&self, src: &[u8], dst: &mut [u8]) {
        assert_eq!(src.len(), dst.len());
        assert_eq!(src.len() % 4, 0);
        #[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
        unsafe {
            arch::copy(src, dst)
        }
    }
    fn scatter_column(&self, src: &[u8], dst: &mut [u8], stride: usize) {
        assert_eq!(src.len() % 4, 0);
        assert!(stride >= 4);
        let n = src.len() / 4;
        let needed = if n == 0 {
            0
        } else {
            (n - 1)
                .checked_mul(stride)
                .and_then(|v| v.checked_add(4))
                .expect("列范围溢出")
        };
        assert_eq!(dst.len(), needed);
        #[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
        unsafe {
            arch::scatter(src, dst, stride)
        }
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod arch {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;
    #[target_feature(enable = "sse2")]
    pub unsafe fn fill(dst: &mut [u8], c: [u8; 4]) {
        unsafe {
            let v = _mm_set1_epi32(i32::from_le_bytes(c));
            let mut i = 0;
            while i + 16 <= dst.len() {
                _mm_storeu_si128(dst.as_mut_ptr().add(i).cast(), v);
                i += 16;
            }
            for p in dst[i..].chunks_exact_mut(4) {
                p.copy_from_slice(&c);
            }
        }
    }
    #[target_feature(enable = "ssse3")]
    pub unsafe fn expand(src: &[u8], dst: &mut [u8]) {
        unsafe {
            let mask = _mm_setr_epi8(0, 1, 2, -128, 3, 4, 5, -128, 6, 7, 8, -128, 9, 10, 11, -128);
            let alpha = _mm_set1_epi32(0xff000000u32 as i32);
            let mut s = 0;
            let mut d = 0;
            // 16 字节加载需额外4字节余量；最后不足部分走有界尾部。
            while s + 16 <= src.len() {
                let v = _mm_loadu_si128(src.as_ptr().add(s).cast());
                _mm_storeu_si128(
                    dst.as_mut_ptr().add(d).cast(),
                    _mm_or_si128(_mm_shuffle_epi8(v, mask), alpha),
                );
                s += 12;
                d += 16;
            }
            for (p, q) in src[s..].chunks_exact(3).zip(dst[d..].chunks_exact_mut(4)) {
                q.copy_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
    }
    #[target_feature(enable = "sse2")]
    pub unsafe fn copy(src: &[u8], dst: &mut [u8]) {
        unsafe {
            let mut i = 0;
            while i + 16 <= src.len() {
                let v = _mm_loadu_si128(src.as_ptr().add(i).cast());
                _mm_storeu_si128(dst.as_mut_ptr().add(i).cast(), v);
                i += 16;
            }
            dst[i..].copy_from_slice(&src[i..]);
        }
    }
    #[target_feature(enable = "sse2")]
    pub unsafe fn scatter(src: &[u8], dst: &mut [u8], stride: usize) {
        unsafe {
            let mut n = 0;
            while n + 4 <= src.len() / 4 {
                let v = _mm_castsi128_ps(_mm_loadu_si128(src.as_ptr().add(n * 4).cast()));
                _mm_store_ss(dst.as_mut_ptr().add(n * stride).cast(), v);
                _mm_store_ss(
                    dst.as_mut_ptr().add((n + 1) * stride).cast(),
                    _mm_shuffle_ps::<0x55>(v, v),
                );
                _mm_store_ss(
                    dst.as_mut_ptr().add((n + 2) * stride).cast(),
                    _mm_shuffle_ps::<0xaa>(v, v),
                );
                _mm_store_ss(
                    dst.as_mut_ptr().add((n + 3) * stride).cast(),
                    _mm_shuffle_ps::<0xff>(v, v),
                );
                n += 4;
            }
            while n < src.len() / 4 {
                dst[n * stride..n * stride + 4].copy_from_slice(&src[n * 4..n * 4 + 4]);
                n += 1;
            }
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    use std::arch::aarch64::*;
    #[target_feature(enable = "neon")]
    pub unsafe fn fill(dst: &mut [u8], c: [u8; 4]) {
        unsafe {
            let v = vreinterpretq_u8_u32(vdupq_n_u32(u32::from_le_bytes(c)));
            let mut i = 0;
            while i + 16 <= dst.len() {
                vst1q_u8(dst.as_mut_ptr().add(i), v);
                i += 16;
            }
            for p in dst[i..].chunks_exact_mut(4) {
                p.copy_from_slice(&c);
            }
        }
    }
    #[target_feature(enable = "neon")]
    pub unsafe fn expand(src: &[u8], dst: &mut [u8]) {
        unsafe {
            let mut s = 0;
            let mut d = 0;
            while s + 48 <= src.len() {
                let v = vld3q_u8(src.as_ptr().add(s));
                vst4q_u8(
                    dst.as_mut_ptr().add(d),
                    uint8x16x4_t(v.0, v.1, v.2, vdupq_n_u8(255)),
                );
                s += 48;
                d += 64;
            }
            for (p, q) in src[s..].chunks_exact(3).zip(dst[d..].chunks_exact_mut(4)) {
                q.copy_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
    }
    #[target_feature(enable = "neon")]
    pub unsafe fn copy(src: &[u8], dst: &mut [u8]) {
        unsafe {
            let mut i = 0;
            while i + 16 <= src.len() {
                vst1q_u8(dst.as_mut_ptr().add(i), vld1q_u8(src.as_ptr().add(i)));
                i += 16;
            }
            dst[i..].copy_from_slice(&src[i..]);
        }
    }
    #[target_feature(enable = "neon")]
    pub unsafe fn scatter(src: &[u8], dst: &mut [u8], stride: usize) {
        unsafe {
            let mut n = 0;
            while n + 4 <= src.len() / 4 {
                let v = vreinterpretq_u32_u8(vld1q_u8(src.as_ptr().add(n * 4)));
                vst1q_lane_u32::<0>(dst.as_mut_ptr().add(n * stride).cast(), v);
                vst1q_lane_u32::<1>(dst.as_mut_ptr().add((n + 1) * stride).cast(), v);
                vst1q_lane_u32::<2>(dst.as_mut_ptr().add((n + 2) * stride).cast(), v);
                vst1q_lane_u32::<3>(dst.as_mut_ptr().add((n + 3) * stride).cast(), v);
                n += 4;
            }
            while n < src.len() / 4 {
                dst[n * stride..n * stride + 4].copy_from_slice(&src[n * 4..n * 4 + 4]);
                n += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_kernels_match_portable_oracle_with_unaligned_buffers() {
        let Some(k) = native_kernel() else {
            return;
        };
        for n in 0..70 {
            let src: Vec<u8> = (0..n * 4 + 1).map(|i| (i * 71) as u8).collect();
            let src = &src[1..];
            let mut dst = vec![13; n * 4 + 2];
            k.copy_bgra(src, &mut dst[1..n * 4 + 1]);
            assert_eq!(&dst[1..n * 4 + 1], src);
            k.fill_bgra(&mut dst[1..n * 4 + 1], [3, 5, 7, 11]);
            assert!(dst[1..n * 4 + 1]
                .chunks_exact(4)
                .all(|v| v == [3, 5, 7, 11]));
            let bgr = &src[..n * 3];
            k.expand_bgr24(bgr, &mut dst[1..n * 4 + 1]);
            let expected: Vec<_> = bgr
                .chunks_exact(3)
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect();
            assert_eq!(&dst[1..n * 4 + 1], expected);
            assert_eq!((dst[0], dst[n * 4 + 1]), (13, 13));
            for stride in [4, 8, 28, 260] {
                let len = if n == 0 { 0 } else { (n - 1) * stride + 4 };
                let mut out = vec![19; len + 2];
                let mut reference = out.clone();
                for i in 0..n {
                    reference[1 + i * stride..1 + i * stride + 4]
                        .copy_from_slice(&src[i * 4..i * 4 + 4]);
                }
                k.scatter_column(src, &mut out[1..len + 1], stride);
                assert_eq!(out, reference);
            }
        }
    }
}
