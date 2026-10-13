#!/usr/bin/env python3
"""Linux 独立固定来源/ELF代码/材料门；静态检查不执行目标或套用OHOS策略。"""
import argparse
import importlib.util
import json
from pathlib import Path
import struct

REPO = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('linux_desktop_source', REPO / 'tools/desktop-ffmpeg-source.py')
source = importlib.util.module_from_spec(spec)
spec.loader.exec_module(source)
require, regular, digest = source.recipes.require, source.recipes.regular, source.recipes.digest
PROFILE = 'linux-aarch64'
LIBRARIES = {'libavcodec.so.62', 'libavutil.so.60', 'libfreeremotedesk_ffmpeg.so'}
TEXTS = {'FFmpeg-LGPL-2.1-or-later.txt', 'FFmpeg-NOTICE.txt'}
PROVENANCE = 'FFmpeg-PROVENANCE.json'
SCHEMA = 'freeremotedesk.linux.ffmpeg-bundle.v1'
HELPERS = ('tools/build-ffmpeg-linux.sh', 'tools/ffmpeg-source-recipe.py',
           'tools/desktop-ffmpeg-source.py', 'tools/linux-ffmpeg-bundle.py',
           'tools/verify-linux-ffmpeg-bundle.sh')
FIXED_ARGS = ['--prefix=/usr', '--arch=aarch64', '--target-os=linux', '--disable-static',
              '--enable-shared', '--disable-programs', '--disable-doc', '--disable-everything',
              '--enable-decoder=hevc,h264', '--enable-parser=hevc,h264', '--enable-protocol=file',
              '--disable-gpl', '--disable-nonfree', '--disable-version3', '--disable-autodetect',
              '--disable-network', '--disable-debug', '--enable-stripping', '--disable-avdevice',
              '--disable-avformat', '--disable-avfilter', '--disable-swresample', '--disable-swscale',
              '--disable-x86asm']


def files_for_recipe(recipe, profile=PROFILE):
    source.metadata(recipe, profile)
    require(profile.startswith('linux-'), '需要 Linux profile')
    return LIBRARIES | TEXTS if recipe == 'upstream' else LIBRARIES | TEXTS | set(source.recipes.EXTRA) | {
        PROVENANCE, source.asset_name(recipe, profile)}


def extent(offset, size, limit, label):
    require(type(offset) is int and type(size) is int and offset >= 0 and size >= 0 and
            offset <= limit and size <= limit - offset, f'ELF {label} 范围越界/overflow')


def cstring(data, offset):
    require(0 <= offset < len(data), 'ELF string offset 越界')
    end = data.find(b'\0', offset)
    require(end != -1, 'ELF string 截断')
    return data[offset:end].decode('ascii', errors='strict')


class ELF:
    def __init__(self, data):
        self.data = data
        extent(0, 64, len(data), 'header')
        require(data[:7] == b'\x7fELF\x02\x01\x01', 'kernel 需要 ELF64 little-endian')
        h = struct.unpack_from('<HHIQQQIHHHHHH', data, 16)
        require(h[:3] == (3, 183, 1), 'kernel 需要 AArch64 ELF shared library')
        require(h[7] == 64 and h[8] == 56 and h[9] > 0 and h[10] == 64 and
                h[11] > 0 and 0 < h[12] < h[11], 'ELF table header 无效/不支持extended编号')
        extent(h[4], h[9] * 56, len(data), 'program table')
        extent(h[5], h[11] * 64, len(data), 'section table')
        self.phoff, self.shoff = h[4], h[5]
        self.loads = []
        for i in range(h[9]):
            p = struct.unpack_from('<IIQQQQQQ', data, h[4] + i * 56)
            extent(p[2], p[5], len(data), 'segment file')
            extent(p[3], p[6], (1 << 64) - 1, 'segment address')
            require(p[5] <= p[6], 'ELF segment filesz 大于 memsz')
            if p[0] == 1:
                require(p[7] in (0, 1) or p[7] & (p[7] - 1) == 0, 'ELF LOAD alignment 无效')
                require(p[7] <= 1 or p[2] % p[7] == p[3] % p[7], 'ELF LOAD alignment 不匹配')
                self.loads.append(dict(offset=p[2], address=p[3], filesize=p[5], size=p[6],
                                       flags=p[1], index=i))
        for i, first in enumerate(self.loads):
            for second in self.loads[i + 1:]:
                for start, size in (('offset', 'filesize'), ('address', 'size')):
                    require(not first[size] or not second[size] or first[start] + first[size] <= second[start] or
                            second[start] + second[size] <= first[start], 'ELF LOAD 范围重叠')
        raw = [struct.unpack_from('<IIQQQQIIQQ', data, h[5] + i * 64) for i in range(h[11])]
        strings = raw[h[12]]
        require(strings[1] == 3, 'ELF section names 必须为 STRTAB')
        extent(strings[4], strings[5], len(data), 'section names')
        names = data[strings[4]:strings[4] + strings[5]]
        self.sections = []
        for i, s in enumerate(raw):
            extent(s[3], s[5], (1 << 64) - 1, 'section address')
            if s[1] != 8:
                extent(s[4], s[5], len(data), 'section file')
            self.sections.append(dict(name=cstring(names, s[0]), kind=s[1], flags=s[2], address=s[3],
                                      offset=s[4], size=s[5], link=s[6], entsize=s[9], index=i))

    def symbol(self, expected, required=True):
        matches = []
        for table in self.sections:
            if table['kind'] not in (2, 11):
                continue
            require(table['entsize'] == 24 and table['size'] % 24 == 0 and
                    0 < table['link'] < len(self.sections), 'ELF symbol table 无效')
            strings = self.sections[table['link']]
            require(strings['kind'] == 3, 'ELF symbol strings 必须为 STRTAB')
            names = self.data[strings['offset']:strings['offset'] + strings['size']]
            found = []
            for offset in range(table['offset'], table['offset'] + table['size'], 24):
                n, info, other, index, value, size = struct.unpack_from('<IBBHQQ', self.data, offset)
                if cstring(names, n) == expected:
                    require(info & 15 == 2 and 0 < index < len(self.sections), 'kernel 必须是已定义FUNC symbol')
                    found.append((value, size, index))
            require(len(found) <= 1, 'ELF 重复 kernel symbol')
            # 发现必须来自未strip的真实.symtab；dynsym只补充冲突检查。
            matches.extend((table['kind'], item) for item in found)
        symtab = [item for kind, item in matches if kind == 2]
        require(len(symtab) <= 1 and (not required or len(symtab) == 1), '缺少/重复固定 kernel .symtab symbol')
        require(not matches or all(item == matches[0][1] for _, item in matches), 'ELF symbol tables identity冲突')
        return matches[0][1] if matches else None

    def executable(self, address, size):
        extent(address, size, (1 << 64) - 1, 'kernel address')
        sections = [s for s in self.sections if s['kind'] == 1 and s['flags'] & 6 == 6 and
                    s['address'] <= address and size <= s['size'] - (address - s['address'])]
        loads = [p for p in self.loads if p['flags'] & 1 and p['address'] <= address and
                 size <= p['filesize'] - (address - p['address'])]
        require(len(sections) == 1 and len(loads) == 1, 'kernel 不在唯一 file-backed可执行section/LOAD内')
        s, p = sections[0], loads[0]
        extent(s['address'] - p['address'], s['size'], p['filesize'], 'kernel section/LOAD')
        require(s['offset'] == p['offset'] + s['address'] - p['address'], 'ELF section/LOAD映射不匹配')
        offset = p['offset'] + address - p['address']
        extent(offset, size, len(self.data), 'kernel bytes')
        return s, offset


def kernel_identity(path, record=None):
    pin = source.recipes.trusted()['kernel']
    image = ELF(regular(path))
    actual_symbol = image.symbol(pin['symbol'], required=record is None)
    if record is None:
        address, size, index = actual_symbol
        require(size == pin['size'], 'kernel symbol size不匹配')
    else:
        require(isinstance(record, dict) and set(record) == {'symbol', 'size', 'sha256', 'address', 'file_offset', 'section'},
                'Linux kernel identity字段集合无效')
        require(all(record.get(k) == pin[k] for k in ('symbol', 'size', 'sha256')) and
                all(type(record[k]) is int for k in ('address', 'file_offset', 'size')),
                'kernel受信代码identity不匹配')
        address, index = record['address'], None
        if actual_symbol is not None:
            require(actual_symbol[:2] == (address, pin['size']), 'kernel record与实际symbol不匹配')
            index = actual_symbol[2]
    section, offset = image.executable(address, pin['size'])
    require(index is None or index == section['index'], 'kernel symbol/section不匹配')
    require(digest(image.data[offset:offset + pin['size']]) == pin['sha256'], 'kernel实际固定276字节代码不匹配')
    actual = dict(pin, address=address, file_offset=offset, section=section['name'])
    require(record is None or record == actual, 'kernel最终section/fileoffset映射不匹配')
    return actual


def recipe_for_materials(path):
    if (path / PROVENANCE).exists():
        record = json.loads(regular(path / PROVENANCE))
        require(isinstance(record, dict) and record.get('recipe') == source.recipes.MODIFIED, '未知Linux bundle recipe')
        return source.recipes.MODIFIED
    return 'upstream'


def notice(recipe, profile=PROFILE):
    info = source.metadata(recipe, profile)
    return (f'FFmpeg 8.1.2, LGPL-2.1-or-later; locally modified by the fixed {recipe} recipe; H.264 and HEVC decoder/parser only.\n'
            f'Source archive SHA-256: {info["archive_sha256"]}\nArchitecture: {profile}\n'
            'Assembly: aarch64-neon (have_neon=1)\n'
            f'Corresponding source is distributed beside the application as {source.asset_name(recipe, profile)}.\n'
            'The original archive pin does not authenticate the local patch.\n'
            'Rebuild with tools/build-ffmpeg-linux.sh; LGPL libraries remain dynamically replaceable.\n').encode()


def validate_configuration(configuration):
    require(isinstance(configuration, dict) and set(configuration) == {'args', 'compiler'}, 'Linux configuration字段无效')
    args = configuration['args']
    require(isinstance(args, list) and args[:len(FIXED_ARGS)] == FIXED_ARGS, 'Linux固定ARM64配置不匹配')
    tail = args[len(FIXED_ARGS):]
    require(not tail or (len(tail) == 3 and tail[0] == '--enable-cross-compile' and
            isinstance(tail[1], str) and tail[1].startswith('--cc=') and len(tail[1]) > 5 and
            isinstance(tail[2], str) and tail[2].startswith('--cross-prefix=') and len(tail[2]) > 15),
            'Linux cross配置不匹配')
    require(isinstance(configuration['compiler'], str) and bool(configuration['compiler'].strip()), 'Linux compiler缺失')


def verify_materials(path, recipe, profile=PROFILE):
    source.metadata(recipe, profile)
    require(regular(path / 'FFmpeg-LGPL-2.1-or-later.txt') ==
            regular(REPO / source.recipes.BASE / 'LICENSE.LGPLv2.1'), 'FFmpeg许可bytes不匹配')
    if recipe == 'upstream':
        text = regular(path / 'FFmpeg-NOTICE.txt').decode()
        require('FFmpeg 8.1.2,' in text and f'Architecture: {profile}\n' in text and
                source.recipes.ARCHIVE_SHA in text, 'upstream来源NOTICE不匹配')
        return
    require(regular(path / 'FFmpeg-NOTICE.txt') == notice(recipe, profile), 'modified来源NOTICE不匹配')
    for name, data in source.recipes.materials().items():
        require(regular(path / name) == data, '受信源码材料bytes不匹配')
    asset = path / source.asset_name(recipe, profile)
    source.verify_embedded_asset(asset, recipe, profile)
    record = json.loads(regular(path / PROVENANCE))
    require(isinstance(record, dict) and set(record) == {'schema', 'version', 'profile', 'recipe', 'source',
            'configuration', 'inputs', 'outputs', 'kernel', 'source_asset'}, 'Linux provenance字段集合无效')
    require(record['schema'] == SCHEMA and record['version'] == '8.1.2' and record['profile'] == profile and
            record['recipe'] == recipe and isinstance(record['source'], dict) and
            type(record['source'].get('modified')) is bool and record['source'] == source.metadata(recipe, profile),
            'Linux provenance来源不匹配')
    validate_configuration(record['configuration'])
    inputs = record['inputs']
    require(isinstance(inputs, dict) and set(inputs) == set(HELPERS) | {'config.h', 'ffbuild/config.mak', 'unstripped_libavcodec'},
            'Linux provenance输入集合无效')
    for name, value in inputs.items():
        require(isinstance(value, str) and len(value) == 64 and all(c in '0123456789abcdef' for c in value),
                'Linux provenance输入hash无效')
        if name in HELPERS:
            require(value == digest(regular(REPO / name)), 'Linux受信helper/builder不匹配')
    require(record['source_asset'] == {'name': source.asset_name(recipe, profile), 'sha256': digest(regular(asset))},
            'Linux source asset身份不匹配')
    require(isinstance(record['outputs'], dict) and set(record['outputs']) == LIBRARIES, 'Linux产物集合无效')
    for name in LIBRARIES:
        require(record['outputs'][name] == digest(regular(path / name)), 'Linux最终shipping bytes不匹配')
    kernel_identity(path / 'libavcodec.so.62', record['kernel'])


def verify_bundle(path, profile=PROFILE, expected_recipe=None):
    for parent in (path, *path.parents):
        require(not parent.is_symlink(), 'Linux FFmpeg路径不得含symlink')
    require(path.is_dir(), 'Linux FFmpeg bundle必须是普通目录')
    recipe = recipe_for_materials(path)
    require(expected_recipe is None or recipe == expected_recipe, '所选recipe与bundle来源不匹配')
    require({p.name for p in path.iterdir()} == files_for_recipe(recipe, profile), 'Linux FFmpeg文件集合不匹配')
    for child in path.iterdir():
        regular(child)
    verify_materials(path, recipe, profile)
    return recipe


def complete(bundle, archive, source_tree, configuration_file, unstripped_library, kernel_file):
    recipe = source.recipes.MODIFIED
    pin = source.recipes.trusted()
    for path, hashes in pin['files'].items():
        require(digest(regular(source_tree / path)) == hashes['post'], '源树固定postimage不匹配')
    for path, expected in pin['unchanged_oracle'].items():
        require(digest(regular(source_tree / path)) == expected, '源树未改C oracle不匹配')
    require(b'#define HAVE_NEON 1\n' in regular(source_tree / 'config.h'), 'Linux源配置缺少HAVE_NEON')
    configuration = json.loads(regular(configuration_file))
    validate_configuration(configuration)
    # 记录来自同源未strip库的实际FUNC symbol/size；最终install/strip后再映射计算bytes。
    kernel = json.loads(regular(kernel_file))
    require(kernel == kernel_identity(unstripped_library), 'Linux strip前record与同源实际symbol不匹配')
    kernel_identity(bundle / 'libavcodec.so.62', kernel)
    (bundle / 'FFmpeg-NOTICE.txt').write_bytes(notice(recipe))
    for name, data in source.recipes.materials().items():
        (bundle / name).write_bytes(data)
    source.write_asset(archive, bundle / source.asset_name(recipe, PROFILE), recipe, PROFILE)
    inputs = {p: digest(regular(source_tree / p)) for p in ('config.h', 'ffbuild/config.mak')}
    inputs['unstripped_libavcodec'] = digest(regular(unstripped_library))
    inputs.update({p: digest(regular(REPO / p)) for p in HELPERS})
    record = dict(schema=SCHEMA, version='8.1.2', profile=PROFILE, recipe=recipe,
                  source=source.metadata(recipe, PROFILE), configuration=configuration, inputs=inputs,
                  outputs={p: digest(regular(bundle / p)) for p in LIBRARIES}, kernel=kernel,
                  source_asset={'name': source.asset_name(recipe, PROFILE),
                                'sha256': digest(regular(bundle / source.asset_name(recipe, PROFILE)))})
    (bundle / PROVENANCE).write_text(json.dumps(record, indent=2, sort_keys=True) + '\n')
    verify_bundle(bundle, PROFILE, recipe)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='action', required=True)
    for action in ('verify', 'files', 'recipe'):
        p = commands.add_parser(action)
        p.add_argument('--profile', default=PROFILE)
        p.add_argument('--path', type=Path)
        p.add_argument('--source-recipe')
    kernel = commands.add_parser('kernel')
    kernel.add_argument('--library', type=Path, required=True)
    kernel.add_argument('--record', type=Path)
    kernel.add_argument('--output', type=Path)
    p = commands.add_parser('complete')
    for key in ('bundle', 'archive', 'source-tree', 'configuration-file', 'unstripped-library', 'kernel-file'):
        p.add_argument('--' + key, type=Path, required=True)
    args = parser.parse_args()
    if args.action == 'complete':
        complete(args.bundle, args.archive, args.source_tree, args.configuration_file, args.unstripped_library, args.kernel_file)
    elif args.action == 'kernel':
        record = json.loads(regular(args.record)) if args.record else None
        result = json.dumps(kernel_identity(args.library, record), indent=2, sort_keys=True) + '\n'
        if args.output:
            args.output.write_text(result)
        else:
            print(result, end='')
    elif args.action == 'verify':
        require(args.path is not None, 'verify需要path')
        print(verify_bundle(args.path, args.profile, args.source_recipe))
    else:
        recipe = args.source_recipe or recipe_for_materials(args.path)
        source.metadata(recipe, args.profile)
        if args.action == 'recipe':
            print(recipe)
        else:
            print('\n'.join(sorted(files_for_recipe(recipe, args.profile))))


if __name__ == '__main__':
    main()
