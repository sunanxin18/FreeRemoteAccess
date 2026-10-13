#!/usr/bin/env python3
"""真实无签名 HAP 的受控 ZIP 变异与签后字节/暂存回归；不签名、不执行 ELF。"""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import struct
import tempfile
import unittest
from unittest import mock
import warnings
import zipfile

TOOLS = Path(__file__).resolve().parents[1]

def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, TOOLS / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

checker = load('hap_checker', 'check-harmony-hap.py')
stager = load('hap_stager', 'stage-harmony-ffmpeg.py')


class HapMutations(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.good = Path(os.environ['FRD_HAP_TEST_HAP'])
        cls.bundle = Path(os.environ['FRD_OHOS_FFMPEG_TEST_BUNDLE'])
        cls.digest = os.environ['FRD_OHOS_FFMPEG_TEST_MANIFEST_SHA256']
        cls.readelf = Path(os.environ.get('FRD_HAP_TEST_READELF', checker.ffmpeg.DEFAULT_SDK + '/native/llvm/bin/llvm-readelf'))
        checker.check(cls.good, cls.readelf, cls.bundle, cls.digest)
        with zipfile.ZipFile(cls.good) as package:
            cls.original = [(info, package.read(info)) for info in package.infolist()]

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='frd-hap-test-')
        self.path = Path(self.temp.name) / 'mutant.hap'
        self.data = {info.filename: data for info, data in self.original}
        self.extra = []

    def tearDown(self):
        self.temp.cleanup()

    def write(self):
        with warnings.catch_warnings():
            warnings.simplefilter('ignore', UserWarning)
            with zipfile.ZipFile(self.path, 'w') as output:
                for info, _ in self.original:
                    if info.filename in self.data:
                        output.writestr(info, self.data[info.filename])
                for info, data in self.extra:
                    output.writestr(info, data)

    def rejected(self, message):
        self.write()
        with self.assertRaisesRegex(ValueError, message):
            checker.check(self.path, self.readelf, self.bundle, self.digest)

    def mutate_elf(self, name, callback):
        name = checker.NATIVE_PREFIX + name
        data = bytearray(self.data[name])
        callback(data)
        self.data[name] = data

    def test_valid_exact_six_and_license(self):
        result = checker.check(self.good, self.readelf, self.bundle, self.digest)
        self.assertEqual(len(result['nativeSha256']), 6)

    def test_duplicate_native(self):
        n = checker.NATIVE_PREFIX + checker.ffmpeg.LIBS[0]
        self.extra.append((n, self.data[n]))
        self.rejected('重复 ZIP')

    def test_duplicate_module(self):
        self.extra.append(('module.json', self.data['module.json']))
        self.rejected('重复 ZIP')

    def test_parent_traversal(self):
        self.extra.append(('../outside', b'x'))
        self.rejected('不安全 ZIP')

    def test_absolute_path(self):
        self.extra.append(('/outside', b'x'))
        self.rejected('不安全 ZIP')

    def test_backslash_path(self):
        self.extra.append(('libs\\escape', b'x'))
        self.rejected('不安全 ZIP')

    def test_drive_path(self):
        self.extra.append(('C:/outside', b'x'))
        self.rejected('不安全 ZIP')

    def test_compressed_native_rejected(self):
        n = checker.NATIVE_PREFIX + checker.ffmpeg.LIBS[0]
        data = self.data.pop(n)
        info = zipfile.ZipInfo(n)
        info.compress_type = zipfile.ZIP_DEFLATED
        self.extra.append((info, data))
        self.rejected('不压缩')

    def test_symlink(self):
        info = zipfile.ZipInfo('extra-link')
        info.create_system = 3
        info.external_attr = (stat.S_IFLNK | 0o777) << 16
        self.extra.append((info, b'module.json'))
        self.rejected('非普通 ZIP')

    def test_wrong_abi_path(self):
        self.extra.append(('libs/x86_64/libextra.so', self.data[checker.NATIVE_PREFIX + checker.ffmpeg.LIBS[0]]))
        self.rejected('未批准 native')

    def test_extra_versioned_library(self):
        self.extra.append((checker.NATIVE_PREFIX + 'libavutil.so.60.26.102', self.data[checker.NATIVE_PREFIX + checker.ffmpeg.LIBS[0]]))
        self.rejected('未批准 native')

    def test_hidden_elf_in_rawfile(self):
        self.extra.append(('resources/rawfile/hidden.bin', self.data[checker.NATIVE_PREFIX + checker.ffmpeg.LIBS[0]]))
        self.rejected('未批准 native')

    def test_missing_library(self):
        del self.data[checker.NATIVE_PREFIX + checker.ffmpeg.LIBS[0]]
        self.rejected('六个 native')

    def test_corrupt_elf(self):
        self.data[checker.NATIVE_PREFIX + checker.ffmpeg.LIBS[0]] = b'bad'
        self.rejected('非 ELF')

    def test_wrong_architecture(self):
        self.mutate_elf(checker.ffmpeg.LIBS[0], lambda b: struct.pack_into('<H', b, 18, 62))
        self.rejected('AArch64')

    def test_wrong_soname(self):
        def change(b):
            p = b.index(b'libavutil.so.60\0')
            b[p:p + len(b'libavutil.so.60')] = b'libavutil.so.61'
        self.mutate_elf(checker.ffmpeg.LIBS[0], change)
        self.rejected('SONAME')

    def test_wrong_dependency(self):
        def change(b):
            p = b.index(b'libc.so\0')
            b[p:p + 8] = b'badc.so\0'
        self.mutate_elf(checker.ffmpeg.LIBS[0], change)
        self.rejected('未批准依赖')

    def test_missing_license(self):
        del self.data[checker.RAW_PREFIX + 'FFmpeg-LGPL-2.1-or-later.txt']
        self.rejected('来源/许可')

    def test_changed_material(self):
        self.data[checker.RAW_PREFIX + 'FFmpeg-SOURCE.txt'] += b'changed'
        self.rejected('来源材料字节')

    def test_changed_manifest_cannot_self_declare_artifacts(self):
        n = checker.RAW_PREFIX + 'manifest.json'
        m = json.loads(self.data[n])
        m['target'] = 'aarch64-unknown-linux-gnu'
        self.data[n] = json.dumps(m).encode()
        self.rejected('来源材料字节')

    def test_changed_artifact_with_same_elf_contract(self):
        self.mutate_elf(checker.ffmpeg.LIBS[2], lambda b: b.extend(b'unsigned new artifact'))
        self.rejected('artifact 字节改变')

    def test_changed_sdk_cpp(self):
        self.mutate_elf('libc++_shared.so', lambda b: b.extend(b'changed'))
        self.rejected('SDK strip 产物')

    def test_unapproved_permission(self):
        m = json.loads(self.data['module.json'])
        m['module']['requestPermissions'].append({'name': 'ohos.permission.MICROPHONE'})
        self.data['module.json'] = json.dumps(m).encode()
        self.rejected('权限集合')

    def test_extraction_mode_required(self):
        m = json.loads(self.data['module.json'])
        del m['module']['extractNativeLibs']
        self.data['module.json'] = json.dumps(m).encode()
        self.rejected('extraction')

    def test_manifest_anchor_mismatch(self):
        with self.assertRaisesRegex(ValueError, '接收的 manifest'):
            checker.check(self.good, self.readelf, self.bundle, '0' * 64)

    def test_signature_metadata_container_change_allowed(self):
        self.extra.append(('META-INF/controlled-signature', b'test-only signature metadata'))
        self.write()
        checker.compare_signed(self.good, self.path)

    def test_signature_native_change_rejected(self):
        self.mutate_elf('libentry.so', lambda b: b.extend(b'changed'))
        self.write()
        with self.assertRaisesRegex(ValueError, '签名改变'):
            checker.compare_signed(self.good, self.path)

    def test_signature_resource_change_rejected(self):
        self.data[checker.RAW_PREFIX + 'FFmpeg-NOTICE.txt'] += b'changed'
        self.write()
        with self.assertRaisesRegex(ValueError, '签名改变'):
            checker.compare_signed(self.good, self.path)


class OwnedStaging(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.bundle = Path(os.environ['FRD_OHOS_FFMPEG_TEST_BUNDLE'])
        cls.digest = os.environ['FRD_OHOS_FFMPEG_TEST_MANIFEST_SHA256']
        cls.sdk = Path(checker.ffmpeg.DEFAULT_SDK)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='frd-stage-test-')
        self.entry = Path(self.temp.name) / 'entry'
        self.entry.mkdir()

    def tearDown(self):
        self.temp.cleanup()

    def test_stage_and_clean_keeps_userfiles_and_input(self):
        user = self.entry / 'user.txt'
        user.write_bytes(b'keep')
        before = checker.ffmpeg.sha(self.bundle / 'manifest.json')
        stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        for relative, source in stager.destinations().items():
            self.assertEqual((self.entry / relative).read_bytes(), (self.bundle / source).read_bytes())
        stager.clean(self.entry)
        self.assertEqual(user.read_bytes(), b'keep')
        self.assertEqual(checker.ffmpeg.sha(self.bundle / 'manifest.json'), before)
        self.assertTrue(all(not (self.entry / n).exists() for n in stager.destinations()))

    def test_unknown_existing_library_never_overwritten(self):
        relative = next(iter(stager.destinations()))
        path = self.entry / relative
        path.parent.mkdir(parents=True)
        path.write_bytes(b'user-owned')
        with self.assertRaisesRegex(ValueError, '未知暂存'):
            stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        self.assertEqual(path.read_bytes(), b'user-owned')

    def test_changed_owned_stage_never_deleted(self):
        stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        path = self.entry / next(iter(stager.destinations()))
        path.write_bytes(b'user changed')
        with self.assertRaisesRegex(ValueError, '被更改'):
            stager.clean(self.entry)
        self.assertEqual(path.read_bytes(), b'user changed')

    def test_partial_publication_failure_cleans_only_owned_stage(self):
        user = self.entry / 'user.txt'
        user.write_bytes(b'keep')
        original = stager.os.link
        calls = []

        def fail_second(source, target):
            calls.append(target)
            if len(calls) == 2:
                raise OSError('controlled publication failure')
            return original(source, target)

        with mock.patch.object(stager.os, 'link', side_effect=fail_second):
            with self.assertRaisesRegex(OSError, 'controlled publication failure'):
                stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        self.assertEqual(user.read_bytes(), b'keep')
        self.assertTrue(all(not (self.entry / n).exists() for n in stager.destinations()))
        self.assertFalse((self.entry / '.frd-ffmpeg-stage.json').exists())

    def test_stage_parent_symlink_never_followed(self):
        outside = Path(self.temp.name) / 'userdir'
        outside.mkdir()
        (self.entry / 'libs').symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, '目录拒绝 symlink'):
            stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        self.assertEqual(list(outside.iterdir()), [])


if __name__ == '__main__':
    unittest.main()
