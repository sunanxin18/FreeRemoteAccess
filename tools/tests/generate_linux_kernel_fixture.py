#!/usr/bin/env python3
"""实际已批准276字节核的ELF包装夹具；只交叉链接，不执行或另写计算公式。"""
import argparse
import hashlib
from pathlib import Path
import shutil
import subprocess

FIXTURE = Path(__file__).resolve().parent / 'fixtures/macos-h264dc-kernel.bin'
CODE_SHA = '1e70d378b34fb196afd0942b9bd0c31c1715b4042c6c3e8f032dad5e76b42476'


def generate(output, stripped=False):
    data = FIXTURE.read_bytes()
    if len(data) != 276 or hashlib.sha256(data).hexdigest() != CODE_SHA:
        raise ValueError('实际数学fixture bytes身份不匹配')
    clang, linker = shutil.which('clang'), shutil.which('ld.lld')
    if not clang or not linker:
        raise ValueError('ELF fixture需要已安装clang/ld.lld；不自动安装')
    assembly, obj = output.with_suffix('.S'), output.with_suffix('.o')
    assembly.write_text('.text\n.p2align 2\n'
                        '.global ff_h264_luma_dc_dequant_idct_neon\n'
                        '.hidden ff_h264_luma_dc_dequant_idct_neon\n'
                        '.type ff_h264_luma_dc_dequant_idct_neon,%function\n'
                        'ff_h264_luma_dc_dequant_idct_neon:\n'
                        f'.incbin "{FIXTURE}"\n'
                        '.size ff_h264_luma_dc_dequant_idct_neon,.-ff_h264_luma_dc_dequant_idct_neon\n'
                        '.global fixture_end\nfixture_end:\n')
    subprocess.run([clang, '-target', 'aarch64-linux-gnu', '-c', str(assembly), '-o', str(obj)],
                   check=True, capture_output=True)
    subprocess.run([linker, '-shared', '--soname=' + output.name, *(['--strip-all'] if stripped else []),
                    str(obj), '-o', str(output)], check=True, capture_output=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--stripped', action='store_true')
    args = parser.parse_args()
    generate(args.output, args.stripped)
