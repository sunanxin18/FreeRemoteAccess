pub(super) fn ssse3_available() -> bool {
    let detected = is_x86_feature_detected!("ssse3");
    #[cfg(test)]
    {
        detected && !DENY_SSSE3.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        detected
    }
}

#[cfg(test)]
std::thread_local! {
    static DENY_SSSE3: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static SSE2_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// 测试只能否决真实 SSSE3；不能制造 SSE2 或其他 CPU 能力。
#[cfg(test)]
pub(super) fn with_ssse3_denied<T>(operation: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            DENY_SSSE3.with(|denied| denied.set(self.0));
        }
    }
    let _restore = Restore(DENY_SSSE3.with(|denied| denied.replace(true)));
    operation()
}

#[cfg(test)]
pub(super) fn sse2_entries() -> usize {
    SSE2_ENTRIES.with(std::cell::Cell::get)
}

#[cfg(target_arch = "x86")]
#[target_feature(enable = "ssse3")]
pub(super) unsafe fn rgba_to_bgrx(source: &[u8], destination: &mut [u8]) {
    use std::arch::x86::{
        _mm_loadu_si128, _mm_or_si128, _mm_set1_epi32, _mm_setr_epi8, _mm_shuffle_epi8,
        _mm_storeu_si128,
    };

    let shuffle = _mm_setr_epi8(2, 1, 0, -1, 6, 5, 4, -1, 10, 9, 8, -1, 14, 13, 12, -1);
    let alpha = _mm_set1_epi32(0xff00_0000_u32 as i32);
    let mut offset = 0;
    while offset + 16 <= source.len() {
        let rgba = _mm_loadu_si128(source.as_ptr().add(offset).cast());
        let bgrx = _mm_or_si128(_mm_shuffle_epi8(rgba, shuffle), alpha);
        _mm_storeu_si128(destination.as_mut_ptr().add(offset).cast(), bgrx);
        offset += 16;
    }
    let tail_len = source.len() - offset;
    if tail_len != 0 {
        // Stage the short tail into a complete vector. This keeps the
        // supported SSSE3 path entirely in the architecture kernel without
        // reading past the source or falling back to the scalar converter.
        let mut rgba_tail = [0_u8; 16];
        rgba_tail[..tail_len].copy_from_slice(&source[offset..]);
        let rgba = _mm_loadu_si128(rgba_tail.as_ptr().cast());
        let bgrx = _mm_or_si128(_mm_shuffle_epi8(rgba, shuffle), alpha);
        let mut bgrx_tail = [0_u8; 16];
        _mm_storeu_si128(bgrx_tail.as_mut_ptr().cast(), bgrx);
        destination[offset..source.len()].copy_from_slice(&bgrx_tail[..tail_len]);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
pub(super) unsafe fn rgba_to_bgrx(source: &[u8], destination: &mut [u8]) {
    use std::arch::x86_64::{
        _mm_loadu_si128, _mm_or_si128, _mm_set1_epi32, _mm_setr_epi8, _mm_shuffle_epi8,
        _mm_storeu_si128,
    };

    let shuffle = _mm_setr_epi8(2, 1, 0, -1, 6, 5, 4, -1, 10, 9, 8, -1, 14, 13, 12, -1);
    let alpha = _mm_set1_epi32(0xff00_0000_u32 as i32);
    let mut offset = 0;
    while offset + 16 <= source.len() {
        let rgba = _mm_loadu_si128(source.as_ptr().add(offset).cast());
        let bgrx = _mm_or_si128(_mm_shuffle_epi8(rgba, shuffle), alpha);
        _mm_storeu_si128(destination.as_mut_ptr().add(offset).cast(), bgrx);
        offset += 16;
    }
    let tail_len = source.len() - offset;
    if tail_len != 0 {
        // Stage the short tail into a complete vector. This keeps the
        // supported SSSE3 path entirely in the architecture kernel without
        // reading past the source or falling back to the scalar converter.
        let mut rgba_tail = [0_u8; 16];
        rgba_tail[..tail_len].copy_from_slice(&source[offset..]);
        let rgba = _mm_loadu_si128(rgba_tail.as_ptr().cast());
        let bgrx = _mm_or_si128(_mm_shuffle_epi8(rgba, shuffle), alpha);
        let mut bgrx_tail = [0_u8; 16];
        _mm_storeu_si128(bgrx_tail.as_mut_ptr().cast(), bgrx);
        destination[offset..source.len()].copy_from_slice(&bgrx_tail[..tail_len]);
    }
}
