#!/usr/bin/env python3
"""Windows ARM64 容器契约；不表示 shipping FFmpeg 或 native 执行通过。"""
import importlib.util
import copy
import json
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import unittest

TOOLS = Path(__file__).resolve().parent.parent


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def put(data, offset, fmt, *values):
    result = bytearray(data)
    struct.pack_into(fmt, result, offset, *values)
    return bytes(result)


class WindowsKernelContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory(prefix='frd-windows-kernel-tests-')
        cls.directory = Path(cls.temporary.name)
        cls.k = load('windows_kernel_tests', TOOLS / 'windows-ffmpeg-kernel.py')
        cls.g = load('windows_fixture_tests', TOOLS / 'tests/generate_windows_kernel_fixture.py')
        cls.g.generate(cls.directory)
        cls.obj = cls.directory / 'windows-h264dc-wrapper.obj'
        cls.dll = cls.directory / 'windows-h264dc-wrapper.dll'
        cls.map = cls.directory / 'windows-h264dc-wrapper.map'
        cls.coff = cls.obj.read_bytes()
        cls.pe = cls.dll.read_bytes()
        cls.symoff = struct.unpack_from('<I', cls.coff, 8)[0]
        cls.nsym = struct.unpack_from('<I', cls.coff, 12)[0]
        cls.kernel_index = cls.nsym - 1
        cls.kernel_offset = cls.symoff + cls.kernel_index * 18
        cls.pe_header = struct.unpack_from('<I', cls.pe, 60)[0]
        cls.optional = cls.pe_header + 24
        cls.section = cls.optional + struct.unpack_from('<H', cls.pe, cls.pe_header + 20)[0]

    @classmethod
    def tearDownClass(cls):
        cls.temporary.cleanup()

    def save(self, name, data):
        path = self.directory / name
        path.write_bytes(data)
        return path

    def aux_object(self, size=276):
        # 手动加入真正 function-definition shape；不是实测 tool 的 Aux 声明。
        data = put(self.coff, 12, '<I', self.nsym + 1)
        data = put(data, self.kernel_offset + 17, '<B', 1)
        end = self.symoff + self.nsym * 18
        return data[:end] + struct.pack('<IIIIH', 0, size, 0, 0, 0) + data[end:]

    def inputs(self, config_tree=None, obj=None, dll=None, link_map=None):
        compiler, linker = Path(shutil.which('clang')).resolve(), Path(shutil.which('lld-link')).resolve()
        config = config_tree / 'config.h' if config_tree else self.save('synthetic-config.h', b'synthetic-config\n')
        make = config_tree / 'ffbuild/config.mak' if config_tree else self.save('synthetic-config.mak', b'synthetic-make\n')
        obj, dll, link_map = obj or self.obj, dll or self.dll, link_map or self.map
        source_inputs = [self.k.artifact(config), self.k.artifact(make)]
        if config_tree:
            source_inputs.extend(self.k.artifact(config_tree / relative) for relative in (
                'libavcodec/aarch64/h264idct_neon.S', 'libavcodec/aarch64/neon.S', 'libavutil/aarch64/asm.S'))
        compile_argv = [str(compiler), 'synthetic-never-executed-receipt']
        if config_tree:
            compile_argv = [str(compiler), '--target=aarch64-w64-mingw32', '-I', str(config_tree),
                            '-c', '-x', 'assembler-with-cpp',
                            str(config_tree / 'libavcodec/aarch64/h264idct_neon.S'), '-o', str(obj)]
        compile_call = self.save('synthetic-compile.json', json.dumps(
            {'argv': compile_argv, 'cwd': str(config_tree or self.directory), 'inputs': source_inputs,
             'output': self.k.artifact(obj)}).encode())
        link_call = self.save('synthetic-link.json', json.dumps(
            {'argv': [shutil.which('lld-link'), '/dll', '/noentry', '/nodefaultlib', '/machine:arm64',
                      '/opt:noref,noicf', '/timestamp:0', '/noimplib', '/lldmap:' + str(link_map),
                      '/out:' + str(dll), str(obj)], 'cwd': str(self.directory), 'inputs': [self.k.artifact(obj)],
             'outputs': {'module': self.k.artifact(dll), 'map': self.k.artifact(link_map)}}).encode())
        strip_call = self.save('synthetic-strip.json', json.dumps(
            {'argv': ['/usr/bin/strip', 'synthetic-never-executed-receipt'], 'cwd': str(self.directory), 'input': self.k.artifact(dll)}).encode())
        return {'compiler': compiler, 'linker': linker, 'strip': Path('/usr/bin/strip'),
                'config.h': config, 'config.mak': make, 'compile_invocation': compile_call,
                'link_invocation': link_call, 'strip_invocation': strip_call}

    def aux_capture(self):
        obj = self.save('windows-h264dc-aux.obj', self.aux_object())
        # Map names are a deliberate synthetic test contract, not link execution evidence.
        link_map = self.save('windows-h264dc-aux.map', self.map.read_bytes().replace(
            str(self.obj).encode(), str(obj).encode()))
        return self.k.capture_pe_kernel(obj, self.dll, link_map, build_inputs=self.inputs(obj=obj, link_map=link_map))

    def test_checked_coff_rejects_truncated_header(self):
        path = TOOLS / 'windows-ffmpeg-kernel.py'
        self.assertTrue(path.is_file(), '缺少独立 Windows COFF/PE 契约实现')
        spec = importlib.util.spec_from_file_location('windows_kernel', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with self.assertRaises(ValueError):
            module.Coff(b'\x64\xaa')

    def test_real_wrapper_coff_start_is_not_extent_proof(self):
        _, symbol, section, offset = self.k.object_start(self.obj)
        self.assertEqual((symbol['value'], section['size'], offset), (0, 276, 140))
        self.assertEqual(symbol['naux'], 0)
        with self.assertRaisesRegex(ValueError, 'endpoint'):
            self.k.coff_kernel_identity(self.obj)
        with self.assertRaises(ValueError):
            self.k.coff_kernel_identity(self.obj, {'size': 276, 'approved': True})

    def test_function_aux_totalsize_is_distinct_from_section_aux(self):
        obj = self.save('aux-positive.obj', self.aux_object())
        record = self.k.coff_kernel_identity(obj)
        self.assertEqual(record['extent_kind'], 'coff_function_aux_totalsize')
        self.assertEqual(record['size'], 276)
        # Clang's existing .text section-definition Aux Length=276 does not authorize it.
        with self.assertRaises(ValueError):
            self.k.coff_kernel_identity(self.obj)

    def test_function_aux_bad_size_type_class_and_count_fail(self):
        valid = self.aux_object()
        bad = [self.aux_object(275), self.aux_object(280),
               put(valid, self.kernel_offset + 14, '<H', 0),
               put(valid, self.kernel_offset + 16, '<B', 3),
               put(valid, self.kernel_offset + 17, '<B', 2),
               put(valid, self.kernel_offset + 18, '<I', 1)]
        for index, data in enumerate(bad):
            with self.subTest(index=index), self.assertRaises(ValueError):
                self.k.coff_kernel_identity(self.save(f'bad-aux-{index}.obj', data))

    def test_coff_truncated_headers_tables_strings_and_aux(self):
        string_start = self.symoff + self.nsym * 18
        cuts = [0, 1, 19, 59, 139, 415, self.symoff + 17, string_start + 3, len(self.coff) - 1]
        for end in cuts:
            with self.subTest(end=end), self.assertRaises(ValueError):
                self.k.Coff(self.coff[:end])
        aux = self.aux_object()
        with self.assertRaises(ValueError):
            self.k.Coff(aux[:self.kernel_offset + 35])

    def test_coff_wrong_machine_section_type_class_and_symbol_value(self):
        mutations = [(0, '<H', 0x8664), (0, '<H', 0xA641), (16, '<H', 8),
                     (self.kernel_offset + 12, '<h', 0), (self.kernel_offset + 12, '<h', 4),
                     (self.kernel_offset + 8, '<I', 276), (self.kernel_offset + 14, '<H', 1),
                     (self.kernel_offset + 16, '<B', 3), (56, '<I', 0x40000040)]
        for index, (offset, fmt, value) in enumerate(mutations):
            with self.subTest(index=index), self.assertRaises(ValueError):
                self.k.object_start(self.save(f'bad-coff-{index}.obj', put(self.coff, offset, fmt, value)))

    def test_coff_duplicate_exact_symbol_rejected_even_if_one_undefined(self):
        data = bytearray(self.coff)
        duplicate = self.coff[self.kernel_offset:self.kernel_offset + 18]
        end = self.symoff + self.nsym * 18
        data[end:end] = duplicate
        struct.pack_into('<I', data, 12, self.nsym + 1)
        for section in (1, 0):
            data2 = put(data, end + 12, '<h', section)
            with self.subTest(section=section), self.assertRaises(ValueError):
                self.k.object_start(self.save(f'duplicate-{section}.obj', data2))

    def test_coff_longname_string_offsets_and_terminator(self):
        self.assertEqual(self.k.Coff(self.coff).symbol('ff_h264_luma_dc_dequant_idct_neon')['section'], 1)
        for offset in (0, 3, 0xFFFFFFFF):
            with self.subTest(offset=offset), self.assertRaises(ValueError):
                self.k.Coff(put(self.coff, self.kernel_offset + 4, '<I', offset))
        data = self.coff[:-1] + b'x'
        with self.assertRaises(ValueError):
            self.k.Coff(data)

    def test_coff_raw_table_collision_header_copy_and_short_code(self):
        for offset, fmt, value in ((40, '<I', self.symoff), (40, '<I', 20), (36, '<I', 275),
                                   (80, '<I', 4), (76, '<I', 276)):
            with self.subTest(offset=offset, value=value), self.assertRaises(ValueError):
                self.k.object_start(self.save('bad-coff-range.obj', put(self.coff, offset, fmt, value)))

    def test_coff_relocation_on_kernel_or_aux_reference_rejected(self):
        # 插在 raw 和 symbol tables 之间，保持其它区间完整。
        for address, target in ((0, self.kernel_index), (275, self.kernel_index), (0, 1)):
            relocation = struct.pack('<IIH', address, target, 3)
            data = self.coff[:self.symoff] + relocation + self.coff[self.symoff:]
            data = put(data, 8, '<I', self.symoff + 10)
            data = put(data, 44, '<I', self.symoff)
            data = put(data, 52, '<H', 1)
            with self.subTest(address=address, target=target), self.assertRaises(ValueError):
                self.k.object_start(self.save('relocation.obj', data))

    def test_coff_code_mutation_rejected_by_trusted_pin(self):
        with self.assertRaises(ValueError):
            self.k.object_start(self.save('mutated.obj', put(self.coff, 140, '<B', self.coff[140] ^ 1)))

    def test_real_pe_maps_fixed_rva_not_va_or_file_offset(self):
        image = self.k.Pe(self.pe)
        self.assertEqual(image.kernel(4096)['file_offset'], 512)
        for address in (512, 4095, 4096 + 1, image.image_base + 4096):
            with self.subTest(address=address), self.assertRaises(ValueError):
                image.kernel(address)

    def test_pe_truncation_machine_magic_and_bounds(self):
        for end in (0, 1, 63, self.pe_header + 23, self.optional + 111,
                    self.section + 39, len(self.pe) - 1):
            with self.subTest(end=end), self.assertRaises(ValueError):
                self.k.Pe(self.pe[:end])
        changes = [(self.pe_header + 4, '<H', 0x8664), (self.optional, '<H', 0x10B),
                   (60, '<I', 2), (self.optional + 108, '<I', 17),
                   (self.optional + 56, '<I', 4096), (self.optional + 60, '<I', 100),
                   (self.pe_header + 20, '<H', 112)]
        for offset, fmt, value in changes:
            with self.subTest(offset=offset), self.assertRaises(ValueError):
                self.k.Pe(put(self.pe, offset, fmt, value))

    def test_pe_unbacked_virtual_tail_nonexec_overlay_and_header_copies(self):
        cases = [put(self.pe, self.section + 8, '<I', 100),
                 put(self.pe, self.section + 8, '<I', 1024),
                 put(self.pe, self.section + 16, '<I', 0),
                 put(self.pe, self.section + 36, '<I', 0x40000040),
                 put(self.pe, self.section + 20, '<I', 0),
                 put(self.pe, self.section + 12, '<I', 0)]
        for index, data in enumerate(cases):
            with self.subTest(index=index), self.assertRaises(ValueError):
                self.k.Pe(data).kernel(4096)
        data = self.pe + self.k.regular(TOOLS / 'tests/fixtures/macos-h264dc-kernel.bin')
        with self.assertRaises(ValueError):
            self.k.Pe(data).kernel(8192)

    def test_pe_duplicate_raw_virtual_and_output_sections_fail(self):
        # 独立复制section；header仍容纳两个section，raw/virtual同址造成歧义。
        for field, value in ((None, None), (12, 8192), (20, 1024)):
            data = bytearray(self.pe + b'\0' * 512)
            data[self.section + 40:self.section + 80] = self.pe[self.section:self.section + 40]
            struct.pack_into('<H', data, self.pe_header + 6, 2)
            struct.pack_into('<I', data, self.optional + 56, 12288)
            if field is not None:
                struct.pack_into('<I', data, self.section + 40 + field, value)
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.k.Pe(bytes(data))

    def test_pe_base_relocation_on_kernel_and_truncated_block_fail(self):
        data = bytearray(put(self.pe, self.section + 8, '<I', 512))
        struct.pack_into('<II', data, self.optional + 112 + 5 * 8, 4372, 12)
        struct.pack_into('<IIHH', data, 788, 4096, 12, 0xA000, 0)
        with self.assertRaisesRegex(ValueError, 'relocation'):
            self.k.Pe(bytes(data)).kernel(4096)
        struct.pack_into('<H', data, 796, 0)
        self.assertEqual(self.k.Pe(bytes(data)).kernel(4096)['rva'], 4096)
        struct.pack_into('<I', data, 792, 20)
        with self.assertRaises(ValueError):
            self.k.Pe(bytes(data)).kernel(4096)

    def test_map_exact_input_contribution_and_symbol_address(self):
        obj = self.save('map-aux.obj', self.aux_object())
        record = self.k.coff_kernel_identity(obj)
        link_map = self.save('map-good.map', self.map.read_bytes().replace(str(self.obj).encode(), str(obj).encode()))
        kernel, contribution = self.k.lld_map_identity(link_map, obj, record, self.k.Pe(self.pe))
        self.assertEqual((kernel['rva'], contribution['size']), (4096, 276))
        original = link_map.read_bytes()
        changes = [original.replace(str(obj).encode(), b'/wrong/object.obj'),
                   original + original.splitlines()[-1] + b'\n',
                   original.replace(b'00000114     4', b'00000115     4'),
                   original.replace(b'00001000 00000000', b'00001004 00000000'),
                   original.replace(b'00001000 00000000', b'180001000 00000000'),
                   original.replace(b'Address', b'Publics'),
                   original.replace(b'00001000 00000114     4', b'00001000 00000114     4', 1) +
                   original.splitlines()[2] + b'\n']
        for index, data in enumerate(changes):
            with self.subTest(index=index), self.assertRaises(ValueError):
                self.k.lld_map_identity(self.save('map-bad.map', data), obj, record, self.k.Pe(self.pe))

    def test_capture_and_final_remap_do_not_reuse_file_offset(self):
        record = self.aux_capture()
        # synthetic strip-like layout move; not a claim that a real strip was invoked.
        data = bytearray(self.pe[:512] + b'\0' * 512 + self.pe[512:])
        struct.pack_into('<I', data, self.section + 20, 1024)
        final = self.save('remapped.dll', bytes(data))
        actual = self.k.verify_pe_kernel(final, record)
        self.assertEqual(actual['kernel']['rva'], 4096)
        self.assertEqual(actual['kernel']['file_offset'], 1024)

    def test_record_cannot_self_authorize_new_hash_size_symbol_rva_or_arch(self):
        record = self.aux_capture()
        changes = [('kernel', 'sha256', '0' * 64), ('kernel', 'size', 272),
                   ('kernel', 'symbol', 'arbitrary'), ('kernel', 'rva', 512),
                   ('object_kernel', 'extent_kind', 'approved'), ('object_kernel', 'size', 272)]
        for section, field, value in changes:
            data = copy.deepcopy(record)
            data[section][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.k.verify_pe_kernel(self.dll, data)
        data = copy.deepcopy(record)
        data['architecture'] = 'amd64'
        with self.assertRaises(ValueError):
            self.k.verify_pe_kernel(self.dll, data)

    def test_record_endpoint_compiler_is_never_executed_without_independent_request(self):
        record = self.aux_capture()
        marker = self.directory / 'must-not-exist'
        executable = self.save('untrusted-compiler', ('#!/bin/sh\ntouch "' + str(marker) + '"\n').encode())
        executable.chmod(0o700)
        record['endpoint_request'] = {'compiler': str(executable), 'approved': True}
        with self.assertRaisesRegex(ValueError, '独立传入'):
            self.k.verify_pe_kernel(self.dll, record)
        self.assertFalse(marker.exists())

    def test_invocation_hash_object_tool_and_output_chain_are_required(self):
        record = self.aux_capture()
        for key, field, value in (('link_invocation', 'outputs', {'module': {}, 'map': {}}),
                                  ('link_invocation', 'inputs', [self.k.artifact(self.obj)]),
                                  ('link_invocation', 'argv', ['not-actual-linker']),
                                  ('compile_invocation', 'output', {'path': 'other', 'sha256': '0' * 64}),
                                  ('strip_invocation', 'input', {'path': 'other', 'sha256': '0' * 64})):
            path = Path(record['build_inputs'][key]['path'])
            before = path.read_bytes()
            data = json.loads(before)
            data[field] = value
            path.write_text(json.dumps(data))
            with self.subTest(key=key, field=field), self.assertRaises(ValueError):
                self.k.capture_pe_kernel(record['object_kernel']['object']['path'], self.dll, record['map']['path'],
                                         build_inputs={k: v['path'] for k, v in record['build_inputs'].items()})
            path.write_bytes(before)
        path = Path(record['build_inputs']['link_invocation']['path'])
        before = path.read_bytes()
        path.write_bytes(b'{"argv":[], ' + before[1:])
        with self.assertRaisesRegex(ValueError, 'duplicate'):
            self.k.capture_pe_kernel(record['object_kernel']['object']['path'], self.dll, record['map']['path'],
                                     build_inputs={k: v['path'] for k, v in record['build_inputs'].items()})
        path.write_bytes(before)

    def test_relative_map_path_needs_explicit_same_link_cwd(self):
        record = self.aux_capture()
        obj = Path(record['object_kernel']['object']['path'])
        original = Path(record['map']['path']).read_bytes()
        link_cwd = self.directory / 'link-directory'
        link_cwd.mkdir(exist_ok=True)
        link_map = self.save('relative.map', original.replace(str(obj).encode(), ('../' + obj.name).encode()))
        with self.assertRaises(ValueError):
            self.k.lld_map_identity(link_map, obj, record['object_kernel'], self.k.Pe(self.pe))
        kernel, _ = self.k.lld_map_identity(link_map, obj, record['object_kernel'], self.k.Pe(self.pe),
                                            link_cwd=str(link_cwd))
        self.assertEqual(kernel['rva'], 4096)

    def test_final_code_change_base_change_and_stale_map_are_rejected(self):
        record = self.aux_capture()
        for data in (put(self.pe, 512, '<B', self.pe[512] ^ 1),
                     put(self.pe, self.optional + 24, '<Q', 0x190000000)):
            with self.assertRaises(ValueError):
                self.k.verify_pe_kernel(self.save('bad-final.dll', data), record)
        Path(record['map']['path']).write_bytes(self.map.read_bytes())
        with self.assertRaises(ValueError):
            self.k.verify_pe_kernel(self.dll, record)

    def test_real_mingw_driver_map_dialect_is_supported(self):
        linker = shutil.which('ld.lld')
        self.assertIsNotNone(linker, '本 fixture 要求已安装 ld.lld，不安装')
        dll, link_map = self.directory / 'mingw.dll', self.directory / 'mingw.map'
        subprocess.run([linker, '-m', 'arm64pe', '--shared', '--entry=ff_h264_luma_dc_dequant_idct_neon',
                        '--no-gc-sections', '--icf=none', '--exclude-all-symbols', '--no-insert-timestamp',
                        '--Map=' + str(link_map), '-o', str(dll), str(self.obj)],
                       check=True, capture_output=True)
        # 精确shape手动声明只用于map/parser；真实wrapper本身仍不能capture。
        record = {'symbol': 'ff_h264_luma_dc_dequant_idct_neon', 'section_index': 1,
                  'section_offset': 0, 'size': 276}
        actual, _ = self.k.lld_map_identity(link_map, self.obj, record, self.k.Pe(dll.read_bytes()))
        self.assertEqual(actual['rva'], 4096)
        data = dll.read_bytes()
        symbol_table = struct.unpack_from('<I', data, struct.unpack_from('<I', data, 60)[0] + 12)[0]
        self.assertGreater(symbol_table, 0)
        for offset, fmt, value in ((symbol_table + 8, '<I', 4), (symbol_table + 12, '<h', 0),
                                   (symbol_table + 4, '<I', 0xFFFFFFFF), (symbol_table + 17, '<B', 1)):
            with self.subTest(offset=offset), self.assertRaises(ValueError):
                self.k.Pe(put(data, offset, fmt, value)).kernel(4096)

    def test_real_unrelated_gnu_archive_contributions_are_parsed_without_kernel_guessing(self):
        for mingw in (False, True):
            dll, link_map = self.g.generate_archive(self.directory, mingw=mingw)
            record = {'symbol': 'ff_h264_luma_dc_dequant_idct_neon', 'section_index': 1,
                      'section_offset': 0, 'size': 276}
            with self.subTest(mingw=mingw):
                actual, _ = self.k.lld_map_identity(link_map, self.obj, record, self.k.Pe(dll.read_bytes()))
                self.assertEqual(actual['rva'], 4096)

    def test_real_kernel_archive_member_cannot_claim_direct_input_object_identity(self):
        for mingw in (False, True):
            dll, link_map = self.g.generate_archive(self.directory, kernel_member=True, mingw=mingw)
            record = {'symbol': 'ff_h264_luma_dc_dequant_idct_neon', 'section_index': 1,
                      'section_offset': 0, 'size': 276}
            with self.subTest(mingw=mingw), self.assertRaises(ValueError):
                self.k.lld_map_identity(link_map, self.obj, record, self.k.Pe(dll.read_bytes()),
                                         link_cwd=str(self.directory))

    def source_fixture(self):
        archive = TOOLS.parent / 'target/ffmpeg-macos/ffmpeg-8.1.2.tar.xz'
        self.assertTrue(archive.is_file(), 'source endpoint test 要求本地已存在固定归档，不下载')
        return self.g.generate_source(self.directory / 'source', archive)

    def test_real_pinned_source_endpoint_replays_compiler_and_full_code_sections(self):
        obj, endpoint = self.source_fixture()
        record = self.k.coff_kernel_identity(obj, endpoint)
        self.assertEqual(record['extent_kind'], 'fixed_recipe_with_source_endpoint')
        self.assertEqual((record['endpoint']['start'], record['endpoint']['end']), (1600, 1876))
        self.assertEqual(record['endpoint']['inputs']['libavcodec/aarch64/h264idct_neon.S']['sha256'],
                         'b0debd265560c16110a0bfa0c2d92ceb283cf1fe967776ea00fad4274eb06cc4')
        # same pinned source, but a wrapper object containing identical276 is insufficient.
        with self.assertRaises(ValueError):
            self.k.coff_kernel_identity(self.obj, endpoint)

    def test_source_endpoint_rejects_arbitrary_source_macro_config_and_response(self):
        obj, endpoint = self.source_fixture()
        tree = Path(endpoint['tree'])
        for relative in ('libavcodec/aarch64/h264idct_neon.S', 'libavutil/aarch64/asm.S'):
            path = tree / relative
            before = path.read_bytes()
            path.write_bytes(before + b'\n// mutation\n')
            with self.subTest(relative=relative), self.assertRaises(ValueError):
                self.k.coff_kernel_identity(obj, endpoint)
            path.write_bytes(before)
        config = tree / 'config.h'
        config.write_bytes(config.read_bytes() + b'#define __ARM_FEATURE_BTI_DEFAULT 1\n')
        with self.assertRaises(ValueError):
            self.k.coff_kernel_identity(obj, endpoint)
        data = copy.deepcopy(endpoint)
        data['flags'].append('@response.txt')
        with self.assertRaises(ValueError):
            self.k.coff_kernel_identity(obj, data)

    def test_source_proof_output_link_cannot_modify_shipping_source(self):
        obj, endpoint = self.source_fixture()
        proof = Path(endpoint['evidence_dir'])
        proof.mkdir(exist_ok=True)
        marker = proof / 'source-endpoint-marked.s'
        source = Path(endpoint['tree']) / 'libavcodec/aarch64/h264idct_neon.S'
        before = source.read_bytes()
        if marker.exists():
            marker.unlink()
        marker.symlink_to(source)
        with self.assertRaisesRegex(ValueError, '拒绝 link'):
            self.k.coff_kernel_identity(obj, endpoint)
        self.assertEqual(source.read_bytes(), before)
        marker.unlink()

    def test_real_source_endpoint_map_capture_final_chain(self):
        obj, endpoint = self.source_fixture()
        dll, link_map = self.directory / 'source.dll', self.directory / 'source.map'
        subprocess.run([shutil.which('lld-link'), '/dll', '/noentry', '/nodefaultlib', '/machine:arm64',
                        '/opt:noref,noicf', '/timestamp:0', '/noimplib', '/lldmap:' + str(link_map),
                        '/out:' + str(dll), str(obj)], check=True, capture_output=True)
        capture = self.k.capture_pe_kernel(obj, dll, link_map, endpoint=endpoint,
                                           build_inputs=self.inputs(Path(endpoint['tree']), obj, dll, link_map))
        with self.assertRaisesRegex(ValueError, '独立传入'):
            self.k.verify_pe_kernel(dll, capture)
        final = self.k.verify_pe_kernel(dll, capture, endpoint=endpoint)
        self.assertEqual(final['kernel']['rva'], 5696)
        self.assertEqual(final['extent_kind'], 'fixed_recipe_with_source_endpoint')
        output = Path(capture['object_kernel']['endpoint']['outputs']['source-endpoint-marked.s']['path'])
        output.write_bytes(output.read_bytes() + b'\n// tampered proof\n')
        with self.assertRaisesRegex(ValueError, '拒绝覆盖'):
            self.k.verify_pe_kernel(dll, capture, endpoint=endpoint)


if __name__ == '__main__':
    unittest.main()
