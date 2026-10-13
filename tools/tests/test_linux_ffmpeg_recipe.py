"""Linux recipe 静态合同；小型 ELF 与模拟外部命令不是目标运行证明。"""
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
    def test_three_upstream_profiles_and_arm64_only_modified(self):
        source = module('desktop-ffmpeg-source')
        for profile in ('linux-x86', 'linux-x86_64', 'linux-aarch64'):
            self.assertFalse(source.metadata('upstream', profile)['modified'])
        with tempfile.TemporaryDirectory() as work:
            destination = Path(work) / 'absent'
            for profile, recipe in [('linux-x86', source.recipes.MODIFIED),
                                    ('linux-x86_64', source.recipes.MODIFIED),
                                    ('linux-aarch64', 'unknown')]:
                with self.subTest(profile=profile), self.assertRaises(ValueError):
                    source.prepare(Path(work) / 'missing', destination, recipe, profile)
                self.assertFalse(destination.exists())

    def test_upstream_and_modified_exact_members(self):
        bundle = module('linux-ffmpeg-bundle')
        for profile in ('linux-x86', 'linux-x86_64', 'linux-aarch64'):
            self.assertEqual(len(bundle.files_for_recipe('upstream', profile)), 5)
        self.assertEqual(len(bundle.files_for_recipe(bundle.source.recipes.MODIFIED, 'linux-aarch64')), 9)



import copy
import hashlib
import json
import os
import shutil
import struct
import subprocess
import zipfile


def archive_path():
    value = Path(os.environ.get('FRD_FFMPEG_TEST_ARCHIVE', ROOT / 'target/ffmpeg-macos/ffmpeg-8.1.2.tar.xz'))
    if not value.is_file():
        raise unittest.SkipTest('需要已有固定archive；本测试不下载')
    return value


class BuilderPreflight(unittest.TestCase):
    def test_wrong_recipe_arch_and_cross_execution_reject_without_side_effects(self):
        # 仅模拟宿主查询以到达preflight，不运行构建，更不当作Linux宿主执行。
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            (root / 'uname').write_text('#!/bin/sh\n[ "$1" = -s ] && echo Linux || echo x86_64\n')
            (root / 'uname').chmod(0o755)
            for arch, recipe, tests in [('aarch64', 'unknown', '0'), ('i686', 'frd-h264dc-aarch64-v1', '0'),
                                         ('x86_64', 'frd-h264dc-aarch64-v1', '0'),
                                         ('aarch64', 'frd-h264dc-aarch64-v1', '1')]:
                destination = root / (arch + recipe + tests)
                result = subprocess.run(['bash', str(ROOT / 'tools/build-ffmpeg-linux.sh')],
                            env=dict(os.environ, PATH=str(root) + ':' + os.environ['PATH'],
                                     FRD_FFMPEG_ARCH=arch, FRD_FFMPEG_SOURCE_RECIPE=recipe,
                                     FRD_FFMPEG_BUILD_ROOT=str(destination), FRD_FFMPEG_RUN_TARGET_TESTS=tests,
                                     FRD_FFMPEG_CC='cross-gcc', FRD_FFMPEG_CROSS_PREFIX='cross-',
                                     FRD_CARGO_TARGET='aarch64-unknown-linux-gnu'), capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(destination.exists())
                if tests == '1':
                    self.assertIn('不能直接执行'.encode(), result.stderr)
                else:
                    self.assertIn('recipe'.encode(), result.stderr)


class SourceAsset(unittest.TestCase):
    def test_fresh_replay_complete_asset_and_tampered_members(self):
        source = module('desktop-ffmpeg-source')
        recipe, profile = source.recipes.MODIFIED, 'linux-aarch64'
        archive = archive_path()
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            asset = root / source.asset_name(recipe, profile)
            source.write_asset(archive, asset, recipe, profile)
            source.verify_embedded_asset(asset, recipe, profile)
            with zipfile.ZipFile(asset) as package:
                members = {n: package.read(n) for n in package.namelist()}
                package.extractall(root / 'asset')
            tree, metadata = source.prepare(root / 'asset/ffmpeg-8.1.2.tar.xz', root / 'fresh', recipe, profile, root / 'asset')
            for name, hashes in metadata['files'].items():
                self.assertEqual(source.recipes.digest((tree / name).read_bytes()), hashes['post'])
            for name, expected in metadata['unchanged_oracle'].items():
                self.assertEqual(source.recipes.digest((tree / name).read_bytes()), expected)
            with self.assertRaisesRegex(ValueError, 'fresh'):
                source.prepare(archive, root / 'fresh', recipe, profile)
            missing = dict(members)
            del missing[str(source.recipes.PATCH)]
            mutants = [missing, dict(members, extra=b'unapproved')]
            for name in members:
                mutant = dict(members)
                mutant[name] += b'changed'
                if name != 'SOURCE-MANIFEST.json':
                    manifest = json.loads(mutant['SOURCE-MANIFEST.json'])
                    manifest['files'][name] = hashlib.sha256(mutant[name]).hexdigest()
                    mutant['SOURCE-MANIFEST.json'] = json.dumps(manifest).encode()
                mutants.append(mutant)
            for index, mutant in enumerate(mutants):
                output = root / f'tampered{index}.zip'
                with zipfile.ZipFile(output, 'w') as package:
                    for name, data in mutant.items():
                        item = zipfile.ZipInfo(name)
                        item.external_attr = 0o100644 << 16
                        package.writestr(item, data)
                with self.subTest(index=index), self.assertRaises(ValueError):
                    source.verify_asset(archive, output, recipe, profile)
            for name in (source.recipes.PIN, source.recipes.PATCH):
                path = root / 'asset' / name
                original = path.read_bytes()
                path.write_bytes(original + b'changed')
                with self.subTest(name=name), self.assertRaises(ValueError):
                    source.prepare(archive, root / 'rejected', recipe, profile, root / 'asset')
                self.assertFalse((root / 'rejected').exists())
                path.write_bytes(original)


class ELFAndMaterials(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not shutil.which('clang') or not shutil.which('ld.lld'):
            raise unittest.SkipTest('需要已安装clang/ld.lld小型ELF工具链；不安装')
        cls.work = tempfile.TemporaryDirectory(prefix='frd-linux-recipe-tests-')
        cls.root = Path(cls.work.name).resolve()
        cls.bundle = module('linux-ffmpeg-bundle')
        spec = importlib.util.spec_from_file_location('linux_fixture', ROOT / 'tools/tests/generate_linux_kernel_fixture.py')
        generator = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(generator)
        cls.library = cls.root / 'unstripped.so'
        generator.generate(cls.library)
        cls.identity = cls.bundle.kernel_identity(cls.library)
        cls.kernel_file = cls.root / 'kernel.json'
        cls.kernel_file.write_text(json.dumps(cls.identity))
        cls.original = cls.library.read_bytes()
        cls.final = cls.root / 'stripped.so'
        generator.generate(cls.final, stripped=True)
        cls.bundle.kernel_identity(cls.final, cls.identity)
        cls.archive = archive_path()
        cls.tree, _ = cls.bundle.source.prepare(cls.archive, cls.root / 'source', cls.bundle.source.recipes.MODIFIED, 'linux-aarch64')
        (cls.tree / 'config.h').write_bytes(b'#define HAVE_NEON 1\n')
        (cls.tree / 'ffbuild/config.mak').write_bytes(b'ARCH=aarch64\n')
        cls.configuration = cls.root / 'configuration.json'
        cls.configuration.write_text(json.dumps({'args': cls.bundle.FIXED_ARGS, 'compiler': 'fixture clang'}))
        cls.good = cls.root / 'bundle'
        cls.good.mkdir()
        for name in cls.bundle.LIBRARIES:
            shutil.copyfile(cls.final, cls.good / name)
        shutil.copyfile(ROOT / 'third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1', cls.good / 'FFmpeg-LGPL-2.1-or-later.txt')
        cls.bundle.complete(cls.good, cls.archive, cls.tree, cls.configuration, cls.library, cls.kernel_file)

    @classmethod
    def tearDownClass(cls):
        cls.work.cleanup()

    def reject(self, data, record=None):
        path = self.root / 'mutant.so'
        path.write_bytes(data)
        with self.assertRaises(ValueError):
            self.bundle.kernel_identity(path, record)

    def test_real_symbol_fixed_bytes_and_final_stripped_mapping(self):
        self.assertEqual(self.identity['size'], 276)
        self.assertEqual(self.identity['sha256'], '1e70d378b34fb196afd0942b9bd0c31c1715b4042c6c3e8f032dad5e76b42476')
        with self.assertRaisesRegex(ValueError, 'symbol'):
            self.bundle.kernel_identity(self.final)
        self.assertEqual(self.bundle.kernel_identity(self.final, self.identity), self.identity)

    def test_container_ranges_and_executable_load_rejection(self):
        for position, fmt, value in [(4, '<B', 1), (5, '<B', 2), (18, '<H', 62), (16, '<H', 1),
                                     (32, '<Q', (1 << 64) - 1), (40, '<Q', (1 << 64) - 1),
                                     (54, '<H', 64), (58, '<H', 56)]:
            data = bytearray(self.original)
            struct.pack_into(fmt, data, position, value)
            self.reject(data)
        for end in (0, 4, 63, len(self.original) - 1):
            self.reject(self.original[:end])
        image = self.bundle.ELF(self.original)
        for load in image.loads:
            if load['flags'] & 1:
                for offset, fmt, value in [(4, '<I', 4), (32, '<Q', 275), (40, '<Q', 0)]:
                    data = bytearray(self.original)
                    struct.pack_into(fmt, data, image.phoff + load['index'] * 56 + offset, value)
                    self.reject(data, self.identity)
        data = bytearray(self.original)
        data[self.identity['file_offset']] ^= 1
        self.reject(data, self.identity)
        for key, value in [('size', 280), ('size', 276.0), ('address', (1 << 64) - 1), ('address', -1),
                           ('file_offset', 0), ('sha256', '0' * 64), ('symbol', 'caller')]:
            self.reject(self.original, dict(self.identity, **{key: value}))

    def test_symbol_definition_size_duplicates_and_string_ranges(self):
        image = self.bundle.ELF(self.original)
        table = next(s for s in image.sections if s['kind'] == 2)
        strings = image.sections[table['link']]
        position = next(i for i in range(table['offset'], table['offset'] + table['size'], 24)
                        if self.original[strings['offset'] + struct.unpack_from('<I', self.original, i)[0]:].startswith(self.identity['symbol'].encode() + b'\0'))
        for offset, fmt, value in [(0, '<I', strings['size']), (4, '<B', 0), (6, '<H', 0), (16, '<Q', 280)]:
            data = bytearray(self.original)
            struct.pack_into(fmt, data, position + offset, value)
            self.reject(data)
            self.reject(data, self.identity)
        data = bytearray(self.original)
        data[table['offset']:table['offset'] + 24] = data[position:position + 24]
        self.reject(data)
        self.reject(data, self.identity)
        data = bytearray(self.original)
        data[position:position + 4] = b'\0' * 4
        self.reject(data)

    def test_section_symbol_shift_cannot_fake_load_mapping(self):
        image = self.bundle.ELF(self.original)
        section, offset = image.executable(self.identity['address'], 276)
        table = next(s for s in image.sections if s['kind'] == 2)
        strings = image.sections[table['link']]
        data = bytearray(self.original)
        struct.pack_into('<Q', data, image.shoff + section['index'] * 64 + 16, section['address'] + 4)
        for i in range(table['offset'], table['offset'] + table['size'], 24):
            n = struct.unpack_from('<I', data, i)[0]
            if data[strings['offset'] + n:].startswith(self.identity['symbol'].encode() + b'\0'):
                struct.pack_into('<Q', data, i + 8, self.identity['address'] + 4)
        self.assertNotEqual(hashlib.sha256(data[offset + 4:offset + 280]).hexdigest(), self.identity['sha256'])
        self.reject(data)
        self.reject(data, dict(self.identity, address=self.identity['address'] + 4))

    def test_materials_missing_extra_tampered_and_copied_bundle(self):
        self.assertEqual(self.bundle.verify_bundle(self.good), self.bundle.source.recipes.MODIFIED)
        for name in self.bundle.files_for_recipe(self.bundle.source.recipes.MODIFIED):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as work:
                path = Path(work).resolve() / 'copied'
                shutil.copytree(self.good, path)
                self.bundle.verify_bundle(path)
                (path / name).unlink()
                with self.assertRaises(ValueError):
                    self.bundle.verify_bundle(path)
                shutil.copyfile(self.good / name, path / name)
                with (path / name).open('ab') as output:
                    output.write(b'changed')
                with self.assertRaises((ValueError, zipfile.BadZipFile)):
                    self.bundle.verify_bundle(path)
        with tempfile.TemporaryDirectory() as work:
            path = Path(work).resolve() / 'copied'
            shutil.copytree(self.good, path)
            (path / 'extra').mkdir()
            with self.assertRaises(ValueError):
                self.bundle.verify_bundle(path)
        with self.assertRaises(ValueError):
            self.bundle.verify_bundle(self.good, 'linux-x86_64')
        with self.assertRaises(ValueError):
            self.bundle.verify_bundle(self.good, expected_recipe='upstream')

    def test_provenance_configuration_output_and_code_cannot_self_authorize(self):
        original = json.loads((self.good / self.bundle.PROVENANCE).read_bytes())
        for field, value in [('schema', 2), ('version', '8.2'), ('profile', 'linux-x86'), ('recipe', 'unknown'),
                             ('source', {'modified': True}), ('inputs', {}), ('outputs', {}), ('kernel', {}),
                             ('source_asset', {'name': 'arbitrary.zip', 'sha256': '0' * 64})]:
            with self.subTest(field=field), tempfile.TemporaryDirectory() as work:
                path = Path(work).resolve() / 'bundle'
                shutil.copytree(self.good, path)
                changed = copy.deepcopy(original)
                changed[field] = value
                (path / self.bundle.PROVENANCE).write_text(json.dumps(changed))
                with self.assertRaises(ValueError):
                    self.bundle.verify_bundle(path)
        bad = copy.deepcopy(original['configuration'])
        bad['args'].append('--enable-gpl')
        with self.assertRaises(ValueError):
            self.bundle.validate_configuration(bad)
        with tempfile.TemporaryDirectory() as work:
            path = Path(work).resolve() / 'bundle'
            shutil.copytree(self.good, path)
            data = bytearray((path / 'libavcodec.so.62').read_bytes())
            data[original['kernel']['file_offset']] ^= 1
            (path / 'libavcodec.so.62').write_bytes(data)
            changed = copy.deepcopy(original)
            changed['outputs']['libavcodec.so.62'] = hashlib.sha256(data).hexdigest()
            (path / self.bundle.PROVENANCE).write_text(json.dumps(changed))
            with self.assertRaisesRegex(ValueError, '276'):
                self.bundle.verify_bundle(path)

    def test_stager_external_cwd_relative_and_absolute_roots(self):
        # 真实stager与来源门；只模拟Linux工具输出/cargo/final product verifier。
        # 包完整tree的独立Python门另由packaging/linux回归覆盖。
        with tempfile.TemporaryDirectory() as work:
            work = Path(work).resolve()
            repo = work / 'repo'
            tools = repo / 'tools'
            tools.mkdir(parents=True)
            for name in (*self.bundle.HELPERS, 'tools/stage-linux-package.sh'):
                shutil.copyfile(ROOT / name, repo / name)
            for name in (self.bundle.source.recipes.PIN, self.bundle.source.recipes.PATCH,
                         self.bundle.source.recipes.BASE / 'LICENSE.LGPLv2.1',
                         Path('packaging/linux/README.md'), Path('packaging/linux/freeremotedesk.desktop'),
                         Path('assets/app-icon/README.md'), Path('assets/ui-icons/LICENSE-APACHE-2.0.txt'),
                         Path('assets/fonts/noto-sans-sc/OFL.txt'),
                         Path('assets/fonts/noto-sans-sc/NotoSansSC-VariableFont_wght.ttf'),
                         Path('packaging/windows/licenses/FreeRDP-APACHE-2.0.txt'),
                         Path('packaging/windows/licenses/FreeRDP-NOTICE.txt')):
                target = repo / name
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / name, target)
            for size in (16, 32, 48, 64, 128, 256, 512):
                name = f'assets/app-icon/linux/hicolor/{size}x{size}/apps/freeremotedesk.png'
                target = repo / name
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / name, target)
            for path in tools.iterdir():
                path.chmod(0o755)
            fake = work / 'fake'
            fake.mkdir()
            def command(name, text):
                path = fake / name
                path.write_text(text)
                path.chmod(0o755)
            python = shutil.which('python3')
            command('uname', '#!/bin/sh\n[ "$1" = -s ] && echo Linux || echo aarch64\n')
            command('cargo', '#!/bin/sh\nmkdir -p "$CARGO_TARGET_DIR/aarch64-unknown-linux-gnu/release"\nprintf fixture > "$CARGO_TARGET_DIR/aarch64-unknown-linux-gnu/release/freeremotedesk-linux"\nchmod 755 "$CARGO_TARGET_DIR/aarch64-unknown-linux-gnu/release/freeremotedesk-linux"\n')
            command('file', '#!/bin/sh\necho "ELF 64-bit LSB shared object, ARM aarch64"\n')
            command('nm', '#!/bin/sh\necho "0 T frd_ffmpeg_get_api_v1"\n')
            command('readelf', '#!/bin/sh\nif [ "$1" = -h ]; then\nprintf "  Class: ELF64\\n  Data: 2\\047s complement, little endian\\n  Machine: AArch64\\n"\nelse\ncase "$2" in\n*libavcodec*) echo "(SONAME) [libavcodec.so.62]"; echo "(NEEDED) [libavutil.so.60]";;\n*libavutil*) echo "(SONAME) [libavutil.so.60]";;\n*) echo "(NEEDED) [libavcodec.so.62]"; echo "(NEEDED) [libavutil.so.60]";;\nesac\nfi\n')
            command('stat', f'#!{python}\nimport os,sys,stat\ns=os.stat(sys.argv[-1]);print(s.st_uid if sys.argv[2]=="%u" else format(stat.S_IMODE(s.st_mode),"o"))\n')
            command('find', f'#!{python}\nimport sys\nfrom pathlib import Path\nr=Path(sys.argv[1]); entries=list(r.iterdir())\nif "-type" in sys.argv: entries=[p for p in entries if p.is_file()]\nfor p in entries: print(p.name if "-printf" in sys.argv else str(p))\n')
            # 不执行客户端；final verifier仅对复制后的codec材料再跑真实adapter。
            verifier = tools / 'verify-linux-package.sh'
            verifier.write_text('#!/bin/sh\nexec python3 "' + str(tools / 'linux-ffmpeg-bundle.py') + '" verify --path "$1/codecs/ffmpeg-8.1.2/$2" --profile "$2"\n')
            verifier.chmod(0o755)
            for relative in (True, False):
                root = repo / ('relative-build' if relative else 'absolute-build')
                codec = root / 'bundle/linux-aarch64'
                codec.parent.mkdir(parents=True)
                shutil.copytree(self.good, codec)
                env = dict(os.environ, PATH=str(fake) + ':' + os.environ['PATH'],
                           CARGO_TARGET_DIR=str(work / ('target-rel' if relative else 'target-abs')),
                           FRD_LINUX_ARCH='aarch64', FRD_FFMPEG_SOURCE_RECIPE=self.bundle.source.recipes.MODIFIED,
                           FRD_FFMPEG_BUILD_ROOT=root.name if relative else str(root))
                result = subprocess.run(['bash', str(tools / 'stage-linux-package.sh'), 'release'], cwd=work,
                                        env=env, capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                package = Path(env['CARGO_TARGET_DIR']) / 'linux/release/linux-aarch64/FreeRemoteDesk'
                actual = package / 'codecs/ffmpeg-8.1.2/linux-aarch64'
                self.bundle.verify_bundle(actual)
                self.assertEqual({p.name for p in actual.iterdir()}, self.bundle.files_for_recipe(self.bundle.source.recipes.MODIFIED))
                original = (package / 'freeremotedesk-linux').read_bytes()
                (codec / 'extra').write_bytes(b'not authorized')
                result = subprocess.run(['bash', str(tools / 'stage-linux-package.sh'), 'release'], cwd=work,
                                        env=env, capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual((package / 'freeremotedesk-linux').read_bytes(), original)

    def test_complete_rejects_postimage_or_oracle_change(self):
        pin = self.bundle.source.recipes.trusted()
        for name in list(pin['files']) + list(pin['unchanged_oracle']):
            path = self.tree / name
            original = path.read_bytes()
            try:
                path.write_bytes(original + b'changed')
                with self.subTest(name=name), self.assertRaises(ValueError):
                    self.bundle.complete(self.good, self.archive, self.tree, self.configuration, self.library, self.kernel_file)
            finally:
                path.write_bytes(original)


if __name__ == '__main__':
    unittest.main()
