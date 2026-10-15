//! MVS 私有颜色与像素转换；默认保留标量，原生后端不做运行期回退。
//!
//! u8 完整输入域：Cb/Cr 偏移为 [-128,127]；带偏置的三个分子绝对值
//! 均小于 15_000_000。右移后加 Y 的 R/G/B 范围分别为
//! [-179,433]、[-134,390]、[-227,480]，故有符号 i32 完整保留数学结果。
//! 所有行布局在发布前由既有调用者校验。短尾只复制到固定栈 scratch，
//! 使用相同 NEON 内核，不做标量颜色或打包变换，也不在提交后检测 CPU。

#[cfg(all(
    feature = "mvs-native-colors-arm64",
    not(all(
        target_arch = "aarch64",
        target_endian = "little",
        target_feature = "neon"
    ))
))]
compile_error!("mvs-native-colors-arm64 仅支持小端 ARM64 且编译基线启用 NEON，禁止标量回退");

#[cfg(all(feature = "mvs-native-colors-arm64", target_arch = "aarch64"))]
#[path = "mvs_pixels_arm64.rs"]
mod native;

pub(crate) fn tile_rgb(y: &[u8; 64], cb: &[u8; 64], cr: &[u8; 64]) -> [u8; 192] {
    let mut rgb = [0; 192];
    #[cfg(not(feature = "mvs-native-colors-arm64"))]
    for index in 0..64 {
        rgb[index * 3..index * 3 + 3].copy_from_slice(&crate::mvs_full::ycbcr_8_to_rgb_scalar(
            y[index], cb[index], cr[index],
        ));
    }
    #[cfg(all(feature = "mvs-native-colors-arm64", target_arch = "aarch64"))]
    // SAFETY: 完整 64 样本输入/192 字节输出；编译基线已保证 NEON。
    unsafe {
        native::ycbcr_blocks8(y.as_ptr(), cb.as_ptr(), cr.as_ptr(), rgb.as_mut_ptr(), 8);
    }
    rgb
}

#[cfg(all(feature = "mvs-native-colors-arm64", target_arch = "aarch64"))]
pub(crate) fn single_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let mut rgb = [0; 24];
    // SAFETY: 每个数组均有 8 样本，输出有 24 字节，编译基线有 NEON。
    unsafe {
        native::ycbcr_blocks8(
            [y; 8].as_ptr(),
            [cb; 8].as_ptr(),
            [cr; 8].as_ptr(),
            rgb.as_mut_ptr(),
            1,
        );
    }
    [rgb[0], rgb[1], rgb[2]]
}

/// 既有调用者保证 RGB 三字节与 BGRX 四字节逐像素对应，X 固定为零。
#[cfg(any(test, feature = "mvs-native-colors-arm64"))]
pub(crate) fn rgb_to_bgrx(rgb: &[u8], out: &mut [u8]) -> bool {
    debug_assert_eq!(rgb.len() / 3 * 4, out.len());
    debug_assert_eq!(rgb.len() % 3, 0);
    #[cfg(not(feature = "mvs-native-colors-arm64"))]
    {
        let mut nonblack = false;
        for (rgb, out) in rgb.chunks_exact(3).zip(out.chunks_exact_mut(4)) {
            nonblack |= rgb.iter().any(|component| *component != 0);
            out.copy_from_slice(&[rgb[2], rgb[1], rgb[0], 0]);
        }
        nonblack
    }
    #[cfg(all(feature = "mvs-native-colors-arm64", target_arch = "aarch64"))]
    {
        let mut nonblack = false;
        let mut input = rgb.chunks_exact(24);
        let mut output = out.chunks_exact_mut(32);
        for (src, dst) in input.by_ref().zip(output.by_ref()) {
            // SAFETY: 精确 chunk 保证 8 RGB 样本与 8 BGRX 输出。
            nonblack |= unsafe { native::rgb_pack8(src.as_ptr(), dst.as_mut_ptr()) };
        }
        let tail = input.remainder();
        let out_tail = output.into_remainder();
        if !tail.is_empty() {
            let mut src = [0; 24];
            let mut dst = [0; 32];
            src[..tail.len()].copy_from_slice(tail);
            // SAFETY: 定长 scratch 满足同一个 8 样本内核的所有边界。
            nonblack |= unsafe { native::rgb_pack8(src.as_ptr(), dst.as_mut_ptr()) };
            out_tail.copy_from_slice(&dst[..out_tail.len()]);
        }
        nonblack
    }
}

/// 新 RGB 替换清零高字节；u32 引用始终自然对齐。
pub(crate) fn rgb_to_words(rgb: &[u8], out: &mut [u32]) {
    debug_assert_eq!(rgb.len(), out.len() * 3);
    #[cfg(not(feature = "mvs-native-colors-arm64"))]
    for (rgb, word) in rgb.chunks_exact(3).zip(out.iter_mut()) {
        *word = (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2]);
    }
    #[cfg(all(feature = "mvs-native-colors-arm64", target_arch = "aarch64"))]
    {
        let mut input = rgb.chunks_exact(24);
        let mut output = out.chunks_exact_mut(8);
        for (src, dst) in input.by_ref().zip(output.by_ref()) {
            // SAFETY: 每个目标是 8 个已对齐 u32；小端门保证 BGRX 字节等于逻辑值。
            unsafe {
                native::rgb_pack8(src.as_ptr(), dst.as_mut_ptr().cast());
            }
        }
        let tail = input.remainder();
        let out_tail = output.into_remainder();
        if !tail.is_empty() {
            let mut src = [0; 24];
            let mut dst = [0u32; 8];
            src[..tail.len()].copy_from_slice(tail);
            // SAFETY: 8 样本 scratch 满足完整边界，目标为真正对齐的 u32 数组。
            unsafe {
                native::rgb_pack8(src.as_ptr(), dst.as_mut_ptr().cast());
            }
            out_tail.copy_from_slice(&dst[..out_tail.len()]);
        }
    }
}

/// BGRX 小端字节到逻辑像素，保留原始高字节。
pub(crate) fn bytes_to_words(bytes: &[u8], out: &mut [u32]) {
    debug_assert_eq!(bytes.len(), out.len() * 4);
    #[cfg(not(feature = "mvs-native-colors-arm64"))]
    for (bytes, word) in bytes.chunks_exact(4).zip(out.iter_mut()) {
        *word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    }
    #[cfg(all(feature = "mvs-native-colors-arm64", target_arch = "aarch64"))]
    {
        let mut input = bytes.chunks_exact(16);
        let mut output = out.chunks_exact_mut(4);
        for (src, dst) in input.by_ref().zip(output.by_ref()) {
            // SAFETY: 16 字节输入与 4 个 u32 输出；vld1 允许非对齐字节地址。
            unsafe {
                native::bytes_words4(src.as_ptr(), dst.as_mut_ptr());
            }
        }
        let tail = input.remainder();
        let out_tail = output.into_remainder();
        if !tail.is_empty() {
            let mut src = [0; 16];
            let mut dst = [0u32; 4];
            src[..tail.len()].copy_from_slice(tail);
            // SAFETY: 定长 scratch 满足完整 16 字节/4 像素边界。
            unsafe {
                native::bytes_words4(src.as_ptr(), dst.as_mut_ptr());
            }
            out_tail.copy_from_slice(&dst[..out_tail.len()]);
        }
    }
}

/// 逻辑像素到小端补丁，保留全部四字节，不把高字节当作新 RGB 的 X。
pub(crate) fn words_to_bytes(words: &[u32], out: &mut [u8]) {
    debug_assert_eq!(out.len(), words.len() * 4);
    #[cfg(not(feature = "mvs-native-colors-arm64"))]
    for (word, bytes) in words.iter().zip(out.chunks_exact_mut(4)) {
        bytes.copy_from_slice(&word.to_le_bytes());
    }
    #[cfg(all(feature = "mvs-native-colors-arm64", target_arch = "aarch64"))]
    {
        let mut input = words.chunks_exact(4);
        let mut output = out.chunks_exact_mut(16);
        for (src, dst) in input.by_ref().zip(output.by_ref()) {
            // SAFETY: 完整 4 像素/16 字节 chunk，小端与 NEON 由编译门保证。
            unsafe {
                native::words_bytes4(src.as_ptr(), dst.as_mut_ptr());
            }
        }
        let tail = input.remainder();
        let out_tail = output.into_remainder();
        if !tail.is_empty() {
            let mut src = [0u32; 4];
            let mut dst = [0; 16];
            src[..tail.len()].copy_from_slice(tail);
            // SAFETY: 对齐的完整 u32 数组与完整字节 scratch。
            unsafe {
                native::words_bytes4(src.as_ptr(), dst.as_mut_ptr());
            }
            out_tail.copy_from_slice(&dst[..out_tail.len()]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pixels_literal_color_rounding_saturation_and_component_order() {
        // -91881 + 32768 < 0：有符号右移向负无穷取整。
        let y = [100; 64];
        assert_eq!(&tile_rgb(&y, &[128; 64], &[127; 64])[..3], &[99, 101, 100]);
        assert_eq!(&tile_rgb(&y, &[0; 64], &[255; 64])[..3], &[255, 53, 0]);
        assert_eq!(&tile_rgb(&y, &[255; 64], &[0; 64])[..3], &[0, 148, 255]);
    }
}

#[cfg(test)]
mod packing_tests {
    use super::*;
    #[test]
    fn pixels_pack_component_order_and_final_tail_nonblack() {
        let mut rgb = [0u8; 9 * 3];
        rgb[24..].copy_from_slice(&[0x11, 0x22, 0x33]);
        let mut bytes = [0xaa; 9 * 4];
        assert!(rgb_to_bgrx(&rgb, &mut bytes));
        assert_eq!(&bytes[..32], &[0; 32]);
        assert_eq!(&bytes[32..], &[0x33, 0x22, 0x11, 0]);
        let mut words = [0xdeadbeef; 9];
        rgb_to_words(&rgb, &mut words);
        assert_eq!(words, [0, 0, 0, 0, 0, 0, 0, 0, 0x00112233]);
    }
    #[test]
    fn pixels_preserve_all_four_bytes() {
        let words = [0xdeadbeef, 0xff112233, 0, 0x88776655, 0x44332211];
        let expected = [
            0xef, 0xbe, 0xad, 0xde, 0x33, 0x22, 0x11, 0xff, 0, 0, 0, 0, 0x55, 0x66, 0x77, 0x88,
            0x11, 0x22, 0x33, 0x44,
        ];
        let mut bytes = [0u8; 20];
        words_to_bytes(&words, &mut bytes);
        assert_eq!(bytes, expected);
        let mut actual = [0; 5];
        bytes_to_words(&expected, &mut actual);
        assert_eq!(actual, words);
    }
}

#[cfg(test)]
mod differential_tests {
    use super::*;

    #[test]
    fn pixels_batched_color_domain_matches_unchanged_scalar() {
        // Release 穷举全部 2^24 三元组；debug 用 4096 个分层 tile 加 4096 个固定 seed tile。
        let tiles = if cfg!(debug_assertions) {
            4096
        } else {
            262_144
        };
        let mut seed = 0x6d76_7320_2610_1502u64;
        for tile in 0..tiles + 4096 {
            let mut y = [0; 64];
            let mut cb = [0; 64];
            let mut cr = [0; 64];
            for lane in 0..64 {
                let value = if tile < tiles {
                    let value = tile * 64 + lane;
                    if cfg!(debug_assertions) {
                        value * 61
                    } else {
                        value
                    }
                } else {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed as usize
                };
                y[lane] = value as u8;
                cb[lane] = (value >> 8) as u8;
                cr[lane] = (value >> 16) as u8;
            }
            let actual = tile_rgb(&y, &cb, &cr);
            for lane in 0..64 {
                assert_eq!(
                    &actual[lane * 3..lane * 3 + 3],
                    &crate::mvs_full::ycbcr_8_to_rgb_scalar(y[lane], cb[lane], cr[lane]),
                    "tile={tile} lane={lane} Y/Cb/Cr={}/{}/{}",
                    y[lane],
                    cb[lane],
                    cr[lane]
                );
            }
        }
        eprintln!(
            "color triples={} exhaustive={} seed=0x6d76732026101502",
            (tiles + 4096) * 64,
            !cfg!(debug_assertions)
        );
    }

    #[test]
    fn pixels_all_tails_unaligned_bytes_canaries_and_high_bytes() {
        // 覆盖 0..=129 像素及全部字节偏移；u32 切片只从真正的 u32 数组构造。
        for count in 0..=129 {
            for offset in 0..16 {
                let mut rgb = vec![0xa5; offset + count * 3 + 16];
                for (index, value) in rgb[offset..offset + count * 3].iter_mut().enumerate() {
                    *value = (index * 137 + count * 17) as u8;
                }
                let src = &rgb[offset..offset + count * 3];
                let mut bgrx = vec![0x5a; offset + count * 4 + 16];
                assert_eq!(
                    rgb_to_bgrx(src, &mut bgrx[offset..offset + count * 4]),
                    src.iter().any(|x| *x != 0)
                );
                let mut words = vec![0xdecafbad; count + 2];
                rgb_to_words(src, &mut words[1..count + 1]);
                for index in 0..count {
                    let source = index * 3;
                    let expected = [src[source + 2], src[source + 1], src[source], 0];
                    assert_eq!(&bgrx[offset + index * 4..offset + index * 4 + 4], &expected);
                    assert_eq!(words[index + 1], u32::from_le_bytes(expected));
                }
                assert!(bgrx[..offset]
                    .iter()
                    .chain(bgrx[offset + count * 4..].iter())
                    .all(|x| *x == 0x5a));
                assert_eq!(words[0], 0xdecafbad);
                assert_eq!(words[count + 1], 0xdecafbad);
                for (index, word) in words[1..count + 1].iter_mut().enumerate() {
                    *word |= ((index * 73 + 193) as u32) << 24;
                }
                words_to_bytes(&words[1..count + 1], &mut bgrx[offset..offset + count * 4]);
                for index in 0..count {
                    assert_eq!(
                        &bgrx[offset + index * 4..offset + index * 4 + 4],
                        &words[index + 1].to_le_bytes()
                    );
                }
                let mut roundtrip = vec![0xdeadbeef; count + 2];
                bytes_to_words(
                    &bgrx[offset..offset + count * 4],
                    &mut roundtrip[1..count + 1],
                );
                assert_eq!(&roundtrip[1..count + 1], &words[1..count + 1]);
                assert_eq!(roundtrip[0], 0xdeadbeef);
                assert_eq!(roundtrip[count + 1], 0xdeadbeef);
                assert!(bgrx[..offset]
                    .iter()
                    .chain(bgrx[offset + count * 4..].iter())
                    .all(|x| *x == 0x5a));
                assert!(rgb[..offset]
                    .iter()
                    .chain(rgb[offset + count * 3..].iter())
                    .all(|x| *x == 0xa5));
            }
        }
    }

    #[test]
    fn pixels_nonblack_every_component_in_final_tail_only() {
        for count in 1..=33 {
            for channel in 0..3 {
                let mut rgb = vec![0; count * 3];
                let mut out = vec![0xff; count * 4];
                assert!(!rgb_to_bgrx(&rgb, &mut out));
                assert!(out.iter().all(|x| *x == 0));
                rgb[(count - 1) * 3 + channel] = 1;
                assert!(rgb_to_bgrx(&rgb, &mut out));
                assert_eq!(out.iter().filter(|x| **x == 1).count(), 1);
                assert_eq!(out[(count - 1) * 4 + 2 - channel], 1);
            }
        }
    }
}

/// 定长 8 像素 RGB 行，位 0x80 对应最左像素。
#[cfg(any(test, feature = "mvs-native-colors-arm64"))]
pub(crate) fn bitmap_rgb_row(mask: u8, one: [u8; 3], zero: [u8; 3]) -> [u8; 24] {
    let mut rgb = [0; 24];
    #[cfg(not(feature = "mvs-native-colors-arm64"))]
    for (pixel, output) in rgb.chunks_exact_mut(3).enumerate() {
        output.copy_from_slice(if mask & (0x80 >> pixel) != 0 {
            &one
        } else {
            &zero
        });
    }
    #[cfg(all(feature = "mvs-native-colors-arm64", target_arch = "aarch64"))]
    // SAFETY: 定长 scratch 提供完整 24 字节；编译基线保证 NEON。
    unsafe {
        native::bitmap_rgb8(mask, one, zero, rgb.as_mut_ptr());
    }
    rgb
}

#[cfg(all(test, feature = "mvs-native-colors-arm64", target_arch = "aarch64"))]
mod native_completion_row_bounds_tests {
    use super::*;

    #[test]
    fn native_completion_bitmap_raw_leaf_unaligned_canaries_and_solid_rows() {
        let mut cases = 0;
        for offset in 0..=16 {
            for mask in 0..=255u8 {
                for (one, zero) in [
                    ([0x11, 0x82, 0xf3], [0xe4, 0x65, 0x26]),
                    ([0, 255, 1], [255, 0, 254]),
                    ([0x49; 3], [0x49; 3]),
                ] {
                    let mut actual = [0xac; 64];
                    let mut expected = actual;
                    for pixel in 0..8 {
                        let color = if (u16::from(mask) >> (7 - pixel)) % 2 == 1 {
                            one
                        } else {
                            zero
                        };
                        expected[offset + pixel * 3..offset + pixel * 3 + 3]
                            .copy_from_slice(&color);
                    }
                    // SAFETY: offset <= 16，完整 24 字节写入均在 64 字节数组内。
                    unsafe {
                        native::bitmap_rgb8(mask, one, zero, actual.as_mut_ptr().add(offset));
                    }
                    assert_eq!(actual, expected, "offset={offset}, mask={mask}");
                    cases += 1;
                }
            }
        }
        assert_eq!(cases, 13_056);
    }
}

/// 独立切片保证不重叠；只搬运已验证的 1..=8 个完整 RGB。
#[cfg(any(test, feature = "mvs-native-colors-arm64"))]
pub(crate) fn copy_rgb_nonoverlap(src: &[u8], dst: &mut [u8]) {
    assert_eq!(src.len(), dst.len());
    assert!(src.len().is_multiple_of(3) && (3..=24).contains(&src.len()));
    // SAFETY: 切片边界与唯一可变借用满足下层的精确大小/不重叠约束。
    unsafe {
        copy_rgb_nonoverlap_raw(src.as_ptr(), dst.as_mut_ptr(), src.len() / 3);
    }
}

/// # Safety
/// 两个范围均有 pixels*3 有效字节，互不重叠；pixels 为 1..=8，允许字节非对齐。
#[cfg(any(test, feature = "mvs-native-colors-arm64"))]
pub(crate) unsafe fn copy_rgb_nonoverlap_raw(src: *const u8, dst: *mut u8, pixels: usize) {
    // SAFETY: 调用者保证两个精确跨度有效、不重叠；基线门保证小端 NEON。
    #[cfg(all(feature = "mvs-native-colors-arm64", target_arch = "aarch64"))]
    unsafe {
        native::copy_rgb_nonoverlap(src, dst, pixels);
    }
    // 默认生产 helper 仍使用原始循环；此分支仅供新 helper 的宿主 oracle 测试。
    #[cfg(all(test, not(feature = "mvs-native-colors-arm64")))]
    unsafe {
        std::ptr::copy_nonoverlapping(src, dst, pixels * 3);
    }
}
