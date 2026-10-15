//! 需要先用独立原版进程生成并固定 hash；本 SOURCE 阶段尚无 fixture 或执行结果。
use frd_rdp_legacy_kernels::{CursorBitmapMode as M, CursorXorFormat as F, NativeKernels};
use sha2::{Digest, Sha256};
#[test]
#[ignore = "root 在独立原版 fixture 生成并冻结 hash 后释放"]
fn fixed_original_full_decoder_rows() {
    let raw = std::fs::read(std::env::var("FRD_CURSOR_ORIGINAL_FIXTURE").unwrap()).unwrap();
    let expected_hash = std::env::var("FRD_CURSOR_ORIGINAL_SHA256").unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(&raw)), expected_hash);
    assert_eq!(&raw[..8], b"FRDCUR01");
    let k = NativeKernels::detect().unwrap();
    let mut p = 8;
    let mut count = 0;
    while p < raw.len() {
        let f = [F::Mono1, F::Rgb565, F::Bgr24, F::Bgra32][raw[p] as usize];
        let m = [
            M::Software,
            M::Accelerated { odd_row: false },
            M::Accelerated { odd_row: true },
        ][raw[p + 1] as usize];
        let has_mask = raw[p + 2] != 0;
        let n = u32::from_le_bytes(raw[p + 4..p + 8].try_into().unwrap()) as usize;
        p += 8;
        let xl = match f {
            F::Mono1 => n.div_ceil(8),
            F::Rgb565 => 2 * n,
            F::Bgr24 => 3 * n,
            F::Bgra32 => 4 * n,
        };
        let x = &raw[p..p + xl];
        p += xl;
        let a = if has_mask {
            let a = &raw[p..p + n.div_ceil(8)];
            p += n.div_ceil(8);
            Some(a)
        } else {
            None
        };
        let expected = &raw[p..p + n * 4];
        p += n * 4;
        let mut out = vec![0; n * 4];
        k.decode_cursor_row(f, m, x, a, &mut out).unwrap();
        assert_eq!(out, expected);
        count += 1;
    }
    assert_eq!(p, raw.len());
    assert_eq!(count, 432);
}
