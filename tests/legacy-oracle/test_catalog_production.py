"""真实生产 inventory 的窄变更、错误 posthash 与额外 source mutation 必须拒绝。"""
import hashlib
import json
from pathlib import Path
import shutil
import tempfile
import unittest

from verify_catalog import ROOT, validate_production_copy


class ProductionInventoryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='frd-original-inventory-')
        self.base = Path(self.temp.name) / 'ironrdp-graphics-0.9.0'
        shutil.copytree(ROOT / 'third_party' / 'ironrdp-graphics-0.9.0', self.base)

    def tearDown(self):
        self.temp.cleanup()

    def test_current_precise_bitmap_change_is_accepted(self):
        validate_production_copy(self.base, 'ironrdp-graphics-0.9.0')

    def test_bitmap_mutation_without_matching_posthash_is_rejected(self):
        path = self.base / 'src/rle.rs'
        path.write_bytes(path.read_bytes() + b'\n// unrecorded mutation\n')
        with self.assertRaises(AssertionError):
            validate_production_copy(self.base, 'ironrdp-graphics-0.9.0')

    def test_wrong_recorded_bitmap_posthash_is_rejected(self):
        path = self.base / 'FRD-MODIFICATIONS.json'
        data = json.loads(path.read_text())
        data['modified_originals']['src/rle.rs']['post_sha256'] = '0' * 64
        path.write_text(json.dumps(data))
        with self.assertRaises(AssertionError):
            validate_production_copy(self.base, 'ironrdp-graphics-0.9.0')

    def test_planar_mutation_without_matching_posthash_is_rejected(self):
        for relative in ['src/rdp6/rle.rs', 'src/rdp6/bitmap_stream/decoder.rs']:
            with self.subTest(relative=relative):
                path = self.base / relative
                original = path.read_bytes()
                path.write_bytes(original + b'\n// unrecorded mutation\n')
                with self.assertRaises(AssertionError):
                    validate_production_copy(self.base, 'ironrdp-graphics-0.9.0')
                path.write_bytes(original)

    def test_wrong_planar_posthash_and_original_identity_are_rejected(self):
        path = self.base / 'FRD-MODIFICATIONS.json'
        original = path.read_text()
        for relative in ['src/rdp6/rle.rs', 'src/rdp6/bitmap_stream/decoder.rs']:
            for key in ['post_sha256', 'upstream_sha256']:
                with self.subTest(relative=relative, key=key):
                    data = json.loads(original)
                    data['modified_originals'][relative][key] = '0' * 64
                    path.write_text(json.dumps(data))
                    with self.assertRaises(AssertionError):
                        validate_production_copy(self.base, 'ironrdp-graphics-0.9.0')
        path.write_text(original)

    def test_unrecorded_other_original_source_mutation_is_rejected(self):
        path = self.base / 'src/dwt.rs'
        path.write_bytes(path.read_bytes() + b'\n// unrecorded mutation\n')
        with self.assertRaises(AssertionError):
            validate_production_copy(self.base, 'ironrdp-graphics-0.9.0')

    def test_extra_original_source_allowance_is_rejected_even_with_correct_hash(self):
        path = self.base / 'FRD-MODIFICATIONS.json'
        data = json.loads(path.read_text())
        checksum = hashlib.sha256((self.base / 'src/dwt.rs').read_bytes()).hexdigest()
        data['modified_originals']['src/dwt.rs'] = {'upstream_sha256': checksum, 'post_sha256': checksum}
        path.write_text(json.dumps(data))
        with self.assertRaises(AssertionError):
            validate_production_copy(self.base, 'ironrdp-graphics-0.9.0')

    def test_rfx_modified_files_require_exact_hashes(self):
        inventory = self.base / 'FRD-MODIFICATIONS.json'
        original = inventory.read_text()
        for relative in ['src/rlgr.rs', 'src/lib.rs']:
            for key in ['post_sha256', 'upstream_sha256']:
                with self.subTest(relative=relative, key=key):
                    data = json.loads(original)
                    data['modified_originals'][relative][key] = '0' * 64
                    inventory.write_text(json.dumps(data))
                    with self.assertRaises(AssertionError):
                        validate_production_copy(self.base, 'ironrdp-graphics-0.9.0')
        inventory.write_text(original)

    def test_session_precise_mutations_and_extra_allowances_are_rejected(self):
        base = Path(self.temp.name) / 'ironrdp-session-0.11.0'
        shutil.copytree(ROOT / 'third_party' / 'ironrdp-session-0.11.0', base)
        validate_production_copy(base, 'ironrdp-session-0.11.0')
        inventory = base / 'FRD-MODIFICATIONS.json'
        original = inventory.read_text()
        for relative in ['src/rfx.rs', 'src/fast_path.rs', 'src/active_stage.rs']:
            path = base / relative
            old = path.read_bytes()
            path.write_bytes(old + b'\n// unrecorded mutation\n')
            with self.assertRaises(AssertionError):
                validate_production_copy(base, 'ironrdp-session-0.11.0')
            path.write_bytes(old)
            for key in ['post_sha256', 'upstream_sha256']:
                data = json.loads(original)
                data['modified_originals'][relative][key] = '0' * 64
                inventory.write_text(json.dumps(data))
                with self.assertRaises(AssertionError):
                    validate_production_copy(base, 'ironrdp-session-0.11.0')
                inventory.write_text(original)
        data = json.loads(original)
        checksum = hashlib.sha256((base / 'src/image.rs').read_bytes()).hexdigest()
        data['modified_originals']['src/image.rs'] = {'upstream_sha256': checksum, 'post_sha256': checksum}
        inventory.write_text(json.dumps(data))
        with self.assertRaises(AssertionError):
            validate_production_copy(base, 'ironrdp-session-0.11.0')

    def test_wrong_original_identity_is_rejected(self):
        path = self.base / 'FRD-MODIFICATIONS.json'
        data = json.loads(path.read_text())
        data['modified_originals']['src/rle.rs']['upstream_sha256'] = '0' * 64
        path.write_text(json.dumps(data))
        with self.assertRaises(AssertionError):
            validate_production_copy(self.base, 'ironrdp-graphics-0.9.0')


if __name__ == '__main__':
    unittest.main()
