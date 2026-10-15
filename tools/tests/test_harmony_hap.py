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


class AppleMvsContract(unittest.TestCase):
    def test_dependencies_are_exact_and_opt_in(self):
        name = 'libfrd_shell_ohos_ffi.so'
        base = checker.BASE_DEPS[name]
        for apple_mvs, rdp in [(False, False), (False, True), (True, False), (True, True)]:
            expected = base | ({'libc++_shared.so'} if apple_mvs else set()) | ({'libz.so'} if apple_mvs and rdp else set())
            for ability in (set(), {'libability_runtime.so'}):
                checker.check_native_dependencies(name, expected | ability, apple_mvs, rdp)
                for actual in [expected | ability | {'libavcodec.so'}, (expected | ability) - {'libc.so'},
                               (expected | ability) ^ {'libc++_shared.so'}, (expected | ability) ^ {'libz.so'}]:
                    with self.subTest(actual=actual, apple=apple_mvs, rdp=rdp), self.assertRaisesRegex(ValueError, '依赖合同'):
                        checker.check_native_dependencies(name, actual, apple_mvs, rdp)
        for name in ('libentry.so', 'libc++_shared.so'):
            checker.check_native_dependencies(name, checker.BASE_DEPS[name], True, True)
            with self.assertRaisesRegex(ValueError, '依赖合同'):
                checker.check_native_dependencies(name, checker.BASE_DEPS[name] | {'libz.so'}, True, True)

    def test_system_icu_required_only_for_entry_and_private_icu_rejected(self):
        self.assertIn('libicu.so', checker.BASE_DEPS['libentry.so'])
        self.assertIn('libicu.so', checker.PUBLIC)
        for library in checker.BASE_DEPS:
            expected = checker.BASE_DEPS[library]
            checker.check_native_dependencies(library, expected)
            for dependency in ('libicui18n.so', 'libicuuc.so', 'libicudata.so'):
                with self.subTest(library=library, dependency=dependency), self.assertRaisesRegex(ValueError, '依赖合同'):
                    checker.check_native_dependencies(library, expected | {dependency})
            if library != 'libentry.so':
                with self.subTest(library=library), self.assertRaisesRegex(ValueError, '依赖合同'):
                    checker.check_native_dependencies(library, expected | {'libicu.so'})
        with self.assertRaisesRegex(ValueError, '依赖合同'):
            checker.check_native_dependencies('libentry.so', checker.BASE_DEPS['libentry.so'] - {'libicu.so'})

    def test_fdk_materials_are_required_exact_bytes(self):
        data = checker.fdk_materials()
        checker.check_fdk_materials(data, True)
        checker.check_fdk_materials({}, False)
        for mutation in ({}, {**data, checker.FDK_PREFIX + 'NOTICE': b'changed'},
                         {**data, checker.FDK_PREFIX + 'unknown': b'x'}):
            with self.subTest(mutation=list(mutation)), self.assertRaisesRegex(ValueError, 'FDK'):
                checker.check_fdk_materials(mutation, True)


class HapMutations(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.good = Path(os.environ['FRD_HAP_TEST_HAP'])
        cls.bundle = Path(os.environ['FRD_OHOS_FFMPEG_TEST_BUNDLE'])
        cls.digest = os.environ['FRD_OHOS_FFMPEG_TEST_MANIFEST_SHA256']
        cls.recipe = checker.ffmpeg.recipes.identify(checker.ffmpeg.read_json(cls.bundle / 'manifest.json'))
        cls.readelf = Path(os.environ.get('FRD_HAP_TEST_READELF', checker.ffmpeg.DEFAULT_SDK + '/native/llvm/bin/llvm-readelf'))
        cls.flags = dict(apple_mvs=os.environ.get('FRD_HAP_TEST_APPLE_MVS') == '1', rdp_validation=os.environ.get('FRD_HAP_TEST_RDP') == '1')
        checker.check(cls.good, cls.readelf, cls.bundle, cls.digest, **cls.flags)
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
            checker.check(self.path, self.readelf, self.bundle, self.digest, **self.flags)

    def mutate_elf(self, name, callback):
        name = checker.NATIVE_PREFIX + name
        data = bytearray(self.data[name])
        callback(data)
        self.data[name] = data

    def test_valid_exact_six_and_license(self):
        result = checker.check(self.good, self.readelf, self.bundle, self.digest, **self.flags)
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

    def test_public_icu_on_wrong_library_is_rejected(self):
        def change(data):
            original = b'libohcrypto.so\0'
            position = data.index(original)
            data[position:position + len(original)] = b'libicu.so\0'.ljust(len(original), b'\0')
        self.mutate_elf('libfrd_shell_ohos_ffi.so', change)
        self.rejected('依赖合同')

    def test_entry_private_icu_is_rejected(self):
        def change(data):
            original = b'libace_napi.z.so\0'
            position = data.index(original)
            data[position:position + len(original)] = b'libicui18n.so\0'.ljust(len(original), b'\0')
        self.mutate_elf('libentry.so', change)
        self.rejected('未批准依赖')

    def test_entry_public_icu_cannot_be_replaced_by_duplicate_dependency(self):
        def change(data):
            original = b'libicu.so\0'
            position = data.index(original)
            data[position:position + len(original)] = b'libc.so\0'.ljust(len(original), b'\0')
        self.mutate_elf('libentry.so', change)
        self.rejected('重复 SONAME/DT_NEEDED')

    def test_bundled_icu_is_rejected(self):
        self.extra.append((checker.NATIVE_PREFIX + 'libicu.so', self.data[checker.NATIVE_PREFIX + 'libentry.so']))
        self.rejected('未批准 native')

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
            checker.check(self.good, self.readelf, self.bundle, '0' * 64, **self.flags)

    def test_signature_metadata_container_change_allowed(self):
        self.extra.append(('META-INF/controlled-signature', b'test-only signature metadata'))
        self.write()
        checker.compare_signed(self.good, self.path, self.recipe)

    def test_signature_fdk_addition_or_removal_rejected(self):
        # 基准普通包可能已有分发材料；有则移除，无则加入完整已审查材料。
        names = checker.fdk_materials()
        if set(names) <= self.data.keys():
            for name in names: del self.data[name]
        else:
            self.extra += list(names.items())
        self.write()
        with self.assertRaisesRegex(ValueError, '签名改变'):
            checker.compare_signed(self.good, self.path, self.recipe)

    def test_changed_fdk_material_is_rejected(self):
        names = checker.fdk_materials()
        changed = {**names, checker.FDK_PREFIX + 'NOTICE': b'changed'}
        for name, content in changed.items():
            if name in self.data: self.data[name] = content
            else: self.extra.append((name, content))
        self.rejected('FDK 来源/许可材料字节')

    def test_signature_native_change_rejected(self):
        self.mutate_elf('libentry.so', lambda b: b.extend(b'changed'))
        self.write()
        with self.assertRaisesRegex(ValueError, '签名改变'):
            checker.compare_signed(self.good, self.path, self.recipe)

    def test_signature_resource_change_rejected(self):
        self.data[checker.RAW_PREFIX + 'FFmpeg-NOTICE.txt'] += b'changed'
        self.write()
        with self.assertRaisesRegex(ValueError, '签名改变'):
            checker.compare_signed(self.good, self.path, self.recipe)


class OwnedStaging(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.bundle = Path(os.environ['FRD_OHOS_FFMPEG_TEST_BUNDLE'])
        cls.digest = os.environ['FRD_OHOS_FFMPEG_TEST_MANIFEST_SHA256']
        cls.recipe = checker.ffmpeg.recipes.identify(checker.ffmpeg.read_json(cls.bundle / 'manifest.json'))
        cls.sdk = Path(checker.ffmpeg.DEFAULT_SDK)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='frd-stage-test-')
        self.entry = Path(self.temp.name) / 'entry'
        self.entry.mkdir()

    def tearDown(self):
        self.temp.cleanup()

    @unittest.skipUnless(os.environ.get('FRD_HAP_OTHER_RECIPE_BUNDLE'), '需要两个实际 recipe bundle')
    def test_old_new_old_transitions_and_legacy_marker(self):
        other = Path(os.environ['FRD_HAP_OTHER_RECIPE_BUNDLE'])
        other_digest = checker.ffmpeg.sha(other / 'manifest.json')
        other_recipe = checker.ffmpeg.recipes.identify(checker.ffmpeg.read_json(other / 'manifest.json'))
        self.assertNotEqual(other_recipe, self.recipe)
        user = self.entry / 'user.txt'
        user.write_text('keep')
        for source, digest, recipe in [(self.bundle, self.digest, self.recipe),
                                       (other, other_digest, other_recipe),
                                       (self.bundle, self.digest, self.recipe)]:
            stager.stage(self.entry, source, self.sdk, digest)
            marker = self.entry / '.frd-ffmpeg-stage.json'
            value = json.loads(marker.read_text())
            self.assertEqual(value['recipe'], recipe)
            self.assertEqual(set(value['files']), set(stager.destinations(recipe)))
            for relative, name in stager.destinations(recipe).items():
                self.assertEqual((self.entry / relative).read_bytes(), (source / name).read_bytes())
            if recipe == 'upstream':
                # 真正的旧版 flat marker 必须也能完成下次过渡/清理。
                marker.write_text(json.dumps(value['files']))
            self.assertEqual(user.read_text(), 'keep')
        stager.clean(self.entry)
        self.assertEqual(user.read_text(), 'keep')
        self.assertFalse(any(p.is_file() for p in self.entry.rglob('*') if p != user))

    @unittest.skipUnless(os.environ.get('FRD_HAP_OTHER_RECIPE_BUNDLE'), '需要两个实际 recipe bundle')
    def test_changed_owned_bytes_block_recipe_transition(self):
        stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        path = self.entry / next(iter(stager.destinations(self.recipe)))
        path.write_bytes(b'changed by user')
        other = Path(os.environ['FRD_HAP_OTHER_RECIPE_BUNDLE'])
        with self.assertRaisesRegex(ValueError, '被更改'):
            stager.stage(self.entry, other, self.sdk, checker.ffmpeg.sha(other / 'manifest.json'))
        self.assertEqual(path.read_bytes(), b'changed by user')

    def test_stage_and_clean_keeps_userfiles_and_input(self):
        user = self.entry / 'user.txt'
        user.write_bytes(b'keep')
        before = checker.ffmpeg.sha(self.bundle / 'manifest.json')
        stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        for relative, source in stager.destinations(self.recipe).items():
            self.assertEqual((self.entry / relative).read_bytes(), (self.bundle / source).read_bytes())
        stager.clean(self.entry)
        self.assertEqual(user.read_bytes(), b'keep')
        self.assertEqual(checker.ffmpeg.sha(self.bundle / 'manifest.json'), before)
        self.assertTrue(all(not (self.entry / n).exists() for n in stager.destinations(self.recipe)))

    def test_unknown_existing_library_never_overwritten(self):
        relative = next(iter(stager.destinations(self.recipe)))
        path = self.entry / relative
        path.parent.mkdir(parents=True)
        path.write_bytes(b'user-owned')
        with self.assertRaisesRegex(ValueError, '未知暂存'):
            stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        self.assertEqual(path.read_bytes(), b'user-owned')

    def test_changed_owned_stage_never_deleted(self):
        stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        path = self.entry / next(iter(stager.destinations(self.recipe)))
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
            if target.name == '.frd-ffmpeg-stage.json':
                return original(source, target)
            calls.append(target)
            if len(calls) == 2:
                raise OSError('controlled publication failure')
            return original(source, target)

        with mock.patch.object(stager.os, 'link', side_effect=fail_second):
            with self.assertRaisesRegex(OSError, 'controlled publication failure'):
                stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        self.assertEqual(user.read_bytes(), b'keep')
        self.assertTrue(all(not (self.entry / n).exists() for n in stager.destinations(self.recipe)))
        self.assertFalse((self.entry / '.frd-ffmpeg-stage.json').exists())
        stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        stager.clean(self.entry)
        self.assertEqual(user.read_bytes(), b'keep')

    def assert_marker_failure_recovers(self):
        self.assertFalse((self.entry / '.frd-ffmpeg-stage.json').exists())
        self.assertTrue(all(not (self.entry / n).exists() for n in stager.destinations(self.recipe)))
        self.assertFalse(any(self.entry.glob('.frd-marker-*')))
        stager.clean(self.entry)
        stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        for relative, source in stager.destinations(self.recipe).items():
            self.assertEqual((self.entry / relative).read_bytes(), (self.bundle / source).read_bytes())
        stager.clean(self.entry)
        self.assertFalse((self.entry / '.frd-ffmpeg-stage.json').exists())

    def test_marker_partial_json_write_failure_allows_clean_and_retry(self):
        user = self.entry / 'user.txt'
        user.write_bytes(b'keep')
        before = checker.ffmpeg.sha(self.bundle / 'manifest.json')

        def partial_write(value, output):
            output.write('{')
            raise OSError('controlled marker write failure')

        with mock.patch.object(stager.json, 'dump', side_effect=partial_write):
            with self.assertRaisesRegex(OSError, 'controlled marker write failure'):
                stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        self.assert_marker_failure_recovers()
        self.assertEqual(user.read_bytes(), b'keep')
        self.assertEqual(checker.ffmpeg.sha(self.bundle / 'manifest.json'), before)

    def test_marker_close_failure_allows_clean_and_retry(self):
        original = Path.open

        class FailClose:
            def __init__(self, output):
                self.output = output

            def __enter__(self):
                return self.output.__enter__()

            def __exit__(self, *args):
                self.output.__exit__(*args)
                raise OSError('controlled marker close failure')

        def open_file(path, *args, **kwargs):
            output = original(path, *args, **kwargs)
            if path.name == 'ownership.json':
                return FailClose(output)
            return output

        with mock.patch.object(Path, 'open', open_file):
            with self.assertRaisesRegex(OSError, 'controlled marker close failure'):
                stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        self.assert_marker_failure_recovers()

    def test_marker_publish_failure_allows_clean_and_retry(self):
        original = stager.os.link

        def fail_marker(source, target):
            if target.name == '.frd-ffmpeg-stage.json':
                raise OSError('controlled marker publish failure')
            return original(source, target)

        with mock.patch.object(stager.os, 'link', side_effect=fail_marker):
            with self.assertRaisesRegex(OSError, 'controlled marker publish failure'):
                stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        self.assert_marker_failure_recovers()

    def test_marker_publish_collision_preserves_unknown_marker(self):
        original = stager.os.link
        marker = self.entry / '.frd-ffmpeg-stage.json'

        def collide(source, target):
            if target == marker:
                marker.write_text('{user-created-during-publication')
            return original(source, target)

        with mock.patch.object(stager.os, 'link', side_effect=collide):
            with self.assertRaises(FileExistsError):
                stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        self.assertEqual(marker.read_text(), '{user-created-during-publication')
        self.assertTrue(all(not (self.entry / n).exists() for n in stager.destinations(self.recipe)))
        self.assertFalse(any(self.entry.glob('.frd-marker-*')))

    def test_preexisting_invalid_marker_is_preserved(self):
        marker = self.entry / '.frd-ffmpeg-stage.json'
        marker.write_text('{user-owned-invalid')
        with self.assertRaises(json.JSONDecodeError):
            stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        with self.assertRaises(json.JSONDecodeError):
            stager.clean(self.entry)
        self.assertEqual(marker.read_text(), '{user-owned-invalid')

    def test_stage_parent_symlink_never_followed(self):
        outside = Path(self.temp.name) / 'userdir'
        outside.mkdir()
        (self.entry / 'libs').symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, '目录拒绝 symlink'):
            stager.stage(self.entry, self.bundle, self.sdk, self.digest)
        self.assertEqual(list(outside.iterdir()), [])


if __name__ == '__main__':
    unittest.main()
