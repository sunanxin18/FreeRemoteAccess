#![allow(dead_code)]
use frd_rdp_legacy_kernels::PlanarLayout;
pub const DIMENSIONS: [usize; 11] = [1, 2, 3, 7, 8, 15, 16, 17, 63, 64, 65];
pub fn bytes(n: usize, seed: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 37 + seed * 71) as u8).collect()
}
pub fn delta(previous: &[u8], current: &mut [u8]) {
    for (p, d) in previous.iter().zip(current) {
        let signed = if *d & 1 == 0 {
            i16::from(*d) / 2
        } else {
            -((i16::from(*d) + 1) / 2)
        };
        *d = (i16::from(*p) + signed).rem_euclid(256) as u8;
    }
}
pub fn rgb(r: &[u8], g: &[u8], b: &[u8], out: &mut [u8]) {
    for (i, p) in out.chunks_exact_mut(3).enumerate() {
        p.copy_from_slice(&[r[i], g[i], b[i]]);
    }
}
pub fn color(l: PlanarLayout, y: &[u8], co: &[u8], cg: &[u8], out: &mut [u8]) {
    let cw = if l.subsampled {
        l.width.div_ceil(2)
    } else {
        l.width
    };
    for (i, p) in out.chunks_exact_mut(3).enumerate() {
        let c = if l.subsampled {
            (i / l.width / 2) * cw + (i % l.width / 2)
        } else {
            i
        };
        let shift = u32::from(l.color_loss_level - 1);
        let co = i16::from(co[c].wrapping_shl(shift) as i8);
        let cg = i16::from(cg[c].wrapping_shl(shift) as i8);
        let y = i16::from(y[i]);
        let r = (y - cg + co).clamp(0, 255) as u8;
        let g = (y + cg).clamp(0, 255) as u8;
        let b = (y - cg - co).clamp(0, 255) as u8;
        let pixel = if l.has_alpha { [r, g, b] } else { [b, g, r] };
        p.copy_from_slice(&pixel);
    }
}
pub struct Record<'a> {
    pub width: usize,
    pub height: usize,
    pub header: u8,
    pub wire: &'a [u8],
    pub expected: &'a [u8],
}
pub fn records(mut blob: &[u8]) -> Vec<Record<'_>> {
    assert_eq!(&blob[..8], b"FRDPLN01");
    blob = &blob[8..];
    let mut records = Vec::new();
    while !blob.is_empty() {
        let values: [usize; 5] = std::array::from_fn(|i| {
            u32::from_le_bytes(blob[4 * i..4 * i + 4].try_into().unwrap()) as usize
        });
        blob = &blob[20..];
        let wire = &blob[..values[3]];
        blob = &blob[values[3]..];
        let expected = &blob[..values[4]];
        blob = &blob[values[4]..];
        records.push(Record {
            width: values[0],
            height: values[1],
            header: values[2] as u8,
            wire,
            expected,
        });
    }
    records
}
