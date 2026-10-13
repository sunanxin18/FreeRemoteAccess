#!/usr/bin/env python3
"""编译实际 public PDU/session harness 和显式 u32 arithmetic-only proof。"""
import argparse
import subprocess
import tempfile
from pathlib import Path
import sys
sys.dont_write_bytecode = True

from surface_support import CARGO, RUSTC, ENV, ROOT, PDU, SOURCE, artifacts, flags, arithmetic_source, observed_session_source

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--release', action='store_true')
    parser.add_argument('--original-red', action='store_true')
    args = parser.parse_args()
    libs = artifacts(original=args.original_red, release=args.release)
    mode = 'release' if args.release else 'debug'
    root = ROOT / 'target'
    if not args.original_red:
        for name in ['surface_public', 'surface_session']:
            binary = root / ('task-4a27p-' + name + '-' + mode)
            subprocess.run([RUSTC, '--test', str(PDU / 'tests' / (name + '.rs')), '-o', str(binary)] + flags(libs, args.release), check=True)
            subprocess.run([str(binary)], check=True)
    source = (PDU / SOURCE).read_text()
    if args.original_red:
        # 已冻结的完整原源码；PDU 自身尚未改动。
        import json
        originals = dict(line.split('  ', 1)[::-1] for line in (PDU / 'UPSTREAM-SHA256SUMS').read_text().splitlines())
        from surface_support import digest
        assert digest(source.encode()) == originals[SOURCE]
    leaf = root / ('task-4a27p-u32-arithmetic-' + mode + '.rs')
    leaf.write_text(arithmetic_source(source))
    binary = leaf.with_suffix('')
    subprocess.run([RUSTC, '--test', str(leaf), '-o', str(binary)] + flags(libs, args.release), check=True)
    result = subprocess.run([str(binary)])
    if args.original_red:
        assert result.returncode != 0, 'original checked arithmetic gate unexpectedly present'
        print('EXPECTED RED: exact original extracted u32 arithmetic has no typed overflow gate; this is arithmetic-only, not i686 runtime')
    else:
        result.check_returncode()
        # 真实session-source观察与合成u32错误传播，分别运行，避免混同证明层。
        for injected in [False, True]:
            with tempfile.TemporaryDirectory(prefix='task-4a27p-session-', dir=root) as tmp:
                base = Path(tmp)
                path = base / 'session.rs'
                path.write_text(observed_session_source(base, source, injected))
                binary = base / 'session'
                session_libs = {k: v for k, v in libs.items() if k != 'ironrdp_session'}
                cmd = [RUSTC, '--test', str(path), '-o', str(binary), '--cfg', 'surface_observed'] + flags(session_libs, args.release)
                if injected:
                    cmd += ['--cfg', 'surface_injected']
                subprocess.run(cmd, check=True)
                filter_name = 'injected_u32_other_dispatch' if injected else 'malformed_surface_front_door'
                subprocess.run([str(binary), filter_name], check=True)

if __name__ == '__main__':
    main()
