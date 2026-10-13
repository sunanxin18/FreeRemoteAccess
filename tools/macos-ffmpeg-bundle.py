#!/usr/bin/env python3
"""macOS ARM64 固定 recipe 的 Mach-O、来源及随包材料门；不执行 codec。"""
import argparse
import importlib.util
import json
from pathlib import Path
import struct

REPO = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('macos_desktop_source', REPO / 'tools/desktop-ffmpeg-source.py')
source = importlib.util.module_from_spec(spec)
spec.loader.exec_module(source)
require, regular, digest = source.recipes.require, source.recipes.regular, source.recipes.digest
LIBRARIES = {'libavcodec.62.dylib', 'libavutil.60.dylib', 'libfreeremotedesk_ffmpeg.dylib'}
TEXTS = {'FFmpeg-LGPL-2.1-or-later.txt', 'FFmpeg-NOTICE.txt'}
PROVENANCE = 'FFmpeg-PROVENANCE.json'
SCHEMA = 'freeremotedesk.macos.ffmpeg-bundle.v1'


def files_for_recipe(recipe):
    source.metadata(recipe)
    if recipe == 'upstream':
        return LIBRARIES | TEXTS
    return LIBRARIES | TEXTS | set(source.recipes.EXTRA) | {PROVENANCE, source.asset_name(recipe)}


def extent(offset, size, limit, label):
    require(type(offset) is int and type(size) is int and offset >= 0 and size >= 0 and
            offset <= limit and size <= limit - offset, f'Mach-O {label} 范围越界/overflow')


def name(raw):
    return raw.split(b'\0', 1)[0].decode('ascii', errors='strict')


class MachO:
    def __init__(self, data):
        self.data = data
        extent(0, 32, len(data), 'header')
        magic, cpu, subtype, kind, count, cmdbytes, flags, reserved = struct.unpack_from('<8I', data)
        require(magic == 0xfeedfacf and cpu == 0x100000c and subtype & 0xffffff == 0,
                'kernel 需要薄 little-endian ARM64 Mach-O（非 fat/Intel/arm64e）')
        require(kind in (1, 6), 'kernel 需要 Mach-O object/dylib')
        extent(32, cmdbytes, len(data), 'load commands')
        require(count <= cmdbytes // 8, 'Mach-O load command 数量越界')
        self.kind, self.commands, self.segments, self.sections = kind, [], [], []
        self.symtab, self.signature = None, None
        cursor = 32
        for _ in range(count):
            extent(cursor, 8, 32 + cmdbytes, 'command header')
            command, size = struct.unpack_from('<II', data, cursor)
            require(size >= 8 and size % 8 == 0, 'Mach-O command size 无效')
            extent(cursor, size, 32 + cmdbytes, 'command')
            self.commands.append((command, cursor, size))
            if command == 0x19:
                require(size >= 72, 'Mach-O segment 截断')
                segname, vmaddr, vmsize, fileoff, filesize, maxprot, initprot, nsects, segflags = struct.unpack_from('<16s4Q4I', data, cursor + 8)
                require(size == 72 + nsects * 80, 'Mach-O section table 无效')
                extent(vmaddr, vmsize, (1 << 64) - 1, 'segment VM')
                extent(fileoff, filesize, len(data), 'segment file')
                require(filesize <= vmsize, 'Mach-O segment filesize 大于 vmsize')
                segment = dict(name=name(segname), address=vmaddr, size=vmsize, offset=fileoff,
                               filesize=filesize, executable=bool(initprot & 4), command_offset=cursor)
                self.segments.append(segment)
                for index in range(nsects):
                    raw = struct.unpack_from('<16s16sQQ8I', data, cursor + 72 + index * 80)
                    section = dict(name=name(raw[0]), segment=name(raw[1]), address=raw[2], size=raw[3],
                                   offset=raw[4], flags=raw[8], parent=segment, index=len(self.sections) + 1)
                    require(kind == 1 or section['segment'] == segment['name'], 'Mach-O section/segment 名称不匹配')
                    extent(section['address'] - vmaddr, section['size'], vmsize, 'section VM')
                    if section['flags'] & 0xff not in (1, 0xc, 0x12):
                        extent(section['offset'] - fileoff, section['size'], filesize, 'section file')
                        if kind == 6:
                            require(section['offset'] == fileoff + section['address'] - vmaddr,
                                    'Mach-O dylib section 地址/file offset 与 segment 映射不匹配')
                    self.sections.append(section)
            elif command == 2:
                require(size == 24 and self.symtab is None, 'Mach-O 重复/无效 symtab')
                self.symtab = struct.unpack_from('<4I', data, cursor + 8)
                symoff, nsyms, stroff, strsize = self.symtab
                extent(symoff, nsyms * 16, len(data), 'symbols')
                extent(stroff, strsize, len(data), 'strings')
            elif command == 0x1d:
                require(size == 16 and self.signature is None, 'Mach-O 重复/无效 code signature')
                offset, length = struct.unpack_from('<II', data, cursor + 8)
                extent(offset, length, len(data), 'signature')
                require(offset + length == len(data), 'Mach-O signature 必须位于文件尾')
                self.signature = (offset, length, cursor)
            cursor += size
        require(cursor == 32 + cmdbytes, 'Mach-O load commands 长度不匹配')
        for i, first in enumerate(self.segments):
            for second in self.segments[i + 1:]:
                for offset_key, size_key in (('offset', 'filesize'), ('address', 'size')):
                    if first[size_key] and second[size_key]:
                        require(first[offset_key] + first[size_key] <= second[offset_key] or
                                second[offset_key] + second[size_key] <= first[offset_key], 'Mach-O segment 范围重叠')

    def executable(self, address, length):
        matches = []
        for section in self.sections:
            parent = section['parent']
            if (parent['executable'] and section['flags'] & 0x80000400 and
                    section['flags'] & 0xff == 0 and section['address'] <= address and
                    length <= section['size'] - (address - section['address'])):
                if self.kind == 6:
                    # dylib 的真实加载地址由 segment 线性映射；不能只相信 section 的独立声明。
                    extent(address - parent['address'], length, parent['filesize'], 'kernel file-backed segment')
                    offset = parent['offset'] + address - parent['address']
                else:
                    # relocatable object 的 section file offset 包含 header/command 前缀，独立处理。
                    offset = section['offset'] + address - section['address']
                extent(offset, length, len(self.data), 'kernel code')
                extent(offset - parent['offset'], length, parent['filesize'], 'kernel executable segment')
                matches.append((section, offset))
        require(len(matches) == 1, 'kernel 不在唯一可执行 section/segment 内')
        return matches[0]

    def symbol(self, expected, required=True):
        if self.symtab is None and not required:
            return None
        require(self.symtab is not None, '缺少 kernel symbol table')
        symoff, count, stroff, strsize = self.symtab
        strings = self.data[stroff:stroff + strsize]
        matches = []
        for i in range(count):
            index, kind, section, desc, address = struct.unpack_from('<IBBHQ', self.data, symoff + i * 16)
            require(index < strsize, 'Mach-O symbol string 越界')
            end = strings.find(b'\0', index)
            require(end != -1, 'Mach-O symbol string 截断')
            if strings[index:end] == expected.encode():
                require(not kind & 0xe0 and kind & 0xe == 0xe and 0 < section <= len(self.sections),
                        'kernel symbol 必须定义于 section')
                matches.append((address, section))
        require(len(matches) <= 1 and (not required or len(matches) == 1), '缺少/重复固定 kernel symbol')
        if not matches:
            return None
        return matches[0]


def kernel_identity(path, record=None):
    pin = source.recipes.trusted()['kernel']
    image = MachO(regular(path))
    if record is None:
        address, index = image.symbol('_' + pin['symbol'])
    else:
        require(isinstance(record, dict) and set(record) == {
            'symbol', 'macho_symbol', 'size', 'sha256', 'address', 'file_offset', 'section', 'segment'},
            'kernel identity 字段集合无效')
        require(all(record.get(k) == pin[k] for k in ('symbol', 'size', 'sha256')) and
                record['macho_symbol'] == '_' + pin['symbol'] and
                all(type(record[k]) is int for k in ('address', 'file_offset', 'size')),
                'kernel 受信 identity 不匹配')
        address, index = record['address'], None
        actual_symbol = image.symbol('_' + pin['symbol'], required=False)
        if actual_symbol is not None:
            require(actual_symbol[0] == address, 'kernel 记录与实际 symbol 地址不匹配')
            index = actual_symbol[1]
    section, offset = image.executable(address, pin['size'])
    require(index is None or index == section['index'], 'kernel symbol/section 映射不匹配')
    require(digest(image.data[offset:offset + pin['size']]) == pin['sha256'], 'kernel 实际固定276字节代码不匹配')
    actual = dict(pin, macho_symbol='_' + pin['symbol'], address=address, file_offset=offset,
                  section=section['name'], segment=section['segment'])
    require(record is None or record == actual, 'kernel 最终 section/fileoffset 映射不匹配')
    return actual


def resignable_digest(path):
    """仅归一化签名命令、尾部blob和LINKEDIT签名扩展；其余字节保持身份。"""
    image = MachO(regular(path))
    require(image.kind == 6 and image.signature is not None, '随包 dylib 必须含最终签名布局')
    offset, length, command = image.signature
    data = bytearray(image.data[:offset])
    data[command + 8:command + 16] = b'\0' * 8
    links = [segment for segment in image.segments if segment['name'] == '__LINKEDIT']
    require(len(links) == 1, 'Mach-O 必须有唯一 LINKEDIT')
    link = links[0]
    require(not link['executable'] and not any(section['parent'] is link for section in image.sections),
            'LINKEDIT 不能含可执行代码或 section')
    require(link['size'] == (link['filesize'] + 16383) // 16384 * 16384,
            'LINKEDIT VM 长度必须准确按 ARM64 16K 页对齐')
    require(offset >= link['offset'] and offset + length == link['offset'] + link['filesize'],
            '签名必须位于 LINKEDIT 尾部')
    require(offset % 16 == 0 and length > 0 and offset >= 32 + struct.unpack_from('<I', image.data, 20)[0],
            '签名区间不能覆盖 Mach-O header/command，必须16字节对齐且非空')
    # codesign 会改变 LINKEDIT vmsize/filesize；固定归一化为真实未签名 payload 长度。
    position = link['command_offset']
    struct.pack_into('<Q', data, position + 32, offset - link['offset'])
    struct.pack_into('<Q', data, position + 48, offset - link['offset'])
    return digest(data)


def exact_directory(path, members):
    require(path.is_dir() and not path.is_symlink(), 'FFmpeg 材料必须是普通目录')
    children = list(path.iterdir())
    require({p.name for p in children} == members, 'FFmpeg bundle/Resources 文件集合不匹配')
    for child in children:
        regular(child)


def recipe_for_materials(path):
    if (path / PROVENANCE).exists():
        record = json.loads(regular(path / PROVENANCE))
        require(isinstance(record, dict) and record.get('recipe') == source.recipes.MODIFIED, '未知 macOS bundle recipe')
        return source.recipes.MODIFIED
    return 'upstream'


def notice(recipe):
    info = source.metadata(recipe)
    state = 'unmodified' if recipe == 'upstream' else 'locally modified by the fixed frd-h264dc-aarch64-v1 recipe'
    asset = 'ffmpeg-8.1.2.tar.xz' if recipe == 'upstream' else source.asset_name(recipe)
    return (f'FFmpeg 8.1.2, LGPL-2.1-or-later; source is {state}, H.264 and HEVC decoder/parser only.\n'
            f'Source archive SHA-256: {info["archive_sha256"]}\nArchitecture: macos-aarch64\n'
            'Assembly: aarch64-neon (have_neon=1)\n'
            f'Corresponding source is distributed beside the application as {asset}.\n'
            'The original archive pin does not authenticate the local patch.\n'
            'Rebuild with tools/build-ffmpeg-macos.sh; LGPL libraries remain dynamically replaceable.\n').encode()


def verify_materials(materials, libraries, recipe, final_app=False):
    source.metadata(recipe)
    require(regular(materials / 'FFmpeg-LGPL-2.1-or-later.txt') ==
            regular(REPO / source.recipes.BASE / 'LICENSE.LGPLv2.1'), 'FFmpeg 许可 bytes 不匹配')
    # 旧 upstream NOTICE 格式保留；modified 必须使用精确真实修改说明。
    if recipe == 'upstream':
        text = regular(materials / 'FFmpeg-NOTICE.txt').decode()
        require('source is unmodified' in text and source.recipes.ARCHIVE_SHA in text and
                'Architecture: macos-aarch64' in text, 'upstream 来源 NOTICE 不匹配')
        return
    require(regular(materials / 'FFmpeg-NOTICE.txt') == notice(recipe), 'modified 来源 NOTICE 不匹配')
    for name, data in source.recipes.materials().items():
        require(regular(materials / name) == data, '受信源码材料 bytes 不匹配')
    asset = materials / source.asset_name(recipe)
    source.verify_embedded_asset(asset, recipe)
    record = json.loads(regular(materials / PROVENANCE))
    require(set(record) == {'schema', 'version', 'profile', 'recipe', 'source', 'configuration', 'inputs', 'outputs',
                           'kernel', 'source_object_kernel', 'source_asset'}, 'macOS provenance 字段集合无效')
    require(record['schema'] == SCHEMA and record['version'] == '8.1.2' and record['profile'] == source.PROFILE and
            record['recipe'] == recipe and isinstance(record['source'], dict) and
            type(record['source'].get('modified')) is bool and record['source'] == source.metadata(recipe),
            'macOS provenance 来源不匹配')
    validate_configuration(record['configuration'])
    inputs = record['inputs']
    require(isinstance(inputs, dict) and set(inputs) == {'config.h', 'ffbuild/config.mak', 'kernel_object',
            'tools/build-ffmpeg-macos.sh', 'tools/ffmpeg-source-recipe.py', 'tools/desktop-ffmpeg-source.py',
            'tools/macos-ffmpeg-bundle.py'}, 'macOS provenance 输入集合无效')
    for path, value in inputs.items():
        require(isinstance(value, str) and len(value) == 64 and all(c in '0123456789abcdef' for c in value),
                'macOS provenance 输入 hash 无效')
        if path.startswith('tools/'):
            require(value == digest(regular(REPO / path)), 'macOS provenance 受信 helper/builder 不匹配')
    require(record['source_asset'] == {'name': source.asset_name(recipe), 'sha256': digest(regular(asset))},
            'macOS source asset 身份不匹配')
    require(isinstance(record['outputs'], dict) and set(record['outputs']) == LIBRARIES, 'macOS 产物集合无效')
    for name in LIBRARIES:
        item = record['outputs'][name]
        require(isinstance(item, dict) and set(item) == {'sha256', 'resignable_sha256'}, 'macOS 产物 hash 字段无效')
        require(item['resignable_sha256'] == resignable_digest(libraries / name), 'macOS 签名外产物身份不匹配')
        require(final_app or item['sha256'] == digest(regular(libraries / name)), 'macOS bundle 完整产物身份不匹配')
    kernel_identity(libraries / 'libavcodec.62.dylib', record['kernel'])
    object_kernel = record['source_object_kernel']
    require(isinstance(object_kernel, dict) and set(object_kernel) == set(record['kernel']) and
            all(object_kernel.get(k) == record['kernel'][k] for k in ('symbol', 'macho_symbol', 'size', 'sha256')),
            'macOS source object 数学核身份不匹配')


def validate_configuration(configuration):
    require(isinstance(configuration, dict) and set(configuration) == {'args', 'deployment_target', 'compiler'},
            'macOS configuration 字段无效')
    args = configuration['args']
    fixed = ['--arch=aarch64', '--target-os=darwin', '--disable-static', '--enable-shared', '--disable-programs',
             '--disable-doc', '--disable-everything', '--enable-decoder=hevc,h264', '--enable-parser=hevc,h264',
             '--disable-gpl', '--disable-nonfree', '--disable-version3', '--disable-autodetect', '--disable-network',
             '--disable-debug', '--enable-stripping', '--disable-avdevice', '--disable-avformat', '--disable-avfilter',
             '--disable-swresample', '--disable-swscale']
    require(isinstance(args, list) and len(args) >= 1 and isinstance(args[0], str) and
            args[0].startswith('--prefix=/') and (args[1:] == fixed + ['--disable-x86asm'] or
            args[1:] == fixed + ['--enable-cross-compile', '--cc=clang', '--extra-cflags=-arch arm64',
                                '--extra-ldflags=-arch arm64', '--disable-x86asm']), 'macOS configure 与固定 ARM64 profile 不匹配')
    import re
    require(isinstance(configuration['deployment_target'], str) and
            re.fullmatch(r'(?:1[2-9]|[2-9][0-9])\.\d+(?:\.\d+)?', configuration['deployment_target']) is not None,
            'macOS deployment target 必须至少12')
    require(isinstance(configuration['compiler'], str) and configuration['compiler'].startswith('Apple clang version '),
            'macOS compiler provenance 缺失')


def verify_bundle(path, expected_recipe=None):
    require(path.is_dir() and not path.is_symlink(), 'FFmpeg bundle 必须是普通目录')
    recipe = recipe_for_materials(path)
    require(expected_recipe is None or recipe == expected_recipe, '所选 recipe 与实际 bundle 来源不匹配')
    exact_directory(path, files_for_recipe(recipe))
    verify_materials(path, path, recipe)
    return recipe


def verify_app(app, expected_recipe=None):
    libraries = app / 'Contents/MacOS/codecs/ffmpeg-8-1-2/macos-aarch64'
    materials = app / 'Contents/Resources/codecs/ffmpeg-8-1-2'
    for path in (libraries, materials):
        for parent in (path, *path.parents):
            require(not parent.is_symlink(), 'FFmpeg app 路径不能含 symlink')
            if parent == app:
                break
    recipe = recipe_for_materials(materials)
    require(expected_recipe is None or recipe == expected_recipe, '所选 recipe 与实际 app 来源不匹配')
    exact_directory(libraries, LIBRARIES)
    exact_directory(materials, files_for_recipe(recipe) - LIBRARIES)
    verify_materials(materials, libraries, recipe, final_app=True)
    return recipe


def complete(bundle, archive, source_tree, configuration_file, kernel_file, object_file):
    recipe = source.recipes.MODIFIED
    source.metadata(recipe)
    pin = source.recipes.trusted()
    for path, hashes in pin['files'].items():
        require(digest(regular(source_tree / path)) == hashes['post'], '源树固定 postimage 不匹配')
    for path, expected in pin['unchanged_oracle'].items():
        require(digest(regular(source_tree / path)) == expected, '源树未改 C oracle 不匹配')
    require(b'#define HAVE_NEON 1\n' in regular(source_tree / 'config.h'), 'macOS 源配置缺少 HAVE_NEON')
    configuration = json.loads(regular(configuration_file))
    validate_configuration(configuration)
    object_kernel = kernel_identity(object_file)
    kernel = json.loads(regular(kernel_file))
    kernel_identity(bundle / 'libavcodec.62.dylib', kernel)
    require(all(object_kernel[k] == kernel[k] for k in ('symbol', 'size', 'sha256')), '源对象和库数学核不匹配')
    (bundle / 'FFmpeg-NOTICE.txt').write_bytes(notice(recipe))
    for name, data in source.recipes.materials().items():
        (bundle / name).write_bytes(data)
    source.write_asset(archive, bundle / source.asset_name(recipe), recipe)
    inputs = {path: digest(regular(source_tree / path)) for path in ('config.h', 'ffbuild/config.mak')}
    inputs['kernel_object'] = digest(regular(object_file))
    for path in ('tools/build-ffmpeg-macos.sh', 'tools/ffmpeg-source-recipe.py', 'tools/desktop-ffmpeg-source.py',
                 'tools/macos-ffmpeg-bundle.py'):
        inputs[path] = digest(regular(REPO / path))
    record = {'schema': SCHEMA, 'version': '8.1.2', 'profile': source.PROFILE, 'recipe': recipe, 'source': source.metadata(recipe),
              'configuration': configuration, 'inputs': inputs,
              'outputs': {name: {'sha256': digest(regular(bundle / name)),
                                 'resignable_sha256': resignable_digest(bundle / name)} for name in LIBRARIES},
              'kernel': kernel, 'source_object_kernel': object_kernel,
              'source_asset': {'name': source.asset_name(recipe), 'sha256': digest(regular(bundle / source.asset_name(recipe)))}}
    (bundle / PROVENANCE).write_text(json.dumps(record, indent=2, sort_keys=True) + '\n')
    verify_bundle(bundle, recipe)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='action', required=True)
    kernel = commands.add_parser('kernel')
    kernel.add_argument('--library', type=Path, required=True)
    kernel.add_argument('--record', type=Path)
    kernel.add_argument('--output', type=Path)
    for action in ('verify-bundle', 'verify-app'):
        verify = commands.add_parser(action)
        verify.add_argument('--path', type=Path, required=True)
        verify.add_argument('--source-recipe')
    finish = commands.add_parser('complete')
    for key in ('bundle', 'archive', 'source-tree', 'configuration-file', 'kernel-file', 'object-file'):
        finish.add_argument('--' + key, type=Path, required=True)
    args = parser.parse_args()
    if args.action == 'kernel':
        record = json.loads(regular(args.record)) if args.record else None
        result = json.dumps(kernel_identity(args.library, record), sort_keys=True, indent=2) + '\n'
        if args.output:
            args.output.write_text(result)
        else:
            print(result, end='')
    elif args.action == 'complete':
        complete(args.bundle, args.archive, args.source_tree, args.configuration_file, args.kernel_file, args.object_file)
    else:
        source.metadata(args.source_recipe) if args.source_recipe else None
        action = verify_bundle if args.action == 'verify-bundle' else verify_app
        print(action(args.path, args.source_recipe))


if __name__ == '__main__':
    main()
