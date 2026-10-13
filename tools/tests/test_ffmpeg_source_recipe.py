"""固定源配方信任边界；不执行目标代码。"""
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('recipe_bundle', ROOT / 'tools/ohos-ffmpeg-bundle.py')
bundle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bundle)


class RecipeBoundary(unittest.TestCase):
    def test_modified_recipe_has_exact_additional_materials(self):
        self.assertTrue(hasattr(bundle, 'files_for_recipe'), '缺少按受信配方选择材料的边界')
        self.assertEqual(bundle.files_for_recipe('frd-h264dc-aarch64-v1'),
                         bundle.FILES | {'FFmpeg-CHANGES.diff', 'FFmpeg-SOURCE-RECIPE.json'})

    def test_source_metadata_is_bound_to_approved_patch(self):
        self.assertTrue(hasattr(bundle, 'recipes'), '缺少固定来源配方实现')
        source = bundle.recipes.metadata('frd-h264dc-aarch64-v1')
        self.assertTrue(source['modified'])
        self.assertEqual(source['patch_sha256'], '4527ec807869f2a8ae8c2843ce51c693f3cbb728104e4fb9e617627dec593c7c')
        with self.assertRaises(ValueError):
            bundle.recipes.metadata('arbitrary-patch')

class SourceReplay(unittest.TestCase):
    archive = ROOT / 'target/ffmpeg-macos/ffmpeg-8.1.2.tar.xz'

    def test_fresh_fixed_replay_and_repeat_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / 'fresh'
            source, metadata = bundle.recipes.prepare(self.archive, target, bundle.recipes.MODIFIED)
            for name, hashes in metadata['files'].items():
                self.assertEqual(bundle.sha(source / name), hashes['post'])
            with self.assertRaisesRegex(ValueError, 'fresh'):
                bundle.recipes.prepare(self.archive, target, bundle.recipes.MODIFIED)

    def test_upstream_remains_pristine(self):
        with tempfile.TemporaryDirectory() as temporary:
            source, metadata = bundle.recipes.prepare(self.archive, Path(temporary) / 'fresh', 'upstream')
            self.assertFalse(metadata['modified'])
            for name, hashes in bundle.recipes.trusted()['files'].items():
                self.assertEqual(bundle.sha(source / name), hashes['pre'])

    def test_wrong_arch_and_unknown_recipe_fail_before_creation(self):
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / 'fresh'
            for recipe, arch in [(bundle.recipes.MODIFIED, 'x86_64'), ('unknown', 'aarch64')]:
                with self.assertRaises(ValueError):
                    bundle.recipes.prepare(self.archive, target, recipe, arch)
                self.assertFalse(target.exists())

    def test_modified_pin_or_patch_cannot_self_authorize(self):
        import shutil
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for path in (bundle.recipes.PIN, bundle.recipes.PATCH):
                (root / path).parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / path, root / path)
            patch = root / bundle.recipes.PATCH
            patch.write_bytes(patch.read_bytes() + b'\nextra hunk')
            with self.assertRaisesRegex(ValueError, 'patch SHA256'):
                bundle.recipes.metadata(bundle.recipes.MODIFIED, repo=root)
            import json
            pin = root / bundle.recipes.PIN
            value = json.loads(pin.read_text())
            value['patch_sha256'] = bundle.sha(patch)
            pin.write_text(json.dumps(value))
            with self.assertRaisesRegex(ValueError, 'pin SHA256'):
                bundle.recipes.metadata(bundle.recipes.MODIFIED, repo=root)

    def test_modified_flag_requires_json_boolean_not_integer(self):
        source = bundle.recipes.metadata(bundle.recipes.MODIFIED)
        source['modified'] = 1
        with self.assertRaisesRegex(ValueError, '来源/recipe'):
            bundle.recipes.identify({'schema': 2, 'source': source})

    def test_exact_source_metadata_mutations(self):
        import copy
        accepted = {'schema': 2, 'source': bundle.recipes.metadata(bundle.recipes.MODIFIED)}
        self.assertEqual(bundle.recipes.identify(accepted), bundle.recipes.MODIFIED)
        mutations = []
        for key, value in [('recipe', 'unknown'), ('modified', False), ('patch_sha256', '0'*64), ('patches', ['arbitrary'])]:
            mutant = copy.deepcopy(accepted)
            mutant['source'][key] = value
            mutations.append(mutant)
        for image in ('pre', 'post'):
            mutant = copy.deepcopy(accepted)
            next(iter(mutant['source']['files'].values()))[image] = '0'*64
            mutations.append(mutant)
        mutations += [{'schema': 1, 'source': accepted['source']}, {'schema': True, 'source': bundle.recipes.metadata('upstream')}]
        for mutant in mutations:
            with self.subTest(mutant=mutant):
                with self.assertRaises(ValueError):
                    bundle.recipes.identify(mutant)


class StageRecipeTransitions(unittest.TestCase):
    def test_recipe_marker_records_exact_filemap(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location('recipe_stage', ROOT / 'tools/stage-harmony-ffmpeg.py')
        stager = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(stager)
        self.assertTrue(hasattr(stager, 'owned_files'), '旧 ownership 未能按记录的 recipe 清理')
        paths = stager.destinations(bundle.recipes.MODIFIED)
        hashes = {name: '1'*64 for name in paths}
        self.assertEqual(stager.owned_files({'schema': 2, 'recipe': bundle.recipes.MODIFIED, 'files': hashes}), hashes)
        legacy = {name: '2'*64 for name in stager.destinations()}
        self.assertEqual(stager.owned_files(legacy), legacy)
        with self.assertRaises(ValueError):
            stager.owned_files({'schema': 2, 'recipe': 'unknown', 'files': hashes})

class OldLibraryCannotClaimModified(unittest.TestCase):
    def test_old_library_is_rejected_even_with_kernel_metadata(self):
        lib = ROOT / 'target/ffmpeg-ohos/bundle-attempt-qjsezbh7/libavcodec.so.62'
        kernel = dict(bundle.recipes.trusted()['kernel'], address=0)
        with self.assertRaisesRegex(ValueError, 'kernel'):
            bundle.recipes.kernel_identity(lib, kernel)

    def test_missing_and_modified_materials_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary)
            with self.assertRaises(ValueError):
                bundle.verify_source_materials(path, bundle.recipes.MODIFIED)
            for name, data in bundle.recipes.materials().items():
                (path / name).write_bytes(data)
            bundle.verify_source_materials(path, bundle.recipes.MODIFIED)
            with self.assertRaises(ValueError):
                bundle.verify_source_materials(path, 'upstream')
            (path / bundle.recipes.EXTRA[0]).write_bytes(b'arbitrary patch')
            with self.assertRaisesRegex(ValueError, '受信源码材料'):
                bundle.verify_source_materials(path, bundle.recipes.MODIFIED)


class SourceAssetAndSafety(unittest.TestCase):
    def test_archive_symlink_and_modified_archive_rejected_before_extract(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            link = root / 'archive'
            link.symlink_to(SourceReplay.archive)
            target = root / 'destination'
            with self.assertRaisesRegex(ValueError, '普通文件'):
                bundle.recipes.prepare(link, target, bundle.recipes.MODIFIED)
            self.assertFalse(target.exists())
            link.unlink()
            link.write_bytes(b'not the trusted archive')
            with self.assertRaisesRegex(ValueError, 'SHA256'):
                bundle.recipes.prepare(link, target, bundle.recipes.MODIFIED)
            self.assertFalse(target.exists())

    def test_complete_asset_reconstruction_and_changed_member_rejection(self):
        import zipfile
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            asset = root / bundle.recipes.ASSET
            bundle.recipes.write_asset(SourceReplay.archive, asset)
            bundle.recipes.verify_asset(SourceReplay.archive, asset)
            with zipfile.ZipFile(asset) as package:
                files = {n: package.read(n) for n in package.namelist()}
                package.extractall(root / 'asset')
            source, expected = bundle.recipes.prepare(root / 'asset/ffmpeg-8.1.2.tar.xz', root / 'replay',
                                                       bundle.recipes.MODIFIED, repo=root / 'asset')
            for name, hashes in expected['files'].items():
                self.assertEqual(bundle.sha(source / name), hashes['post'])
            files[str(bundle.recipes.PATCH)] += b'\nextra patch'
            mutant = root / 'mutant.zip'
            with zipfile.ZipFile(mutant, 'w') as package:
                for name, content in files.items():
                    package.writestr(name, content)
            with self.assertRaisesRegex(ValueError, 'asset bytes'):
                bundle.recipes.verify_asset(SourceReplay.archive, mutant)


if __name__ == '__main__':
    unittest.main()
