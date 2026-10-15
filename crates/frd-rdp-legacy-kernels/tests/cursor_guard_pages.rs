//! 真实保护页；独立子进程隔离 fault，负控制必须证明两端保护页生效。
#![cfg(unix)]
#[path = "common/cursor_reference.rs"]
mod reference;
use frd_rdp_legacy_kernels::NativeKernels;
use reference::*;
use std::os::unix::process::ExitStatusExt;
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
                    .add(if end { self.page * 2 - n } else { self.page }),
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
            )
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe { assert_eq!(libc::munmap(self.base, self.page * 3), 0) }
    }
}
#[test]
fn cursor_exact_ranges_and_fault_controls_run_in_isolated_children() {
    for variant in ["valid", "below", "above"] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cursor_guard_child", "--ignored", "--nocapture"])
            .env("FRD_CURSOR_GUARD_CHILD", variant)
            .status()
            .unwrap();
        if variant == "valid" {
            assert!(status.success());
        } else {
            assert!(
                matches!(status.signal(), Some(libc::SIGSEGV) | Some(libc::SIGBUS)),
                "{variant}: {status}"
            );
        }
    }
}
#[test]
#[ignore = "只由上层隔离子进程派发"]
fn cursor_guard_child() {
    let variant = std::env::var("FRD_CURSOR_GUARD_CHILD").unwrap();
    if variant != "valid" {
        let guard = Guard::new();
        let offset = if variant == "below" {
            guard.page - 1
        } else {
            guard.page * 2
        };
        unsafe {
            std::ptr::read_volatile(guard.base.cast::<u8>().add(offset));
        }
        panic!("保护页没有 fault");
    }
    let k = NativeKernels::detect().unwrap();
    for format in FORMATS {
        for n in WIDTHS {
            for mode in MODES {
                for end in [false, true] {
                    let x = bytes(encoded_len(format, n), 78);
                    let a = bytes(n.div_ceil(8), 14);
                    let mut xs = Guard::new();
                    let mut am = Guard::new();
                    let mut os = Guard::new();
                    let xp = {
                        let data = xs.edge(x.len(), end);
                        data.copy_from_slice(&x);
                        data.as_ptr()
                    };
                    let ap = {
                        let data = am.edge(a.len(), end);
                        data.copy_from_slice(&a);
                        data.as_ptr()
                    };
                    xs.readonly();
                    am.readonly();
                    let xr = unsafe { std::slice::from_raw_parts(xp, x.len()) };
                    let ar = unsafe { std::slice::from_raw_parts(ap, a.len()) };
                    for mask in [None, Some(ar)] {
                        let out = os.edge(n * 4, end);
                        k.decode_cursor_row(format, mode, xr, mask, out).unwrap();
                        assert_eq!(out, decode(format, mode, &x, mask, n));
                    }
                }
            }
        }
    }
    for n in WIDTHS {
        for end in [false, true] {
            let mut s = bytes(n * 4, 39);
            let b = bytes(n * 4, 55);
            for (i, p) in s.chunks_exact_mut(4).enumerate() {
                if i % 3 == 0 {
                    p.copy_from_slice(&[255, 255, 255, 0]);
                } else if i % 3 == 1 {
                    p.copy_from_slice(&[254, 254, 254, 1]);
                }
            }
            let mut sg = Guard::new();
            let mut bg = Guard::new();
            let mut og = Guard::new();
            let sp = {
                let d = sg.edge(s.len(), end);
                d.copy_from_slice(&s);
                d.as_ptr()
            };
            let bp = {
                let d = bg.edge(b.len(), end);
                d.copy_from_slice(&b);
                d.as_ptr()
            };
            sg.readonly();
            bg.readonly();
            let out = og.edge(n * 4, end);
            k.composite_cursor_row(
                unsafe { std::slice::from_raw_parts(sp, s.len()) },
                unsafe { std::slice::from_raw_parts(bp, b.len()) },
                out,
            )
            .unwrap();
            assert_eq!(out, wrapping_reference(&s, &b));
        }
    }
}
