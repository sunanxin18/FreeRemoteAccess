#!/usr/bin/env python3
"""Windows ARM64 build-side 核身份；记录是数据，不能授权新 recipe/字节。

coff_kernel_identity: coherent function Aux 或 replay_source_endpoint 的端点。
capture_pe_kernel: 同输入 object、lld-link /lldmap、未 strip PE 的链。
verify_pe_kernel: 重验全部 capture 输入，再独立映射 final RVA；偏移可改变。
本模块不执行记录里的命令、不扫描 exports、不认证构建日志的真实性。
"""
import importlib.util
import json
from pathlib import Path
import re
import struct
import subprocess
import tarfile

TOOLS = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location('windows_kernel_recipes', TOOLS / 'ffmpeg-source-recipe.py')
recipes = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(recipes)
require, digest, regular = recipes.require, recipes.digest, recipes.regular
SCHEMA = 'freeremotedesk.windows.arm64.kernel.v1'
SOURCE = 'libavcodec/aarch64/h264idct_neon.S'
START, END = 'frd_private_endpoint_start', 'frd_private_endpoint_end'
CODE_EXEC = 0x20000020


def extent(start, size, total, label):
    require(type(start) is int and type(size) is int and start >= 0 and size >= 0 and
            start <= total and size <= total - start, f'{label} 范围无效')


def unpack(data, fmt, offset, label):
    extent(offset, struct.calcsize(fmt), len(data), label)
    return struct.unpack_from(fmt, data, offset)


def disjoint(ranges, label):
    ranges = sorted((a, a + n) for a, n in ranges if n)
    require(all(a >= end for (_, end), (a, _) in zip(ranges, ranges[1:])), f'{label} 重叠')


def cstring(data, offset, label):
    require(0 <= offset < len(data), f'{label} 字符串偏移无效')
    end = data.find(b'\0', offset)
    require(end != -1, f'{label} 字符串截断')
    try:
        return data[offset:end].decode('ascii')
    except UnicodeDecodeError as error:
        raise ValueError(f'{label} 非 ASCII 名称') from error


class Coff:
    """只支持常规 ARM64 little-endian COFF；bigobj/ARM64EC 显式拒绝。"""
    def __init__(self, data):
        self.data = data
        machine, count, _, symoff, nsym, optsize, flags = unpack(data, '<HHIIIHH', 0, 'COFF header')
        require(machine == 0xAA64 and 0 < count <= 32767 and optsize == 0 and not flags & 2,
                '需要常规 ARM64 COFF object')
        table_end = 20 + count * 40
        extent(20, count * 40, len(data), 'COFF section table')
        require(nsym > 0 and symoff >= table_end, 'COFF symbol table 缺失/覆盖 header')
        extent(symoff, nsym * 18 + 4, len(data), 'COFF symbol/string table')
        strsize = unpack(data, '<I', symoff + nsym * 18, 'COFF string size')[0]
        require(strsize >= 4, 'COFF string table 太短')
        extent(symoff + nsym * 18, strsize, len(data), 'COFF strings')
        self.strings = data[symoff + nsym * 18:symoff + nsym * 18 + strsize]
        self.sections = []
        regions = [(0, table_end), (symoff, nsym * 18 + strsize)]
        for index in range(1, count + 1):
            name, virtual, address, size, raw, reloc, line, nr, nl, characteristics = unpack(
                data, '<8sIIIIIIHHI', 20 + (index - 1) * 40, 'COFF section')
            require(virtual == address == 0 and not characteristics & 0x01000000,
                    'COFF section 地址/overflow relocation 不支持')
            name = self.name(name, section=True)
            if size and not characteristics & 0x80:
                require(raw >= table_end, 'COFF raw 覆盖 header')
                extent(raw, size, len(data), 'COFF raw')
                regions.append((raw, size))
            else:
                require(size == 0 or raw == 0, 'COFF uninitialized section 有 raw bytes')
                require(raw == 0 or table_end <= raw <= len(data), 'COFF empty raw pointer 越界/header')
            for pointer, length, label in ((reloc, nr * 10, 'relocation'), (line, nl * 6, 'line table')):
                require((length == 0 and pointer == 0) or (length > 0 and pointer >= table_end),
                        f'COFF {label} 指针/count 不一致')
                extent(pointer, length, len(data), f'COFF {label}')
                regions.append((pointer, length))
            self.sections.append(dict(index=index, name=name, size=size, raw=raw,
                                      flags=characteristics, reloc=reloc, nr=nr))
        disjoint(regions, 'COFF tables/raw')
        self.symbols = []
        self.by_index = {}
        index = 0
        while index < nsym:
            name, value, section, typ, storage, naux = unpack(
                data, '<8sIhHBB', symoff + index * 18, 'COFF symbol')
            require(index + naux < nsym and -2 <= section <= count, 'COFF symbol/Aux 范围无效')
            aux = data[symoff + (index + 1) * 18:symoff + (index + 1 + naux) * 18]
            symbol = dict(index=index, name=self.name(name), value=value, section=section,
                          type=typ, storage=storage, naux=naux, aux=aux)
            if section > 0:
                require(value <= self.sections[section - 1]['size'], 'COFF symbol Value 越界')
            self.symbols.append(symbol)
            self.by_index[index] = symbol
            index += 1 + naux
        for section in self.sections:
            for index in range(section['nr']):
                address, symbol, typ = unpack(data, '<IIH', section['reloc'] + index * 10, 'COFF relocation')
                require(address < section['size'] and symbol in self.by_index and typ <= 0x11,
                        'COFF relocation 地址/symbol/type 无效')

    def name(self, raw, section=False):
        if section and raw.startswith(b'/'):
            offset = raw.rstrip(b'\0')[1:]
            require(offset.isdigit(), 'COFF section longname 无效')
            value = int(offset)
        elif not section and raw[:4] == b'\0' * 4:
            value = struct.unpack('<I', raw[4:])[0]
        else:
            return cstring(raw + b'\0', 0, 'COFF inline name')
        require(value >= 4, 'COFF longname 指向 string header')
        return cstring(self.strings, value, 'COFF longname')

    def symbol(self, name):
        matches = [s for s in self.symbols if s['name'] == name]
        require(len(matches) == 1 and matches[0]['section'] > 0, 'COFF symbol 缺失/重复/未定义')
        return matches[0]

    def code(self, symbol, size):
        section = self.sections[symbol['section'] - 1]
        require(section['flags'] & CODE_EXEC == CODE_EXEC and not section['flags'] & 0x80,
                'COFF kernel 不是 file-backed executable code')
        extent(symbol['value'], size, section['size'], 'COFF kernel section')
        for index in range(section['nr']):
            address, _, _ = unpack(self.data, '<IIH', section['reloc'] + index * 10, 'COFF relocation')
            # ARM64 最大受影响字段按8字节保守处理；不能漏掉起点前的覆盖。
            require(address + 8 <= symbol['value'] or address >= symbol['value'] + size,
                    'COFF relocation 覆盖 kernel')
        offset = section['raw'] + symbol['value']
        extent(offset, size, len(self.data), 'COFF kernel bytes')
        return section, offset, self.data[offset:offset + size]


class Pe:
    """PE32+ ARM64 的唯一 file-backed section 映射，地址全部为 RVA。"""
    def __init__(self, data):
        self.data = data
        require(data[:2] == b'MZ', 'PE DOS signature 无效')
        pe = unpack(data, '<I', 60, 'DOS e_lfanew')[0]
        require(pe >= 64, 'PE header 覆盖 DOS')
        require(data[pe:pe + 4] == b'PE\0\0', 'PE signature 无效')
        machine, count, _, symoff, nsym, optsize, flags = unpack(data, '<HHIIIHH', pe + 4, 'PE COFF header')
        require(machine == 0xAA64 and count > 0 and flags & 2 and bool(nsym) == bool(symoff),
                '需要 ARM64 linked PE，COFF symbol pointer/count 一致')
        optional = pe + 24
        extent(optional, optsize, len(data), 'PE optional header')
        require(optsize >= 112 and unpack(data, '<H', optional, 'PE magic')[0] == 0x20B,
                '需要 PE32+ optional header')
        self.image_base = unpack(data, '<Q', optional + 24, 'PE ImageBase')[0]
        alignment, file_alignment = unpack(data, '<II', optional + 32, 'PE alignments')
        image_size, header_size = unpack(data, '<II', optional + 56, 'PE image/header size')
        self.image_size = image_size
        dirs = unpack(data, '<I', optional + 108, 'PE directories')[0]
        require(dirs <= 16 and optsize == 112 + dirs * 8 and
                self.image_base % 65536 == 0 and self.image_base + image_size <= 1 << 64 and
                alignment >= file_alignment >= 512 and file_alignment <= 65536 and
                alignment & (alignment - 1) == file_alignment & (file_alignment - 1) == 0,
                'PE optional layout/alignment 无效')
        table = optional + optsize
        extent(table, count * 40, len(data), 'PE section table')
        require(table + count * 40 <= header_size <= len(data) and
                header_size % file_alignment == 0 and image_size % alignment == 0,
                'PE SizeOfHeaders/Image 无效')
        self.sections = []
        for index in range(1, count + 1):
            name, virtual, rva, raw_size, raw, reloc, line, nr, nl, characteristics = unpack(
                data, '<8sIIIIIIHHI', table + (index - 1) * 40, 'PE section')
            require(not name.startswith(b'/') and reloc == line == nr == nl == 0,
                    'PE section longname/COFF tables 不支持')
            require(rva >= header_size and rva % alignment == 0 and
                    raw_size % file_alignment == 0, 'PE section alignment/header copy 无效')
            extent(rva, max(virtual, raw_size), image_size, 'PE virtual section')
            if raw_size:
                require(raw >= header_size and raw % file_alignment == 0, 'PE raw 覆盖 header/未对齐')
                extent(raw, raw_size, len(data), 'PE raw section')
            else:
                require(raw == 0, 'PE empty raw pointer 无效')
            require(not characteristics & 0x20000000 or virtual <= raw_size,
                    'PE executable section 包含 unbacked virtual tail')
            self.sections.append(dict(index=index, name=cstring(name + b'\0', 0, 'PE section name'),
                                      rva=rva, virtual=virtual, size=raw_size, raw=raw, flags=characteristics))
        disjoint([(s['raw'], s['size']) for s in self.sections], 'PE raw sections')
        disjoint([(s['rva'], max(s['virtual'], s['size'])) for s in self.sections], 'PE virtual sections')
        self.symbols = []
        if nsym:
            require(symoff >= header_size, 'PE COFF symbols 覆盖 headers')
            extent(symoff, nsym * 18 + 4, len(data), 'PE COFF symbol/string table')
            strsize = unpack(data, '<I', symoff + nsym * 18, 'PE COFF string size')[0]
            require(strsize >= 4, 'PE COFF string table 太短')
            extent(symoff + nsym * 18, strsize, len(data), 'PE COFF strings')
            strings = data[symoff + nsym * 18:symoff + nsym * 18 + strsize]
            disjoint([(s['raw'], s['size']) for s in self.sections] +
                     [(symoff, nsym * 18 + strsize)], 'PE COFF symbols/raw sections')
            index = 0
            while index < nsym:
                name, value, section, typ, storage, aux = unpack(data, '<8sIhHBB', symoff + index * 18, 'PE COFF symbol')
                require(index + aux < nsym and -2 <= section <= count, 'PE COFF symbol/Aux 无效')
                if section > 0:
                    extent(value, 0, max(self.sections[section - 1]['virtual'],
                                         self.sections[section - 1]['size']), 'PE COFF symbol value')
                if name[:4] == b'\0' * 4:
                    offset = struct.unpack('<I', name[4:])[0]
                    require(offset >= 4, 'PE COFF longname 指向 string header')
                    name = cstring(strings, offset, 'PE COFF longname')
                else:
                    name = cstring(name + b'\0', 0, 'PE COFF name')
                self.symbols.append(dict(name=name, value=value, section=section, type=typ, storage=storage))
                index += 1 + aux
        self.directories = [unpack(data, '<II', optional + 112 + i * 8, 'PE directory') for i in range(dirs)]
        for index, (address, size) in enumerate(self.directories):
            require(bool(address) == bool(size), 'PE directory pointer/size 不一致')
            if size:
                if index == 4:
                    extent(address, size, len(data), 'PE certificate file range')
                    require(address >= max(s['raw'] + s['size'] for s in self.sections),
                            'PE certificate 覆盖 section')
                elif index == 12:
                    self.memory(address, size)
                else:
                    self.map(address, size, executable=False)

    def map(self, rva, size, executable=True):
        extent(rva, size, (1 << 32) - 1, 'PE RVA')
        matches = [s for s in self.sections if s['rva'] <= rva and
                   rva + size <= s['rva'] + s['virtual'] and
                   rva + size <= s['rva'] + s['size']]
        require(len(matches) == 1, 'PE RVA 不在唯一 file-backed virtual section')
        section = matches[0]
        require(not executable or section['flags'] & CODE_EXEC == CODE_EXEC,
                'PE kernel 不是 executable code')
        offset = section['raw'] + rva - section['rva']
        extent(offset, size, len(self.data), 'PE mapped bytes')
        return section, offset

    def memory(self, rva, size):
        """loader 写目标可在合法 data zero-fill；读取表仍必须走 file-backed map。"""
        extent(rva, size, self.image_size, 'PE loader memory RVA')
        matches = [s for s in self.sections if s['rva'] <= rva and rva + size <= s['rva'] + s['virtual']]
        require(size > 0 and len(matches) == 1, 'PE loader target 不在唯一 virtual section')
        return matches[0]

    def va_rva(self, va):
        require(type(va) is int and self.image_base <= va < self.image_base + self.image_size,
                'PE loader VA/RVA 范围无效')
        return va - self.image_base

    def read(self, rva, size, label):
        _, offset = self.map(rva, size, executable=False)
        return self.data[offset:offset + size]

    def name(self, rva, hint=False):
        section, offset = self.map(rva, 3 if hint else 1, executable=False)
        end = section['raw'] + min(section['size'], section['virtual'])
        name = cstring(self.data[offset + (2 if hint else 0):end], 0, 'PE import name')
        require(bool(name), 'PE import name 为空')

    def thunks(self, rva):
        require(rva != 0 and rva % 8 == 0, 'PE import lookup alignment/pointer 无效')
        section, offset = self.map(rva, 8, executable=False)
        available = min(section['virtual'], section['size']) - (rva - section['rva'])
        for index in range(available // 8):
            value = unpack(self.data, '<Q', offset + index * 8, 'PE import lookup')[0]
            if value == 0:
                return index + 1  # 保守包括终止slot，不把缺失目录当空write set。
            if value >> 63:
                require(value & 0x7FFFFFFFFFFF0000 == 0, 'PE import ordinal reserved bits 无效')
            else:
                require(value <= 0xFFFFFFFF, 'PE import thunk 不是 RVA')
                self.name(value, hint=True)
        raise ValueError('PE import lookup 缺少有界终止项')

    def loader_writes(self):
        """枚举 PE metadata 指定的 loader 写；不执行/分析 TLS callback 或 DllMain。"""
        writes = []
        def directory(index):
            return self.directories[index] if index < len(self.directories) else (0, 0)
        def write(address, size, label, alignment=1):
            require(address != 0 and address % alignment == 0, f'PE loader {label} pointer/alignment 无效')
            self.memory(address, size)
            writes.append((address, size, label))
        for index in (7, 8, 10, 11, 14, 15):
            require(directory(index) == (0, 0), f'PE loader 不支持 directory {index} mutation mechanism')
        # IAT directory 仅补充保守范围；authoritative FirstThunk 不依赖它。
        address, size = directory(12)
        if size:
            require(size % 8 == 0, 'PE import IAT directory size 无效')
            write(address, size, 'declared IAT', 8)
        for index, descriptor_size in ((1, 20), (13, 32)):
            address, size = directory(index)
            if not size:
                continue
            require(size >= descriptor_size and size % descriptor_size == 0,
                    'PE import descriptor table/terminator size 无效')
            table = self.read(address, size, 'import descriptors')
            terminated = False
            for offset in range(0, size, descriptor_size):
                fields = struct.unpack_from('<' + 'I' * (descriptor_size // 4), table, offset)
                if not any(fields):
                    require(not any(table[offset:]), 'PE import descriptor 终止后存在非零记录')
                    terminated = True
                    break
                if index == 1:
                    lookup, timestamp, forwarder, name, target = fields
                    require(timestamp == 0 and forwarder in (0, 0xFFFFFFFF),
                            'PE loader 不支持 pre-bound import/forwarder write mechanism')
                    require(name and target, 'PE import descriptor Name/FirstThunk 缺失')
                    self.name(name)
                    count = self.thunks(lookup or target)
                    write(target, count * 8, 'normal import FirstThunk', 8)
                else:
                    attributes, name, module, target, lookup, bound, unload, _ = fields
                    require(attributes == 1, 'PE delay import 仅支持明确 RVA descriptors')
                    require(name and module and target and lookup, 'PE delay import required pointer 缺失')
                    self.name(name)
                    count = self.thunks(lookup)
                    write(module, 8, 'delay import module handle', 8)
                    write(target, count * 8, 'delay import IAT', 8)
                    for pointer, label in ((bound, 'bound IAT'), (unload, 'unload IAT')):
                        if pointer:
                            require(pointer % 8 == 0, f'PE delay {label} alignment 无效')
                            data = self.read(pointer, count * 8, label)
                            require(data[-8:] == b'\0' * 8, f'PE delay {label} 缺少终止项')
                            if label == 'unload IAT':
                                write(pointer, count * 8, 'delay unload IAT', 8)
            require(terminated, 'PE import descriptor 缺少终止项')
        address, size = directory(9)
        if size:
            require(size == 40, 'PE TLS directory size 不支持')
            start, end, index, callbacks, zero_fill, characteristics = struct.unpack(
                '<QQQQII', self.read(address, 40, 'TLS directory'))
            require(bool(start) == bool(end) and start <= end and characteristics & ~0x00F00000 == 0 and
                    characteristics >> 20 <= 14, 'PE TLS template/characteristics 无效')
            if start:
                start_rva = self.va_rva(start)
                require(end <= self.image_base + self.image_size and end - start + zero_fill <= 0xFFFFFFFF,
                        'PE TLS template extent 无效')
                if end > start:
                    self.map(start_rva, end - start, executable=False)
            write(self.va_rva(index), 4, 'TLS AddressOfIndex', 4)
            if callbacks:
                callback_rva = self.va_rva(callbacks)
                require(callback_rva % 8 == 0, 'PE TLS callbacks alignment 无效')
                section, offset = self.map(callback_rva, 8, executable=False)
                available = min(section['virtual'], section['size']) - (callback_rva - section['rva'])
                terminated = False
                for index in range(available // 8):
                    callback = unpack(self.data, '<Q', offset + index * 8, 'TLS callback')[0]
                    if not callback:
                        terminated = True
                        break
                    self.map(self.va_rva(callback), 1)
                require(terminated, 'PE TLS callbacks 缺少有界终止项')
        address, size = directory(5)
        if size:
            table = self.read(address, size, 'base relocation directory')
            position = 0
            while position < size:
                require(size - position >= 8, 'PE base relocation header 截断')
                page, block = struct.unpack_from('<II', table, position)
                require(page % 4096 == 0 and block >= 8 and block % 2 == 0 and block <= size - position,
                        'PE base relocation block 无效')
                for delta in range(8, block, 2):
                    entry = struct.unpack_from('<H', table, position + delta)[0]
                    typ = entry >> 12
                    require(typ in (0, 10), 'PE loader 不支持 ARM64 base relocation type')
                    if typ:
                        write(page + (entry & 4095), 8, 'base relocation DIR64')
                position += block
        return writes

    def kernel(self, rva):
        pin = recipes.trusted()['kernel']
        symbols = [s for s in self.symbols if s['name'] == pin['symbol']]
        require(len(symbols) <= 1, 'PE COFF kernel symbol 重复')
        if symbols:
            symbol = symbols[0]
            require(symbol['section'] > 0 and symbol['storage'] == 2 and symbol['type'] in (0, 0x20) and
                    self.sections[symbol['section'] - 1]['rva'] + symbol['value'] == rva,
                    'PE COFF kernel symbol 与 map/final RVA 不匹配')
        section, offset = self.map(rva, pin['size'])
        require(digest(self.data[offset:offset + pin['size']]) == pin['sha256'], 'PE 固定核 bytes 不匹配')
        for target, size, label in self.loader_writes():
            require(target + size <= rva or target >= rva + pin['size'], f'PE loader {label} 覆盖 kernel')
        return dict(rva=rva, section=section['name'], section_index=section['index'], file_offset=offset,
                    image_base=self.image_base, **pin)


def object_start(path):
    """仅定位/固定 bytes；故意不把它升级为函数范围证明。"""
    pin = recipes.trusted()['kernel']
    obj = Coff(regular(Path(path)))
    symbol = obj.symbol(pin['symbol'])
    require(symbol['storage'] == 2 and symbol['type'] in (0, 0x20), 'COFF kernel type/class 无效')
    section, offset, code = obj.code(symbol, pin['size'])
    require(digest(code) == pin['sha256'], 'COFF 固定核 bytes 不匹配')
    return obj, symbol, section, offset


def artifact(path):
    path = Path(path).resolve()
    return {'path': str(path), 'sha256': digest(regular(path))}


def _run(argv, cwd, log=None):
    result = subprocess.run(argv, cwd=cwd, check=False, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if log is not None:
        log.append({'argv': argv, 'returncode': result.returncode,
                    'stdout': result.stdout.decode(errors='replace'), 'stderr': result.stderr.decode(errors='replace')})
    require(result.returncode == 0, f'受控 compiler 调用失败：{result.stderr.decode(errors="replace")}')
    return result.stdout


def reject_external_assembler_input(text):
    """逐 statement 检查，不把 labels/分号/macro 动态 opcode 当作源依赖证明。

    当前固定 source 只需要 static opcode 与 macro labels/operands。
    动态构造 opcode 一律不支持；未展开的 macro body 也必须经过此门。
    """
    clean, index = [], 0
    while index < len(text):
        if text.startswith('//', index):
            end = text.find('\n', index)
            index = len(text) if end < 0 else end
            clean.append(' ')
        elif text.startswith('/*', index):
            end = text.find('*/', index + 2)
            require(end >= 0, 'assembler comment 截断')
            clean.append(' ' + '\n' * text[index:end + 2].count('\n'))
            index = end + 2
        elif text[index] in ('"', "'"):
            quote = text[index]
            index += 1
            # literal 的位置不能消失；opcode/label位置的 quoted token 不支持。
            clean.append(' @QUOTED@ ')
            closed = False
            while index < len(text):
                char = text[index]
                index += 1
                if char == '\\':
                    require(index < len(text), 'assembler string escape 截断')
                    index += 1
                elif char == quote:
                    closed = True
                    break
            require(closed, 'assembler string 截断')
        else:
            clean.append(text[index])
            index += 1
    for statement in re.split(r'[;\n]', ''.join(clean)):
        statement = statement.strip()
        if not statement or statement.startswith('#'):
            continue
        while True:
            label = re.match(r'(?:[A-Za-z_.$\\][A-Za-z0-9_.$\\()]*|[0-9]+)\s*:', statement)
            if label is None:
                break
            statement = statement[label.end():].lstrip()
        if not statement:
            continue
        opcode = statement.split()[0]
        require(re.fullmatch(r'(?:\.[A-Za-z_][A-Za-z0-9_]*|[A-Za-z_][A-Za-z0-9_.]*)', opcode) is not None,
                'assembler 动态 opcode/未知 statement 不支持')
        require(opcode.lower() not in ('.include', '.incbin'), 'assembler 外部文件读取不支持')


def validate_physical_config(data):
    """物理 config 域只允许 generated CPP配置；不信可伪造的逻辑#line owner。"""
    try:
        text = data.decode('utf-8')
    except UnicodeDecodeError as error:
        raise ValueError('config.h 不是受支持的 UTF-8 CPP configuration') from error
    require('\x00' not in text and '??/' not in text, 'config.h NUL/trigraph control 不支持')
    # C phase2先拼接 escaped-newline，然后phase3移除comments；顺序不可交换。
    text = text.replace('\r\n', '\n').replace('\r', '\n').replace('\\\n', '')
    clean, index = [], 0
    while index < len(text):
        if text.startswith('//', index):
            end = text.find('\n', index)
            index = len(text) if end < 0 else end
            clean.append(' ')
        elif text.startswith('/*', index):
            end = text.find('*/', index + 2)
            require(end >= 0, 'config.h comment 截断')
            clean.append(' ')
            index = end + 2
        elif text[index] in ('"', "'"):
            start, quote = index, text[index]
            index += 1
            closed = False
            while index < len(text):
                char = text[index]
                index += 1
                if char == '\\':
                    require(index < len(text), 'config.h string escape 截断')
                    index += 1
                elif char == quote:
                    closed = True
                    break
                else:
                    require(char != '\n', 'config.h multiline string 不支持')
            require(closed, 'config.h string 截断')
            clean.append(text[start:index])
        else:
            clean.append(text[index])
            index += 1
    allowed = {'define', 'undef', 'if', 'ifdef', 'ifndef', 'elif', 'else', 'endif', 'error'}
    for line in ''.join(clean).splitlines():
        if not line.strip():
            continue
        directive = re.fullmatch(r'\s*#\s*([A-Za-z_][A-Za-z0-9_]*)?(.*)', line)
        require(directive is not None, 'config.h 不能发出 raw assembler statement/未知 CPP control')
        name, body = directive.groups()
        require((name is None and not body.strip()) or name in allowed,
                'config.h #line/numeric/include/pragma/未知 CPP control 不支持')
        require(not re.search(r'\b(?:_Pragma|__pragma|__has_include|__has_include_next)\b', body),
                'config.h 副作用 CPP builtin 不支持')


def cpp_physical_includes(trace):
    """只解析成功受控Clang该次-H stderr；未知诊断不当作依赖授权。"""
    inputs = []
    for line in trace.splitlines():
        if not line.strip():
            continue
        match = re.fullmatch(r'\.+ (.+)', line)
        require(match is not None, 'config/source CPP physical include trace 含未知诊断/格式')
        inputs.append(match[1])
    require(inputs, 'config/source CPP physical include trace 缺失')
    return inputs


def replay_source_endpoint(tree, archive, object_path, compiler, flags, evidence_dir):
    """重放真实 compiler；私有 marker 仅加到预处理副本，从不修改 shipping .S/object。

    flags 是受控列表，只接受明确 ARM64 Windows target、机器码/预定义选项。
    所有预处理依赖都必须为 pinned archive bytes、fixed patched .S 或 config.h。
    config.mak 只记录，不解释/执行。受信调用者还须证明这是实际 build 的配置。
    """
    tree, archive, object_path = map(lambda p: Path(p).resolve(), (tree, archive, object_path))
    # lld/clang 工具可能按 argv[0] 名称选择 driver；hash 跟随实际 bytes，调用名必须保留。
    compiler = Path(compiler).absolute()
    evidence_dir = Path(evidence_dir).resolve()
    require(not evidence_dir.is_relative_to(tree) and not object_path.is_relative_to(evidence_dir),
            'endpoint evidence 必须位于 shipping tree/object 之外')
    evidence_dir.mkdir(parents=True, exist_ok=True)
    for name in ('source-endpoint-preprocessed.s', 'source-endpoint-marked.s', 'source-endpoint-proof.obj',
                 'source-endpoint-replay.obj', 'source-endpoint-predefines.txt', 'source-endpoint-command-log.json'):
        output = evidence_dir / name
        require(not output.is_symlink() and
                (not output.exists() or (output.is_file() and output.stat().st_nlink == 1)),
                'endpoint output 必须是独立普通文件，拒绝 link/其它节点')
    logs = []
    require(isinstance(flags, list) and all(type(f) is str for f in flags), 'compiler flags 必须是字符串列表')
    require(len(flags) > 0 and flags[0] in ('--target=aarch64-w64-mingw32', '--target=aarch64-pc-windows-msvc'),
            'source endpoint 需要显式 ARM64 Windows target')
    require(all(re.fullmatch(r'-(?:march|mcpu|mtune|mbranch-protection)=[A-Za-z0-9_+.-]+', f) or
                f in ('-fPIC', '-fpic', '-g0', '-O0', '-O1', '-O2', '-O3', '-Os', '-Oz') or
                re.fullmatch(r'-[DU][A-Za-z_][A-Za-z0-9_]*(?:=[A-Za-z0-9_]+)?', f) for f in flags[1:]),
            'compiler flags 包含未支持的输入/输出/plugin/response 参数')
    pin = recipes.trusted()
    require(digest(regular(archive)) == recipes.ARCHIVE_SHA, 'endpoint 原归档 pin 不匹配')
    require(digest(regular(tree / SOURCE)) == pin['files'][SOURCE]['post'], 'endpoint source postimage 不匹配')
    configs = {p: artifact(tree / p) for p in ('config.h', 'ffbuild/config.mak')}
    validate_physical_config(regular(tree / 'config.h'))
    compiler_record = artifact(compiler)
    compiler_record['version'] = _run([str(compiler), '--version'], tree, logs).decode().strip()
    prefix = [str(compiler), *flags, '-I', str(tree)]
    preprocess = [*prefix, '-H', '-E', '-x', 'assembler-with-cpp', str(tree / SOURCE)]
    preprocessed = _run(preprocess, tree, logs)
    text = preprocessed.decode('utf-8')
    reject_external_assembler_input(text)
    # 主source是受控argv本身；include物理路径来自这次真实-H，不能由#line伪造。
    dependencies = set([str(tree / SOURCE), *cpp_physical_includes(logs[-1]['stderr'])])
    inputs = {}
    with tarfile.open(archive) as package:
        for dependency in sorted(dependencies):
            path = Path(dependency)
            path = (tree / path).resolve() if not path.is_absolute() else path.resolve()
            require(path.is_relative_to(tree), 'endpoint 包含树外输入')
            relative = str(path.relative_to(tree))
            data = regular(path)
            if relative not in (SOURCE, 'config.h'):
                try:
                    member = package.getmember('ffmpeg-8.1.2/' + relative)
                except KeyError as error:
                    raise ValueError('endpoint include 不是归档成员') from error
                require(member.isfile() and data == package.extractfile(member).read(),
                        'endpoint include 未修改源 pin 不匹配')
            inputs[relative] = artifact(path)
    require(SOURCE in inputs and 'config.h' in inputs, 'endpoint 缺少 source/config preprocessing 依赖')
    symbol = pin['kernel']['symbol']
    pattern = rf'(?m)^function\s+{re.escape(symbol)},\s*export=1\s*$'
    starts = list(re.finditer(pattern, text))
    require(len(starts) == 1 and START not in text and END not in text, 'endpoint function/marker 不唯一')
    start = starts[0].end()
    end_match = re.search(r'(?m)^endfunc\s*$', text[start:])
    require(end_match is not None and not re.search(r'(?m)^function\s', text[start:start + end_match.start()]),
            'endpoint endfunc 不唯一匹配')
    end = start + end_match.start()
    marked = text[:start] + f'\n.globl {START}\n.set {START}, {symbol}\n' + text[start:end] + \
        f'.globl {END}\n{END}:\n' + text[end:]
    actual, actual_symbol, actual_section, _ = object_start(object_path)
    # 固定私有路径保留原始输出及实际 argv，重放结果可逐字节核对。
    proof_source = evidence_dir / 'source-endpoint-marked.s'
    proof_object = evidence_dir / 'source-endpoint-proof.obj'
    replay_object = evidence_dir / 'source-endpoint-replay.obj'
    (evidence_dir / 'source-endpoint-preprocessed.s').write_bytes(preprocessed)
    proof_source.write_text(marked, encoding='utf-8')
    # 重放未加 marker 的同一次 source/config 与真正输入 object 独立比对。
    compile_argv = [*prefix, '-c', '-x', 'assembler-with-cpp', str(tree / SOURCE), '-o', str(replay_object)]
    _run(compile_argv, tree, logs)
    require(regular(replay_object) == regular(object_path), 'endpoint compiler replay 与 actual object 不相同')
    proof_argv = [str(compiler), *flags, '-c', '-x', 'assembler', str(proof_source), '-o', str(proof_object)]
    _run(proof_argv, tree, logs)
    proof = Coff(regular(proof_object))
    proof_symbol, first, last = proof.symbol(symbol), proof.symbol(START), proof.symbol(END)
    require(first['section'] == last['section'] == proof_symbol['section'] == actual_symbol['section'] and
            first['value'] == proof_symbol['value'] == actual_symbol['value'] and
            last['value'] - first['value'] == pin['kernel']['size'], 'endpoint actual start/end 不符合固定276范围')
    _, _, code = proof.code(first, pin['kernel']['size'])
    require(digest(code) == pin['kernel']['sha256'], 'endpoint proof 固定 code 不匹配')
    actual_exec = [(s['name'], s['size'], actual.data[s['raw']:s['raw'] + s['size']])
                   for s in actual.sections if s['flags'] & CODE_EXEC == CODE_EXEC]
    proof_exec = [(s['name'], s['size'], proof.data[s['raw']:s['raw'] + s['size']])
                  for s in proof.sections if s['flags'] & CODE_EXEC == CODE_EXEC]
    require(actual_exec == proof_exec, 'endpoint proof 改变 executable sections')
    proof_hash = digest(proof.data)
    predefines = _run([*prefix, '-dM', '-E', '-x', 'assembler-with-cpp', str(tree / SOURCE)], tree, logs)
    (evidence_dir / 'source-endpoint-predefines.txt').write_bytes(predefines)
    (evidence_dir / 'source-endpoint-command-log.json').write_text(
        json.dumps(logs, indent=2, sort_keys=True) + '\n', encoding='utf-8')
    outputs = {name: artifact(evidence_dir / name) for name in (
        'source-endpoint-preprocessed.s', 'source-endpoint-marked.s', 'source-endpoint-proof.obj',
        'source-endpoint-replay.obj', 'source-endpoint-predefines.txt', 'source-endpoint-command-log.json')}
    return dict(kind='fixed_recipe_with_source_endpoint', archive=artifact(archive), inputs=inputs,
                configuration=configs, compiler=compiler_record, flags=flags, object=artifact(object_path),
                preprocess_argv=preprocess, preprocessed_sha256=digest(preprocessed),
                source_compile_argv=[*prefix, '-c', '-x', 'assembler-with-cpp', str(tree / SOURCE),
                                     '-o', str(object_path)],
                marked_preprocessed_sha256=digest(marked.encode()), proof_object_sha256=proof_hash,
                start=first['value'], end=last['value'], section=actual_section['name'], outputs=outputs)


def coff_kernel_identity(path, endpoint=None):
    obj, symbol, section, offset = object_start(path)
    pin = recipes.trusted()['kernel']
    if symbol['naux']:
        require(endpoint is None, 'function Aux extent 不接受未使用的 endpoint assertion')
        require(symbol['type'] == 0x20 and symbol['storage'] == 2 and symbol['naux'] == 1,
                'COFF kernel Aux 不是 function-definition')
        tag, size, line, next_function, reserved = struct.unpack('<IIIIH', symbol['aux'])
        require(tag == line == next_function == reserved == 0 and size == pin['size'],
                'COFF function Aux TotalSize/association 不符合有界支持')
        require(not any(s['index'] != symbol['index'] and s['section'] == symbol['section'] and
                        s['storage'] == 2 and symbol['value'] <= s['value'] < symbol['value'] + size
                        for s in obj.symbols), 'COFF function Aux 与其它定义冲突')
        method, evidence = 'coff_function_aux_totalsize', None
    else:
        require(isinstance(endpoint, dict), 'COFF 无函数 Aux：需要独立 source endpoint replay')
        require(set(endpoint) == {'tree', 'archive', 'compiler', 'flags', 'evidence_dir'}, 'endpoint 输入字段集合无效')
        evidence = replay_source_endpoint(endpoint['tree'], endpoint['archive'], path,
                                          endpoint['compiler'], endpoint['flags'], endpoint['evidence_dir'])
        method = 'fixed_recipe_with_source_endpoint'
    return dict(schema=SCHEMA, architecture='arm64', recipe=recipes.MODIFIED, object=artifact(path),
                section=section['name'], section_index=section['index'], section_offset=symbol['value'],
                file_offset=offset, extent_kind=method, endpoint=evidence, **pin)


def lld_map_identity(map_path, object_path, obj_record, image, *, link_cwd=None):
    """支持实测 lld-link /lldmap 与 ld.lld MinGW --Map 的同一列格式。"""
    lines = regular(Path(map_path)).decode('utf-8').splitlines()
    require(lines and lines[0].split() == ['Address', 'Size', 'Align', 'Out', 'In', 'Symbol'],
            '不支持的 link map format')
    outputs, contributions, symbols = [], [], []
    output = contribution = None
    for line in lines[1:]:
        if not line.strip():
            continue
        row = re.fullmatch(r'([0-9A-Fa-f]{8,16})\s+([0-9A-Fa-f]{8,16})\s+([0-9]+) (.*)', line)
        require(row is not None, 'link map row 无效')
        address, size, align = int(row[1], 16), int(row[2], 16), int(row[3])
        tail = row[4]
        if not tail.startswith(' '):
            require(tail.startswith('.') and align > 0 and align & (align - 1) == 0,
                    'link map output row 无效')
            matches = [s for s in image.sections if s['name'] == tail and s['rva'] == address and s['virtual'] == size]
            require(len(matches) == 1, 'link map output 与 PE section 不匹配')
            output = dict(address=address, size=size, name=tail)
            outputs.append(output)
            contribution = None
        elif ':(' in tail:
            require(output is not None and align > 0 and align & (align - 1) == 0,
                    'link map contribution context 无效')
            match = re.fullmatch(r' +(.+):\((\.[A-Za-z0-9_$.-]+)\)', tail)
            require(match is not None and '(' not in match[1] and ')' not in match[1],
                    '不支持 archive/ambiguous map contribution')
            extent(address - output['address'], size, output['size'], 'map contribution')
            contribution = dict(address=address, size=size, path=match[1], section=match[2], output=output['name'])
            contributions.append(contribution)
        else:
            require(contribution is not None and size == align == 0 and
                    re.fullmatch(r' +[A-Za-z_.$?@][A-Za-z0-9_.$?@]*', tail) is not None,
                    'link map symbol context/row 无效')
            extent(address - contribution['address'], 0, contribution['size'], 'map symbol')
            symbols.append(dict(address=address, name=tail.strip(), contribution=contribution))
    disjoint([(o['address'], o['size']) for o in outputs], 'map outputs')
    disjoint([(c['address'], c['size']) for c in contributions], 'map input contributions')
    matches = [s for s in symbols if s['name'] == obj_record['symbol']]
    require(len(matches) == 1, 'link map 固定 symbol 缺失/重复')
    symbol = matches[0]
    contribution = symbol['contribution']
    mapped_object = Path(contribution['path'])
    require(mapped_object.is_absolute() or '/' in contribution['path'] or '\\' in contribution['path'],
            'bare kernel member basename 无法区分 direct object/archive；不支持 kernel archive')
    if not mapped_object.is_absolute():
        require(link_cwd is not None and Path(link_cwd).is_absolute() and Path(link_cwd).is_dir(),
                'relative map input object 需要明确 actual link cwd')
        mapped_object = Path(link_cwd) / mapped_object
    require(mapped_object.resolve() == Path(object_path).resolve(), 'map input object path 不匹配')
    obj = Coff(regular(Path(object_path)))
    section = obj.sections[obj_record['section_index'] - 1]
    require(contribution['section'] == section['name'] and contribution['size'] == section['size'] and
            symbol['address'] == contribution['address'] + obj_record['section_offset'],
            'map exact object contribution/offset 不匹配')
    extent(obj_record['section_offset'], obj_record['size'], contribution['size'], 'map kernel')
    return image.kernel(symbol['address']), contribution


def capture_pe_kernel(object_path, linked_pe, link_map, *, endpoint=None, build_inputs):
    """build_inputs 每项为实际 tool/config/结构化 invocation 文件路径；只 hash，绝不执行。"""
    require(isinstance(build_inputs, dict) and set(build_inputs) ==
            {'compiler', 'linker', 'strip', 'config.h', 'config.mak', 'compile_invocation', 'link_invocation',
             'strip_invocation'}, 'build inputs 字段集合无效')
    obj_record = coff_kernel_identity(object_path, endpoint)
    image = Pe(regular(Path(linked_pe)))
    inputs = {k: artifact(v) for k, v in build_inputs.items()}
    invocations = {}
    for name, fields, tool in (
            ('compile_invocation', {'argv', 'cwd', 'inputs', 'output'}, 'compiler'),
            ('link_invocation', {'argv', 'cwd', 'inputs', 'outputs'}, 'linker'),
            ('strip_invocation', {'argv', 'cwd', 'input'}, 'strip')):
        try:
            def unique_fields(pairs):
                require(len({key for key, _ in pairs}) == len(pairs), 'invocation JSON duplicate fields')
                return dict(pairs)
            data = json.loads(regular(Path(build_inputs[name])), object_pairs_hook=unique_fields)
        except (json.JSONDecodeError, UnicodeDecodeError) as error:
            raise ValueError('invocation 必须为结构化 JSON data') from error
        require(isinstance(data, dict) and set(data) == fields and isinstance(data['argv'], list) and
                data['argv'] and all(type(arg) is str and '\0' not in arg for arg in data['argv']) and
                Path(data['argv'][0]).is_absolute() and
                Path(data['argv'][0]).resolve() == Path(inputs[tool]['path']) and type(data['cwd']) is str and
                Path(data['cwd']).is_absolute() and Path(data['cwd']).is_dir(),
                'invocation fields/actual tool/cwd 不匹配')
        if 'inputs' in data:
            require(isinstance(data['inputs'], list) and data['inputs'] and
                    all(isinstance(item, dict) and set(item) == {'path', 'sha256'} and
                        type(item['path']) is str and item == artifact(item['path']) for item in data['inputs']),
                    'invocation input bytes/path 不匹配')
            paths = [item['path'].casefold() for item in data['inputs']]
            require(len(paths) == len(set(paths)), 'invocation input duplicate/case collision')
        invocations[name] = data
    compile_data, link_data, strip_data = (invocations[name] for name in
                                          ('compile_invocation', 'link_invocation', 'strip_invocation'))
    require(compile_data['output'] == artifact(object_path) and artifact(object_path) in link_data['inputs'] and
            link_data['outputs'] == {'module': artifact(linked_pe), 'map': artifact(link_map)} and
            strip_data['input'] == artifact(linked_pe), 'same compile/link/strip input/output chain 不匹配')
    def command_path(value, cwd):
        path = Path(value)
        return path.resolve() if path.is_absolute() else (Path(cwd) / path).resolve()
    link_args = link_data['argv'][1:]
    require(any(command_path(arg, link_data['cwd']) == Path(object_path).resolve()
                for arg in link_args if not arg.startswith(('-', '/out:', '/lldmap:', '/machine:', '/opt:'))),
            'link argv 缺少 exact input object')
    module_args, map_args = [], []
    for index, arg in enumerate(link_args):
        if arg.startswith('/out:'):
            module_args.append(arg[5:])
        elif arg == '-o' and index + 1 < len(link_args):
            module_args.append(link_args[index + 1])
        elif arg.startswith('/lldmap:'):
            map_args.append(arg[8:])
        elif arg.startswith('--Map='):
            map_args.append(arg[6:])
    require(len(module_args) == len(map_args) == 1 and
            command_path(module_args[0], link_data['cwd']) == Path(linked_pe).resolve() and
            command_path(map_args[0], link_data['cwd']) == Path(link_map).resolve(),
            'link argv module/map output 与同次 evidence 不匹配')
    require(inputs['config.h'] in compile_data['inputs'] and inputs['config.mak'] in compile_data['inputs'],
            'compile invocation 缺少 actual configuration inputs')
    kernel, contribution = lld_map_identity(link_map, object_path, obj_record, image, link_cwd=link_data['cwd'])
    if obj_record['endpoint'] is not None:
        proof = obj_record['endpoint']
        require(inputs['compiler'] == {k: proof['compiler'][k] for k in ('path', 'sha256')} and
                inputs['config.h'] == proof['configuration']['config.h'] and
                inputs['config.mak'] == proof['configuration']['ffbuild/config.mak'] and
                all(item in compile_data['inputs'] for item in proof['inputs'].values()),
                'endpoint compiler/config/source 与 compile evidence 不匹配')
        require(compile_data['argv'] == proof['source_compile_argv'] and
                compile_data['cwd'] == str(Path(endpoint['tree']).resolve()),
                'source compile argv/cwd 与 endpoint replay 不相同')
    return dict(schema=SCHEMA, architecture='arm64', recipe=recipes.MODIFIED,
                kernel=kernel, object_kernel=obj_record, linked=artifact(linked_pe), map=artifact(link_map),
                map_format='lld-coff-address-size-v1', contribution=contribution,
                endpoint_request=endpoint, build_inputs=inputs)


def verify_pe_kernel(final_pe, record, *, endpoint=None):
    """信任 record 外层 authenticity 由消费者负责；重放所有输入，拒绝自授权与 stale 链。"""
    require(isinstance(record, dict) and set(record) == {'schema', 'architecture', 'recipe', 'kernel',
            'object_kernel', 'linked', 'map', 'map_format', 'contribution', 'endpoint_request', 'build_inputs'},
            'Windows kernel capture 字段集合无效')
    require(record['schema'] == SCHEMA and record['architecture'] == 'arm64' and
            record['recipe'] == recipes.MODIFIED and record['map_format'] == 'lld-coff-address-size-v1',
            'Windows fixed identity 不匹配')
    require(record['endpoint_request'] == endpoint,
            'source replay 需要调用者独立传入受信 endpoint；绝不执行记录中的 compiler')
    try:
        artifacts = [record['object_kernel']['object'], record['linked'], record['map'],
                     *record['build_inputs'].values()]
        proof = record['object_kernel']['endpoint']
        if proof is not None:
            artifacts.extend([proof['archive'], proof['object'], *proof['inputs'].values(),
                              *proof['configuration'].values(), *proof['outputs'].values()])
            artifacts.append({k: proof['compiler'][k] for k in ('path', 'sha256')})
        require(all(isinstance(item, dict) and set(item) == {'path', 'sha256'} and
                    type(item['path']) is str and item == artifact(item['path']) for item in artifacts),
                'capture 文件/hash 改变；拒绝覆盖原始 evidence')
    except (KeyError, TypeError, AttributeError) as error:
        raise ValueError('Windows capture artifact 结构无效') from error
    try:
        rebuilt = capture_pe_kernel(record['object_kernel']['object']['path'], record['linked']['path'],
                                    record['map']['path'], endpoint=endpoint,
                                    build_inputs={k: v['path'] for k, v in record['build_inputs'].items()})
    except (KeyError, TypeError) as error:
        raise ValueError('Windows capture 输入结构无效') from error
    require(record == rebuilt, 'Windows capture 输入/hash/metadata 不匹配')
    actual = Pe(regular(Path(final_pe))).kernel(record['kernel']['rva'])
    require(actual['image_base'] == record['kernel']['image_base'], 'final ImageBase 与同次链接不匹配')
    return dict(schema=SCHEMA, extent_kind=record['object_kernel']['extent_kind'],
                final=artifact(final_pe), linked_sha256=record['linked']['sha256'], kernel=actual)
