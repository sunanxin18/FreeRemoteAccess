use crate::format::{self, Records};
use ironrdp_graphics::{
    color_conversion::{ycbcr_to_rgba, YCbCrBuffer},
    dwt, quantization, subband_reconstruction,
};
use ironrdp_pdu::codecs::rfx::Quant;
pub fn samples(n: usize, mut state: u32) -> Vec<i16> {
    (0..n)
        .map(|_| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 16) as i16
        })
        .collect()
}
pub fn quant(q: [u8; 10]) -> Quant {
    Quant {
        hl1: q[0],
        lh1: q[1],
        hh1: q[2],
        hl2: q[3],
        lh2: q[4],
        hh2: q[5],
        hl3: q[6],
        lh3: q[7],
        hh3: q[8],
        ll3: q[9],
    }
}
pub fn qbytes(q: &Quant) -> [u8; 10] {
    [
        q.hl1, q.lh1, q.hh1, q.hl2, q.lh2, q.hh2, q.hl3, q.lh3, q.hh3, q.ll3,
    ]
}
pub fn valid(y: i16, cb: i16, cr: i16) -> bool {
    let Some(y) = y
        .checked_add(4096)
        .map(i32::from)
        .and_then(|y| y.checked_mul(65536))
    else {
        return false;
    };
    let (Some(rc), Some(gc), Some(gu), Some(bc)) = (
        i32::from(cr).checked_mul(91915),
        i32::from(cr).checked_mul(46818),
        i32::from(cb).checked_mul(22526),
        i32::from(cb).checked_mul(115992),
    ) else {
        return false;
    };
    y.checked_add(rc).is_some()
        && y.checked_sub(gc).and_then(|x| x.checked_sub(gu)).is_some()
        && y.checked_add(bc).is_some()
}
fn record(r: &mut Records, op: u8, input: &[i16], q: Option<[u8; 10]>) {
    let bytes = format::i16_bytes(input);
    let mut out = input.to_vec();
    let key = match op {
        1 => {
            let q = q.unwrap();
            quantization::decode(&mut out, &quant(q));
            format::key(op, &[&bytes, &q])
        }
        2 => {
            dwt::decode(&mut out, &mut [0; 4096]);
            format::key(op, &[&bytes])
        }
        3 => {
            subband_reconstruction::decode(&mut out);
            format::key(op, &[&bytes])
        }
        _ => unreachable!(),
    };
    r.insert(key, format::i16_bytes(&out));
}
fn color(r: &mut Records, y: &[i16], cb: &[i16], cr: &[i16]) {
    let key = format::key(
        4,
        &[
            &format::i16_bytes(y),
            &format::i16_bytes(cb),
            &format::i16_bytes(cr),
        ],
    );
    let mut out = vec![0; y.len() * 4];
    ycbcr_to_rgba(YCbCrBuffer { y, cb, cr }, &mut out).unwrap();
    r.insert(key, out);
}
pub fn generate() -> Records {
    let mut r = Records::new();
    for value in 0..=16 {
        for band in 0..10 {
            let mut q = [1; 10];
            q[band] = value;
            record(&mut r, 1, &samples(4096, band as u32 + 42), Some(q));
        }
    }
    for i in 0..4096 {
        let mut v = [0; 4096];
        v[i] = if i % 2 == 0 { 32767 } else { -32768 };
        record(&mut r, 2, &v, None);
    }
    for seed in 0..12 {
        record(&mut r, 2, &samples(4096, seed), None);
    }
    for val in [i16::MIN, i16::MAX, -1, 0, 1] {
        record(&mut r, 2, &[val; 4096], None);
    }
    for len in 0..=137 {
        for offset in 0..8 {
            let v = samples(len + 16, 37);
            record(&mut r, 3, &v[offset..offset + len], None);
        }
    }
    for val in [i16::MIN, i16::MAX, -1, 1] {
        record(&mut r, 3, &[val; 64], None);
    }
    for p in samples(300000, 23).chunks_exact(3) {
        if valid(p[0], p[1], p[2]) {
            color(&mut r, &p[..1], &p[1..2], &p[2..]);
        }
    }
    for len in 1..=137 {
        for offset in 0..8 {
            let planes: Vec<Vec<i16>> = [9, 13, 7]
                .iter()
                .map(|s| {
                    samples(len + offset, *s)
                        .into_iter()
                        .map(|x| x / 8)
                        .collect()
                })
                .collect();
            color(
                &mut r,
                &planes[0][offset..],
                &planes[1][offset..],
                &planes[2][offset..],
            );
        }
    }
    r
}
