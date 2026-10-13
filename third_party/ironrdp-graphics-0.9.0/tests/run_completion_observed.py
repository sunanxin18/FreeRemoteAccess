#!/usr/bin/env python3
"""固定 Cargo JSON 库、实际源码观察器、不可变原版夹具与 registry-only 对照。"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
from planar_support import ROOT, VENDOR, ORIGINAL, artifacts, compiler_args, source_modules
from run_planar_tests import harness_source


SNAPSHOT = '''
pub fn observed_snapshot(wire: &[u8], initial: &[u8], w: usize, h: usize) -> (Vec<u8>, String, Vec<u8>) {
 let mut decoder=BitmapStreamDecoder::default(); let mut dst=initial.to_vec();
 let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decoder.decode_bitmap_stream_to_rgb24_observed(wire,&mut dst,w,h)));
 let result=match result {
 Ok(Ok(shape)) => { let valid=w>0 && h>0 && w.checked_mul(h).and_then(|n|n.checked_mul(4)).filter(|&n|n<=isize::MAX as usize).is_some();
 assert_eq!(shape,valid.then_some((w,h))); "Ok(())".to_owned() },
 Ok(Err(e))=>format!("Err({e:?})"), Err(_)=>"Panic geometry".to_owned() };
 (dst,result,decoder.planes_buffer)
}
'''


def test_source(packed=None):
    modules = harness_source(VENDOR)
    decoder = json.dumps(str(VENDOR / 'src/rdp6/bitmap_stream/decoder.rs'))
    modules = modules.replace(f'include!({decoder});', f'include!({decoder}); {SNAPSHOT}')
    return (modules + '\n#[path=' + json.dumps(str(packed or VENDOR / 'src/rle.rs')) + '] pub mod packed;\n'
            + '#[path=' + json.dumps(str(VENDOR / 'tests/completion_observed.rs')) + '] mod completion;\n')


def compile_run(source, libs, output, release=False, clippy=False, test=True, filter_name=None, expect_failure=False):
    path = output.with_suffix('.rs')
    path.write_text(source)
    command = ['clippy-driver' if clippy else 'rustc', str(path), '-o', str(output)] + compiler_args(libs, release)
    if test:
        command += ['--test']
    if clippy:
        command += ['-D', 'warnings']
    subprocess.run(command, check=True)
    result = subprocess.run([str(output)] + ([filter_name] if filter_name else []), text=True, capture_output=True)
    print(result.stdout, flush=True)
    if expect_failure:
        if result.returncode == 0 or 'assertion `left == right` failed' not in result.stdout + result.stderr:
            raise SystemExit('mutant 未因实际完成计数断言失败')
    else:
        if result.returncode:
            print(result.stderr, flush=True)
            raise SystemExit(result.returncode)
    return result.stdout


PROBE = '''
fn main() {
 std::panic::set_hook(Box::new(|_|{}));
 for bpp in [8,15,16,24] { for wire in [&[][..],&[0xfd][..],&[0xfd,0x82][..],&[0x02][..]] {
 let mut dst=vec![0x39,0x67,0x91]; let r=packed::decompress(wire,&mut dst,2,1,bpp);
 println!("packed {bpp} {wire:?}: {r:?} {dst:?}"); }}
 for (wire,w,h) in [(vec![0x20,17,34,51,0],1,1), (vec![0x30,16,17,16,34,16,51],1,1),
 (vec![0,145,17,34,51,0],1,1), (vec![0x29,30,40,50,60,70,80,0,0,0,0,0],3,2), (vec![0x20,0],0,1)] {
 for end in 0..=wire.len() { println!("planar {w} {h} {:?}",rdp6::bitmap_stream::decoder::snapshot(&wire[..end],&[0xa5,0x39],w,h)); }}
}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--release', action='store_true')
    parser.add_argument('--clippy', action='store_true')
    parser.add_argument('--original', action='store_true')
    parser.add_argument('--mutants', action='store_true')
    args = parser.parse_args()
    print(subprocess.check_output(['rustc', '--version', '--verbose'], text=True), flush=True)
    libs = artifacts(release=args.release)
    output_dir = ROOT / 'target/task-4a28a-completion'
    output_dir.mkdir(exist_ok=True)
    mode = 'release' if args.release else 'debug'
    source = test_source()
    compile_run(source, libs, output_dir / (mode + ('-clippy' if args.clippy else '')), args.release, args.clippy,
                filter_name='completion_')
    if args.original:
        oracle_libs = artifacts(original=True, release=args.release)
        def probe(base):
            return source_modules(base, snapshot=True) + '\n#[path=' + json.dumps(str(base / 'src/rle.rs')) + '] pub mod packed;\n' + PROBE
        original = compile_run(probe(ORIGINAL), oracle_libs, output_dir / (mode + '-original'), args.release, test=False)
        current = compile_run(probe(VENDOR), libs, output_dir / (mode + '-current'), args.release, test=False)
        assert original == current, '实际原版/当前 legacy 入口像素、错误、private scratch 不同'
        print('independent original snapshots identical:', len(original.splitlines()),
              'sha256:', hashlib.sha256(original.encode()).hexdigest(), flush=True)
    if args.mutants:
        packed = (VENDOR / 'src/rle.rs').read_text()
        for name, before, after in [
            ('resize-count', 'written: dst.pos,', 'written: dst.inner.len(),'),
            ('no-real-progress', 'written: dst.pos,', 'written: 0,'),
            ('no-source-progress', 'consumed: src.pos,', 'consumed: 0,'),
        ]:
            assert packed.count(before) == 1
            mutated = packed.replace(before, after)
            # 源码副本的旧测试导入仍指向未修改的原文件。
            mutated = mutated.replace('"../tests/bitmap_wire.rs"', json.dumps(str(VENDOR / 'tests/bitmap_wire.rs')))
            path = output_dir / (name + '.rs')
            path.write_text(mutated)
            compile_run(test_source(path), libs, output_dir / (name + '-test'), filter_name='completion_packed_real_cursors', expect_failure=True)
            print('mutant killed:', name, flush=True)


if __name__ == '__main__':
    main()
