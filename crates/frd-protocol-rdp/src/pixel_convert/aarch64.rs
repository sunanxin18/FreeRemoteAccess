use super::rgba_to_bgrx_scalar;

#[target_feature(enable = "neon")]
pub(super) unsafe fn rgba_to_bgrx(source: &[u8], destination: &mut [u8]) {
    use std::arch::aarch64::{uint8x8x4_t, vdup_n_u8, vld4_u8, vst4_u8};

    let mut offset = 0;
    while offset + 32 <= source.len() {
        let rgba = vld4_u8(source.as_ptr().add(offset));
        let bgrx = uint8x8x4_t(rgba.2, rgba.1, rgba.0, vdup_n_u8(0xff));
        vst4_u8(destination.as_mut_ptr().add(offset), bgrx);
        offset += 32;
    }
    rgba_to_bgrx_scalar(&source[offset..], &mut destination[offset..]);
}
