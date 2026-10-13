#!/usr/bin/env python3
"""将已有实际数学 bytes 包装成小型 Mach-O；不重写公式、不构建 FFmpeg。"""
import argparse
import hashlib
from pathlib import Path
import subprocess

FIXTURE = Path(__file__).resolve().parent / 'fixtures/macos-h264dc-kernel.bin'
CODE_SHA = '1e70d378b34fb196afd0942b9bd0c31c1715b4042c6c3e8f032dad5e76b42476'


def generate(output):
    data = FIXTURE.read_bytes()
    if len(data) != 276 or hashlib.sha256(data).hexdigest() != CODE_SHA:
        raise ValueError('实际数学 fixture bytes 身份不匹配')
    assembly = output.with_suffix('.S')
    # 字节通过 .incbin 原样链接；没有重新汇编另一份计算公式。
    assembly.write_text('.section __TEXT,__text,regular,pure_instructions\n.p2align 2\n'
                        '.globl _ff_h264_luma_dc_dequant_idct_neon\n'
                        '_ff_h264_luma_dc_dequant_idct_neon:\n'
                        f'.incbin "{FIXTURE}"\n.globl _fixture_end\n_fixture_end:\n')
    exports = output.with_suffix('.exports')
    exports.write_text('_fixture_end\n')
    subprocess.run(['clang', '-arch', 'arm64', '-mmacosx-version-min=12.0', '-dynamiclib', str(assembly),
                    '-Wl,-headerpad_max_install_names', '-Wl,-install_name,@loader_path/' + output.name,
                    '-Wl,-exported_symbols_list,' + str(exports),
                    '-o', str(output)], check=True, capture_output=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    generate(parser.parse_args().output)
