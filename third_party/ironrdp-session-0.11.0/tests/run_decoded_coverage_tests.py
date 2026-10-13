#!/usr/bin/env python3
"""Fixed Rust 1.96 actual-source coverage harness; offline/locked dependencies."""
import argparse,json,os,subprocess
from pathlib import Path
from rfx_support import ROOT,S,artifacts,flags,session_source

def source(base=S, optional=False, production=False):
    value=session_source(base)
    if not production:
        wrapper=ROOT/'target'/f'task-4a28b-{os.getpid()}-image.rs'
        observers = ('#[cfg(test)] pub(crate) fn optional_native_calls_clear() { NATIVE_IMAGE_CALLS.with(|v| v.borrow_mut().clear()); }\n'
            '#[cfg(test)] pub(crate) fn optional_native_calls_count() -> usize { NATIVE_IMAGE_CALLS.with(|v| v.borrow().len()) }\n') if optional else ''
        wrapper.write_text('include!('+json.dumps(str(base/'src/image.rs'))+');\n'+observers+
            '#[cfg(test)] mod original_image_tests { include!('+json.dumps(str(S/'tests/image_consumers_native.rs'))+'); }\n'
            '#[cfg(test)] mod decoded_coverage_tests { include!('+json.dumps(str(S/'tests/decoded_coverage.rs'))+'); }\n')
        value=value.replace('#[path='+json.dumps(str(base/'src/image.rs'))+'] pub mod image;',
            '#[path='+json.dumps(str(wrapper))+'] pub mod image;')
        if optional:
            wrapper=ROOT/'target'/f'task-4a28b-{os.getpid()}-fast.rs'
            wrapper.write_text('include!('+json.dumps(str(base/'src/fast_path.rs'))+');\n'
                '#[cfg(test)] pub(crate) fn coverage_fail(size: usize) { original_optional_tests::coverage_fail(size); }\n'
                '#[cfg(test)] mod original_optional_tests { include!('+json.dumps(str(S/'tests/image_optional_wire.rs'))+'); pub(super) fn coverage_fail(size: usize) { FAIL_ALLOCATION.with(|v| v.set(size)); } }\n')
            value=value.replace('#[path='+json.dumps(str(base/'src/fast_path.rs'))+'] pub mod fast_path;',
                '#[path='+json.dumps(str(wrapper))+'] pub mod fast_path;')
    if not production:
        if optional:
            value += '\n#[cfg(test)] pub(crate) fn coverage_fail(size: usize) { fast_path::coverage_fail(size); }\n'
        else:
            value += r"""
#[cfg(test)] pub(crate) fn coverage_fail(size: usize) { coverage_allocator::fail(size); }
#[cfg(test)] mod coverage_allocator {
    use std::alloc::{GlobalAlloc, Layout, System};
    std::thread_local! { static FAIL: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
    pub(super) fn fail(size: usize) { FAIL.with(|v| v.set(size)); }
    fn failed(size: usize) -> bool {
        FAIL.try_with(|v| if v.get() != 0 && v.get() == size { v.set(0); true } else { false }).unwrap_or(false)
    }
    struct Probe;
    unsafe impl GlobalAlloc for Probe {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if failed(layout.size()) { core::ptr::null_mut() } else { unsafe { System.alloc(layout) } }
        }
        unsafe fn dealloc(&self, p: *mut u8, layout: Layout) { unsafe { System.dealloc(p, layout) } }
        unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            if failed(size) { core::ptr::null_mut() } else { unsafe { System.realloc(p, layout, size) } }
        }
    }
    #[global_allocator] static ALLOCATOR: Probe = Probe;
}
"""
    return value

def build_flags(libs,release=False,optional=False):
    if optional:
        from optional_image_support import optional_artifacts,optional_flags
        extra,native=optional_artifacts(release)
        return optional_flags(libs,extra,native,release)
    return flags(libs,release)

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--release',action='store_true');p.add_argument('--clippy',action='store_true')
    p.add_argument('--optional',action='store_true');p.add_argument('--production',action='store_true')
    p.add_argument('--source',type=Path);p.add_argument('--filter',default='');a=p.parse_args()
    libs=artifacts(release=a.release);mode='release' if a.release else 'debug'
    variant=mode+('-optional' if a.optional else '-default')+('-production' if a.production else '-tests');path=ROOT/'target'/f'task-4a28b-session-{variant}.rs';path.write_text(source(a.source or S,a.optional,a.production));out=path.with_suffix('')
    cmd=['clippy-driver' if a.clippy else 'rustc', '--crate-type=lib' if a.production else '--test',str(path),'-o',str(out)]+build_flags(libs,a.release,a.optional)
    if a.clippy:cmd+=['-D','warnings','-A','clippy::collapsible_if']
    subprocess.run(cmd,check=True)
    if not a.production:subprocess.run([str(out),a.filter,'--nocapture'],check=True)
    print('PASS actual session source',mode,'optional='+str(a.optional),'production='+str(a.production))
if __name__=='__main__':main()
