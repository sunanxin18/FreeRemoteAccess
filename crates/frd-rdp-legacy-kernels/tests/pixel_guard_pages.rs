//! 输入/输出两端真实 PROT_NONE 页，palette 为只读且最后 RGB 项紧贴保护页。
#![cfg(unix)]
#[path = "common/pixel.rs"]
mod frozen;
use frd_rdp_legacy_kernels::{NativeKernels, PackedDepth, PixelRowSource};
use frozen::*;
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
    fn edge(&mut self, n: usize, end: bool) -> &mut [u8] {
        assert!(n <= self.page);
        unsafe {
            std::slice::from_raw_parts_mut(
                self.base
                    .cast::<u8>()
                    .add(if end { 2 * self.page - n } else { self.page }),
                n,
            )
        }
    }
    fn readonly(&self) {
        unsafe {
            assert_eq!(
                libc::mprotect(
                    self.base.cast::<u8>().add(self.page).cast(),
                    self.page,
                    libc::PROT_READ
                ),
                0
            );
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
fn all_row_formats_and_short_tails_touch_only_their_exact_guarded_ranges() {
    let k = NativeKernels::detect().unwrap();
    let mut input = Guard::new();
    let mut output = Guard::new();
    for record in records() {
        for end in [false, true] {
            let mut palette = Guard::new();
            let guarded_palette = if let Some(pal) = &record.palette {
                let data = palette.edge(768, true);
                for (pixel, original) in data.chunks_exact_mut(3).zip(pal) {
                    pixel.copy_from_slice(original);
                }
                let ptr = data.as_ptr().cast::<[[u8; 3]; 256]>();
                palette.readonly();
                Some(unsafe { &*ptr })
            } else {
                None
            };
            for n in 0..=65 {
                let source = input.edge(n * record.width(), end);
                source.copy_from_slice(&record.source[..n * record.width()]);
                let out = output.edge(n * 4, end);
                k.convert_pixel_row(
                    record.source_format,
                    record.destination_format,
                    source,
                    guarded_palette,
                    out,
                )
                .unwrap();
                assert_eq!(out, &record.expected[..n * 4]);
            }
            // first index255 在 n1 的短尾和 n4 bulk 都读到palette最后3byte。
            if record.source_format == PixelRowSource::Packed(PackedDepth::Index8) {
                assert_eq!(record.source[0], 255);
            }
        }
    }
}
