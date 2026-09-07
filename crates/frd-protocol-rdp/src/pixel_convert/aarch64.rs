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
    let tail_len = source.len() - offset;
    if tail_len != 0 {
        // Stage the short tail into a complete NEON interleaved vector. The
        // supported AArch64 path therefore never calls the scalar converter.
        let mut rgba_tail = [0_u8; 32];
        rgba_tail[..tail_len].copy_from_slice(&source[offset..]);
        let rgba = vld4_u8(rgba_tail.as_ptr());
        let bgrx = uint8x8x4_t(rgba.2, rgba.1, rgba.0, vdup_n_u8(0xff));
        let mut bgrx_tail = [0_u8; 32];
        vst4_u8(bgrx_tail.as_mut_ptr(), bgrx);
        destination[offset..source.len()].copy_from_slice(&bgrx_tail[..tail_len]);
    }
}
