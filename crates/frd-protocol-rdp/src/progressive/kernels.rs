//! Progressive 数值内核；wire/entropy 与持久 tile 状态由调用者管理。
//! Math reference: FreeRDP 54a873e2710710841c6ec2b756df64285ec6e29a,
//! libfreerdp/codec/progressive.c and libfreerdp/primitives/prim_colors.c.
//! Copyright 2011 Stephen Erisman, Norbert Federa, Martin Fleisz;
//! Copyright 2012 Hewlett-Packard Development Company, L.P.
//! Copyright 2011 Vic Lee; 2014 Marc-Andre Moreau;
//! Copyright 2019 Armin Novak and Thincast Technologies GmbH.
//! Reference licensed under Apache-2.0: https://www.apache.org/licenses/LICENSE-2.0
//! This implementation uses explicit SSE2/NEON arithmetic, including short tails.
#![allow(dead_code)]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BandLayout {
    Normal,
    ReduceExtrapolate,
}
impl BandLayout {
    pub(super) fn lengths(self) -> [usize; 10] {
        match self {
            Self::Normal => [1024, 1024, 1024, 256, 256, 256, 64, 64, 64, 64],
            Self::ReduceExtrapolate => [1023, 1023, 961, 272, 272, 256, 72, 72, 64, 81],
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KernelError {
    InvalidShift,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct NativeKernels(());
impl NativeKernels {
    pub(super) fn new() -> Option<Self> {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if std::is_x86_feature_detected!("sse2") {
            return Some(Self(()));
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            return Some(Self(()));
        }
        None
    }
    /// shifts 按 HL1,LH1,HH1,...,LL3 排列，合法范围0..=22。
    /// base quant最多15、progressive quant最多8，有效shift=base+progressive-1。
    /// 先执行32位左移，再保留低16位；shift>=16归零，不对16取模。
    pub(super) fn shift_bands(
        &self,
        c: &mut [i16; 4096],
        shifts: &[u8; 10],
        layout: BandLayout,
    ) -> Result<(), KernelError> {
        if shifts.iter().any(|s| *s > 22) {
            return Err(KernelError::InvalidShift);
        }
        let mut offset = 0;
        for (len, shift) in layout.lengths().into_iter().zip(shifts) {
            for start in (offset..offset + len).step_by(4) {
                let n = (offset + len - start).min(4);
                let mut a = [0; 4];
                for j in 0..n {
                    a[j] = i32::from(c[start + j]);
                }
                let out = unsafe { V::load(a).shl(*shift as i32).array() };
                for j in 0..n {
                    c[start + j] = out[j] as i16;
                }
            }
            offset += len;
        }
        Ok(())
    }
    pub(super) fn capture_sign(&self, c: &[i16; 4096], sign: &mut [i16; 4096]) {
        for i in (0..4096).step_by(4) {
            unsafe {
                let v = V::load(std::array::from_fn(|j| i32::from(c[i + j])));
                let s = v.sign().array();
                for j in 0..4 {
                    sign[i + j] = s[j] as i16;
                }
            }
        }
    }
    /// Signed refinement deltas are resolved by the strict entropy decoder.
    pub(super) fn add(&self, c: &mut [i16; 4096], delta: &[i16; 4096]) {
        for i in (0..4096).step_by(4) {
            unsafe {
                let a = V::load(std::array::from_fn(|j| i32::from(c[i + j])));
                let b = V::load(std::array::from_fn(|j| i32::from(delta[i + j])));
                let out = a.add(b).array();
                for j in 0..4 {
                    c[i + j] = out[j] as i16;
                }
            }
        }
    }
    /// LL3 differential applies to both layouts, before dequantization.
    pub(super) fn prefix_sum_ll3(&self, c: &mut [i16; 4096], layout: BandLayout) {
        let start = match layout {
            BandLayout::Normal => 4032,
            BandLayout::ReduceExtrapolate => 4015,
        };
        let mut carry = 0;
        for i in (start..4096).step_by(4) {
            let n = (4096 - i).min(4);
            unsafe {
                let a = V::load(std::array::from_fn(|j| {
                    if j < n {
                        i32::from(c[i + j])
                    } else {
                        0
                    }
                }));
                let out = a.prefix().add(V::splat(carry)).wrap16().array();
                for j in 0..n {
                    c[i + j] = out[j] as i16;
                }
                carry = out[n - 1];
            }
        }
    }
    /// The entropy layer bounds magnitudes by numBits and supplies resolved signs.
    /// Shifts 0..=22 use 32-bit wrapping arithmetic followed by low-16-bit storage.
    /// DAS is captured before LL3 differential and is updated only on first significance.
    pub(super) fn apply_refinement(
        &self,
        current: &mut [i16; 4096],
        das: &mut [i16; 4096],
        magnitudes: &[u16; 4096],
        negative: &[bool; 4096],
        shifts: &[u8; 10],
        layout: BandLayout,
    ) -> Result<(), KernelError> {
        if shifts.iter().any(|s| *s > 22) {
            return Err(KernelError::InvalidShift);
        }
        let mut offset = 0;
        for (len, shift) in layout.lengths().into_iter().zip(shifts) {
            for i in (offset..offset + len).step_by(4) {
                let n = (offset + len - i).min(4);
                unsafe {
                    let mag = V::load(std::array::from_fn(|j| {
                        if j < n {
                            i32::from(magnitudes[i + j])
                        } else {
                            0
                        }
                    }));
                    let neg = V::load(std::array::from_fn(|j| {
                        if j < n && negative[i + j] {
                            -1
                        } else {
                            0
                        }
                    }));
                    let old = V::load(std::array::from_fn(|j| {
                        if j < n {
                            i32::from(current[i + j])
                        } else {
                            0
                        }
                    }));
                    let sign = V::load(std::array::from_fn(|j| {
                        if j < n {
                            i32::from(das[i + j])
                        } else {
                            0
                        }
                    }));
                    let signed = mag.xor(neg).sub(neg);
                    let out = old.add(signed.shl(i32::from(*shift))).array();
                    let updated = sign.replace_zero(signed.sign()).array();
                    for j in 0..n {
                        current[i + j] = out[j] as i16;
                        das[i + j] = updated[j] as i16;
                    }
                }
            }
            offset += len;
        }
        Ok(())
    }
    pub(super) fn ycbcr_to_bgra(
        &self,
        y: &[i16; 4096],
        cb: &[i16; 4096],
        cr: &[i16; 4096],
        dst: &mut [u8; 16384],
    ) {
        for i in (0..4096).step_by(4) {
            unsafe {
                let yy = V::load(std::array::from_fn(|j| i32::from(y[i + j])))
                    .add(V::splat(4096))
                    .shl(16);
                let u = V::load(std::array::from_fn(|j| i32::from(cb[i + j])));
                let v = V::load(std::array::from_fn(|j| i32::from(cr[i + j])));
                let r = yy.add(v.mul(91916)).sar(21).clip(0, 255).array();
                let g = yy
                    .sub(v.mul(46819))
                    .sub(u.mul(22527))
                    .sar(21)
                    .clip(0, 255)
                    .array();
                let b = yy.add(u.mul(115992)).sar(21).clip(0, 255).array();
                for j in 0..4 {
                    dst[(i + j) * 4..(i + j) * 4 + 4]
                        .copy_from_slice(&[b[j] as u8, g[j] as u8, r[j] as u8, 255]);
                }
            }
        }
    }
    pub(super) fn inverse_dwt(
        &self,
        c: &mut [i16; 4096],
        scratch: &mut [i16; 4096],
        layout: BandLayout,
    ) -> Result<(), KernelError> {
        let levels = match layout {
            BandLayout::Normal => [(3840, 8, 8), (3072, 16, 16), (0, 32, 32)],
            BandLayout::ReduceExtrapolate => [(3807, 9, 8), (3007, 17, 16), (0, 33, 31)],
        };
        for (off, l, h) in levels {
            unsafe {
                block(&mut c[off..], scratch, l, h, layout);
            }
        }
        Ok(())
    }
}

// 四条独立 scanline 同时进行 lifting。Gather/scatter 只搬运，不执行像素算术。
unsafe fn lines(
    src: &[i16],
    dst: &mut [i16],
    lo: usize,
    hi: usize,
    ls: usize,
    hs: usize,
    ds: usize,
    line_src: usize,
    line_dst: usize,
    count: usize,
    l: usize,
    h: usize,
    layout: BandLayout,
) {
    for first in (0..count).step_by(4) {
        let n = (count - first).min(4);
        let read = |base: usize, step: usize, index: usize| {
            V::load(std::array::from_fn(|k| {
                if k < n {
                    i32::from(src[base + (first + k) * line_src + index * step])
                } else {
                    0
                }
            }))
        };
        let mut write = |index: usize, v: V| {
            let a = v.array();
            for k in 0..n {
                dst[(first + k) * line_dst + index * ds] = a[k] as i16;
            }
        };
        let norm = |v: V| match layout {
            BandLayout::Normal => v.wrap16(),
            BandLayout::ReduceExtrapolate => v.clip(-32768, 32767),
        };
        let half = |v: V| match layout {
            BandLayout::Normal => v.sar(1),
            BandLayout::ReduceExtrapolate => v.half_zero(),
        };
        let mut hp = read(hi, hs, 0);
        let mut even = norm(read(lo, ls, 0).sub(hp));
        write(0, even);
        for j in 1..h {
            let hn = read(hi, hs, j);
            let sum = hp.add(hn);
            let correction = match layout {
                BandLayout::Normal => sum.add(V::splat(1)).sar(1),
                BandLayout::ReduceExtrapolate => sum.half_zero(),
            };
            let next = norm(read(lo, ls, j).sub(correction));
            write(j * 2 - 1, norm(half(even.add(next)).add(hp.shl(1))));
            write(j * 2, next);
            even = next;
            hp = hn;
        }
        if l == h {
            write(h * 2 - 1, norm(even.add(hp.shl(1))));
        } else {
            let correction = if l == h + 1 { hp } else { hp.half_zero() };
            let next = norm(read(lo, ls, h).sub(correction));
            write(h * 2 - 1, norm(half(even.add(next)).add(hp.shl(1))));
            write(h * 2, next);
            if l == h + 2 {
                write(h * 2 + 1, norm(next.add(read(lo, ls, h + 1)).half_zero()));
            }
        }
    }
}
unsafe fn block(c: &mut [i16], temp: &mut [i16], l: usize, h: usize, layout: BandLayout) {
    let w = l + h;
    let lh = l * h;
    let hh = 2 * lh;
    let ll = hh + h * h;
    // Horizontal rows have different low/high row strides: stage contiguous band rows.
    let mut staged = [0i16; 4096];
    for row in 0..l {
        staged[row * w..row * w + l].copy_from_slice(&c[ll + row * l..ll + (row + 1) * l]);
        staged[row * w + l..(row + 1) * w].copy_from_slice(&c[row * h..(row + 1) * h]);
    }
    lines(&staged, temp, 0, l, 1, 1, 1, w, w, l, l, h, layout);
    for row in 0..h {
        staged[row * w..row * w + l].copy_from_slice(&c[lh + row * l..lh + (row + 1) * l]);
        staged[row * w + l..(row + 1) * w].copy_from_slice(&c[hh + row * h..hh + (row + 1) * h]);
    }
    lines(
        &staged,
        &mut temp[l * w..],
        0,
        l,
        1,
        1,
        1,
        w,
        w,
        h,
        l,
        h,
        layout,
    );
    lines(temp, c, 0, l * w, w, w, w, 1, 1, w, l, h, layout);
}

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;
#[cfg(target_arch = "x86")]
use std::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;
#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy)]
struct V(int32x4_t);
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[derive(Clone, Copy)]
struct V(__m128i);

#[cfg(target_arch = "aarch64")]
impl V {
    #[target_feature(enable = "neon")]
    unsafe fn xor(self, b: Self) -> Self {
        Self(veorq_s32(self.0, b.0))
    }
    #[target_feature(enable = "neon")]
    unsafe fn replace_zero(self, b: Self) -> Self {
        Self(vbslq_s32(vceqq_s32(self.0, vdupq_n_s32(0)), b.0, self.0))
    }
    #[target_feature(enable = "neon")]
    unsafe fn prefix(self) -> Self {
        let a = vaddq_s32(self.0, vextq_s32::<3>(vdupq_n_s32(0), self.0));
        Self(vaddq_s32(a, vextq_s32::<2>(vdupq_n_s32(0), a)))
    }
    #[target_feature(enable = "neon")]
    unsafe fn load(a: [i32; 4]) -> Self {
        Self(vld1q_s32(a.as_ptr()))
    }
    #[target_feature(enable = "neon")]
    unsafe fn splat(a: i32) -> Self {
        Self(vdupq_n_s32(a))
    }
    #[target_feature(enable = "neon")]
    unsafe fn array(self) -> [i32; 4] {
        let mut a = [0; 4];
        vst1q_s32(a.as_mut_ptr(), self.0);
        a
    }
    #[target_feature(enable = "neon")]
    unsafe fn add(self, b: Self) -> Self {
        Self(vaddq_s32(self.0, b.0))
    }
    #[target_feature(enable = "neon")]
    unsafe fn sub(self, b: Self) -> Self {
        Self(vsubq_s32(self.0, b.0))
    }
    #[target_feature(enable = "neon")]
    unsafe fn shl(self, n: i32) -> Self {
        Self(vshlq_s32(self.0, vdupq_n_s32(n)))
    }
    #[target_feature(enable = "neon")]
    unsafe fn sar(self, n: i32) -> Self {
        Self(vshlq_s32(self.0, vdupq_n_s32(-n)))
    }
    #[target_feature(enable = "neon")]
    unsafe fn mul(self, n: i32) -> Self {
        Self(vmulq_n_s32(self.0, n))
    }
    #[target_feature(enable = "neon")]
    unsafe fn clip(self, lo: i32, hi: i32) -> Self {
        Self(vminq_s32(
            vmaxq_s32(self.0, vdupq_n_s32(lo)),
            vdupq_n_s32(hi),
        ))
    }
    #[target_feature(enable = "neon")]
    unsafe fn sign(self) -> Self {
        Self(vsubq_s32(
            vreinterpretq_s32_u32(vcltq_s32(self.0, vdupq_n_s32(0))),
            vreinterpretq_s32_u32(vcgtq_s32(self.0, vdupq_n_s32(0))),
        ))
    }
    #[target_feature(enable = "neon")]
    unsafe fn half_zero(self) -> Self {
        self.sub(self.sar(31)).sar(1)
    }
    #[target_feature(enable = "neon")]
    unsafe fn wrap16(self) -> Self {
        self.shl(16).sar(16)
    }
}
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
impl V {
    #[target_feature(enable = "sse2")]
    unsafe fn xor(self, b: Self) -> Self {
        Self(_mm_xor_si128(self.0, b.0))
    }
    #[target_feature(enable = "sse2")]
    unsafe fn replace_zero(self, b: Self) -> Self {
        let mask = _mm_cmpeq_epi32(self.0, _mm_setzero_si128());
        Self(_mm_or_si128(
            _mm_and_si128(mask, b.0),
            _mm_andnot_si128(mask, self.0),
        ))
    }
    #[target_feature(enable = "sse2")]
    unsafe fn prefix(self) -> Self {
        let a = _mm_add_epi32(self.0, _mm_slli_si128::<4>(self.0));
        Self(_mm_add_epi32(a, _mm_slli_si128::<8>(a)))
    }
    #[target_feature(enable = "sse2")]
    unsafe fn load(a: [i32; 4]) -> Self {
        Self(_mm_loadu_si128(a.as_ptr().cast()))
    }
    #[target_feature(enable = "sse2")]
    unsafe fn splat(a: i32) -> Self {
        Self(_mm_set1_epi32(a))
    }
    #[target_feature(enable = "sse2")]
    unsafe fn array(self) -> [i32; 4] {
        let mut a = [0; 4];
        _mm_storeu_si128(a.as_mut_ptr().cast(), self.0);
        a
    }
    #[target_feature(enable = "sse2")]
    unsafe fn add(self, b: Self) -> Self {
        Self(_mm_add_epi32(self.0, b.0))
    }
    #[target_feature(enable = "sse2")]
    unsafe fn sub(self, b: Self) -> Self {
        Self(_mm_sub_epi32(self.0, b.0))
    }
    #[target_feature(enable = "sse2")]
    unsafe fn shl(self, n: i32) -> Self {
        Self(_mm_sll_epi32(self.0, _mm_cvtsi32_si128(n)))
    }
    #[target_feature(enable = "sse2")]
    unsafe fn sar(self, n: i32) -> Self {
        Self(_mm_sra_epi32(self.0, _mm_cvtsi32_si128(n)))
    }
    #[target_feature(enable = "sse2")]
    unsafe fn mul(self, n: i32) -> Self {
        let b = _mm_set1_epi32(n);
        let even = _mm_mul_epu32(self.0, b);
        let odd = _mm_mul_epu32(_mm_srli_si128::<4>(self.0), b);
        Self(_mm_unpacklo_epi32(
            _mm_shuffle_epi32::<0x88>(even),
            _mm_shuffle_epi32::<0x88>(odd),
        ))
    }
    #[target_feature(enable = "sse2")]
    unsafe fn clip(self, lo: i32, hi: i32) -> Self {
        let l = _mm_set1_epi32(lo);
        let h = _mm_set1_epi32(hi);
        let below = _mm_cmpgt_epi32(l, self.0);
        let a = _mm_or_si128(_mm_and_si128(below, l), _mm_andnot_si128(below, self.0));
        let above = _mm_cmpgt_epi32(a, h);
        Self(_mm_or_si128(
            _mm_and_si128(above, h),
            _mm_andnot_si128(above, a),
        ))
    }
    #[target_feature(enable = "sse2")]
    unsafe fn sign(self) -> Self {
        Self(_mm_sub_epi32(
            _mm_cmpgt_epi32(_mm_setzero_si128(), self.0),
            _mm_cmpgt_epi32(self.0, _mm_setzero_si128()),
        ))
    }
    #[target_feature(enable = "sse2")]
    unsafe fn half_zero(self) -> Self {
        self.sub(self.sar(31)).sar(1)
    }
    #[target_feature(enable = "sse2")]
    unsafe fn wrap16(self) -> Self {
        self.shl(16).sar(16)
    }
}

#[cfg(test)]
#[path = "kernel_tests.rs"]
mod tests;
