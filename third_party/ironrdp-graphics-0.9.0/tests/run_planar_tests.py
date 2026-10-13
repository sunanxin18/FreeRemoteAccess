#!/usr/bin/env python3
"""实际 shipping library + 原始源码模块测试；不改变 Cargo/lock 或复制实现。"""
import argparse
import json
import subprocess
from planar_support import ROOT, VENDOR, artifacts, compiler_args, source_modules


def harness_source(base):
    test=json.dumps(str(VENDOR/'tests/planar_wire.rs'))
    modules=source_modules(base)
    modules=modules.replace(' } pub use decoder::*;', f' #[path={test}] mod planar_wire; }} pub use decoder::*;')
    # expect-test is absent offline. This small exact debug snapshot assertion runs all existing
    # rle.rs upstream assertions; it never changes/skips the production implementation.
    adapter='''extern crate self as expect_test;
#[macro_export] macro_rules! expect { ([$value:literal]) => { $crate::Snapshot($value) }; }
pub struct Snapshot(&'static str);
impl Snapshot { pub fn assert_debug_eq(&self, value: &impl core::fmt::Debug) {
let indent=self.0.lines().filter(|line| !line.trim().is_empty()).map(|line|line.len()-line.trim_start().len()).min().unwrap_or(0);
let expected=self.0.lines().map(|line|line.get(indent..).unwrap_or("")).collect::<Vec<_>>().join("\\n");
assert_eq!(format!("{value:#?}").trim(),expected.trim()); } }
'''
    return adapter+modules


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--release',action='store_true')
    parser.add_argument('--clippy',action='store_true')
    args=parser.parse_args()
    libs=artifacts(release=args.release)
    mode='release' if args.release else 'debug'
    source=ROOT/'target'/f'task-4a25-planar-{mode}.rs'
    source.write_text(harness_source(VENDOR))
    output=source.with_suffix('')
    compiler='clippy-driver' if args.clippy else 'rustc'
    command=[compiler,'--test',str(source),'-o',str(output)]+compiler_args(libs,args.release)
    if args.clippy: command += ['-D','warnings']
    print('actual source harness:',source,flush=True)
    subprocess.run(command,check=True)
    subprocess.run([str(output)],check=True)


if __name__=='__main__':
    main()
