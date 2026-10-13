//! 每个输入/输出紧靠保护页，覆盖零长度及所有 16-byte 尾部；不提供外部 padding。
#![cfg(unix)]
#[path = "common/bitmap.rs"]
mod oracle;
use frd_rdp_legacy_kernels::NativeKernels;
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
fn packed_inputs_outputs_and_pixel_parameters_end_at_guard_pages() {
    let k = NativeKernels::detect().unwrap();
    let mut source = Guard::new();
    let mut output = Guard::new();
    let mut a = Guard::new();
    let mut b = Guard::new();
    for at_end in [false, true] {
        for (depth, width) in DEPTHS {
            let fg = a.edge(width, at_end);
            fg.copy_from_slice(&[0x91, 0x42, 0x73][..width]);
            let other = b.edge(width, at_end);
            other.copy_from_slice(&[0x24, 0x85, 0xc6][..width]);
            for n in 0..=65 {
                let previous = source.edge(n * width, at_end);
                previous.copy_from_slice(&bytes(n * width));
                let out = output.edge(n * width, at_end);
                k.copy_u8(previous, out).unwrap();
                assert_eq!(out, previous);
                k.fill_pixel(depth, fg, out).unwrap();
                assert_eq!(out, fill(fg, n));
                k.xor_previous(depth, previous, fg, out).unwrap();
                assert_eq!(out, xor(previous, fg));
                for phase in [false, true] {
                    k.dither_pixels(depth, fg, other, phase, out).unwrap();
                    assert_eq!(out, dither(fg, other, phase, n));
                }
            }
            for offset in 0..=8 {
                for n in 0..=8 - offset {
                    let previous = source.edge(n * width, at_end);
                    previous.copy_from_slice(&bytes(n * width));
                    let out = output.edge(n * width, at_end);
                    for m in 0..=255 {
                        for bg in [None, Some(&*previous)] {
                            k.mask_previous(depth, bg, fg, m, offset as u8, out)
                                .unwrap();
                            assert_eq!(out, mask(bg, fg, m, offset as u8, n));
                        }
                    }
                }
            }
        }
    }
}
