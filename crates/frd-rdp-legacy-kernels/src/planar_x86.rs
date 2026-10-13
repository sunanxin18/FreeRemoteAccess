//! i686 与 AMD64 分别 cfg/import/编译；只有 SSE2，不使用 SSSE3 shuffle。
#[cfg(target_arch = "x86")]
use core::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;
#[inline(never)]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn delta16(previous: &[u8; 16], current: &mut [u8; 16]) {
    let d = _mm_loadu_si128(current.as_ptr().cast());
    let half = _mm_and_si128(_mm_srli_epi16::<1>(d), _mm_set1_epi8(127));
    let odd = _mm_cmpeq_epi8(_mm_and_si128(d, _mm_set1_epi8(1)), _mm_set1_epi8(1));
    let out = _mm_add_epi8(
        _mm_loadu_si128(previous.as_ptr().cast()),
        _mm_xor_si128(half, odd),
    );
    _mm_storeu_si128(current.as_mut_ptr().cast(), out);
}
#[inline(never)]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn duplicate4(src: &[u8; 4]) -> [u8; 8] {
    let mut stage = [0; 8];
    stage[..4].copy_from_slice(src);
    let v = _mm_loadl_epi64(stage.as_ptr().cast());
    let mut out = [0; 8];
    _mm_storel_epi64(out.as_mut_ptr().cast(), _mm_unpacklo_epi8(v, v));
    out
}
#[inline]
#[target_feature(enable = "sse2")]
unsafe fn compact4(v: __m128i) -> __m128i {
    let mask = _mm_set_epi32(0, 0, 0, 0xffffff);
    let p0 = _mm_and_si128(v, mask);
    let p1 = _mm_slli_si128::<3>(_mm_and_si128(_mm_srli_si128::<4>(v), mask));
    let p2 = _mm_slli_si128::<6>(_mm_and_si128(_mm_srli_si128::<8>(v), mask));
    let p3 = _mm_slli_si128::<9>(_mm_and_si128(_mm_srli_si128::<12>(v), mask));
    _mm_or_si128(_mm_or_si128(p0, p1), _mm_or_si128(p2, p3))
}
#[inline]
#[target_feature(enable = "sse2")]
unsafe fn pack(r: __m128i, g: __m128i, b: __m128i, out: &mut [u8; 24]) {
    let rg = _mm_unpacklo_epi8(r, g);
    let bz = _mm_unpacklo_epi8(b, _mm_setzero_si128());
    let lo = compact4(_mm_unpacklo_epi16(rg, bz));
    let hi = compact4(_mm_unpackhi_epi16(rg, bz));
    _mm_storeu_si128(
        out.as_mut_ptr().cast(),
        _mm_or_si128(lo, _mm_slli_si128::<12>(hi)),
    );
    _mm_storel_epi64(out.as_mut_ptr().add(16).cast(), _mm_srli_si128::<4>(hi));
}
#[inline(never)]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn pack8(r: &[u8; 8], g: &[u8; 8], b: &[u8; 8], out: &mut [u8; 24]) {
    pack(
        _mm_loadl_epi64(r.as_ptr().cast()),
        _mm_loadl_epi64(g.as_ptr().cast()),
        _mm_loadl_epi64(b.as_ptr().cast()),
        out,
    );
}
#[inline(never)]
#[target_feature(enable = "sse2")]
pub(super) unsafe fn color8(
    y: &[u8; 8],
    co: &[u8; 8],
    cg: &[u8; 8],
    shift: u8,
    alpha: bool,
    out: &mut [u8; 24],
) {
    let zero = _mm_setzero_si128();
    let count = _mm_cvtsi32_si128(i32::from(shift));
    // 先把 u8 放进 word lane；shift 后 sign-extend 低八 bit，等价 u8 截断再 cast i8。
    let co = _mm_sra_epi16(
        _mm_slli_epi16::<8>(_mm_sll_epi16(
            _mm_unpacklo_epi8(_mm_loadl_epi64(co.as_ptr().cast()), zero),
            count,
        )),
        _mm_cvtsi32_si128(8),
    );
    let cg = _mm_sra_epi16(
        _mm_slli_epi16::<8>(_mm_sll_epi16(
            _mm_unpacklo_epi8(_mm_loadl_epi64(cg.as_ptr().cast()), zero),
            count,
        )),
        _mm_cvtsi32_si128(8),
    );
    let y = _mm_unpacklo_epi8(_mm_loadl_epi64(y.as_ptr().cast()), zero);
    let t = _mm_sub_epi16(y, cg);
    let r = _mm_packus_epi16(_mm_add_epi16(t, co), zero);
    let g = _mm_packus_epi16(_mm_add_epi16(y, cg), zero);
    let b = _mm_packus_epi16(_mm_sub_epi16(t, co), zero);
    let (first, last) = if alpha { (r, b) } else { (b, r) };
    pack(first, g, last, out);
}
