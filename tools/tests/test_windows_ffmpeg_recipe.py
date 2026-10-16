"""Windows source/host 边界；不构建 FFmpeg 或执行 PE。"""
import importlib.util
from contextlib import contextmanager
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[2]


def module(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'tools' / (name + '.py'))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


@contextmanager
def workarea():
    parent = os.environ.get('FRD_WINDOWS_RECIPE_TEST_ARTIFACTS')
    if parent:
        Path(parent).mkdir(parents=True, exist_ok=True)
        yield tempfile.mkdtemp(prefix='source-recipe-', dir=parent)
    else:
        with tempfile.TemporaryDirectory() as work:
            yield work


class RecipeBoundary(unittest.TestCase):
    def test_defaults_profiles_and_rejection_before_side_effects(self):
        bundle = module('windows-ffmpeg-bundle')
        self.assertEqual(bundle.selection(), {'recipe': 'upstream', 'architecture': 'x86_64',
                                            'modified': False, 'publishable': True})
        for arch in ('x86', 'x86_64', 'arm64'):
            self.assertFalse(bundle.selection('upstream', arch)['modified'])
        self.assertFalse(bundle.selection(bundle.source.recipes.MODIFIED, 'arm64')['publishable'])
        with workarea() as work:
            out = Path(work) / 'absent'
            for profile, recipe in [('windows-x86', 'upstream'), ('windows-x86_64', 'upstream'),
                                    ('windows-arm64ec', bundle.source.recipes.MODIFIED),
                                    ('windows-aarch64', 'unknown')]:
                with self.subTest(profile=profile), self.assertRaises(ValueError):
                    bundle.source.prepare(Path(work) / 'no-archive', out, recipe, profile)
                self.assertFalse(out.exists())
            for arch, recipe in [('x86', bundle.source.recipes.MODIFIED),
                                 ('x86_64', bundle.source.recipes.MODIFIED),
                                 ('arm64ec', 'upstream'), ('arm64', 'unknown')]:
                result = subprocess.run(['python3', str(ROOT / 'tools/windows-ffmpeg-bundle.py'),
                                         'selection', '--source-recipe', recipe, '--architecture', arch],
                                        cwd=work, capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(list(Path(work).iterdir()), [])

    @unittest.skipUnless(shutil.which('pwsh'), 'pwsh 不可用；不下载')
    def test_builder_rejects_before_tree_or_external_tool(self):
        with workarea() as work:
            for arch, recipe in [('arm64', 'unknown'), ('x86', 'frd-h264dc-aarch64-v1'),
                                 ('x86_64', 'frd-h264dc-aarch64-v1'), ('arm64ec', 'upstream')]:
                result = subprocess.run(['pwsh', '-NoProfile', '-File', str(ROOT / 'tools/build-ffmpeg-windows.ps1'),
                                         '-Architecture', arch, '-SourceRecipe', recipe], cwd=work, capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(list(Path(work).iterdir()), [])

    def test_exact_windows_source_closure_and_fresh_patch_replay(self):
        source = module('desktop-ffmpeg-source')
        archive = Path(os.environ.get('FRD_FFMPEG_TEST_ARCHIVE', ROOT / 'target/ffmpeg-macos/ffmpeg-8.1.2.tar.xz'))
        if not archive.is_file():
            self.skipTest('需要已有固定 archive；不下载')
        with workarea() as work:
            root = Path(work)
            asset = root / source.asset_name(source.recipes.MODIFIED, 'windows-aarch64')
            source.write_asset(archive, asset, source.recipes.MODIFIED, 'windows-aarch64')
            with zipfile.ZipFile(asset) as package:
                members = {name: package.read(name) for name in package.namelist()}
                self.assertEqual(set(members), {'ffmpeg-8.1.2.tar.xz', 'LICENSE.LGPLv2.1', 'REBUILD.txt',
                    'SOURCE-MANIFEST.json', str(source.recipes.PIN), str(source.recipes.PATCH),
                    'tools/ffmpeg-source-recipe.py', 'tools/desktop-ffmpeg-source.py',
                    'tools/build-ffmpeg-windows.ps1', 'tools/ffmpeg-build-common.psm1',
                    'tools/windows-ffmpeg-bundle.py', 'tools/windows-ffmpeg-build-capture.py',
                    'tools/windows-ffmpeg-kernel.py'})
                self.assertIn(b'not an upstream-signed release', members['REBUILD.txt'])
                self.assertIn(b'old stage/installer contracts do not admit', members['REBUILD.txt'])
                package.extractall(root / 'materials')
            tree, info = source.prepare(root / 'materials/ffmpeg-8.1.2.tar.xz', root / 'fresh',
                                        source.recipes.MODIFIED, 'windows-aarch64', root / 'materials')
            for name, hashes in info['files'].items():
                self.assertEqual(source.recipes.digest((tree / name).read_bytes()), hashes['post'])
            for name, expected in info['unchanged_oracle'].items():
                self.assertEqual(source.recipes.digest((tree / name).read_bytes()), expected)
            source.verify_embedded_asset(asset, source.recipes.MODIFIED, 'windows-aarch64')
            mutants = [dict(members, unknown=b'x')]
            missing = dict(members)
            del missing['tools/windows-ffmpeg-kernel.py']
            mutants.append(missing)
            for name in (str(source.recipes.PATCH), str(source.recipes.PIN), 'tools/windows-ffmpeg-build-capture.py',
                         'tools/build-ffmpeg-windows.ps1', 'ffmpeg-8.1.2.tar.xz'):
                changed = dict(members)
                changed[name] += b'tamper'
                manifest = json.loads(changed['SOURCE-MANIFEST.json'])
                manifest['files'][name] = source.recipes.digest(changed[name])
                changed['SOURCE-MANIFEST.json'] = json.dumps(manifest).encode()
                mutants.append(changed)
            for index, mutant in enumerate(mutants):
                path = root / f'mutant-{index}.zip'
                with zipfile.ZipFile(path, 'w') as package:
                    for name, data in mutant.items():
                        item = zipfile.ZipInfo(name)
                        item.external_attr = 0o100644 << 16
                        package.writestr(item, data)
                with self.subTest(index=index), self.assertRaises(ValueError):
                    source.verify_embedded_asset(path, source.recipes.MODIFIED, 'windows-aarch64')
            with self.assertRaisesRegex(ValueError, 'fresh'):
                source.prepare(archive, root / 'fresh', source.recipes.MODIFIED, 'windows-aarch64')

    def test_failed_attempt_keeps_partial_and_has_no_publish_authority(self):
        bundle = module('windows-ffmpeg-bundle')
        with workarea() as work:
            attempt = Path(work)
            tree, dist = attempt / 'source', attempt / 'dist'
            tree.mkdir()
            dist.mkdir()
            partial = tree / 'partial.o'
            partial.write_bytes(b'failed build partial')
            with self.assertRaises(ValueError):
                bundle.complete(attempt, tree, attempt / 'archive', dist)
            receipt = json.loads((attempt / 'build-side-result.json').read_bytes())
            self.assertEqual(receipt['status'], 'failed')
            self.assertFalse(receipt['publishable'])
            self.assertEqual(partial.read_bytes(), b'failed build partial')
            self.assertFalse((attempt / 'codec').exists())


if __name__ == '__main__':
    unittest.main()
