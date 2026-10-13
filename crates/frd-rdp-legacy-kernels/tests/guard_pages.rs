//! 将 slice 末端紧贴不可访问页面，捕获哪怕结果被丢弃的 padded load/store。
#![cfg(unix)]
use frd_rdp_legacy_kernels::NativeKernels;
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
    fn tail_i16(&mut self, n: usize) -> &mut [i16] {
        assert!(n * 2 <= self.page);
        unsafe {
            std::slice::from_raw_parts_mut(
                self.base.cast::<u8>().add(2 * self.page - n * 2).cast(),
                n,
            )
        }
    }
    fn tail_u8(&mut self, n: usize) -> &mut [u8] {
        assert!(n <= self.page);
        unsafe { std::slice::from_raw_parts_mut(self.base.cast::<u8>().add(2 * self.page - n), n) }
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
fn vector_tails_end_exactly_at_guard_page() {
    let k = NativeKernels::detect().unwrap();
    let mut y = Guard::new();
    let mut cb = Guard::new();
    let mut cr = Guard::new();
    let mut rgba = Guard::new();
    for n in 0..=65 {
        let ys = y.tail_i16(n);
        ys.fill(1);
        k.differential(ys);
        assert_eq!(ys.to_vec(), (1..=n).map(|x| x as i16).collect::<Vec<_>>());
        k.zero_i16(ys);
        assert!(ys.iter().all(|&x| x == 0));
        let us = cb.tail_i16(n);
        us.fill(0);
        let vs = cr.tail_i16(n);
        vs.fill(0);
        let out = rgba.tail_u8(n * 4);
        out.fill(7);
        k.ycbcr_to_rgba(ys, us, vs, out).unwrap();
        assert!(out.chunks_exact(4).all(|p| p == [128, 128, 128, 255]));
    }
}
