//! 只管理已检查的布局与有限 staging；数值、chroma duplicate 与 RGB pack 在 ISA 层。
use crate::PlanarLayout;
#[cfg(target_arch = "aarch64")]
#[path = "planar_neon.rs"]
mod arch;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[path = "planar_x86.rs"]
mod arch;

pub(super) unsafe fn delta(previous: &[u8], current: &mut [u8]) {
    let full = current.len() / 16 * 16;
    for i in (0..full).step_by(16) {
        arch::delta16(
            previous[i..i + 16].try_into().unwrap(),
            (&mut current[i..i + 16]).try_into().unwrap(),
        );
    }
    let n = current.len() - full;
    if n != 0 {
        let mut p = [0; 16];
        let mut d = [0; 16];
        p[..n].copy_from_slice(&previous[full..]);
        d[..n].copy_from_slice(&current[full..]);
        arch::delta16(&p, &mut d);
        current[full..].copy_from_slice(&d[..n]);
    }
}
unsafe fn stage8(src: &[u8]) -> [u8; 8] {
    let mut stage = [0; 8];
    stage[..src.len()].copy_from_slice(src);
    stage
}
pub(super) unsafe fn rgb(r: &[u8], g: &[u8], b: &[u8], out: &mut [u8]) {
    let full = r.len() / 8 * 8;
    for i in (0..full).step_by(8) {
        arch::pack8(
            r[i..i + 8].try_into().unwrap(),
            g[i..i + 8].try_into().unwrap(),
            b[i..i + 8].try_into().unwrap(),
            (&mut out[i * 3..i * 3 + 24]).try_into().unwrap(),
        );
    }
    if full != r.len() {
        let mut packed = [0; 24];
        arch::pack8(
            &stage8(&r[full..]),
            &stage8(&g[full..]),
            &stage8(&b[full..]),
            &mut packed,
        );
        out[full * 3..].copy_from_slice(&packed[..(r.len() - full) * 3]);
    }
}
unsafe fn chroma8(src: &[u8], subsampled: bool) -> [u8; 8] {
    if subsampled {
        let mut stage = [0; 4];
        stage[..src.len()].copy_from_slice(src);
        arch::duplicate4(&stage)
    } else {
        stage8(src)
    }
}
pub(super) unsafe fn ycocg(l: PlanarLayout, y: &[u8], co: &[u8], cg: &[u8], out: &mut [u8]) {
    let cw = if l.subsampled {
        l.width.div_ceil(2)
    } else {
        l.width
    };
    for row in 0..l.height {
        let yr = row * l.width;
        let cr = if l.subsampled { row / 2 * cw } else { yr };
        for x in (0..l.width).step_by(8) {
            let n = (l.width - x).min(8);
            let c = if l.subsampled { x / 2 } else { x };
            let cn = if l.subsampled { n.div_ceil(2) } else { n };
            let ys = &y[yr + x..yr + x + n];
            let cs = &co[cr + c..cr + c + cn];
            let gs = &cg[cr + c..cr + c + cn];
            if n == 8 {
                // 完整块直接借用精确数组，避免每八 pixels 的动态 staging 搬运。
                let dst = (&mut out[(yr + x) * 3..(yr + x + 8) * 3])
                    .try_into()
                    .unwrap();
                if l.subsampled {
                    let cs = arch::duplicate4(cs.try_into().unwrap());
                    let gs = arch::duplicate4(gs.try_into().unwrap());
                    arch::color8(
                        ys.try_into().unwrap(),
                        &cs,
                        &gs,
                        l.color_loss_level - 1,
                        l.has_alpha,
                        dst,
                    );
                } else {
                    arch::color8(
                        ys.try_into().unwrap(),
                        cs.try_into().unwrap(),
                        gs.try_into().unwrap(),
                        l.color_loss_level - 1,
                        l.has_alpha,
                        dst,
                    );
                }
            } else {
                let cs = chroma8(cs, l.subsampled);
                let gs = chroma8(gs, l.subsampled);
                let mut packed = [0; 24];
                // 与完整块相同的固定八 lane 函数；不能按尾长度 scalar 化。
                arch::color8(
                    &stage8(ys),
                    &cs,
                    &gs,
                    l.color_loss_level - 1,
                    l.has_alpha,
                    &mut packed,
                );
                out[(yr + x) * 3..(yr + x + n) * 3].copy_from_slice(&packed[..n * 3]);
            }
        }
    }
}
