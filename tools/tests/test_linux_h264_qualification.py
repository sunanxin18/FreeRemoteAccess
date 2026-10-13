"""资格门的路径/输入/身份契约；不执行数值核，不代表 Linux native pass。"""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('qualification', ROOT / 'tools/qualify-linux-h264-recipe.py')
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class QualificationContracts(unittest.TestCase):
    def test_retained_actual_gcc_configuration_accepted_without_evaluation(self):
        retained = ROOT / ('target/ci-h264-linux-soname-20261014/failed-modified-arm64-artifact/'
                           'build.qtKBox/source/ffmpeg-8.1.2/ffbuild/config.mak')
        if not retained.exists():
            self.skipTest('本机未保留失败 CI 的配置材料；其他合同不依赖 artifact')
        self.assertTrue(callable(getattr(gate, '_configured_fixture_compiler', None)),
                        '缺少固定 GCC 配置验证')
        record = gate._configured_fixture_compiler(retained, gate.digest(retained))
        self.assertEqual(record['command'], ['gcc'])
        self.assertEqual(record['configuration_sha256'], gate.digest(retained))

    def test_fixed_compiler_parser_requires_hash_and_unique_exact_assignment(self):
        self.assertTrue(callable(getattr(gate, '_configured_fixture_compiler', None)),
                        '缺少固定 GCC 配置验证')
        with tempfile.TemporaryDirectory() as work:
            path = Path(work).resolve() / 'config.mak'
            path.write_text('# CC=ignored\nCC_IDENT=gcc 13\nCC=gcc\nCC_C=-c\n')
            self.assertEqual(gate._configured_fixture_compiler(path, gate.digest(path))['command'], ['gcc'])
            with self.assertRaisesRegex(ValueError, 'hash'):
                gate._configured_fixture_compiler(path, '0' * 64)
            for text in ('CC=gcc\r\n', 'CC::=gcc\n', 'private CC=gcc\n', 'define CC\ngcc\nendef\n',
                         'define OTHER\nCC=gcc\nendef\n',
                         'CC=gcc\nundefine CC\n', '\tCC=gcc\n', 'CC="gcc"\n', 'CC=/usr/bin/gcc\n'):
                path.write_bytes(text.encode())
                with self.subTest(text=text), self.assertRaisesRegex(ValueError, 'CC=gcc'):
                    gate._configured_fixture_compiler(path, gate.digest(path))

    def test_exact_installed_aliases_preserve_generic_no_link_gate(self):
        with tempfile.TemporaryDirectory() as work:
            root = Path(work).resolve()
            attempt = root / 'build.ABC123'
            lib = attempt / 'dist/usr/lib'
            lib.mkdir(parents=True)
            for name, concrete in [('libavutil.so.60', 'libavutil.so.60.26.102'),
                                   ('libavcodec.so.62', 'libavcodec.so.62.28.102')]:
                (lib / concrete).write_bytes(b'installed bytes')
                (lib / name).symlink_to(concrete)
                self.assertTrue(callable(getattr(gate, 'installed_library', None)),
                                '缺少受限安装 SONAME 解析器')
                identity = gate.installed_library(lib / name, attempt, root)
                self.assertEqual(identity['alias']['path'], str(lib / name))
                self.assertEqual(identity['alias']['readlink'], concrete)
                self.assertEqual(identity['target']['path'], str(lib / concrete))
                with self.assertRaises(ValueError):
                    gate.safe_path(lib / name, root)

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


class InstalledLibraryContracts(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name).resolve()
        self.attempt = self.root / 'build.ABC123'
        self.lib = self.attempt / 'dist/usr/lib'
        self.lib.mkdir(parents=True)
        self.util = self.lib / 'libavutil.so.60'
        self.target = self.lib / 'libavutil.so.60.26.102'
        self.codec = self.lib / 'libavcodec.so.62'
        self.codec_target = self.lib / 'libavcodec.so.62.28.102'
        self.target.write_bytes(b'util bytes')
        self.codec_target.write_bytes(b'codec bytes')
        self.util.symlink_to(self.target.name)
        self.codec.symlink_to(self.codec_target.name)

    def identities(self):
        return {p.name: gate.installed_library(p, self.attempt, self.root)
                for p in (self.util, self.codec)}

    def test_rejects_absolute_path_bearing_wrong_version_chain_cycle_and_dangling_links(self):
        chain = self.lib / 'chain'
        chain.symlink_to(self.target.name)
        for link in (str(self.target), './' + self.target.name, '../lib/' + self.target.name,
                     'libavutil.so.60.99.102', 'chain', self.util.name, 'missing',
                     '../escape', '../../../../../escape'):
            with self.subTest(link=link):
                self.util.unlink()
                self.util.symlink_to(link)
                with self.assertRaises(ValueError):
                    gate.installed_library(self.util, self.attempt, self.root)

    def test_fixed_target_must_be_regular_and_not_a_link_directory_fifo_or_missing(self):
        self.target.unlink()
        for kind in ('link', 'directory', 'fifo', 'missing'):
            with self.subTest(kind=kind):
                if kind == 'link':
                    self.target.symlink_to(self.codec_target.name)
                elif kind == 'directory':
                    self.target.mkdir()
                elif kind == 'fifo':
                    os.mkfifo(self.target)
                with self.assertRaises((ValueError, OSError)):
                    gate.installed_library(self.util, self.attempt, self.root)
                if self.target.is_dir():
                    self.target.rmdir()
                elif self.target.exists() or self.target.is_symlink():
                    self.target.unlink()

    def test_rejects_nonlink_alias_other_prefix_and_linked_ancestors(self):
        self.util.unlink()
        self.util.write_bytes(self.target.read_bytes())
        with self.assertRaises(ValueError):
            gate.installed_library(self.util, self.attempt, self.root)
        self.util.unlink()
        self.util.symlink_to(self.target.name)
        other = self.root / 'build.OTHER/dist/usr/lib'
        other.mkdir(parents=True)
        (other / self.target.name).write_bytes(self.target.read_bytes())
        (other / self.util.name).symlink_to(self.target.name)
        for alias in (other / self.util.name, self.lib / 'unknown.so', self.target):
            with self.subTest(alias=alias), self.assertRaises(ValueError):
                gate.installed_library(alias, self.attempt, self.root)
        original = self.attempt / 'dist/usr'
        moved = self.attempt / 'dist/moved'
        original.rename(moved)
        original.symlink_to('moved', target_is_directory=True)
        with self.assertRaises(ValueError):
            gate.installed_library(self.util, self.attempt, self.root)

    def test_source_and_bundle_aliases_remain_rejected(self):
        for relative in ('source/ffmpeg-8.1.2/libavcodec/libavcodec.so.62',
                         'bundle/linux-aarch64/libavutil.so.60'):
            path = self.root / relative
            path.parent.mkdir(parents=True)
            path.write_bytes(b'regular')
            self.assertEqual(gate.safe_path(path, self.root), path)
            path.unlink()
            path.symlink_to(self.target)
            with self.assertRaises(ValueError):
                gate.safe_path(path, self.root)
            with self.assertRaises(ValueError):
                gate.installed_library(path, self.attempt, self.root)

    def test_alias_and_same_byte_target_replacement_are_identity_changes(self):
        mutations = ('util-bytes', 'target-replace', 'target-link', 'alias-recreate',
                     'alias-readlink', 'alias-regular', 'codec-bytes', 'codec-replace',
                     'codec-alias-recreate')
        for mutation in mutations:
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as work:
                root = Path(work).resolve()
                attempt = root / 'build.TEST'
                lib = attempt / 'dist/usr/lib'
                shutil.copytree(self.lib, lib, symlinks=True)
                before = {name: gate.installed_library(lib / name, attempt, root)
                          for name in ('libavutil.so.60', 'libavcodec.so.62')}
                alias = lib / ('libavcodec.so.62' if mutation.startswith('codec') else self.util.name)
                target = lib / ('libavcodec.so.62.28.102' if mutation.startswith('codec') else self.target.name)
                if mutation.endswith('bytes'):
                    target.write_bytes(b'mutated')
                elif mutation.endswith('replace'):
                    replacement = lib / 'replacement'
                    replacement.write_bytes(target.read_bytes())
                    os.replace(replacement, target)
                elif mutation == 'target-link':
                    target.unlink()
                    target.symlink_to(self.codec_target.name)
                else:
                    alias.unlink()
                    if mutation.endswith('recreate'):
                        alias.symlink_to(target.name)
                    elif mutation == 'alias-readlink':
                        alias.symlink_to(self.codec_target.name)
                    else:
                        alias.write_bytes(target.read_bytes())
                with self.assertRaises((ValueError, OSError)):
                    gate.assert_installed_unchanged(before, attempt, root)
                failure = gate.installed_failure_snapshot(before, attempt, root)
                self.assertTrue(failure[alias.name]['changed'])
                self.assertEqual(before[alias.name]['alias']['readlink'], target.name)

    def test_read_races_reject_target_replacement_alias_recreation_and_parent_replacement(self):
        for kind in ('target', 'alias', 'directory'):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as work:
                root = Path(work).resolve()
                attempt = root / 'build.RACE'
                lib = attempt / 'dist/usr/lib'
                shutil.copytree(self.lib, lib, symlinks=True)
                real_open = gate.os.open
                changed = False

                def racing_open(path, flags, *args, **kwargs):
                    nonlocal changed
                    if path == self.target.name and not changed:
                        changed = True
                        if kind == 'target':
                            replacement = lib / 'replacement'
                            replacement.write_bytes((lib / self.target.name).read_bytes())
                            os.replace(replacement, lib / self.target.name)
                        elif kind == 'alias':
                            (lib / self.util.name).unlink()
                            (lib / self.util.name).symlink_to(self.target.name)
                        else:
                            moved = lib.parent / 'moved'
                            lib.rename(moved)
                            shutil.copytree(moved, lib, symlinks=True)
                    return real_open(path, flags, *args, **kwargs)

                with patch.object(gate.os, 'open', side_effect=racing_open), \
                     self.assertRaises(ValueError):
                    gate.installed_library(lib / self.util.name, attempt, root)

    def test_ldd_requires_exact_lexical_soname_then_binds_target(self):
        before = self.identities()
        good = f'\tlibavutil.so.60 => {self.util} (0x0000)\n\tlibc.so.6 => /lib/libc.so.6 (0x1000)\n'
        binding = gate.ldd_binding(good, before, self.attempt, self.root)
        self.assertEqual(binding['lexical_path'], str(self.util))
        self.assertEqual(binding['canonical_path'], str(self.target))
        alternate = self.lib / 'same-bytes'
        alternate.symlink_to(self.target.name)
        for output in (good.replace(str(self.util), str(self.target)),
                       good.replace(str(self.util), str(alternate)),
                       good.replace(str(self.util), '/lib/libavutil.so.60'),
                       good + good.splitlines()[0] + '\n',
                       'libavutil.so.60 => not found\n', good + 'another => not found\n',
                       good.replace('libavutil.so.60 =>', 'libavutil.so.60')):
            with self.subTest(output=output), self.assertRaises(ValueError):
                gate.ldd_binding(output, before, self.attempt, self.root)
        self.util.unlink()
        self.util.symlink_to(self.target.name)
        with self.assertRaises(ValueError):
            gate.ldd_binding(good, before, self.attempt, self.root)

    def test_numeric_canonical_input_keys_match_real_resolved_paths(self):
        installed = self.identities()
        obj = self.root / 'shipping.o'
        obj.write_bytes(b'object')
        paths = gate.numeric_input_paths([obj], installed)
        self.assertEqual(paths, [obj, self.target])
        before = gate.snapshot(paths)
        # 原 runner 的输入键使用 resolve；这里实际解析真实 symlink 与普通文件。
        actual = {str(path.resolve()): gate.digest(path.resolve()) for path in paths}
        gate.validate_numeric_inputs({'inputs': actual}, paths, before)
        for wrong in ({str(obj): actual[str(obj)], str(self.util): actual[str(self.target)]},
                      {str(obj): actual[str(obj)], str(self.lib / 'libavutil.so.60.99.102'): actual[str(self.target)]},
                      dict(actual, **{str(self.target): '0' * 64})):
            with self.subTest(wrong=wrong), self.assertRaises(ValueError):
                gate.validate_numeric_inputs({'inputs': wrong}, paths, before)


class QualificationPhaseContracts(unittest.TestCase):
    def run_contract(self, work, mutation_phase=None, failure_phase=None,
                     config_text='CC=gcc\n', config_hash_mismatch=False,
                     numeric_compiler='gcc', claimed_compiler='gcc (contract_double) 13.3.0\n',
                     config_mutation_phase=None, numeric_mutation=None):
        """外部构建/目标运行的合同替身；真实文件身份与 qualify 控制流，不是 native 验证。"""
        root = Path(work).resolve() / 'fresh'
        archive = Path(work).resolve() / 'archive'
        archive.write_bytes(b'archive')
        attempt = root / 'build.TEST'
        src = attempt / 'source/ffmpeg-8.1.2'
        lib = attempt / 'dist/usr/lib'
        util = lib / 'libavutil.so.60'
        util_target = lib / 'libavutil.so.60.26.102'
        codec_target = lib / 'libavcodec.so.62.28.102'
        kernel = {'fixture': 'contract-only'}
        config = {'args': [], 'compiler': 'gcc (contract_double) 13.3.0'}
        adapter = SimpleNamespace(FIXED_ARGS=[], HELPERS=[], verify_bundle=lambda *args: None,
                                  kernel_identity=lambda path: kernel)
        source_api = SimpleNamespace(recipes=SimpleNamespace(ARCHIVE_SHA=gate.digest(archive),
            trusted=lambda: {'kernel': kernel, 'files': {}, 'unchanged_oracle': {}}))
        benchmark = ('benchmark C: warmup=65536 measured_calls=8126464 batches=31 ns/call '
                     'median=6.039 p95=60.306 max=70.000 checksum=6170d61369b74c40\n'
                     'benchmark NEON: warmup=65536 measured_calls=8126464 batches=31 ns/call '
                     'median=4.761 p95=65.165 max=75.000 checksum=6170d61369b74c40\n')

        def run(evidence, label, argv, **kwargs):
            evidence.record['commands'].append({'label': label, 'argv': [str(p) for p in argv],
                                                'contract_double': True})
            if label == 'builder':
                lib.mkdir(parents=True)
                util_target.write_bytes(b'util')
                codec_target.write_bytes(b'codec')
                util.symlink_to(util_target.name)
                (lib / 'libavcodec.so.62').symlink_to(codec_target.name)
                for name in gate.OBJECTS + ('libavcodec/libavcodec.so.62', 'config.h', 'ffbuild/config.mak'):
                    path = src / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(b'source fixture')
                (src / 'ffbuild/config.mak').write_text(config_text)
                (attempt / 'configuration.json').write_text(json.dumps(config))
                (attempt / 'kernel.json').write_text(json.dumps(kernel))
                bundle = root / 'bundle/linux-aarch64'
                bundle.mkdir(parents=True)
                outputs = {}
                for name, target in [('libavutil.so.60', util_target), ('libavcodec.so.62', codec_target)]:
                    shutil.copyfile(target, bundle / name)
                    outputs[name] = gate.digest(target)
                record = {'configuration': config, 'kernel': kernel, 'source': {}, 'source_asset': {},
                          'outputs': outputs, 'inputs': {name: gate.digest(src / name)
                            for name in ('config.h', 'ffbuild/config.mak')}}
                record['inputs']['unstripped_libavcodec'] = gate.digest(src / 'libavcodec/libavcodec.so.62')
                if config_hash_mismatch:
                    record['inputs']['ffbuild/config.mak'] = '0' * 64
                (bundle / 'FFmpeg-PROVENANCE.json').write_text(json.dumps(record))
                output = (f'Linux FFmpeg retained configured attempt: {attempt}\n'
                          f'Linux FFmpeg retained shipping source: {src}\n')
            elif label == 'prepare-upstream':
                original = root / 'upstream/ffmpeg-8.1.2'
                original.mkdir(parents=True)
                output = str(original)
            elif label == 'numeric':
                out = root / 'runtime-evidence/numeric'
                out.mkdir()
                for name in ('h264-luma-dc-parity', 'baseline-init.o', 'parity.o', 'abi.o'):
                    (out / name).write_bytes(b'contract executable, not native')
                paths = [Path(argv[i + 1]) for i, value in enumerate(argv) if value == '--object']
                inputs = {str(path.resolve()): gate.digest(path.resolve()) for path in paths}
                common = [numeric_compiler, '-O3', '-std=c11', '-D_GNU_SOURCE', '-I', str(src)]
                fixtures = ROOT / 'third_party/ffmpeg/8.1.2/tests'
                executable = out / 'h264-luma-dc-parity'
                commands = [
                    [numeric_compiler, '--version'],
                    common + ['-Dff_h264dsp_init_aarch64=ff_h264dsp_init_aarch64_baseline', '-c',
                              str(root / 'upstream/ffmpeg-8.1.2/libavcodec/aarch64/h264dsp_init_aarch64.c'),
                              '-o', str(out / 'baseline-init.o')],
                    common + ['-Wall', '-Wextra', '-Werror', '-c', str(fixtures / 'h264-luma-dc-parity.c'),
                              '-o', str(out / 'parity.o')],
                    [numeric_compiler, '-I', str(src), '-c', str(fixtures / 'h264-luma-dc-abi.S'),
                     '-o', str(out / 'abi.o')],
                    [numeric_compiler, str(out / 'parity.o'), str(out / 'abi.o'), str(out / 'baseline-init.o')]
                    + [str(p) for p in paths] + ['-lm', '-pthread', '-Wl,-rpath,' + str(lib), '-o', str(executable)],
                    [str(executable)],
                ]
                command_records = [{'argv': command, 'cwd': str(src), 'returncode': 0,
                                    'output': claimed_compiler if i == 0 else '', 'contract_double': True}
                                   for i, command in enumerate(commands)]
                result = {'status': 'passed', 'mode': 'parity',
                    'target': 'aarch64-linux-gnu', 'inputs': inputs, 'commands': command_records,
                    'compiler': claimed_compiler, 'contract_double': True}
                if numeric_mutation:
                    numeric_mutation(result)
                (out / 'results.json').write_text(json.dumps(result))
                output = 'PASS parity cases=5974518\ndispatch: 30 contexts\nguard pages; AArch64 ABI sentinels\n'
            elif label == 'runtime-dependencies':
                output = f'libavutil.so.60 => {util} (0x0000)\n'
            elif label.startswith('benchmark-'):
                output = benchmark
            else:
                output = ''
            (evidence.path / (label + '.log')).write_text(output)
            if label == mutation_phase:
                replacement = lib / 'replacement'
                replacement.write_bytes(codec_target.read_bytes())
                os.replace(replacement, codec_target)
            if label == config_mutation_phase:
                (src / 'ffbuild/config.mak').write_text('CC=clang\n')
            if label == failure_phase:
                # 初始错误必须在 finally 中保留；alias 本体也进入失败记录。
                util.unlink()
                util.write_bytes(b'replaced alias')
                raise ValueError('initial external failure')
            evidence.save()
            return output

        with patch.object(gate, 'module', return_value=adapter), \
             patch.object(gate, 'object_identity'), patch.object(gate.Evidence, 'run', run):
            status = gate.qualify(root, archive, source_api)
        record = json.loads((root / 'runtime-evidence/qualification.json').read_text())
        return status, record, util_target, codec_target

    def test_fixed_gcc_selection_and_version_bind_numeric_command_evidence(self):
        with tempfile.TemporaryDirectory() as work:
            status, record, _, _ = self.run_contract(work)
            self.assertEqual(status, 0)
            numeric = next(c for c in record['commands'] if c['label'] == 'numeric')
            at = numeric['argv'].index('--cc')
            self.assertEqual(numeric['argv'][at + 1], 'gcc')
            self.assertEqual(numeric['argv'].count('--cc'), 1)
            self.assertEqual(record['runtime_environment']['compiler'], 'gcc')
            self.assertEqual(record['runtime_environment']['compiler_version'], 'gcc (contract_double) 13.3.0\n')
            self.assertEqual(record['fixture_compiler']['command'], ['gcc'])
            self.assertTrue(numeric['contract_double'])

    def test_compiler_config_rejects_before_private_preparation_and_numeric(self):
        bad = ['', 'CC=gcc\nCC=gcc\n', 'CC=clang\n', 'CC=aarch64-linux-gnu-gcc\n',
               'CC=gcc -O3\n', 'CC=$(shell touch injected)\n', 'CC=gcc; touch injected\n',
               'CC=gcc\nCC+= -O3\n', 'CC:=gcc\n', 'CC?=gcc\n', 'CC!=gcc\n',
               'export CC=gcc\n', 'override CC=gcc\n', 'CC =gcc\n', 'CC=gcc # comment\n',
               'CC=gcc\n$(eval CC=clang)\n', 'CC=gcc\\\n', 'IGNORED=\\\nCC=gcc\n']
        for text in bad:
            with self.subTest(text=text), tempfile.TemporaryDirectory() as work:
                status, record, _, _ = self.run_contract(work, config_text=text)
                self.assertEqual(status, 1)
                self.assertEqual(record['status'], 'failed')
                self.assertIn('CC=gcc', record['error'])
                self.assertEqual([c['label'] for c in record['commands']], ['builder'])
                self.assertNotIn('numeric_results_sha256', record)

    def test_configuration_hash_gate_is_preserved_before_compiler_selection(self):
        with tempfile.TemporaryDirectory() as work:
            status, record, _, _ = self.run_contract(work, config_hash_mismatch=True)
            self.assertEqual(status, 1)
            self.assertIn('实际配置 hash', record['error'])
            self.assertEqual([c['label'] for c in record['commands']], ['builder'])
            self.assertNotIn('fixture_compiler', record)

    def test_claimed_gcc_cannot_authorize_clang_or_other_numeric_compiler(self):
        for compiler in ('clang', 'aarch64-linux-gnu-gcc', '/usr/bin/gcc', 'gcc -O3'):
            with self.subTest(compiler=compiler), tempfile.TemporaryDirectory() as work:
                status, record, _, _ = self.run_contract(work, numeric_compiler=compiler)
                self.assertEqual(status, 1)
                self.assertIn('gcc', record['error'])
                self.assertEqual([c['label'] for c in record['commands']], ['builder', 'prepare-upstream', 'numeric'])
                self.assertEqual(record['benchmarks'], [])

    def test_numeric_version_must_match_configured_compiler(self):
        for claimed in ('', 'Ubuntu clang version 18.1.3\n', 'gcc (other) 12.0.0\n', True):
            with self.subTest(claimed=claimed), tempfile.TemporaryDirectory() as work:
                status, record, _, _ = self.run_contract(work, claimed_compiler=claimed)
                self.assertEqual(status, 1)
                self.assertIn('compiler', record['error'])
                self.assertEqual(record['benchmarks'], [])

    def test_numeric_command_trace_must_cover_real_compile_link_and_execution(self):
        def mutate(result, kind):
            if kind == 'missing':
                result['commands'].pop(3)
            elif kind == 'extra':
                result['commands'].append(dict(result['commands'][0]))
            elif kind == 'clang-abi':
                result['commands'][3]['argv'][0] = 'clang'
            elif kind == 'clang-link':
                result['commands'][4]['argv'][0] = 'clang'
            elif kind == 'flags':
                result['commands'][3]['argv'].insert(1, '-fno-integrated-as')
            elif kind == 'wrong-version-output':
                result['commands'][0]['output'] = 'clang version 18\n'
            elif kind == 'wrong-cwd':
                result['commands'][1]['cwd'] = '/unrelated'
            elif kind == 'bool':
                result['commands'][3]['returncode'] = False
            else:
                result['commands'][3]['returncode'] = 1
        for kind in ('missing', 'extra', 'clang-abi', 'clang-link', 'flags', 'wrong-version-output',
                     'wrong-cwd', 'bool', 'failure'):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as work:
                status, record, _, _ = self.run_contract(work, numeric_mutation=lambda r: mutate(r, kind))
                self.assertEqual(status, 1)
                self.assertEqual(record['status'], 'failed')
                self.assertEqual(record['benchmarks'], [])
                self.assertNotIn('runtime_avutil_binding', record)

    def test_config_stays_bound_at_numeric_three_windows_and_final(self):
        for mutate, reject in [('prepare-upstream', 'before-numeric'), ('quiet-1', 'before-benchmark-1'),
                               ('quiet-2', 'before-benchmark-2'), ('quiet-3', 'before-benchmark-3'),
                               ('benchmark-3', 'final')]:
            with self.subTest(mutate=mutate), tempfile.TemporaryDirectory() as work:
                status, record, _, _ = self.run_contract(work, config_mutation_phase=mutate)
                self.assertEqual(status, 1)
                checks = {row['label']: row for row in record['identity_checks']}
                self.assertEqual(checks[reject]['status'], 'failed')
                config_path = record['source'] + '/ffbuild/config.mak'
                self.assertNotEqual(record['inputs_before'][config_path],
                                    record['inputs_after_failure'][config_path]['sha256'])

    def test_phase_checks_cover_numeric_ldd_three_windows_and_final(self):
        with tempfile.TemporaryDirectory() as work:
            status, record, util, codec = self.run_contract(work)
            self.assertEqual(status, 0)  # 仅控制流替身状态，record 明确标注 contract_double。
            self.assertEqual([row['label'] for row in record['identity_checks']],
                             ['before-numeric', 'before-ldd', 'after-ldd',
                              'before-benchmark-1', 'before-benchmark-2', 'before-benchmark-3', 'final'])
            self.assertTrue(all(row['status'] == 'passed' for row in record['identity_checks']))
            self.assertIn(str(codec), record['inputs_before'])
            self.assertEqual(record['installed_libraries_before'], record['installed_libraries_after'])
            numeric = next(c for c in record['commands'] if c['label'] == 'numeric')
            inputs = [numeric['argv'][i + 1] for i, v in enumerate(numeric['argv']) if v == '--object']
            self.assertEqual(inputs[-1], str(util))
            self.assertEqual(len(inputs), 7)

    def test_codec_only_replacement_fails_each_window_and_final_without_benchmark_retry(self):
        phases = [('prepare-upstream', 'before-numeric'), ('numeric', 'before-ldd'),
                  ('runtime-dependencies', 'after-ldd'), ('quiet-1', 'before-benchmark-1'),
                  ('quiet-2', 'before-benchmark-2'), ('quiet-3', 'before-benchmark-3'),
                  ('benchmark-1', 'before-benchmark-2'), ('benchmark-2', 'before-benchmark-3'),
                  ('benchmark-3', 'final')]
        for mutate, reject in phases:
            with self.subTest(mutate=mutate), tempfile.TemporaryDirectory() as work:
                status, record, _, _ = self.run_contract(work, mutation_phase=mutate)
                self.assertEqual(status, 1)
                self.assertEqual(record['status'], 'failed')
                checks = {row['label']: row for row in record['identity_checks']}
                self.assertEqual(checks[reject]['status'], 'failed')
                self.assertTrue(record['installed_libraries_after_failure']['libavcodec.so.62']['changed'])
                self.assertEqual(sum(c['label'] == 'builder' for c in record['commands']), 1)
                for number in (1, 2, 3):
                    self.assertLessEqual(sum(c['label'] == f'benchmark-{number}' for c in record['commands']), 1)

    def test_finally_preserves_initial_error_alias_stat_and_original_target_hash(self):
        with tempfile.TemporaryDirectory() as work:
            status, record, _, _ = self.run_contract(work, failure_phase='numeric')
            self.assertEqual(status, 1)
            self.assertEqual(record['error'], 'initial external failure')
            self.assertEqual(record['status'], 'failed')
            before = record['installed_libraries_before']['libavutil.so.60']
            self.assertEqual(before['alias']['readlink'], 'libavutil.so.60.26.102')
            self.assertEqual(len(before['target']['sha256']), 64)
            after = record['installed_libraries_after_failure']['libavutil.so.60']
            self.assertTrue(after['changed'])
            self.assertIn('stat', after['alias'])
            self.assertIn('inputs_after_failure', record)

    def test_builder_failure_is_single_attempt_and_terminal_without_identity_baseline(self):
        with tempfile.TemporaryDirectory() as work:
            status, record, _, _ = self.run_contract(work, failure_phase='builder')
            self.assertEqual(status, 1)
            self.assertEqual(record['error'], 'initial external failure')
            self.assertEqual([c['label'] for c in record['commands']], ['builder'])
            self.assertNotIn('installed_libraries_before', record)


if __name__ == '__main__':
    unittest.main()
