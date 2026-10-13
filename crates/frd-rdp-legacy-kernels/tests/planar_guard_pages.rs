//! 各 plane 与 output 单独位于保护页边界；短尾没有外部 padding。
#![cfg(unix)]
#[path = "common/planar.rs"]
mod oracle;
use frd_rdp_legacy_kernels::{NativeKernels, PlanarLayout};
use oracle::*;
struct Guard {
    base: *mut libc::c_void,
    page: usize,
}
impl Guard {
    fn new() -> Self {
        unsafe {
            let page = usize::try_from(libc::sysconf(libc::_SC_PAGESIZE)).unwrap();
            let base = libc::mmap(
                std::ptr::null_mut(),
                page * 3,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            );
            assert_ne!(base, libc::MAP_FAILED);
            assert_eq!(
                libc::mprotect(
                    base.cast::<u8>().add(page).cast(),
                    page,
                    libc::PROT_READ | libc::PROT_WRITE
                ),
                0
            );
            Self { base, page }
        }
    }
    fn edge(&mut self, n: usize, at_end: bool) -> &mut [u8] {
        assert!(n <= self.page);
        unsafe {
            std::slice::from_raw_parts_mut(
                self.base
                    .cast::<u8>()
                    .add(if at_end { 2 * self.page - n } else { self.page }),
                n,
            )
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            assert_eq!(libc::munmap(self.base, self.page * 3), 0);
        }
    }
}

#[test]
fn planar_full_and_short_blocks_at_both_guard_edges() {
    let k = NativeKernels::detect().unwrap();
    let mut yy = Guard::new();
    let mut uu = Guard::new();
    let mut vv = Guard::new();
    let mut oo = Guard::new();
    for end in [false, true] {
        for n in 0..=65 {
            let y = yy.edge(n, end);
            y.copy_from_slice(&bytes(n, 1));
            let co = uu.edge(n, end);
            co.copy_from_slice(&bytes(n, 2));
            let cg = vv.edge(n, end);
            cg.copy_from_slice(&bytes(n, 3));
            let out = oo.edge(n * 3, end);
            let mut expected = vec![0; n * 3];
            rgb(y, co, cg, &mut expected);
            k.rgb_planes_to_rgb24(y, co, cg, out).unwrap();
            assert_eq!(out, expected);
            let out = oo.edge(n, end);
            out.copy_from_slice(cg);
            let mut expected = cg.to_vec();
            delta(y, &mut expected);
            k.resolve_planar_delta(y, out).unwrap();
            assert_eq!(out, expected);
        }
        for width in 1usize..=65 {
            for height in [1usize, 2, 3, 7] {
                for sub in [false, true] {
                    let n = width * height;
                    let c = if sub {
                        width.div_ceil(2) * height.div_ceil(2)
                    } else {
                        n
                    };
                    let y = yy.edge(n, end);
                    y.copy_from_slice(&bytes(n, 1));
                    let co = uu.edge(c, end);
                    co.copy_from_slice(&bytes(c, 2));
                    let cg = vv.edge(c, end);
                    cg.copy_from_slice(&bytes(c, 3));
                    let out = oo.edge(n * 3, end);
                    for cll in 1..=7 {
                        for has_alpha in [false, true] {
                            let l = PlanarLayout {
                                width,
                                height,
                                subsampled: sub,
                                color_loss_level: cll,
                                has_alpha,
                            };
                            let mut expected = vec![0; 3 * n];
                            color(l, y, co, cg, &mut expected);
                            k.ycocg_planes_to_rgb24(l, y, co, cg, out).unwrap();
                            assert_eq!(out, expected);
                        }
                    }
                }
            }
        }
    }
}
