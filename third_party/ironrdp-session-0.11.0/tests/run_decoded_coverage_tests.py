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
        allocator_bridge = ('#[cfg(test)] pub(crate) fn coverage_fail(size: usize) { cursor_consumers::fail_allocation(size); }\n'
            '#[cfg(test)] pub(crate) fn coverage_failure_size() -> usize { cursor_consumers::allocation_failure_size() }\n')
        if optional:
            allocator_bridge += '#[cfg(test)] pub(crate) fn coverage_allocation_is_denied(size: usize) -> bool { cursor_consumers::allocation_is_denied(size) }\n'
        wrapper.write_text('include!('+json.dumps(str(base/'src/image.rs'))+');\n'+observers+allocator_bridge+
            '#[cfg(test)] mod original_image_tests { include!('+json.dumps(str(S/'tests/image_consumers_native.rs'))+'); }\n'
            '#[cfg(test)] mod decoded_coverage_tests { include!('+json.dumps(str(S/'tests/decoded_coverage.rs'))+'); }\n')
        value=value.replace('#[path='+json.dumps(str(base/'src/image.rs'))+'] pub mod image;',
            '#[path='+json.dumps(str(wrapper))+'] pub mod image;')
        if optional:
            wrapper=ROOT/'target'/f'task-4a28b-{os.getpid()}-fast.rs'
            wrapper.write_text('include!('+json.dumps(str(base/'src/fast_path.rs'))+');\n'
                '#[cfg(test)] mod original_optional_tests { include!('+json.dumps(str(S/'tests/image_optional_wire.rs'))+'); }\n')
            value=value.replace('#[path='+json.dumps(str(base/'src/fast_path.rs'))+'] pub mod fast_path;',
                '#[path='+json.dumps(str(wrapper))+'] pub mod fast_path;')
    if not production:
        value += ('\n#[cfg(test)] pub(crate) fn coverage_fail(size: usize) { image::coverage_fail(size); }\n'
            '#[cfg(test)] pub(crate) fn coverage_failure_size() -> usize { image::coverage_failure_size() }\n')
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
    cmd+=['--check-cfg','cfg(test)', '--check-cfg','cfg(feature, values("qoi", "qoiz"))',
        '--check-cfg','cfg(frd_decoded_coverage_optional_allocator)']
    if a.optional and not a.production:cmd+=['--cfg','frd_decoded_coverage_optional_allocator']
    if a.clippy:cmd+=['-D','warnings','-A','clippy::collapsible_if']
    subprocess.run(cmd,check=True)
    if not a.production:subprocess.run([str(out),a.filter,'--nocapture'],check=True)
    print('PASS actual session source',mode,'optional='+str(a.optional),'production='+str(a.production))
if __name__=='__main__':main()
