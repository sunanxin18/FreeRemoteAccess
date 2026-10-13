//! 安全不变量：只能经检查 ISA 的私有函数表进入；数组 load/store 不越界。
//! scalar 循环仅处理布局和 staging；所有系数算术及尾部均经 SIMD。
use crate::{Backend, KernelError, NativeKernels};
#[cfg(target_arch = "aarch64")]
#[path = "neon.rs"]
mod arch;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[path = "x86.rs"]
mod arch;
use arch::*;
#[path = "bitmap.rs"]
mod bitmap;
#[path = "pixel.rs"]
mod pixel;
#[path = "planar.rs"]
mod planar;

pub(super) fn available() -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("neon")
    }
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        std::arch::is_x86_feature_detected!("sse2")
    }
}
pub(super) fn kernels() -> NativeKernels {
    NativeKernels {
        #[cfg(target_arch = "aarch64")]
        backend: Backend::Neon,
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        backend: Backend::Sse2,
        dequant: dequantize,
        dwt: inverse_dwt,
        prefix: differential,
        zero: zero_i16,
        color,
        copy: bitmap::copy,
        fill: bitmap::fill,
        xor_previous: bitmap::xor_previous,
        mask_previous: bitmap::mask_previous,
        dither: bitmap::dither,
        planar_delta: planar::delta,
        planar_rgb: planar::rgb,
        planar_ycocg: planar::ycocg,
        pixel_row: pixel::row,
    }
}

unsafe fn dequantize(data: &mut [i16], quant: &[u8; 10]) {
    let mut offset = 0;
    for (band, &q) in quant.iter().enumerate() {
        let length = if band < 3 {
            1024
        } else if band < 6 {
            256
        } else {
            64
        };
        if q > 1 {
            for chunk in data[offset..offset + length].chunks_exact_mut(8) {
                // SAFETY: 完整八元素数组；q-1 已限定为 1..=15。
                shift8(chunk.try_into().unwrap(), q - 1);
            }
        }
        offset += length;
    }
}

unsafe fn zero_i16(data: &mut [i16]) {
    let mut chunks = data.chunks_exact_mut(8);
    for chunk in &mut chunks {
        zero8(chunk.try_into().unwrap());
    }
    let tail = chunks.into_remainder();
    if !tail.is_empty() {
        let mut stage = [1; 8];
        zero8(&mut stage);
        tail.copy_from_slice(&stage[..tail.len()]);
    }
}

unsafe fn differential(data: &mut [i16]) {
    let mut carry = 0;
    let mut chunks = data.chunks_exact_mut(4);
    for chunk in &mut chunks {
        let out = narrow(add(
            prefix(load((&*chunk).try_into().unwrap())),
            splat(carry),
        ));
        chunk.copy_from_slice(&out);
        carry = i32::from(out[3]);
    }
    let tail = chunks.into_remainder();
    if !tail.is_empty() {
        let mut stage = [0; 4];
        stage[..tail.len()].copy_from_slice(tail);
        let out = narrow(add(prefix(load(&stage)), splat(carry)));
        tail.copy_from_slice(&out[..tail.len()]);
    }
}

/// 一维 lifting：复制边缘；存储偶数后再计算奇数，保持 i16 截断顺序。
unsafe fn inverse_line(
    low: &[i16],
    high: &[i16],
    stride: usize,
    dst: &mut [i16],
    dst_stride: usize,
    width: usize,
) {
    for n in (0..width).step_by(4) {
        let lo = std::array::from_fn(|j| low[(n + j) * stride]);
        let hi = std::array::from_fn(|j| high[(n + j) * stride]);
        let prev = std::array::from_fn(|j| high[(n + j).saturating_sub(1) * stride]);
        let even = narrow(sub(
            load(&lo),
            shr::<1>(add(add(load(&prev), load(&hi)), splat(1))),
        ));
        for j in 0..4 {
            dst[2 * (n + j) * dst_stride] = even[j];
        }
    }
    for n in (0..width).step_by(4) {
        let hi = std::array::from_fn(|j| high[(n + j) * stride]);
        let even = std::array::from_fn(|j| dst[2 * (n + j) * dst_stride]);
        let next = std::array::from_fn(|j| dst[2 * (n + j + 1).min(width - 1) * dst_stride]);
        // 上游 (hi << 1) 的 i16 截断发生在 i32 加法之前。
        let doubled = shr::<16>(shl::<17>(load(&hi)));
        let odd = narrow(add(doubled, shr::<1>(add(load(&even), load(&next)))));
        for j in 0..4 {
            dst[(2 * (n + j) + 1) * dst_stride] = odd[j];
        }
    }
}

unsafe fn inverse_dwt(data: &mut [i16], scratch: &mut [i16]) {
    for (offset, width) in [(3840, 8), (3072, 16), (0, 32)] {
        let data = &mut data[offset..];
        let square = width * width;
        for row in 0..width {
            inverse_line(
                &data[3 * square + row * width..],
                &data[row * width..],
                1,
                &mut scratch[row * width * 2..],
                1,
                width,
            );
            inverse_line(
                &data[square + row * width..],
                &data[2 * square + row * width..],
                1,
                &mut scratch[2 * square + row * width * 2..],
                1,
                width,
            );
        }
        for col in 0..width * 2 {
            inverse_line(
                &scratch[col..],
                &scratch[2 * square + col..],
                width * 2,
                &mut data[col..],
                width * 2,
                width,
            );
        }
    }
}

#[inline]
unsafe fn checked_add(a: V, b: V) -> Result<V, KernelError> {
    let v = add(a, b);
    if any_negative(and(not(xor(a, b)), xor(a, v))) {
        Err(KernelError::ColorOverflow)
    } else {
        Ok(v)
    }
}
#[inline]
unsafe fn checked_sub(a: V, b: V) -> Result<V, KernelError> {
    let v = sub(a, b);
    if any_negative(and(xor(a, b), xor(a, v))) {
        Err(KernelError::ColorOverflow)
    } else {
        Ok(v)
    }
}
#[inline]
unsafe fn checked_mul(a: V, coefficient: i32) -> Result<V, KernelError> {
    if any_negative(gt(a, splat(i32::MAX / coefficient)))
        || any_negative(gt(splat(i32::MIN / coefficient), a))
    {
        return Err(KernelError::ColorOverflow);
    }
    Ok(mul(a, coefficient))
}
unsafe fn color4(y: &[i16; 4], cb: &[i16; 4], cr: &[i16; 4]) -> Result<[u8; 16], KernelError> {
    let y = load(y);
    let cb = load(cb);
    let cr = load(cr);
    // 上游 y+4096 本身在 i16 中求值，不能仅检查最后 i32 通道。
    if any_negative(gt(y, splat(28671))) {
        return Err(KernelError::ColorOverflow);
    }
    let yy = mul(add(y, splat(4096)), 65536);
    let r = checked_add(yy, checked_mul(cr, 91915)?)?;
    let g = checked_sub(
        checked_sub(yy, checked_mul(cr, 46818)?)?,
        checked_mul(cb, 22526)?,
    )?;
    let b = checked_add(yy, checked_mul(cb, 115992)?)?;
    Ok(rgba(
        clamp_byte(shr::<21>(r)),
        clamp_byte(shr::<21>(g)),
        clamp_byte(shr::<21>(b)),
    ))
}
unsafe fn color(y: &[i16], cb: &[i16], cr: &[i16], out: &mut [u8]) -> Result<(), KernelError> {
    // 两遍检查保证后部非法系数也不会留下部分已写像素；不分配整帧副本。
    let full = y.len() / 4 * 4;
    for write in [false, true] {
        // 完整向量直接借用已检查的定长数组，只有短尾需要 staging。
        for start in (0..full).step_by(4) {
            let rgba = color4(
                y[start..start + 4].try_into().unwrap(),
                cb[start..start + 4].try_into().unwrap(),
                cr[start..start + 4].try_into().unwrap(),
            )?;
            if write {
                out[4 * start..4 * start + 16].copy_from_slice(&rgba);
            }
        }
        if full != y.len() {
            let len = y.len() - full;
            let mut ys = [0; 4];
            let mut us = [0; 4];
            let mut vs = [0; 4];
            ys[..len].copy_from_slice(&y[full..]);
            us[..len].copy_from_slice(&cb[full..]);
            vs[..len].copy_from_slice(&cr[full..]);
            let rgba = color4(&ys, &us, &vs)?;
            if write {
                out[4 * full..].copy_from_slice(&rgba[..4 * len]);
            }
        }
    }
    Ok(())
}
