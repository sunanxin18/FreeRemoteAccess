"""桌面固定配方边界；不会构建 FFmpeg 或运行 codec。"""
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def module(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'tools' / (name + '.py'))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


class RecipeBoundary(unittest.TestCase):
    def test_profile_and_recipe_reject_before_source_creation(self):
        source = module('desktop-ffmpeg-source')
        with tempfile.TemporaryDirectory() as work:
            target = Path(work) / 'fresh'
            for profile, recipe in [('macos-x86_64', 'upstream'),
                                    ('macos-aarch64', 'unknown'),
                                    ('ohos-aarch64', source.recipes.MODIFIED)]:
                with self.subTest(profile=profile, recipe=recipe):
                    with self.assertRaises(ValueError):
                        source.prepare(Path(work) / 'absent.tar', target, recipe, profile)
                    self.assertFalse(target.exists())

    def test_upstream_five_members_and_modified_exact_materials(self):
        bundle = module('macos-ffmpeg-bundle')
        self.assertEqual(len(bundle.files_for_recipe('upstream')), 5)
        self.assertEqual(bundle.files_for_recipe(bundle.source.recipes.MODIFIED),
                         bundle.files_for_recipe('upstream') | {
                             'FFmpeg-CHANGES.diff', 'FFmpeg-SOURCE-RECIPE.json',
                             'FFmpeg-PROVENANCE.json', bundle.source.asset_name(bundle.source.recipes.MODIFIED)})



import copy
import hashlib
import json
import os
import platform
import shutil
import struct
import subprocess
import zipfile


class BuilderPreflight(unittest.TestCase):
    def test_unknown_recipe_and_wrong_arch_do_not_create_buildroot(self):
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            for arch, recipe in [('arm64', 'unknown'), ('x86_64', 'frd-h264dc-aarch64-v1'),
                                 ('i686', 'upstream')]:
                target = root / (arch + '-' + recipe)
                result = subprocess.run(['bash', str(ROOT / 'tools/build-ffmpeg-macos.sh')],
                                        env=dict(os.environ, FRD_MACOS_ARCH=arch,
                                                 FRD_FFMPEG_SOURCE_RECIPE=recipe,
                                                 FRD_FFMPEG_BUILD_ROOT=str(target)), capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(target.exists())


class SourceAsset(unittest.TestCase):
    def test_full_source_asset_replay_and_tamper_gates(self):
        source = module('desktop-ffmpeg-source')
        archive = ROOT / 'target/ffmpeg-macos/ffmpeg-8.1.2.tar.xz'
        # CI builder 使用架构独立 build root。
        if not archive.exists():
            archive = Path(os.environ['FRD_FFMPEG_TEST_ARCHIVE'])
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            asset = root / source.asset_name(source.recipes.MODIFIED)
            source.write_asset(archive, asset, source.recipes.MODIFIED)
            source.verify_asset(archive, asset, source.recipes.MODIFIED)
            with zipfile.ZipFile(asset) as package:
                members = {name: package.read(name) for name in package.namelist()}
                package.extractall(root / 'asset')
            tree, metadata = source.prepare(root / 'asset/ffmpeg-8.1.2.tar.xz', root / 'fresh',
                                             source.recipes.MODIFIED, repo=root / 'asset')
            for name, values in metadata['files'].items():
                self.assertEqual(source.recipes.digest((tree / name).read_bytes()), values['post'])
            with self.assertRaisesRegex(ValueError, 'fresh'):
                source.prepare(archive, root / 'fresh', source.recipes.MODIFIED)
            mutants = []
            missing = dict(members)
            del missing[str(source.recipes.PATCH)]
            mutants.append(missing)
            extra = dict(members, extra=b'not approved')
            mutants.append(extra)
            for name in (str(source.recipes.PATCH), 'tools/build-ffmpeg-macos.sh',
                         'tools/desktop-ffmpeg-source.py', 'SOURCE-MANIFEST.json'):
                changed = dict(members)
                changed[name] += b'\nchanged'
                # 即使 ZIP manifest 同步重写 hash，也不能自授权新 bytes。
                if name != 'SOURCE-MANIFEST.json':
                    manifest = json.loads(changed['SOURCE-MANIFEST.json'])
                    manifest['files'][name] = hashlib.sha256(changed[name]).hexdigest()
                    changed['SOURCE-MANIFEST.json'] = json.dumps(manifest).encode()
                mutants.append(changed)
            for i, mutant in enumerate(mutants):
                path = root / f'mutant{i}.zip'
                with zipfile.ZipFile(path, 'w') as package:
                    for name, data in mutant.items():
                        info = zipfile.ZipInfo(name)
                        info.external_attr = 0o100644 << 16
                        package.writestr(info, data)
                with self.subTest(mutant=i), self.assertRaises(ValueError):
                    source.verify_asset(archive, path, source.recipes.MODIFIED)


@unittest.skipUnless(platform.system() == 'Darwin' and platform.machine() == 'arm64', '需要 Apple ARM64 小型 Mach-O 工具链')
class MachOAndBundle(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.work = tempfile.TemporaryDirectory(prefix='frd-macos-recipe-tests-')
        cls.root = Path(cls.work.name)
        cls.bundle = module('macos-ffmpeg-bundle')
        generator_spec = importlib.util.spec_from_file_location('fixture_generator', ROOT / 'tools/tests/generate_macos_kernel_fixture.py')
        generator = importlib.util.module_from_spec(generator_spec)
        generator_spec.loader.exec_module(generator)
        cls.lib = cls.root / 'libavcodec.62.dylib'
        generator.generate(cls.lib)
        cls.identity = cls.bundle.kernel_identity(cls.lib)
        cls.original = cls.lib.read_bytes()
        cls.object = cls.root / 'kernel.o'
        subprocess.run(['clang', '-arch', 'arm64', '-c', str(cls.lib.with_suffix('.S')), '-o', str(cls.object)], check=True)
        archive = ROOT / 'target/ffmpeg-macos/ffmpeg-8.1.2.tar.xz'
        if not archive.exists():
            archive = Path(os.environ['FRD_FFMPEG_TEST_ARCHIVE'])
        cls.archive = archive
        cls.tree, _ = cls.bundle.source.prepare(archive, cls.root / 'source', cls.bundle.source.recipes.MODIFIED)
        (cls.tree / 'config.h').write_bytes(b'#define HAVE_NEON 1\n')
        (cls.tree / 'ffbuild/config.mak').write_bytes(b'ARCH=aarch64\n')
        args = ['--prefix=/private/test/dist', '--arch=aarch64', '--target-os=darwin', '--disable-static',
                '--enable-shared', '--disable-programs', '--disable-doc', '--disable-everything',
                '--enable-decoder=hevc,h264', '--enable-parser=hevc,h264', '--disable-gpl', '--disable-nonfree',
                '--disable-version3', '--disable-autodetect', '--disable-network', '--disable-debug', '--enable-stripping',
                '--disable-avdevice', '--disable-avformat', '--disable-avfilter', '--disable-swresample', '--disable-swscale', '--disable-x86asm']
        cls.configuration = cls.root / 'configuration.json'
        cls.configuration.write_text(json.dumps({'args': args, 'deployment_target': '12.0',
                    'compiler': subprocess.check_output(['clang', '--version'], text=True).splitlines()[0]}))
        cls.record_file = cls.root / 'kernel.json'
        cls.record_file.write_text(json.dumps(cls.identity))
        cls.good = cls.root / 'bundle'
        cls.good.mkdir()
        for name in cls.bundle.LIBRARIES:
            shutil.copyfile(cls.lib, cls.good / name)
        shutil.copyfile(ROOT / 'third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1', cls.good / 'FFmpeg-LGPL-2.1-or-later.txt')
        cls.bundle.complete(cls.good, archive, cls.tree, cls.configuration, cls.record_file, cls.object)

    @classmethod
    def tearDownClass(cls):
        cls.work.cleanup()

    def check_mutant(self, data, record=None):
        path = self.root / 'mutant.dylib'
        path.write_bytes(data)
        with self.assertRaises(ValueError):
            self.bundle.kernel_identity(path, record)

    def test_actual_math_fixture_and_symbol_identity(self):
        self.assertEqual(self.identity['sha256'], '1e70d378b34fb196afd0942b9bd0c31c1715b4042c6c3e8f032dad5e76b42476')
        self.assertEqual(self.identity['size'], 276)
        self.assertEqual(self.identity['macho_symbol'], '_ff_h264_luma_dc_dequant_idct_neon')
        self.bundle.kernel_identity(self.object)
        provenance = json.loads((ROOT / 'tools/tests/fixtures/macos-h264dc-kernel-provenance.json').read_bytes())
        self.assertEqual(provenance['sha256'], self.identity['sha256'])
        self.assertEqual(hashlib.sha256((ROOT / 'tools/tests/fixtures/macos-h264dc-kernel.bin').read_bytes()).hexdigest(), self.identity['sha256'])

    def test_fat_wrongcpu_endianness_and_truncated(self):
        for offset, value in [(0, 0xcafebabe), (0, 0xcffaedfe), (4, 0x1000007), (8, 2)]:
            data = bytearray(self.original)
            struct.pack_into('<I', data, offset, value)
            self.check_mutant(data)
        for end in (0, 4, 31, 40, len(self.original) - 1):
            self.check_mutant(self.original[:end])

    def test_command_and_section_range_overflow(self):
        image = self.bundle.MachO(self.original)
        for position, fmt, value in [(20, '<I', 0xffffffff), (36, '<I', 0xffffffff),
                                     (image.sections[0]['parent']['command_offset'] + 24, '<Q', (1 << 64) - 1)]:
            data = bytearray(self.original)
            struct.pack_into(fmt, data, position, value)
            self.check_mutant(data)
        data = bytearray(self.original)
        section_command = image.sections[0]['parent']['command_offset']
        struct.pack_into('<Q', data, section_command + 72 + 40, (1 << 64) - 1)
        self.check_mutant(data)

    def test_missing_duplicate_and_wrong_symbol(self):
        image = self.bundle.MachO(self.original)
        data = bytearray(self.original)
        command = next(c[1] for c in image.commands if c[0] == 2)
        struct.pack_into('<I', data, command, 0)
        self.check_mutant(data)
        off, count, strings, strsize = image.symtab
        entries = [self.original[off + i * 16:off + (i + 1) * 16] for i in range(count)]
        found = next(entry for entry in entries if self.original[strings + struct.unpack_from('<I', entry)[0]:].startswith(self.identity['macho_symbol'].encode() + b'\0'))
        index = next(i for i, entry in enumerate(entries) if entry != found)
        data = bytearray(self.original)
        data[off + index * 16:off + (index + 1) * 16] = found
        self.check_mutant(data)
        self.check_mutant(data, self.identity)
        data = bytearray(self.original)
        at = data.find(self.identity['macho_symbol'].encode() + b'\0', strings)
        data[at] = ord('X')
        self.check_mutant(data)

    def test_nonexecuting_mapping_and_corrupted_bytes_record_cannot_authorize(self):
        image = self.bundle.MachO(self.original)
        data = bytearray(self.original)
        for segment in image.segments:
            if segment['name'] == '__TEXT':
                struct.pack_into('<I', data, segment['command_offset'] + 60, 1)
        self.check_mutant(data, self.identity)
        data = bytearray(self.original)
        data[self.identity['file_offset']] ^= 1
        self.check_mutant(data, self.identity)
        mutant_record = dict(self.identity, sha256=hashlib.sha256(data[self.identity['file_offset']:self.identity['file_offset'] + 276]).hexdigest())
        self.check_mutant(data, mutant_record)
        for key, value in [('size', 280), ('size', 276.0), ('address', (1 << 64) - 1),
                           ('file_offset', 0), ('symbol', 'caller_authorized')]:
            self.check_mutant(self.original, dict(self.identity, **{key: value}))

    def test_final_stripped_library_requires_prior_actual_symbol_record(self):
        path = self.root / 'stripped.dylib'
        path.write_bytes(self.original)
        subprocess.run(['strip', '-x', str(path)], check=True, capture_output=True)
        subprocess.run(['codesign', '--force', '--sign', '-', str(path)], check=True, capture_output=True)
        with self.assertRaisesRegex(ValueError, 'symbol'):
            self.bundle.kernel_identity(path)
        self.assertEqual(self.bundle.kernel_identity(path, self.identity), self.identity)

    def test_dylib_section_and_symbol_shift_without_file_offset_is_rejected(self):
        # 只改变声明地址，保留原数学bytes与文件offset；实际segment映射已经偏移。
        image = self.bundle.MachO(self.original)
        section, offset = image.executable(self.identity['address'], self.identity['size'])
        parent_sections = [item for item in image.sections if item['parent'] is section['parent']]
        section_position = section['parent']['command_offset'] + 72 + parent_sections.index(section) * 80
        data = bytearray(self.original)
        struct.pack_into('<Q', data, section_position + 32, section['address'] + 4)
        symoff, count, stroff, _ = image.symtab
        matches = []
        for i in range(count):
            position = symoff + i * 16
            name_index = struct.unpack_from('<I', data, position)[0]
            if data[stroff + name_index:].startswith(self.identity['macho_symbol'].encode() + b'\0'):
                matches.append(position)
        self.assertEqual(len(matches), 1)
        shifted_address = self.identity['address'] + 4
        struct.pack_into('<Q', data, matches[0] + 8, shifted_address)
        shifted_record = dict(self.identity, address=shifted_address)
        actual_offset = section['parent']['offset'] + shifted_address - section['parent']['address']
        self.assertEqual(offset, self.identity['file_offset'])
        self.assertEqual(actual_offset, offset + 4)
        self.assertNotEqual(hashlib.sha256(data[actual_offset:actual_offset + 276]).hexdigest(),
                            self.identity['sha256'])
        path = self.root / 'false-segment-mapping.dylib'
        path.write_bytes(data)
        for signed in (False, True):
            if signed:
                subprocess.run(['codesign', '--force', '--sign', '-', str(path)], check=True, capture_output=True)
                subprocess.run(['codesign', '--verify', '--strict', str(path)], check=True, capture_output=True)
            for record in (None, shifted_record):
                with self.subTest(signed=signed, record=record is not None), self.assertRaises(ValueError):
                    self.bundle.kernel_identity(path, record)

    def test_signature_normalization_cannot_hide_other_bytes_or_invalid_vm_sizes(self):
        path = self.root / 'signed-mutant.dylib'
        image = self.bundle.MachO(self.original)
        link = next(segment for segment in image.segments if segment['name'] == '__LINKEDIT')
        for value in (link['size'] + 16384, (1 << 64) - 1):
            data = bytearray(self.original)
            struct.pack_into('<Q', data, link['command_offset'] + 32, value)
            path.write_bytes(data)
            with self.assertRaises(ValueError):
                self.bundle.resignable_digest(path)
        data = bytearray(self.original)
        data[self.identity['file_offset'] + 275] ^= 1
        path.write_bytes(data)
        self.assertNotEqual(self.bundle.resignable_digest(path), self.bundle.resignable_digest(self.lib))

    def test_bundle_source_materials_set_and_hash_tampering(self):
        self.assertEqual(self.bundle.verify_bundle(self.good), self.bundle.source.recipes.MODIFIED)
        for name in self.bundle.files_for_recipe(self.bundle.source.recipes.MODIFIED) - self.bundle.LIBRARIES:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as work:
                path = Path(work) / 'bundle'
                shutil.copytree(self.good, path)
                (path / name).unlink()
                with self.assertRaises(ValueError):
                    self.bundle.verify_bundle(path)
                shutil.copyfile(self.good / name, path / name)
                with (path / name).open('ab') as output:
                    output.write(b'\nchanged')
                with self.assertRaises((ValueError, zipfile.BadZipFile)):
                    self.bundle.verify_bundle(path)
        with tempfile.TemporaryDirectory() as work:
            path = Path(work) / 'bundle'
            shutil.copytree(self.good, path)
            (path / 'extra.dylib').write_bytes(self.original)
            with self.assertRaisesRegex(ValueError, '文件集合'):
                self.bundle.verify_bundle(path)

    def test_manifest_cannot_authorize_wrong_profile_inputs_outputs_or_configuration(self):
        for field, value in [('profile', 'macos-x86_64'), ('schema', 2), ('version', '8.2'), ('recipe', 'unknown'),
                             ('source', {'modified': True}), ('outputs', {'extra.dylib': {'sha256': '0'*64}}),
                             ('source_asset', {'name': 'arbitrary.zip', 'sha256': '0'*64})]:
            with tempfile.TemporaryDirectory() as work:
                path = Path(work) / 'bundle'
                shutil.copytree(self.good, path)
                record = json.loads((path / self.bundle.PROVENANCE).read_bytes())
                record[field] = value
                (path / self.bundle.PROVENANCE).write_text(json.dumps(record))
                with self.subTest(field=field), self.assertRaises(ValueError):
                    self.bundle.verify_bundle(path)
        record = json.loads((self.good / self.bundle.PROVENANCE).read_bytes())
        with tempfile.TemporaryDirectory() as work:
            path = Path(work) / 'bundle'
            shutil.copytree(self.good, path)
            changed = copy.deepcopy(record)
            changed['source']['modified'] = 1
            (path / self.bundle.PROVENANCE).write_text(json.dumps(changed))
            with self.assertRaises(ValueError):
                self.bundle.verify_bundle(path)
        bad = copy.deepcopy(record['configuration'])
        bad['args'].append('--enable-gpl')
        with self.assertRaises(ValueError):
            self.bundle.validate_configuration(bad)

    def test_app_layout_resign_code_identity_and_non_signature_change(self):
        with tempfile.TemporaryDirectory() as work:
            app = Path(work) / 'fixture.app'
            libraries = app / 'Contents/MacOS/codecs/ffmpeg-8-1-2/macos-aarch64'
            materials = app / 'Contents/Resources/codecs/ffmpeg-8-1-2'
            libraries.mkdir(parents=True)
            materials.mkdir(parents=True)
            for path in self.good.iterdir():
                shutil.copyfile(path, (libraries if path.name in self.bundle.LIBRARIES else materials) / path.name)
            self.bundle.verify_app(app)
            before = {name: self.bundle.resignable_digest(libraries / name) for name in self.bundle.LIBRARIES}
            for name in self.bundle.LIBRARIES:
                subprocess.run(['codesign', '--force', '--sign', '-', '--identifier', 'fixture.resigned.' + name,
                                str(libraries / name)], check=True, capture_output=True)
                self.assertEqual(self.bundle.resignable_digest(libraries / name), before[name])
            self.bundle.verify_app(app)
            target = libraries / 'libavcodec.62.dylib'
            data = bytearray(target.read_bytes())
            data[self.identity['file_offset']] ^= 1
            target.write_bytes(data)
            subprocess.run(['codesign', '--force', '--sign', '-', str(target)], check=True, capture_output=True)
            with self.assertRaises(ValueError):
                self.bundle.verify_app(app)
            shutil.copyfile(self.good / target.name, target)
            misplaced = libraries / 'FFmpeg-SOURCE-RECIPE.json'
            shutil.move(materials / misplaced.name, misplaced)
            with self.assertRaisesRegex(ValueError, '文件集合'):
                self.bundle.verify_app(app)

    def test_upstream_original_five_file_contract(self):
        with tempfile.TemporaryDirectory() as work:
            path = Path(work)
            for name in self.bundle.LIBRARIES:
                (path / name).write_bytes(b'upstream library fixture; architecture gate belongs to existing shell verifier')
            shutil.copyfile(self.good / 'FFmpeg-LGPL-2.1-or-later.txt', path / 'FFmpeg-LGPL-2.1-or-later.txt')
            (path / 'FFmpeg-NOTICE.txt').write_bytes(self.bundle.notice('upstream'))
            self.assertEqual(self.bundle.verify_bundle(path), 'upstream')
            with self.assertRaises(ValueError):
                self.bundle.verify_bundle(path, self.bundle.source.recipes.MODIFIED)
            (path / 'FFmpeg-CHANGES.diff').write_bytes(b'unapproved')
            with self.assertRaisesRegex(ValueError, '文件集合'):
                self.bundle.verify_bundle(path)

    def test_stage_script_modified_materials_with_mock_product_build(self):
        self.exercise_stage_script()

    def test_stage_script_external_cwd_relative_and_absolute_roots(self):
        for relative in (False, True):
            with self.subTest(relative=relative):
                self.exercise_stage_script(relative_root=relative, external_cwd=True)

    def exercise_stage_script(self, relative_root=False, external_cwd=False):
        # 执行真实 stager 的材料路径/重签门；cargo、图标和最终应用 verifier 隔离。
        # 这不是产品 .app 构建，也不执行 codec/GUI。
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            repo = root / 'repo'
            for name in ('tools/stage-macos-package.sh', 'tools/build-ffmpeg-macos.sh',
                         'tools/desktop-ffmpeg-source.py', 'tools/macos-ffmpeg-bundle.py',
                         'tools/ffmpeg-source-recipe.py', 'packaging/macos/Info.plist',
                         'packaging/windows/licenses/FreeRDP-APACHE-2.0.txt',
                         'packaging/windows/licenses/FreeRDP-NOTICE.txt',
                         'third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1',
                         str(self.bundle.source.recipes.PIN), str(self.bundle.source.recipes.PATCH)):
                destination = repo / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / name, destination)
            (repo / 'assets/app-icon').mkdir(parents=True)
            (repo / 'assets/app-icon/README.md').write_bytes(b'isolated icon fixture')
            (repo / 'tools/verify-macos-package.sh').write_text('#!/bin/bash\nexit 0\n')
            (repo / 'tools/verify-macos-package.sh').chmod(0o755)
            target = root / 'target'
            (target / 'debug').mkdir(parents=True)
            (target / 'debug/freeremotedesk-macos').write_bytes(b'mock product binary; not executed')
            codec = repo / 'target/codec'
            shutil.copytree(self.good, codec / 'bundle')
            binaries = root / 'bin'
            binaries.mkdir()
            stubs = {
                'uname': '#!/bin/bash\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n',
                'cargo': '#!/bin/bash\nexit 0\n',
                'swift': '#!/bin/bash\nexit 0\n',
                'iconutil': '#!/bin/bash\ntouch "${@: -1}"\n',
                'codesign': '#!/bin/bash\ncase "${@: -1}" in *.dylib) exec /usr/bin/codesign "$@";; *) exit 0;; esac\n',
            }
            for name, content in stubs.items():
                path = binaries / name
                path.write_text(content)
                path.chmod(0o755)
            env = dict(os.environ, PATH=str(binaries) + ':' + os.environ['PATH'], CARGO_TARGET_DIR=str(target),
                       FRD_FFMPEG_BUILD_ROOT='target/codec' if relative_root else str(codec), FRD_MACOS_SIGNING_IDENTITY='-',
                       FRD_FFMPEG_SOURCE_RECIPE=self.bundle.source.recipes.MODIFIED)
            caller = root / 'external-caller'
            caller.mkdir()
            cwd = caller if external_cwd else ROOT
            result = subprocess.run(['bash', str(repo / 'tools/stage-macos-package.sh')], env=env,
                                    cwd=cwd, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            app = target / 'macos/debug/FreeRemoteDesk.app'
            self.assertEqual(self.bundle.verify_app(app), self.bundle.source.recipes.MODIFIED)
            resources = app / 'Contents/Resources/codecs/ffmpeg-8-1-2'
            self.assertEqual({p.name for p in resources.iterdir()},
                             self.bundle.files_for_recipe(self.bundle.source.recipes.MODIFIED) - self.bundle.LIBRARIES)
            (codec / 'bundle/extra.dylib').write_bytes(self.original)
            result = subprocess.run(['bash', str(repo / 'tools/stage-macos-package.sh')], env=env,
                                    cwd=cwd, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('文件集合', result.stderr)


if __name__ == '__main__':
    unittest.main()
