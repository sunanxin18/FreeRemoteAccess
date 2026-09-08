//! NSCodec 子码流：独立区域、严格长度校验、SSE2/NEON 颜色转换。
//!
//! 协议依据：MS-RDPNSC 2.2.2、2.2.2.1、3.1.8.1、3.1.8.4；
//! https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpnsc/949ee2fd-f35f-4cab-be49-e00567554177
//! 位移符号与填充规则对照 FreeRDP commit
//! 54a873e2710710841c6ec2b756df64285ec6e29a 的 libfreerdp/codec/nsc.c。
//! 本文件按协议独立实现；没有复制 FreeRDP C 源码或引入其运行时依赖。
//! alpha 平面必须合法解码；ClearCodec 接口返回 BGR24，由父层赋不透明 alpha。

use super::{Error, NsCodecProvider, Result};

/// 能力由构造器检查，支持目标不会静默执行标量颜色转换。
pub struct SimdNsCodec {
    max_pixels: usize,
}

impl SimdNsCodec {
    pub fn new(max_pixels: usize) -> Result<Self> {
        if !simd_available() {
            return Err(Error::NsCodecUnavailable);
        }
        Ok(Self { max_pixels })
    }
}

impl NsCodecProvider for SimdNsCodec {
    fn decode_bgr24(&self, data: &[u8], width: u16, height: u16) -> Result<Vec<u8>> {
        let width = usize::from(width);
        let height = usize::from(height);
        let pixels = width.checked_mul(height).ok_or(Error::ResourceLimit)?;
        if pixels == 0 {
            return Err(Error::Invalid("NSCodec empty dimensions"));
        }
        if pixels > self.max_pixels {
            return Err(Error::ResourceLimit);
        }
        let mut input = Cursor::new(data);
        let lengths = [input.u32()?, input.u32()?, input.u32()?, input.u32()?];
        if lengths[..3].contains(&0) {
            return Err(Error::Invalid("NSCodec missing color plane"));
        }
        let loss = input.byte()?;
        let subsampling = input.byte()?;
        // Reserved 字段按协议接收时忽略。
        input.take(2)?;
        if !(1..=7).contains(&loss) || subsampling > 1 {
            return Err(Error::Invalid("NSCodec color parameters"));
        }
        let total = lengths.iter().try_fold(0usize, |total, &length| {
            total
                .checked_add(usize::try_from(length).map_err(|_| Error::ResourceLimit)?)
                .ok_or(Error::ResourceLimit)
        })?;
        if total != input.remaining() {
            return Err(Error::Invalid("NSCodec plane extent"));
        }
        let padded_width = width.checked_add(7).ok_or(Error::ResourceLimit)? & !7;
        let padded_height = height.checked_add(1).ok_or(Error::ResourceLimit)? & !1;
        let stride = if subsampling == 1 {
            padded_width
        } else {
            width
        };
        let chroma_size = if subsampling == 1 {
            (padded_width / 2)
                .checked_mul(padded_height / 2)
                .ok_or(Error::ResourceLimit)?
        } else {
            pixels
        };
        let original = [
            stride.checked_mul(height).ok_or(Error::ResourceLimit)?,
            chroma_size,
            chroma_size,
            pixels,
        ];
        let mut planes = Vec::with_capacity(3);
        for (index, (&length, &size)) in lengths.iter().zip(&original).enumerate() {
            let length = usize::try_from(length).map_err(|_| Error::ResourceLimit)?;
            let plane = decode_plane(input.take(length)?, size)?;
            if index < 3 {
                planes.push(plane);
            }
            // alpha 已完整验证；该层不会把透明度带入 ClearCodec 桌面像素。
        }
        let mut output = allocate(pixels.checked_mul(3).ok_or(Error::ResourceLimit)?)?;
        for row in 0..height {
            let chroma_row = if subsampling == 1 {
                (row / 2) * (stride / 2)
            } else {
                row * stride
            };
            for x in (0..width).step_by(8) {
                let count = (width - x).min(8);
                let mut y = [0u8; 8];
                let mut co = [0u8; 8];
                let mut cg = [0u8; 8];
                y[..count].copy_from_slice(&planes[0][row * stride + x..row * stride + x + count]);
                let chroma_x = if subsampling == 1 { x / 2 } else { x };
                let chroma_count = if subsampling == 1 {
                    count.div_ceil(2)
                } else {
                    count
                };
                co[..chroma_count].copy_from_slice(
                    &planes[1][chroma_row + chroma_x..chroma_row + chroma_x + chroma_count],
                );
                cg[..chroma_count].copy_from_slice(
                    &planes[2][chroma_row + chroma_x..chroma_row + chroma_x + chroma_count],
                );
                let block = convert8(&y, &co, &cg, loss - 1, subsampling == 1)?;
                let offset = (row * width + x) * 3;
                output[offset..offset + count * 3].copy_from_slice(&block[..count * 3]);
            }
        }
        Ok(output)
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    offset: usize,
}
impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }
    fn remaining(&self) -> usize {
        self.data.len() - self.offset
    }
    fn take(&mut self, size: usize) -> Result<&'a [u8]> {
        let end = self.offset.checked_add(size).ok_or(Error::ResourceLimit)?;
        let bytes = self
            .data
            .get(self.offset..end)
            .ok_or(Error::Invalid("NSCodec truncated plane"))?;
        self.offset = end;
        Ok(bytes)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| Error::Invalid("NSCodec u32"))?,
        ))
    }
}

fn allocate(size: usize) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(size)
        .map_err(|_| Error::ResourceLimit)?;
    buffer.resize(size, 0);
    Ok(buffer)
}

fn decode_plane(encoded: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut output = allocate(size)?;
    if encoded.is_empty() {
        bulk_fill(&mut output, 255)?;
    } else if encoded.len() == size {
        bulk_copy(encoded, &mut output)?;
    } else if encoded.len() > size || size < 4 {
        return Err(Error::Invalid("NSCodec oversized plane"));
    } else {
        let mut cursor = Cursor::new(encoded);
        let mut position = 0;
        // RLE 不得吞并最后四个原始字节，独立 plane 切片防止跨平面借字节。
        while position < size - 4 {
            let value = cursor.byte()?;
            if position == size - 5 || cursor.data.get(cursor.offset) != Some(&value) {
                output[position] = value;
                position += 1;
            } else {
                cursor.byte()?;
                let marker = cursor.byte()?;
                let count = if marker == 255 {
                    usize::try_from(cursor.u32()?).map_err(|_| Error::ResourceLimit)?
                } else {
                    usize::from(marker) + 2
                };
                if count < 2 || count > size - 4 - position {
                    return Err(Error::Invalid("NSCodec run extent"));
                }
                bulk_fill(&mut output[position..position + count], value)?;
                position += count;
            }
        }
        output[position..].copy_from_slice(cursor.take(4)?);
        if cursor.remaining() != 0 {
            return Err(Error::Invalid("NSCodec trailing RLE bytes"));
        }
    }
    Ok(output)
}

// 像素/RLE 展开与语法解析分离；批量内存操作显式使用本架构 SIMD。
fn bulk_fill(dst: &mut [u8], value: u8) -> Result<()> {
    if !simd_available() {
        return Err(Error::NsCodecUnavailable);
    }
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    unsafe {
        x86::fill(dst, value);
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        arm::fill(dst, value);
    }
    Ok(())
}
fn bulk_copy(src: &[u8], dst: &mut [u8]) -> Result<()> {
    if src.len() != dst.len() {
        return Err(Error::Invalid("NSCodec copy extent"));
    }
    if !simd_available() {
        return Err(Error::NsCodecUnavailable);
    }
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    unsafe {
        x86::copy(src, dst);
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        arm::copy(src, dst);
    }
    Ok(())
}

fn simd_available() -> bool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        return std::is_x86_feature_detected!("sse2");
    }
    #[cfg(target_arch = "aarch64")]
    {
        return std::arch::is_aarch64_feature_detected!("neon");
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

fn convert8(
    y: &[u8; 8],
    co: &[u8; 8],
    cg: &[u8; 8],
    shift: u8,
    subsampled: bool,
) -> Result<[u8; 24]> {
    if !simd_available() {
        return Err(Error::NsCodecUnavailable);
    }
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    // SAFETY: 上方检查 SSE2，所有输入完整八字节，输出固定二十四字节。
    {
        return Ok(unsafe { x86::convert(y, co, cg, shift, subsampled) });
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: 上方检查 NEON，输入输出固定尺寸；移位由已验证的 colorLossLevel 导出。
    {
        return Ok(unsafe { arm::convert(y, co, cg, shift, subsampled) });
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = (y, co, cg, shift, subsampled);
        Err(Error::NsCodecUnavailable)
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use std::arch::aarch64::*;
    #[target_feature(enable = "neon")]
    pub unsafe fn fill(dst: &mut [u8], value: u8) {
        unsafe {
            let end = dst.len() / 16 * 16;
            let v = vdupq_n_u8(value);
            for offset in (0..end).step_by(16) {
                vst1q_u8(dst.as_mut_ptr().add(offset), v);
            }
            dst[end..].fill(value);
        }
    }
    #[target_feature(enable = "neon")]
    pub unsafe fn copy(src: &[u8], dst: &mut [u8]) {
        unsafe {
            let end = dst.len() / 16 * 16;
            for offset in (0..end).step_by(16) {
                vst1q_u8(
                    dst.as_mut_ptr().add(offset),
                    vld1q_u8(src.as_ptr().add(offset)),
                );
            }
            dst[end..].copy_from_slice(&src[end..]);
        }
    }

    #[target_feature(enable = "neon")]
    pub unsafe fn convert(
        y: &[u8; 8],
        co: &[u8; 8],
        cg: &[u8; 8],
        shift: u8,
        subsampled: bool,
    ) -> [u8; 24] {
        // SAFETY: 调用方保证运行时特性，load/store 正好对应固定数组。
        unsafe {
            let y = vreinterpretq_s16_u16(vmovl_u8(vld1_u8(y.as_ptr())));
            let mut co = vld1_u8(co.as_ptr());
            let mut cg = vld1_u8(cg.as_ptr());
            if subsampled {
                co = vzip1_u8(co, co);
                cg = vzip1_u8(cg, cg);
            }
            // 先在八位域恢复低位再符号扩展，不能对原字节先符号扩展后保留高位。
            let shifts = vdup_n_s8(shift as i8);
            let co = vmovl_s8(vreinterpret_s8_u8(vshl_u8(co, shifts)));
            let cg = vmovl_s8(vreinterpret_s8_u8(vshl_u8(cg, shifts)));
            let b = vqmovun_s16(vsubq_s16(vsubq_s16(y, co), cg));
            let g = vqmovun_s16(vaddq_s16(y, cg));
            let r = vqmovun_s16(vsubq_s16(vaddq_s16(y, co), cg));
            let mut result = [0u8; 24];
            vst3_u8(result.as_mut_ptr(), uint8x8x3_t(b, g, r));
            result
        }
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod x86 {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;
    #[target_feature(enable = "sse2")]
    pub unsafe fn fill(dst: &mut [u8], value: u8) {
        unsafe {
            let end = dst.len() / 16 * 16;
            let v = _mm_set1_epi8(value as i8);
            for offset in (0..end).step_by(16) {
                _mm_storeu_si128(dst.as_mut_ptr().add(offset).cast(), v);
            }
            dst[end..].fill(value);
        }
    }
    #[target_feature(enable = "sse2")]
    pub unsafe fn copy(src: &[u8], dst: &mut [u8]) {
        unsafe {
            let end = dst.len() / 16 * 16;
            for offset in (0..end).step_by(16) {
                _mm_storeu_si128(
                    dst.as_mut_ptr().add(offset).cast(),
                    _mm_loadu_si128(src.as_ptr().add(offset).cast()),
                );
            }
            dst[end..].copy_from_slice(&src[end..]);
        }
    }

    #[target_feature(enable = "sse2")]
    unsafe fn packed12(bgra: __m128i, dst: *mut u8) {
        unsafe {
            let a = _mm_and_si128(bgra, _mm_set_epi32(0, 0, 0, 0x00ffffff));
            let b = _mm_and_si128(
                _mm_srli_si128::<1>(bgra),
                _mm_set_epi32(0, 0, 0x0000ffff, -16777216),
            );
            let c = _mm_and_si128(
                _mm_srli_si128::<2>(bgra),
                _mm_set_epi32(0, 0x000000ff, -65536, 0),
            );
            let d = _mm_and_si128(_mm_srli_si128::<3>(bgra), _mm_set_epi32(0, -256, 0, 0));
            let result = _mm_or_si128(_mm_or_si128(a, b), _mm_or_si128(c, d));
            _mm_storel_epi64(dst.cast(), result);
            std::ptr::write_unaligned(
                dst.add(8).cast::<i32>(),
                _mm_cvtsi128_si32(_mm_srli_si128::<8>(result)),
            );
        }
    }

    #[target_feature(enable = "sse2")]
    pub unsafe fn convert(
        y: &[u8; 8],
        co: &[u8; 8],
        cg: &[u8; 8],
        shift: u8,
        subsampled: bool,
    ) -> [u8; 24] {
        unsafe {
            let zero = _mm_setzero_si128();
            let y = _mm_unpacklo_epi8(_mm_loadl_epi64(y.as_ptr().cast()), zero);
            let mut co = _mm_loadl_epi64(co.as_ptr().cast());
            let mut cg = _mm_loadl_epi64(cg.as_ptr().cast());
            if subsampled {
                co = _mm_unpacklo_epi8(co, co);
                cg = _mm_unpacklo_epi8(cg, cg);
            }
            let shifts = _mm_cvtsi32_si128(i32::from(shift) + 8);
            let co = _mm_srai_epi16::<8>(_mm_sll_epi16(_mm_unpacklo_epi8(co, zero), shifts));
            let cg = _mm_srai_epi16::<8>(_mm_sll_epi16(_mm_unpacklo_epi8(cg, zero), shifts));
            let b = _mm_packus_epi16(_mm_sub_epi16(_mm_sub_epi16(y, co), cg), zero);
            let g = _mm_packus_epi16(_mm_add_epi16(y, cg), zero);
            let r = _mm_packus_epi16(_mm_sub_epi16(_mm_add_epi16(y, co), cg), zero);
            let bg = _mm_unpacklo_epi8(b, g);
            let ra = _mm_unpacklo_epi8(r, zero);
            let mut result = [0u8; 24];
            packed12(_mm_unpacklo_epi16(bg, ra), result.as_mut_ptr());
            packed12(_mm_unpackhi_epi16(bg, ra), result.as_mut_ptr().add(12));
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(planes: &[Vec<u8>; 4], loss: u8, subsample: u8) -> Vec<u8> {
        let mut data = Vec::new();
        for plane in planes {
            data.extend_from_slice(&u32::try_from(plane.len()).unwrap().to_le_bytes());
        }
        data.extend_from_slice(&[loss, subsample, 0, 0]);
        for plane in planes {
            data.extend_from_slice(plane);
        }
        data
    }

    // 独立标量数学 oracle，仅在测试编译；颜色输出不经过生产内核。
    fn reference(y: u8, co: u8, cg: u8, shift: u8) -> [u8; 3] {
        let y = i32::from(y);
        let signed = |v: u8| {
            let bits = (u32::from(v) << shift) & 255;
            if bits >= 128 {
                bits as i32 - 256
            } else {
                bits as i32
            }
        };
        let co = signed(co);
        let cg = signed(cg);
        [
            (y - co - cg).clamp(0, 255) as u8,
            (y + cg).clamp(0, 255) as u8,
            (y + co - cg).clamp(0, 255) as u8,
        ]
    }

    #[test]
    fn simd_matches_oracle_all_chroma_values_and_losses() {
        if !simd_available() {
            return;
        }
        for shift in 0..=6 {
            for start in (0u16..256).step_by(8) {
                let y = std::array::from_fn(|i| [0, 1, 63, 127, 128, 192, 254, 255][i]);
                let co = std::array::from_fn(|i| (start + i as u16) as u8);
                let cg = std::array::from_fn(|i| 255 - co[i]);
                for subsample in [false, true] {
                    let actual = convert8(&y, &co, &cg, shift, subsample).unwrap();
                    for i in 0..8 {
                        let c = if subsample { i / 2 } else { i };
                        assert_eq!(
                            &actual[i * 3..i * 3 + 3],
                            &reference(y[i], co[c], cg[c], shift)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn raw_odd_dimensions_padding_subsampling_alpha_and_tail() {
        let decoder = SimdNsCodec::new(10000).unwrap();
        for width in [1usize, 3, 7, 8, 9, 17] {
            for height in [1usize, 2, 3, 5] {
                for subsample in [0, 1] {
                    let stride = if subsample == 1 {
                        (width + 7) & !7
                    } else {
                        width
                    };
                    let chroma = if subsample == 1 {
                        stride / 2 * height.div_ceil(2)
                    } else {
                        width * height
                    };
                    let planes = [
                        (0..stride * height).map(|i| (i * 17) as u8).collect(),
                        (0..chroma).map(|i| (i * 7) as u8).collect(),
                        (0..chroma).map(|i| (i * 19) as u8).collect(),
                        (0..width * height).map(|i| i as u8).collect(),
                    ];
                    for loss in 1..=7 {
                        let actual = decoder
                            .decode_bgr24(
                                &fixture(&planes, loss, subsample),
                                width as u16,
                                height as u16,
                            )
                            .unwrap();
                        let mut expected = Vec::new();
                        for row in 0..height {
                            for x in 0..width {
                                let c = if subsample == 1 {
                                    row / 2 * (stride / 2) + x / 2
                                } else {
                                    row * stride + x
                                };
                                expected.extend_from_slice(&reference(
                                    planes[0][row * stride + x],
                                    planes[1][c],
                                    planes[2][c],
                                    loss - 1,
                                ));
                            }
                        }
                        assert_eq!(actual, expected);
                        let mut absent_alpha = planes.clone();
                        absent_alpha[3].clear();
                        assert_eq!(
                            decoder
                                .decode_bgr24(
                                    &fixture(&absent_alpha, loss, subsample),
                                    width as u16,
                                    height as u16
                                )
                                .unwrap(),
                            expected
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn rle_short_long_and_exact_four_byte_tail() {
        assert_eq!(
            decode_plane(&[42, 42, 6, 1, 2, 3, 4], 12).unwrap(),
            [vec![42; 8], vec![1, 2, 3, 4]].concat()
        );
        let mut long = vec![99, 99, 255];
        long.extend_from_slice(&300u32.to_le_bytes());
        long.extend_from_slice(&[9, 8, 7, 6]);
        assert_eq!(
            decode_plane(&long, 304).unwrap(),
            [vec![99; 300], vec![9, 8, 7, 6]].concat()
        );
        // 剩五字节时第一个字节是literal，即使与EndData首字节相同。
        assert_eq!(
            decode_plane(&[8, 8, 5, 7, 7, 1, 2, 3], 12).unwrap(),
            [vec![8; 7], vec![7, 7, 1, 2, 3]].concat()
        );
        assert!(decode_plane(&[42, 42, 10, 1, 2, 3, 4], 12).is_err());
        assert!(decode_plane(&[42, 42, 255, 0, 0, 0, 0, 1, 2, 3, 4], 100).is_err());
        assert!(decode_plane(&[42, 42, 6, 1, 2, 3, 4, 5], 12).is_err());
    }

    #[test]
    fn compressed_stream_is_transactional_and_rejects_every_truncation() {
        let decoder = SimdNsCodec::new(10000).unwrap();
        let plane = vec![32, 32, 6, 32, 32, 32, 32];
        let data = fixture(&[plane.clone(), plane.clone(), plane.clone(), plane], 1, 0);
        let expected = reference(32, 32, 32, 0).repeat(12);
        assert_eq!(decoder.decode_bgr24(&data, 12, 1).unwrap(), expected);
        for end in 0..data.len() {
            assert!(
                decoder.decode_bgr24(&data[..end], 12, 1).is_err(),
                "truncation {end}"
            );
        }
        let mut trailing = data.clone();
        trailing.push(0);
        assert!(decoder.decode_bgr24(&trailing, 12, 1).is_err());
        // alpha 不进入BGR输出，但非法alpha仍必须导致整个区域失败。
        let mut bad_alpha = data.clone();
        let end = bad_alpha.len();
        bad_alpha[end - 5] = 100;
        assert!(decoder.decode_bgr24(&bad_alpha, 12, 1).is_err());
        assert_eq!(decoder.decode_bgr24(&data, 12, 1).unwrap(), expected);
    }

    #[test]
    fn invalid_parameters_dimensions_and_limits_are_explicit() {
        let decoder = SimdNsCodec::new(100).unwrap();
        let planes = [vec![0; 8], vec![0; 8], vec![0; 8], vec![]];
        for loss in [0, 8, 255] {
            assert!(decoder
                .decode_bgr24(&fixture(&planes, loss, 0), 8, 1)
                .is_err());
        }
        assert!(decoder.decode_bgr24(&fixture(&planes, 1, 2), 8, 1).is_err());
        assert!(decoder.decode_bgr24(&fixture(&planes, 1, 0), 0, 1).is_err());
        assert_eq!(
            decoder.decode_bgr24(&fixture(&planes, 1, 0), 101, 1),
            Err(Error::ResourceLimit)
        );
        let mut missing = planes;
        missing[1].clear();
        assert!(decoder
            .decode_bgr24(&fixture(&missing, 1, 0), 8, 1)
            .is_err());
    }
    fn microsoft_example() -> (Vec<u8>, Vec<u8>) {
        // MS-RDPNSC v20240423 §4：官方 15×10 压缩示例及完整 BGRA 期望值。
        fn hex(s: &str) -> Vec<u8> {
            s.split_whitespace()
                .map(|v| u8::from_str_radix(v, 16).unwrap())
                .collect()
        }
        let compressed = hex("71 00 00 00 07 00 00 00 0b 00 00 00 07 00 00 00 03 01 00 00 63 63 01 64 64 00 63 63 02 64 64 00 63 63 00 64 64 01 63 63 01 64 64 01 63 63 01 64 64 00 63 63 00 64 64 01 63 63 00 64 64 0c 63 63 00 64 64 0c 63 63 00 64 64 0c 63 63 00 64 64 0c 63 64 64 04 63 64 63 63 00 64 64 03 63 64 64 03 63 63 00 64 63 63 00 64 64 03 65 63 64 64 01 63 64 64 00 65 64 64 06 63 64 64 00 63 63 00 64 64 04 64 65 65 65 22 22 22 22 22 22 22 37 37 19 36 37 37 06 37 37 37 37 ff ff 90 ff ff ff ff");
        let bgra = hex("ff 3f 0f ff ff 3f 0f ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 3f 0f ff ff 3f 0f ff ff 3f 0f ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 3f 0f ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 3f 0f ff ff 3f 0f ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 3f 0f ff ff 3f 0f ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 3f 0f ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 3f 0f ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 3f 0f ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 3f 0f ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 3f 0f ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 3c 14 ff ff 3b 13 ff ff 40 10 ff ff 3f 0f ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 3b 13 ff ff 3b 13 ff ff 40 10 ff ff 3f 0f ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 41 11 ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 41 11 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 3f 0f ff ff 3f 0f ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 40 10 ff ff 41 11 ff ff 41 11 ff");
        let expected: Vec<u8> = bgra
            .chunks_exact(4)
            .flat_map(|p| p[..3].iter().copied())
            .collect();
        (compressed, expected)
    }

    #[test]
    fn microsoft_section_four_example_matches_published_pixels() {
        let (compressed, expected) = microsoft_example();
        let decoder = SimdNsCodec::new(150).unwrap();
        assert_eq!(decoder.decode_bgr24(&compressed, 15, 10).unwrap(), expected);
    }
    #[test]
    #[ignore = "有界性能采样，使用 --release --ignored --nocapture 单独执行"]
    fn bounded_decode_benchmark_nscodec() {
        use std::{hint::black_box, time::Instant};
        assert!(!cfg!(debug_assertions), "benchmark 必须使用 --release");
        let (compressed, expected) = microsoft_example();
        let decoder = SimdNsCodec::new(150).unwrap();
        assert_eq!(decoder.decode_bgr24(&compressed, 15, 10).unwrap(), expected);
        for _ in 0..32 {
            black_box(
                decoder
                    .decode_bgr24(black_box(&compressed), 15, 10)
                    .unwrap(),
            );
        }
        let iterations = 100000;
        let start = Instant::now();
        for _ in 0..iterations {
            black_box(
                decoder
                    .decode_bgr24(black_box(&compressed), 15, 10)
                    .unwrap(),
            );
        }
        let elapsed = start.elapsed();
        println!("FRD_BENCH arch={} backend={} kernel=nscodec-ms-example-15x10 iterations={} elapsed_ns={} ns_decode={:.3} allocations=included",
            std::env::consts::ARCH, if cfg!(target_arch = "aarch64") { "neon" } else { "sse2" },
            iterations, elapsed.as_nanos(), elapsed.as_nanos() as f64 / iterations as f64);
    }
}
