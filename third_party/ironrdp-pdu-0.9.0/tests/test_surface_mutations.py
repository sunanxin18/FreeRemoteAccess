#!/usr/bin/env python3
"""长度门/错误标签 mutation 必须由 arithmetic 和真实 session dispatch 拒绝。"""
from pathlib import Path
import subprocess
import tempfile
import unittest

import sys
sys.dont_write_bytecode = True

from surface_support import ROOT, PDU, SOURCE, RUSTC, artifacts, flags, arithmetic_source, observed_session_source


class SurfaceMutations(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.libs = artifacts()
        cls.release_libs = artifacts(release=True)
        cls.source = (PDU / SOURCE).read_text()

    def compile_run(self, source, session=False, release=False):
        with tempfile.TemporaryDirectory(prefix='task-4a27p-mutation-', dir=ROOT / 'target') as tmp:
            base = Path(tmp)
            path = base / 'mutation.rs'
            libs = dict(self.release_libs if release else self.libs)
            cmd = [RUSTC, '--test', str(path), '-o', str(base / 'mutation')]
            if session:
                path.write_text(observed_session_source(base, source, injected=True))
                libs.pop('ironrdp_session')
                cmd += ['--cfg', 'surface_observed', '--cfg', 'surface_injected']
            else:
                path.write_text(arithmetic_source(source))
            compiled = subprocess.run(cmd + flags(libs, release), capture_output=True, text=True)
            self.assertEqual(compiled.returncode, 0, compiled.stderr)
            command = [str(base / 'mutation')]
            if session:
                command += ['injected_u32_other_dispatch']
            return subprocess.run(command, capture_output=True, text=True)

    def test_unchecked_original_arithmetic_fails_debug_and_release(self):
        import re
        mutant = re.sub(r'data_length\s*\.checked_add\(BitmapDataHeader::ENCODED_SIZE\)\s*\.ok_or_else\(\|\| other_err!\("bitmap data length including header exceeds usize"\)\)\?', 'data_length + BitmapDataHeader::ENCODED_SIZE', self.source)
        self.assertNotEqual(mutant, self.source)
        for release in [False, True]:
            with self.subTest(release=release):
                result = self.compile_run(mutant, release=release)
                self.assertNotEqual(result.returncode, 0, result.stdout)

    def test_invalid_field_mutant_fails_arithmetic_variant_assertion(self):
        mutant = self.source.replace('other_err!("bitmap data length including header exceeds usize")', 'invalid_field_err!("bitmap data length", "overflow")')
        self.assertNotEqual(mutant, self.source)
        result = self.compile_run(mutant)
        self.assertNotEqual(result.returncode, 0, result.stdout)

    def test_invalid_field_mutant_is_caught_when_actual_session_dispatch_swallows_it(self):
        mutant = self.source.replace('other_err!("bitmap data length including header exceeds usize")', 'invalid_field_err!("bitmap data length", "overflow")')
        result = self.compile_run(mutant, session=True)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn('Ok', result.stdout)
        self.assertIn('None', result.stdout)


if __name__ == '__main__':
    unittest.main()
