"""资格门的路径/输入/身份契约；不执行数值核，不代表 Linux native pass。"""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('qualification', ROOT / 'tools/qualify-linux-h264-recipe.py')
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class QualificationContracts(unittest.TestCase):
    def test_native_and_recipe_guard_precedes_output_creation(self):
        with tempfile.TemporaryDirectory() as work:
            absent = Path(work).resolve() / 'absent'
            for system, machine, settings in [
                ('Darwin', 'arm64', {}), ('Linux', 'x86_64', {}),
                ('Linux', 'aarch64', {'FRD_FFMPEG_SOURCE_RECIPE': 'unknown'}),
                ('Linux', 'aarch64', {'FRD_FFMPEG_SOURCE_RECIPE': 'upstream'}),
                ('Linux', 'aarch64', {'FRD_FFMPEG_ARCH': 'i686'}),
                ('Linux', 'aarch64', {'FRD_CARGO_TARGET': 'aarch64-linux-ohos'}),
                ('Linux', 'aarch64', {'FRD_FFMPEG_CROSS_PREFIX': 'cross-'}),
                ('Linux', 'aarch64', {'FRD_FFMPEG_CC': 'cross-gcc'})]:
                env = dict(FRD_FFMPEG_SOURCE_RECIPE=gate.RECIPE, **{})
                env.update(settings)
                with self.subTest(system=system, machine=machine, settings=settings), \
                     patch.object(gate.platform, 'system', return_value=system), \
                     patch.object(gate.platform, 'machine', return_value=machine), \
                     self.assertRaises(ValueError):
                    gate.preflight(env)
                self.assertFalse(absent.exists())

    def test_actual_non_linux_host_cli_rejects_before_side_effects(self):
        if gate.platform.system() == 'Linux' and gate.platform.machine().lower() in ('arm64', 'aarch64'):
            self.skipTest('此测试只验证不匹配宿主的早期拒绝')
        with tempfile.TemporaryDirectory() as work:
            output = Path(work).resolve() / 'absent'
            result = subprocess.run(['bash', str(ROOT / 'tools/qualify-linux-h264-recipe.sh'),
                                      '--build-root', str(output)], capture_output=True,
                                    env=dict(os.environ, FRD_FFMPEG_SOURCE_RECIPE=gate.RECIPE))
            self.assertEqual(result.returncode, 2)
            self.assertIn('真实原生 Linux ARM64', result.stderr.decode())
            self.assertFalse(output.exists())

    def test_fresh_root_and_paths_reject_links_escape_and_nonregular_files(self):
        with tempfile.TemporaryDirectory() as work:
            root = Path(work).resolve()
            data = root / 'data'
            data.write_bytes(b'data')
            linked = root / 'linked'
            linked.symlink_to(root, target_is_directory=True)
            for path, kwargs in [(linked / 'data', {}), (data, {'absent': True}),
                                 (root, {}), (data, {'root': root / 'other'})]:
                with self.subTest(path=path, kwargs=kwargs), self.assertRaises(ValueError):
                    gate.safe_path(path, **kwargs)
            dangling = root / 'dangling'
            dangling.symlink_to(root / 'missing')
            with self.assertRaises(ValueError):
                gate.safe_path(dangling, absent=True)
            self.assertEqual(gate.safe_path(data, root), data)

    def test_retained_attempt_ambiguity_outside_root_and_wrong_source_reject(self):
        with tempfile.TemporaryDirectory() as work:
            root = Path(work).resolve()
            attempt = root / 'build.ABC123'
            source = attempt / 'source/ffmpeg-8.1.2'
            source.mkdir(parents=True)
            good = f'Linux FFmpeg retained configured attempt: {attempt}\nLinux FFmpeg retained shipping source: {source}\n'
            self.assertEqual(gate.retained_paths(good, root), (attempt, source))
            for text in [good + good, good.replace(str(source), str(attempt)),
                         good.replace(str(attempt), str(root.parent))]:
                with self.subTest(text=text), self.assertRaises(ValueError):
                    gate.retained_paths(text, root)
            (root / 'build.OTHER').mkdir()
            with self.assertRaises(ValueError):
                gate.retained_paths(good, root)

    def test_before_after_identity_detects_modified_input(self):
        with tempfile.TemporaryDirectory() as work:
            path = Path(work).resolve() / 'object'
            path.write_bytes(b'original')
            before = gate.snapshot([path])
            self.assertEqual(gate.assert_unchanged(before), before)
            path.write_bytes(b'mutation')
            with self.assertRaises(ValueError):
                gate.assert_unchanged(before)

    def test_real_cross_object_fixed_kernel_and_mutations(self):
        # 既有已批准 276 字节 fixture 的对象包装；交叉编译不执行任何 ARM64 指令。
        compiler = shutil.which('clang')
        if not compiler:
            self.skipTest('需要已安装 clang，不自动安装')
        pin = json.loads((ROOT / 'third_party/ffmpeg/8.1.2/h264-luma-dc-aarch64-recipe.json').read_text())['kernel']
        fixture = ROOT / 'tools/tests/fixtures/macos-h264dc-kernel.bin'
        with tempfile.TemporaryDirectory() as work:
            assembly, obj = Path(work).resolve() / 'fixture.S', Path(work).resolve() / 'fixture.o'
            assembly.write_text('.text\n.global ' + pin['symbol'] + '\n.type ' + pin['symbol'] +
                                ',%function\n' + pin['symbol'] + ':\n.incbin "' + str(fixture) +
                                '"\n.size ' + pin['symbol'] + ',.-' + pin['symbol'] + '\n')
            subprocess.run([compiler, '-target', 'aarch64-linux-gnu', '-c', str(assembly), '-o', str(obj)],
                           check=True, capture_output=True)
            gate.object_identity(obj, pin)
            original = obj.read_bytes()
            mutant = bytearray(original)
            at = mutant.find(fixture.read_bytes())
            self.assertGreaterEqual(at, 0)
            mutant[at] ^= 1
            obj.write_bytes(mutant)
            with self.assertRaisesRegex(ValueError, 'bytes'):
                gate.object_identity(obj, pin)
            mutant = bytearray(original)
            mutant[18] = 62  # x86_64 machine 不能冒充 ARM64
            obj.write_bytes(mutant)
            with self.assertRaisesRegex(ValueError, 'AArch64'):
                gate.object_identity(obj, pin)

    def test_benchmark_retains_slow_tail_and_rejects_incomplete_results(self):
        text = ('benchmark C: warmup=65536 measured_calls=8126464 batches=31 ns/call '
                'median=6.039 p95=60.306 max=70.000 checksum=6170d61369b74c40\n'
                'benchmark NEON: warmup=65536 measured_calls=8126464 batches=31 ns/call '
                'median=4.761 p95=65.165 max=75.000 checksum=6170d61369b74c40\n')
        result = gate.benchmark_result(text)
        self.assertEqual(result['C']['p95_ns'], 60.306)
        self.assertEqual(result['NEON']['p95_ns'], 65.165)
        for mutation in [text.splitlines()[0], text + text, text.replace('batches=31', 'batches=30'),
                         text.replace('checksum=6170d61369b74c40', 'checksum=0000000000000000', 1)]:
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                gate.benchmark_result(mutation)

    def test_terminal_failure_preserves_status_and_log(self):
        with tempfile.TemporaryDirectory() as work:
            evidence = gate.Evidence(Path(work).resolve(), dict(os.environ))
            with self.assertRaises(ValueError):
                evidence.run('failure', [gate.sys.executable, '-c', 'print("failure preserved"); raise SystemExit(7)'])
            record = json.loads((Path(work).resolve() / 'qualification.json').read_text())
            self.assertEqual(record['commands'][0]['returncode'], 7)
            self.assertIn('failure preserved', (Path(work).resolve() / 'failure.log').read_text())


if __name__ == '__main__':
    unittest.main()
