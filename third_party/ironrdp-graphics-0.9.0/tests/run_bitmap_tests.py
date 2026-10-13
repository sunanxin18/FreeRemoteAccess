#!/usr/bin/env python3
"""用 locked/offline kernel artifact 对实际生产 rle.rs 执行独立测试，不复制 parser。"""
import argparse
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--release', action='store_true')
    parser.add_argument('--clippy', action='store_true')
    args = parser.parse_args()
    vendor = Path(__file__).resolve().parents[1]
    root = vendor.parent.parent
    identity = subprocess.check_output(['rustc', '--version', '--verbose'], text=True)
    if not identity.startswith('rustc 1.96.0 '):
        raise SystemExit('使用固定 Rust 1.96.0 PATH')
    print(identity, flush=True)
    command = ['cargo', 'build', '--offline', '--locked', '--manifest-path', str(root / 'Cargo.toml'),
               '-p', 'frd-rdp-legacy-kernels', '--message-format=json']
    if args.release:
        command += ['--release']
    process = subprocess.run(command, text=True, stdout=subprocess.PIPE, check=True)
    artifacts = [json.loads(line) for line in process.stdout.splitlines() if line.startswith('{')]
    files = [Path(name) for artifact in artifacts if artifact.get('reason') == 'compiler-artifact'
             and artifact['target']['name'] == 'frd_rdp_legacy_kernels'
             for name in artifact['filenames'] if name.endswith('.rlib')]
    if len(files) != 1:
        raise SystemExit(f'kernel artifact 不唯一: {files}')
    rlib = files[0]
    mode = 'release' if args.release else 'debug'
    output = root / 'target' / f'task-4a24-bitmap-{mode}'
    compiler = 'clippy-driver' if args.clippy else 'rustc'
    command = [compiler, '--edition=2024', '--test', str(vendor / 'src/rle.rs'), '--extern',
               f'frd_rdp_legacy_kernels={rlib}', '-L', f'dependency={rlib.parent}', '-o', str(output)]
    if args.release:
        command += ['-C', 'opt-level=3', '-C', 'lto=thin', '-C', 'embed-bitcode=yes']
    if args.clippy:
        command += ['-D', 'warnings']
    print('kernel artifact:', rlib, flush=True)
    print('compile:', ' '.join(command), flush=True)
    subprocess.run(command, check=True)
    subprocess.run([str(output), '--nocapture'], check=True)


if __name__ == '__main__':
    main()
