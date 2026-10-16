"""Windows shipping纯host合成fixture：不执行Cargo、PE或PowerShell。"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('windows_shipping', ROOT / 'tools/collect-windows-publication-shipping.py')
COL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COL)
LOAD_HELPER = COL.helper_functions


class ShippingFixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='frd-windows-shipping-host-')
        self.repo = Path(self.temp.name) / 'repo'
        self.repo.mkdir()
        for name in [name for name, _ in COL.SOURCES] + list(COL.CONFIGS):
            dest = self.repo / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, dest)
        _, self.windows_argv, self.binding = LOAD_HELPER(self.repo)
        self.env = mock.patch.dict(os.environ, {'GITHUB_ACTIONS': 'true', 'FRD_WINDOWS_PUBLICATION_SHIPPING': '1'}, clear=True)
        self.env.start()
        self.arch = 'x86_64'
        self.calls = []
        self.exit_code = 0
        self.partial = False
        self.no_map = False
        self.extra_codes = []
        tools = self.repo / 'fixture-tools'
        tools.mkdir()
        self.compiler = tools / 'rustc.exe'
        self.cargo = tools / 'cargo.exe'
        for path in (self.compiler, self.cargo):
            path.write_bytes(b'host synthetic identity only; never executed')
        self.lookup = mock.patch.object(COL.shutil, 'which', side_effect=lambda name: str(tools / (name + '.exe')))
        self.lookup.start()
        self.capture = mock.patch.object(COL, 'helper_functions', return_value=(self.synthetic_build, self.windows_argv, self.binding))
        self.capture.start()

    def tearDown(self):
        self.capture.stop()
        self.lookup.stop()
        self.env.stop()
        self.temp.cleanup()

    def paths(self):
        return COL.locations(self.repo, self.arch)

    def synthetic_build(self, stage, argv, repo, env, evidence):
        self.assertEqual(stage, 'build')
        self.assertEqual(repo, self.repo)
        self.calls.append(argv)
        paths = self.paths()
        paths['built'].parent.mkdir(parents=True, exist_ok=True)
        data = bytearray(160)
        data[:2] = b'MZ'
        struct.pack_into('<I', data, 0x3c, 128)
        data[128:132] = b'PE\0\0'
        struct.pack_into('<H', data, 132, COL.ARCHITECTURES[self.arch][1])
        struct.pack_into('<H', data, 148, 2)
        struct.pack_into('<H', data, 152, COL.ARCHITECTURES[self.arch][2])
        paths['built'].write_bytes(data)
        if not self.no_map:
            (evidence / (COL.APPLICATION + '.map')).write_bytes(b'host synthetic map; no linker executed\n')
        args = [str(self.compiler), '--crate-name', COL.CRATE, 'apps/freeremotedesk-windows/src/main.rs', '--crate-type', 'bin', '--emit=dep-info,link',
                '-C', 'opt-level=3', '-C', 'lto=thin', '-C', argv[-1]]
        if self.arch == 'x86':
            args += ['--target', COL.ARCHITECTURES[self.arch][0]]
        for code in self.extra_codes:
            args += ['-C', code]
        (evidence / 'build.stderr').write_text('     Running `' + subprocess.list2cmdline(args) + '`\n')
        (evidence / 'build.stdout').write_bytes(b'host synthetic only\n')
        return dict(stage='build', argv=argv, started=True, pid=None, terminal_observed=True, exit_code=self.exit_code,
                    capture_complete=not self.partial, stdout_eof=not self.partial, stderr_eof=True, files_closed=not self.partial,
                    failure_exit=self.exit_code or (1 if self.partial else 0), first_failure='host synthetic native/partial' if self.exit_code or self.partial else None)

    def build(self):
        return COL.build(self.repo, self.arch, self.paths())

    def identity(self):
        return json.loads((self.paths()['evidence'] / 'identity.json').read_bytes())

    def staged(self, bom=False):
        paths = self.paths()
        paths['staged'].parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(paths['built'], paths['staged'])
        manifest = {'schema': 'freeremotedesk.windows.ffmpeg-package.v1', 'platform': 'windows-' + self.arch,
                    'files': [{'path': COL.APPLICATION + '.exe', 'role': 'application', 'sha256': hashlib.sha256(paths['staged'].read_bytes()).hexdigest().upper()}]}
        self.write_manifest(manifest, bom)
        return manifest

    def write_manifest(self, manifest, bom=False):
        self.paths()['manifest'].write_bytes((b'\xef\xbb\xbf' if bom else b'') + json.dumps(manifest).encode())

    def finalize(self):
        return COL.finalize(self.repo, self.arch, self.paths())

    def test_both_targets_single_build_same_final_bytes_and_ten_outputs(self):
        for arch in COL.ARCHITECTURES:
            self.arch = arch
            with self.subTest(arch=arch):
                self.assertEqual(self.build(), 0)
                self.assertTrue(self.identity()['build_complete'])
                self.assertFalse(self.identity()['collection_complete'])
                self.staged(bom=True)
                self.assertEqual(self.finalize(), 0)
                row = self.identity()
                self.assertEqual(row['built_pe'], row['staged_pe'])
                self.assertEqual(row['saved_pe'], row['built_pe'])
                self.assertEqual(row['runtime'], 'NOT_RUN')
                self.assertEqual(row['shipping_qualification'], 'OPEN')
                self.assertTrue(row['collection_complete'])
                names = {p.name for p in self.paths()['evidence'].iterdir()}
                self.assertEqual(names, {COL.APPLICATION + '.exe', COL.APPLICATION + '.map', 'build.stdout', 'build.stderr', 'identity.json'} | {snapshot for _, snapshot in COL.SOURCES})
                expected = [str(self.cargo), 'rustc', '--locked', '--release', '-v', '-p', COL.APPLICATION, '--bin', COL.APPLICATION]
                if arch == 'x86':
                    expected += ['--target', COL.ARCHITECTURES[arch][0]]
                self.assertEqual(self.calls[-1], expected + ['--', '-C', 'link-arg=/MAP:' + str(self.paths()['evidence'] / (COL.APPLICATION + '.map'))])
        self.assertEqual(len(self.calls), 2)

    def test_x64_original_default_path_and_i686_explicit_path(self):
        self.assertEqual(self.paths()['built'], self.repo / 'target/release/freeremotedesk-windows.exe')
        self.arch = 'x86'
        self.assertEqual(self.paths()['built'], self.repo / 'target/i686-pc-windows-msvc/release/freeremotedesk-windows.exe')

    def test_fresh_root_missing_target_is_supported(self):
        self.assertFalse((self.repo / 'target').exists())
        self.assertEqual(self.build(), 0)

    def test_setup_failure_after_fresh_evidence_retains_partial_identity(self):
        (self.repo / COL.SOURCES[0][0]).unlink()
        self.assertNotEqual(self.build(), 0)
        row = self.identity()
        self.assertIsNone(row['build'])
        self.assertFalse(row['build_complete'])
        self.assertEqual(self.calls, [])

    def test_existing_evidence_refused_without_overwrite(self):
        self.assertEqual(self.build(), 0)
        before = (self.paths()['evidence'] / 'identity.json').read_bytes()
        with self.assertRaisesRegex(ValueError, 'existing evidence'):
            self.build()
        self.assertEqual(before, (self.paths()['evidence'] / 'identity.json').read_bytes())

    def test_target_symlink_refused(self):
        other = self.repo / 'other'
        other.mkdir()
        (self.repo / 'target').symlink_to(other, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, 'symlink/reparse'):
            self.build()
        self.assertEqual(list(other.iterdir()), [])

    def test_missing_map_fails_closed(self):
        self.no_map = True
        self.assertNotEqual(self.build(), 0)
        self.assertFalse(self.identity()['build_complete'])

    def test_native_77_firsterror_survives_finalize(self):
        self.exit_code = 77
        self.assertEqual(self.build(), 77)
        first = self.identity()['first_failure']
        self.assertEqual(self.finalize(), 77)
        self.assertEqual(self.identity()['first_failure'], first)

    def test_partial_capture_not_pass(self):
        self.partial = True
        self.assertNotEqual(self.build(), 0)
        self.assertFalse(self.identity()['build_complete'])

    def test_profile_and_cpu_overrides_refused_before_build(self):
        for key in ('RUSTFLAGS', 'CARGO_PROFILE_RELEASE_LTO', 'CARGO_BUILD_TARGET', 'CARGO_TARGET_DIR'):
            with self.subTest(key=key), mock.patch.dict(os.environ, {key: 'unreviewed'}):
                self.assertNotEqual(self.build(), 0)
                self.assertIsNone(self.identity()['build'])
                shutil.rmtree(self.paths()['evidence'])
        self.assertEqual(self.calls, [])

    def test_actual_rustc_cpu_override_refused(self):
        self.extra_codes = ['target-feature=+avx2']
        self.assertNotEqual(self.build(), 0)
        self.assertFalse(self.identity()['build_complete'])

    def test_changed_build_map_source_and_raw_refused(self):
        for changed in ('built', 'map', 'source', 'raw'):
            with self.subTest(changed=changed):
                self.assertEqual(self.build(), 0)
                self.staged()
                path = {'built': self.paths()['built'], 'map': self.paths()['evidence'] / (COL.APPLICATION + '.map'),
                        'source': self.repo / COL.SOURCES[0][0], 'raw': self.paths()['evidence'] / 'build.stdout'}[changed]
                old = path.read_bytes()
                path.write_bytes(old + b'changed')
                self.assertNotEqual(self.finalize(), 0)
                self.assertFalse(self.identity()['collection_complete'])
                path.write_bytes(old)
                shutil.rmtree(self.paths()['evidence'])

    def test_staged_changed_bytes_refused(self):
        self.assertEqual(self.build(), 0)
        self.staged()
        self.paths()['staged'].write_bytes(self.paths()['staged'].read_bytes() + b'different')
        self.assertNotEqual(self.finalize(), 0)

    def test_manifest_schema_platform_duplicates_hash_and_absence_refused(self):
        self.assertEqual(self.build(), 0)
        good = self.staged()
        initial = (self.paths()['evidence'] / 'identity.json').read_bytes()
        for mutation in ('schema', 'platform', 'duplicate', 'digest', 'absent'):
            with self.subTest(mutation=mutation):
                manifest = json.loads(json.dumps(good))
                if mutation in ('schema', 'platform'):
                    manifest[mutation] = 'wrong'
                elif mutation == 'duplicate':
                    manifest['files'] *= 2
                elif mutation == 'digest':
                    manifest['files'][0]['sha256'] = '0' * 64
                self.write_manifest(manifest)
                if mutation == 'absent':
                    self.paths()['manifest'].unlink()
                self.assertNotEqual(self.finalize(), 0)
                self.assertFalse(self.identity()['collection_complete'])
                (self.paths()['evidence'] / 'identity.json').write_bytes(initial)

    def test_duplicate_json_key_and_repeated_finalize_refused(self):
        with self.assertRaisesRegex(ValueError, 'duplicate JSON'):
            COL.unique_json(b'{"files":[],"files":[]}')
        self.assertEqual(self.build(), 0)
        self.staged()
        self.assertEqual(self.finalize(), 0)
        before = (self.paths()['evidence'] / 'identity.json').read_bytes()
        with self.assertRaisesRegex(ValueError, 'repeated finalize'):
            self.finalize()
        self.assertEqual(before, (self.paths()['evidence'] / 'identity.json').read_bytes())

    def test_helper_exact_lf_and_crlf_only_before_compile(self):
        path = self.repo / COL.HELPER
        lf = path.read_bytes()
        for raw in (lf, lf.replace(b'\n', b'\r\n')):
            path.write_bytes(raw)
            _, _, binding = LOAD_HELPER(self.repo)
            self.assertEqual(binding['normalized_lf_sha256'], COL.LF_SHA)
        path.write_bytes(lf + b'\n')
        with mock.patch('builtins.compile', side_effect=AssertionError('must reject before compile')):
            with self.assertRaisesRegex(ValueError, 'unreviewed raw helper'):
                LOAD_HELPER(self.repo)

    def test_architecture_class_and_caps_refused(self):
        self.assertEqual(self.build(), 0)
        with self.assertRaisesRegex(ValueError, 'machine/optional'):
            COL.pe_header(self.paths()['built'], 'x86')
        with self.assertRaisesRegex(ValueError, 'byte cap'):
            COL.file_identity(self.paths()['built'], 1)
        with self.assertRaisesRegex(ValueError, 'symlink/reparse'):
            COL.no_reparse(type('Info', (), {'st_mode': 0o100644, 'st_file_attributes': 0x400})())

    def test_closed_cli_and_non_windows_host_refused(self):
        for mode, arch in (('run', 'x86'), ('build', 'arm64'), ('build', '../x86')):
            with self.assertRaisesRegex(ValueError, 'fixed build/finalize'):
                COL.run(mode, arch)
        if os.name != 'nt':
            with self.assertRaisesRegex(ValueError, 'actual Windows'):
                COL.run('build', 'x86_64')

    def test_workflow_same_link_and_final_binding_contract(self):
        source = (ROOT / '.github/workflows/build-windows.yml').read_text()
        self.assertIn('collect-windows-publication-shipping.py build', source)
        self.assertIn('collect-windows-publication-shipping.py finalize', source)
        self.assertLess(source.index('- name: Run Windows package Pester tests'), source.index('- name: Finalize ordinary Windows publication shipping identity'))
        self.assertLess(source.index('- name: Finalize ordinary Windows publication shipping identity'), source.index('- name: Upload exact Windows package'))


if __name__ == '__main__':
    unittest.main()
