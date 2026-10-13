#!/usr/bin/env python3
"""已受信276字节的真实 COFF/PE 包装夹具；只编译容器，不授权源码端点。"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tarfile

TOOLS = Path(__file__).resolve().parent.parent
FIXTURE = TOOLS / 'tests/fixtures/macos-h264dc-kernel.bin'
LLVM_AR = Path('/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/native/llvm/bin/llvm-ar')


def generate(directory):
    spec = importlib.util.spec_from_file_location('recipe', TOOLS / 'ffmpeg-source-recipe.py')
    recipe = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(recipe)
    pin = recipe.trusted()['kernel']
    code = recipe.regular(FIXTURE)
    recipe.require(len(code) == pin['size'] and recipe.digest(code) == pin['sha256'],
                   '受信数学 bytes 不匹配')
    clang, linker = shutil.which('clang'), shutil.which('lld-link')
    recipe.require(clang and linker, '需要已安装 clang/lld-link；不安装或下载')
    directory = Path(directory).resolve()
    directory.mkdir(parents=True, exist_ok=True)
    assembly = directory / 'windows-h264dc-wrapper.S'
    obj = directory / 'windows-h264dc-wrapper.obj'
    dll = directory / 'windows-h264dc-wrapper.dll'
    link_map = directory / 'windows-h264dc-wrapper.map'
    assembly.write_text('.text\n.p2align 2\n'
                        f'.globl {pin["symbol"]}\n'
                        f'.def {pin["symbol"]}; .scl 2; .type 32; .endef\n'
                        f'{pin["symbol"]}:\n'
                        f'.incbin "{FIXTURE}"\n', encoding='utf-8')
    commands = [[clang, '-target', 'aarch64-pc-windows-msvc', '-c', str(assembly), '-o', str(obj)],
                [linker, '/dll', '/noentry', '/nodefaultlib', '/machine:arm64',
                 '/opt:noref,noicf', '/timestamp:0', '/noimplib', f'/lldmap:{link_map}',
                 f'/out:{dll}', str(obj)]]
    for command in commands:
        subprocess.run(command, check=True, capture_output=True, text=True)
    files = {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
             for p in (assembly, obj, dll, link_map)}
    record = {'qualification': 'compiled-only synthetic assembled-wrapper container contract',
              'source_endpoint': 'unavailable: incbin wrapper is not pinned FFmpeg source',
              'kernel': pin, 'commands': commands, 'files': files,
              'tools': {tool: {'path': path, 'version': subprocess.run(
                  [path, '--version'], check=True, capture_output=True, text=True).stdout.strip(),
                  'sha256': hashlib.sha256(Path(path).read_bytes()).hexdigest()}
                  for tool, path in (('compiler', clang), ('linker', linker))}}
    (directory / 'windows-h264dc-wrapper.json').write_text(
        json.dumps(record, indent=2, sort_keys=True) + '\n', encoding='utf-8')
    return record


def generate_source(directory, archive, *, config_append=''):
    """固定源码+归档宏+最小 synthetic config；不是 actual shipping 配置。"""
    spec = importlib.util.spec_from_file_location('recipe', TOOLS / 'ffmpeg-source-recipe.py')
    recipe = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(recipe)
    pin = recipe.trusted()
    directory = Path(directory).resolve()
    recipe.require(recipe.digest(recipe.regular(Path(archive))) == recipe.ARCHIVE_SHA,
                   'source fixture 需要实际受信归档')
    with tarfile.open(archive) as package:
        for name in ('libavcodec/aarch64/h264idct_neon.S', 'libavutil/aarch64/asm.S',
                     'libavcodec/aarch64/neon.S'):
            path = directory / name
            path.parent.mkdir(parents=True, exist_ok=True)
            data = package.extractfile('ffmpeg-8.1.2/' + name).read()
            if name in pin['files']:
                recipe.require(recipe.digest(data) == pin['files'][name]['pre'], 'source preimage 不匹配')
                patch = recipe.regular(recipe.REPO / recipe.PATCH).decode()
                first = patch.split('--- a/libavcodec/aarch64/h264dsp_init_aarch64.c')[0]
                addition = '\n'.join(line[1:] for line in first.splitlines()
                                     if line.startswith('+') and not line.startswith('+++')) + '\n'
                data += addition.encode()
                recipe.require(recipe.digest(data) == pin['files'][name]['post'], 'source postimage 不匹配')
            path.write_bytes(data)
    (directory / 'config.h').write_text('#define EXTERN_ASM\n#define HAVE_AS_FUNC 0\n'
                                      '#define HAVE_AS_ARCH_DIRECTIVE 0\n#define CONFIG_PIC 1\n'
                                      '#define HAVE_NEON 1\n' + config_append, encoding='utf-8')
    (directory / 'ffbuild').mkdir(exist_ok=True)
    (directory / 'ffbuild/config.mak').write_text('fixture=synthetic-config-compiled-only\n', encoding='utf-8')
    clang = shutil.which('clang')
    recipe.require(clang is not None, '需要已安装 clang')
    obj = directory / 'windows-h264dc-source.obj'
    flags = ['--target=aarch64-w64-mingw32']
    command = [clang, *flags, '-I', str(directory), '-c', '-x', 'assembler-with-cpp',
               str(directory / 'libavcodec/aarch64/h264idct_neon.S'), '-o', str(obj)]
    subprocess.run(command, cwd=directory, check=True, capture_output=True, text=True)
    return obj, {'tree': str(directory), 'archive': str(Path(archive).resolve()),
                 'compiler': str(Path(clang).resolve()), 'flags': flags,
                 'evidence_dir': str(directory.parent / 'endpoint-proof')}


def generate_archive(directory, *, kernel_member=False, mingw=False):
    """仅用于实际 lld map dialect；GNU archive 无关成员不改数学代码。"""
    directory = Path(directory).resolve()
    if not LLVM_AR.is_file():
        raise ValueError('archive fixture 需要已安装 SDK llvm-ar；不安装或下载')
    if kernel_member:
        member = directory / 'windows-h264dc-wrapper.obj'
    else:
        source = directory / 'windows-h264dc-unrelated-member.S'
        member = source.with_suffix('.obj')
        source.write_text('.section .rdata,"dr"\n.globl unrelated_fixture_word\n'
                          'unrelated_fixture_word:\n.long 123\n', encoding='utf-8')
        subprocess.run([shutil.which('clang'), '--target=aarch64-w64-mingw32', '-c', str(source),
                        '-o', str(member)], check=True, capture_output=True)
    archive = directory / ('windows-h264dc-kernel-member.lib' if kernel_member else 'windows-h264dc-unrelated-member.lib')
    subprocess.run([str(LLVM_AR), '--format=gnu', 'rcs', str(archive), str(member)],
                   check=True, capture_output=True)
    tag = ('kernel-member' if kernel_member else 'unrelated-member') + ('-mingw' if mingw else '-lld-link')
    dll, link_map = directory / f'windows-h264dc-{tag}.dll', directory / f'windows-h264dc-{tag}.map'
    direct = [] if kernel_member else [str(directory / 'windows-h264dc-wrapper.obj')]
    if mingw:
        argv = [shutil.which('ld.lld'), '-m', 'arm64pe', '--shared',
                '--entry=ff_h264_luma_dc_dequant_idct_neon', '--no-gc-sections', '--icf=none',
                '--exclude-all-symbols', '--no-insert-timestamp', '--Map=' + str(link_map),
                '-o', str(dll), *direct, '--whole-archive', str(archive)]
    else:
        argv = [shutil.which('lld-link'), '/dll', '/noentry', '/nodefaultlib', '/opt:noref,noicf',
                '/timestamp:0', '/noimplib', '/lldmap:' + str(link_map), '/out:' + str(dll),
                *direct, '/wholearchive:' + str(archive)]
    subprocess.run(argv, check=True, capture_output=True)
    return dll, link_map


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    generate(args.directory)
