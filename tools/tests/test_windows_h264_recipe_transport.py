"""RELEASE A 外层运输/checkout 控制；无构建、网络、PE或设备。"""
import copy
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]


def load():
    spec = importlib.util.spec_from_file_location('release_a_transport', ROOT / 'tools/windows-h264-recipe-transport.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def digest(data):
    return hashlib.sha256(data).hexdigest()


class TransportBoundary(unittest.TestCase):
    def setUp(self):
        self.t = load()
        parent = os.environ.get('FRD_WINDOWS_EXECUTION_TEST_ARTIFACTS')
        if parent:
            Path(parent).mkdir(parents=True, exist_ok=True)
        self.work = Path(tempfile.mkdtemp(prefix='transport-', dir=parent))
        self.root = self.work / 'payload'
        self.expected = {name: digest(('fixture:' + name).encode()) for name in self.t.CHECKOUT}
        self.t.initialize(self.root, 'a' * 40, '1234', '1')
        state = self.t.read_json(self.root / 'provenance/state.json')
        state.update(stage='builder', checkout_ready=True, setup_ready=True, builder_started=True,
                     builder_exit=7, quiescent=True, attempt='.attempt-123-nonce',
                     build_origin='/mnt/d/work/repo/.codex-target/ffmpeg-8.1.2')
        self.t.note_error(state, 'builder', 'fixture exit 7; not a shipping build')
        self.t.write_json(self.root / 'provenance/state.json', state)
        for name in self.t.CHECKOUT:
            path = self.root / 'checkout-inputs' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(('fixture:' + name).encode())
        (self.root / 'checkout-inputs/attributes.bin').write_bytes(b'* -text\n')
        self.t.write_json(self.root / 'checkout-inputs/blob-identities.json', self.expected)
        (self.root / 'checkout-inputs/check-attr.bin').write_bytes(b'fixture attrs\0')
        self.t.write_json(self.root / 'checkout-inputs/checkout-commands.json', [])
        (self.root / 'checkout-inputs/git-executable.bin').write_bytes(b'inert checkout Git fixture')
        self.t.write_json(self.root / 'provenance/checkout.json', {'source': 'a' * 40, 'tree': 'b' * 40})
        self.t.write_json(self.root / 'provenance/paths.json', {'windows': 'D:\\work\\repo', 'wsl': '/mnt/d/work/repo'})
        for name in ('prepare.sh', 'collect.sh', 'os-release', 'apt-packages.txt', 'tools.json', 'host.json', 'control-results.json'):
            (self.root / 'setup' / name).write_bytes(b'fixture setup bytes\0\xff')
        install = self.root / 'toolchain/install/bin'
        install.mkdir(parents=True)
        (install / 'clang').write_bytes(b'fixture executable, never executed')
        (install / 'clang').chmod(0o755)
        for name in self.t.TOOL_LINKS:
            (install / name).symlink_to('/opt/frd-llvm-mingw-20250709/bin/clang')
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode='w:xz', format=tarfile.PAX_FORMAT) as package:
            package.add(self.root / 'toolchain/install', arcname='fixture-toolchain', recursive=True)
        self.archive = buffer.getvalue()
        (self.root / 'toolchain/archive.tar.xz').write_bytes(self.archive)
        system = self.root / 'toolchain/system/usr/bin'
        system.mkdir(parents=True)
        catalog = {}
        for name in self.t.SYSTEM_TOOLS:
            target = system / name
            target.write_bytes(('fixture system tool:' + name).encode())
            target.chmod(0o755)
            catalog[name] = 'toolchain/system/usr/bin/' + name
        self.t.write_json(self.root / 'toolchain/system-tools.json', catalog)
        windows = self.root / 'toolchain/windows'
        windows.mkdir()
        catalog = {}
        for name in self.t.WINDOWS_TOOLS:
            data = ('inert Windows tool fixture:' + name).encode()
            (windows / (name + '.bin')).write_bytes(data)
            catalog[name] = dict(invoked_name=name, path='C:\\fixture\\' + name, size=len(data),
                before_sha256=digest(data), after_sha256=digest(data), saved_sha256=digest(data), final_sha256=digest(data))
        self.t.write_json(windows / 'catalog.json', catalog)
        self.attempt = self.root / 'build-root/build-side-frd-h264dc-aarch64-v1/windows-aarch64/Release/.attempt-123-nonce'
        self.attempt.mkdir(parents=True)
        (self.attempt / '.hidden-partial').write_bytes(b'partial\0\xff')
        (self.attempt / 'source').mkdir()
        (self.attempt / 'source/inside').symlink_to('../.hidden-partial')
        self.t.write_json(self.root / 'setup/lifecycle.json', {'stable': True, 'reason': 'host contract fixture'})
        calls = self.root / 'setup/calls/builder'
        calls.mkdir(parents=True)
        (calls / 'stdout.bin').write_bytes(b'out\0\xff')
        (calls / 'stderr.bin').write_bytes(b'err\0\xfe')
        self.t.write_json(calls / 'exit.json', {'exit': 7})
        self.t.write_json(calls / 'argv.json', {'argv': ['fixture-only'], 'cwd': str(self.work)})
        self.patch = mock.patch.object(self.t, 'LLVM_SHA', digest(self.archive))
        self.patch.start()

    def tearDown(self):
        self.patch.stop()
        if not os.environ.get('FRD_WINDOWS_EXECUTION_TEST_ARTIFACTS'):
            shutil.rmtree(self.work)

    def seal(self, name='evidence'):
        archive, receipt = self.work / (name + '.tar'), self.work / (name + '.json')
        self.t.seal(self.root, archive, receipt, self.expected)
        return archive, receipt

    def verify(self, archive, receipt):
        return self.t.verify(archive, receipt, source='a' * 40, run_id='1234', run_attempt='1',
                             expected_inputs=self.expected)

    def test_full_producer_reader_roundtrip_hidden_symlinks_and_bytes(self):
        archive, receipt = self.seal()
        checked = self.verify(archive, receipt)
        self.assertEqual(checked['builder_exit'], 7)
        self.assertFalse(checked['build_identity_passed'])
        with tarfile.open(archive) as package:
            self.assertEqual(package.extractfile('setup/calls/builder/stdout.bin').read(), b'out\0\xff')
            link = package.getmember('toolchain/install/bin/aarch64-w64-mingw32-clang')
            self.assertTrue(link.issym())
            self.assertEqual(link.linkname, '/opt/frd-llvm-mingw-20250709/bin/clang')
            self.assertIn(str((self.attempt / '.hidden-partial').relative_to(self.root)), package.getnames())

    def test_large_material_paths_are_streamed_and_archive_tree_is_bound(self):
        # 对具体大材料禁用read_bytes，真实seal/reader仍须逐字读取并核对。
        archive_path = self.root / 'toolchain/archive.tar.xz'
        actual_read = Path.read_bytes
        def guarded(path):
            if path == archive_path or path.suffix == '.tar':
                raise AssertionError('整份大archive不得read_bytes进入内存')
            return actual_read(path)
        with mock.patch.object(Path, 'read_bytes', guarded):
            archive, receipt = self.seal('streamed')
            self.verify(archive, receipt)
        # 固定archive仍原样、hash正确，安装后的bytes变化必须单独拒绝。
        (self.root / 'toolchain/install/bin/clang').write_bytes(b'replaced after extraction')
        with self.assertRaisesRegex(ValueError, 'archive.*安装|安装.*archive'):
            self.seal('different-install')

    def test_external_pack_stream_bytes_are_bound_without_tar_self_reference(self):
        import base64
        archive, receipt = self.seal('external-capture')
        original_hash = digest(archive.read_bytes())
        capture = {'argv.json': b'{"argv":["fixture"],"cwd":"/fixture"}\n',
                   'stdout.bin': b'pack-out\0\xff', 'stderr.bin': b'pack-err\0\xfe',
                   'exit.json': b'{"exit":0,"terminated":true,"capture_complete":true,"timed_out":false,"error":null,"argv":["fixture"],"cwd":"/fixture"}\n'}
        value = self.t.read_json(receipt)
        value['packing'] = {name: {'size': len(data), 'sha256': digest(data),
                                  'base64': base64.b64encode(data).decode('ascii')}
                            for name, data in capture.items()}
        self.t.write_json(receipt, value)
        self.verify(archive, receipt)
        self.assertEqual(digest(archive.read_bytes()), original_hash)
        value['packing']['stdout.bin']['base64'] = base64.b64encode(b'forged').decode('ascii')
        self.t.write_json(receipt, value)
        with self.assertRaisesRegex(ValueError, 'pack'):
            self.verify(archive, receipt)

    def test_first_error_is_immutable_and_phase_errors_are_separate(self):
        state = self.t.read_json(self.root / 'provenance/state.json')
        first = copy.deepcopy(state['first_error'])
        self.t.note_error(state, 'transport', 'later archive failure')
        self.t.note_error(state, 'capture', 'later capture failure')
        self.assertEqual(state['first_error'], first)
        self.assertIn('later archive failure', state['errors']['transport'])
        self.assertIn('later capture failure', state['errors']['capture'])

    def test_source_trust_and_required_missing_unknown_members_refuse(self):
        for name, change in (
            ('missing', lambda: (self.root / 'setup/os-release').unlink()),
            ('unknown', lambda: (self.root / 'unknown-top').mkdir()),
            ('windows-tool', lambda: (self.root / 'toolchain/windows/gpg.exe.bin').write_bytes(b'changed')),
            ('wrong-arch', lambda: (self.root / 'build-root/build-side-frd-h264dc-aarch64-v1/windows-x86').mkdir()),
            ('untrusted', lambda: (self.root / 'checkout-inputs/tools/windows-ffmpeg-kernel.py').write_bytes(b'forged')),
            ('escape', lambda: (self.attempt / 'escape').symlink_to('/etc/passwd'))):
            with self.subTest(name=name):
                # 每个拒绝用独立完整 fixture，避免错误叠加掩盖首个实际边界。
                saved = self.work / ('snapshot-' + name)
                shutil.copytree(self.root, saved, symlinks=True)
                change()
                with self.assertRaises((ValueError, FileNotFoundError)):
                    self.seal(name)
                shutil.rmtree(self.root)
                saved.rename(self.root)

    def test_mutating_payload_and_unproven_quiescence_refuse_seal(self):
        with self.assertRaisesRegex(ValueError, '变化'):
            self.t.seal(self.root, self.work / 'changing.tar', self.work / 'changing.json', self.expected,
                        after_scan=lambda: (self.attempt / '.hidden-partial').write_bytes(b'changed'))
        state = self.t.read_json(self.root / 'provenance/state.json')
        state['quiescent'] = False
        self.t.write_json(self.root / 'provenance/state.json', state)
        with self.assertRaisesRegex(ValueError, '停止|稳定'):
            self.seal('unproven')

    def test_tar_missing_extra_duplicate_absolute_escape_tamper_and_receipt_identity(self):
        archive, receipt = self.seal()
        with tarfile.open(archive) as package:
            members = [(copy.copy(item), package.extractfile(item).read() if item.isfile() else None)
                       for item in package.getmembers()]
        regular_index = next(i for i, (item, _) in enumerate(members) if item.name == 'setup/os-release')
        mutants = {}
        mutants['missing'] = [pair for pair in members if pair[0].name != 'setup/os-release']
        mutants['duplicate'] = members + [members[regular_index]]
        for label, path in [('unknown', 'other/top'), ('absolute', '/etc/escape'), ('escape', '../escape')]:
            item = tarfile.TarInfo(path)
            item.size = 1
            mutants[label] = members + [(item, b'x')]
        changed = list(members)
        item, data = changed[regular_index]
        item = copy.copy(item)
        changed[regular_index] = (item, b'X' * len(data))
        mutants['tamper'] = changed
        for label, items in mutants.items():
            output = self.work / (label + '.tar')
            with tarfile.open(output, 'w') as package:
                for item, data in items:
                    package.addfile(item, io.BytesIO(data) if data is not None else None)
            value = self.t.read_json(receipt)
            value['tar_sha256'] = digest(output.read_bytes())
            value['tar_size'] = output.stat().st_size
            rewritten = self.work / (label + '.json')
            self.t.write_json(rewritten, value)
            with self.subTest(label=label), self.assertRaises(ValueError):
                self.verify(output, rewritten)
        trailing = self.work / 'trailing.tar'
        trailing.write_bytes(archive.read_bytes() + b'unlisted-after-tar-end')
        value = self.t.read_json(receipt)
        value['tar_sha256'] = digest(trailing.read_bytes())
        value['tar_size'] = trailing.stat().st_size
        rewritten = self.work / 'trailing.json'
        self.t.write_json(rewritten, value)
        with self.assertRaisesRegex(ValueError, 'tar.*尾|尾.*tar'):
            self.verify(trailing, rewritten)
        with self.assertRaises(ValueError):
            self.t.verify(archive, receipt, source='c' * 40, run_id='1234', run_attempt='1',
                          expected_inputs=self.expected)


class InitializedCheckoutBoundary(unittest.TestCase):
    def setUp(self):
        self.t = load()
        parent = os.environ.get('FRD_WINDOWS_EXECUTION_TEST_ARTIFACTS')
        if parent:
            Path(parent).mkdir(parents=True, exist_ok=True)
        self.work = Path(tempfile.mkdtemp(prefix='initialized-checkout-', dir=parent))
        self.repo = self.work / 'repo-must-remain-absent'

    def tearDown(self):
        if not os.environ.get('FRD_WINDOWS_EXECUTION_TEST_ARTIFACTS'):
            shutil.rmtree(self.work)

    def assert_empty_output_reaches_only_discovery(self, output):
        with mock.patch.object(self.t.shutil, 'which', side_effect=RuntimeError('stop-before-Git-discovery')) as discovery, \
                mock.patch.object(self.t.subprocess, 'Popen', side_effect=AssertionError('forbidden Git spawn')) as spawn:
            with self.assertRaisesRegex(RuntimeError, 'stop-before-Git-discovery'):
                self.t.canonical_checkout(self.repo, 'a' * 40, output)
            self.assertEqual(discovery.call_count, 1)
            spawn.assert_not_called()
        self.assertFalse(self.repo.exists())
        self.assertEqual(self.t.read_json(output / 'checkout-commands.json'), [])

    def test_actual_initialize_empty_output_is_accepted_before_git(self):
        root = self.work / 'payload'
        self.t.initialize(root, 'a' * 40, '1234', '1')
        output = root / 'checkout-inputs'
        self.assertTrue(output.is_dir())
        self.assertEqual(list(output.iterdir()), [])
        self.assert_empty_output_reaches_only_discovery(output)

    def test_original_absent_output_still_creates_empty_directory_before_git(self):
        self.assert_empty_output_reaches_only_discovery(self.work / 'new-parent/inputs')

    def test_nonempty_file_and_symlink_outputs_refuse_before_git(self):
        nonempty = self.work / 'nonempty'
        nonempty.mkdir()
        marker = nonempty / 'original.bin'
        marker.write_bytes(b'original\0\xff')
        regular = self.work / 'file'
        regular.write_bytes(b'file\0\xfe')
        target = self.work / 'target'
        target.mkdir()
        link = self.work / 'link'
        link.symlink_to(target, target_is_directory=True)
        broken = self.work / 'broken'
        broken.symlink_to(self.work / 'missing-target', target_is_directory=True)
        for output in (nonempty, regular, link, broken):
            with self.subTest(output=output.name), \
                    mock.patch.object(self.t.shutil, 'which', side_effect=AssertionError('forbidden Git discovery')) as discovery, \
                    mock.patch.object(self.t.subprocess, 'Popen', side_effect=AssertionError('forbidden Git spawn')) as spawn:
                with self.assertRaises((ValueError, FileExistsError)):
                    self.t.canonical_checkout(self.repo, 'a' * 40, output)
                discovery.assert_not_called()
                spawn.assert_not_called()
        self.assertEqual(marker.read_bytes(), b'original\0\xff')
        self.assertEqual(regular.read_bytes(), b'file\0\xfe')
        self.assertEqual(list(target.iterdir()), [])
        self.assertTrue(link.is_symlink())
        self.assertTrue(broken.is_symlink())
        self.assertFalse(self.repo.exists())


class CheckoutBoundary(unittest.TestCase):
    def test_isolated_git_crlf_and_attributes_produce_exact_commit_bytes(self):
        import time
        t = load()
        parent = os.environ.get('FRD_WINDOWS_EXECUTION_TEST_ARTIFACTS')
        if parent:
            Path(parent).mkdir(parents=True, exist_ok=True)
        work = Path(tempfile.mkdtemp(prefix='checkout-', dir=parent))
        try:
            repo, output = work / 'repo', work / 'payload/checkout-inputs'
            repo.mkdir()
            environment = dict(os.environ, GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull)
            def git(*args):
                argv = ['git', '-c', 'core.hooksPath=/dev/null', '-c', 'commit.gpgsign=false', *args]
                result = subprocess.run(argv, cwd=repo, env=environment, capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                return result.stdout
            git('init', '-b', 'fixture')
            git('config', 'user.name', 'Isolated Fixture')
            git('config', 'user.email', 'fixture@example.invalid')
            for name in t.CHECKOUT:
                path = repo / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b'line one\nline two\n')
            (repo / '.gitattributes').write_bytes(b'* text=auto\n*.ps1 eol=crlf\n')
            unlisted = repo / 'unlisted.ps1'
            unlisted.write_bytes(b'unlisted fixture\n')
            git('add', '--all')
            git('commit', '-m', 'fixture')
            source = git('rev-parse', 'HEAD').decode().strip()
            t.initialize(output.parent, source, '1234', '1')
            # 模拟初次porcelain checkout记录实际CRLF的index stat，避免旧fixture的stale cache。
            for name in (*t.CHECKOUT, 'unlisted.ps1'):
                (repo / name).unlink()
            git('checkout', '--force', 'HEAD', '--', '.')
            self.assertIn(b'\r\n', (repo / 'tools/build-ffmpeg-windows.ps1').read_bytes())
            time.sleep(1.1)
            git('update-index', '--refresh')
            cached = (repo / 'tools/build-ffmpeg-windows.ps1').stat()
            index = (repo / '.git/index').stat()
            self.assertGreater(index.st_mtime_ns // 1000000000, cached.st_mtime_ns // 1000000000)
            (work / 'cached-builder-before.bin').write_bytes((repo / 'tools/build-ffmpeg-windows.ps1').read_bytes())
            (work / 'cached-index-before.bin').write_bytes((repo / '.git/index').read_bytes())
            untouched = (unlisted.read_bytes(), unlisted.stat().st_ino, unlisted.stat().st_mtime_ns)
            # 仅对该临时仓库运行新函数；实际项目Git从未调用。
            with mock.patch.dict(os.environ, environment, clear=True):
                expected = t.canonical_checkout(repo, source, output)
            self.assertNotIn(b'\r', (repo / 'tools/build-ffmpeg-windows.ps1').read_bytes())
            for name in t.CHECKOUT:
                blob = git('cat-file', 'blob', source + ':' + name)
                self.assertEqual((repo / name).read_bytes(), blob)
                self.assertEqual(expected[name], digest(blob))
            self.assertEqual((repo / '.git/info/attributes').read_bytes(), b'* -text\n')
            self.assertEqual((unlisted.read_bytes(), unlisted.stat().st_ino, unlisted.stat().st_mtime_ns), untouched)
            commands = t.read_json(output / 'checkout-commands.json')
            saved_git = digest((output / 'git-executable.bin').read_bytes())
            for call in commands:
                self.assertEqual(call['executable']['before_sha256'], saved_git)
                self.assertEqual(call['executable']['after_sha256'], saved_git)
                self.assertIn('observed', call['main_module'])

            # 后部固定路径无效时，必须在任何前部文件evict之前拒绝；未列路径也不能被删。
            late = repo / t.CHECKOUT[-1]
            late.unlink(); late.mkdir()
            for kind in ('directory', 'symlink'):
                if kind == 'symlink':
                    late.rmdir(); late.symlink_to(unlisted)
                with mock.patch.dict(os.environ, environment, clear=True), mock.patch.object(Path, 'unlink') as evict:
                    with self.assertRaisesRegex(ValueError, '固定源必须是普通文件且非symlink'):
                        t.canonical_checkout(repo, source, work / ('invalid-' + kind))
                    evict.assert_not_called()
                self.assertEqual((unlisted.read_bytes(), unlisted.stat().st_ino, unlisted.stat().st_mtime_ns), untouched)

        finally:
            if not parent:
                shutil.rmtree(work)


class WorkflowAuthority(unittest.TestCase):
    def test_real_workflow_has_one_fixed_natural_authority_and_exact_failure_forensics(self):
        import yaml
        # BaseLoader保留on字段；单一固定自然入口，无任意mode/ref/tool/flags/retry。
        workflow = yaml.load((ROOT / '.github/workflows/qualify-windows-h264-recipe.yml').read_text(),
                             Loader=yaml.BaseLoader)
        self.assertEqual(set(workflow['on']), {'push', 'workflow_dispatch'})
        self.assertEqual(workflow['on']['push']['branches'], ['codex/rdp-egfx-hevc-preference'])
        self.assertEqual(workflow['on']['push']['paths'], ['.github/workflows/qualify-windows-h264-recipe.yml'])
        inputs = workflow['on']['workflow_dispatch']['inputs']
        self.assertEqual(set(inputs), {'execute_release_a'})
        self.assertEqual((inputs['execute_release_a']['type'], inputs['execute_release_a']['required'],
                          inputs['execute_release_a']['default']), ('boolean', 'true', 'false'))
        self.assertEqual(workflow['permissions'], {'contents': 'read'})
        self.assertEqual(workflow['concurrency']['cancel-in-progress'], 'false')
        self.assertEqual(set(workflow['jobs']), {'release-a'})
        job = workflow['jobs']['release-a']
        self.assertEqual((job['runs-on'], job['timeout-minutes']), ('windows-2025', '90'))
        self.assertEqual(job['if'], "(github.event_name == 'workflow_dispatch' && inputs.execute_release_a == true) || "
                                   "(github.event_name == 'push' && github.ref == 'refs/heads/codex/rdp-egfx-hevc-preference')")
        self.assertEqual(set(job['env']), {'RELEASE_A_EXECUTE', 'RELEASE_A_WORKFLOW_SHA', 'RELEASE_A_WORKFLOW_REF'})
        checkout, execute, collect, upload, forensic = job['steps']
        self.assertEqual(checkout['uses'], 'actions/checkout@11d5960a326750d5838078e36cf38b85af677262')
        self.assertEqual(checkout['with'], {'ref': '${{ github.sha }}', 'persist-credentials': 'false'})
        self.assertEqual(execute['run'], 'pwsh -NoProfile -File tools/qualify-windows-h264-recipe-A.ps1 -Action Execute')
        self.assertNotIn('${{', execute['run'])
        active_commands = [step['run'] for step in job['steps'] if 'run' in step and step.get('if') != '${{ false }}']
        self.assertEqual(active_commands, [execute['run'], collect['run']])
        self.assertEqual(collect['if'], 'always()')
        self.assertEqual(collect['run'], 'pwsh -NoProfile -File tools/qualify-windows-h264-recipe-A.ps1 -Action Collect')
        self.assertEqual(upload['if'], 'always()')
        self.assertEqual(upload['with']['compression-level'], '0')
        self.assertEqual(upload['with']['if-no-files-found'], 'error')
        self.assertEqual(upload['with']['path'].splitlines(), ['${{ runner.temp }}/release-a.tar',
                                                             '${{ runner.temp }}/release-a.receipt.json',
                                                             '${{ runner.temp }}/release-a.verification.json'])
        self.assertEqual(forensic['if'], '${{ failure() || cancelled() }}')
        self.assertEqual(forensic['continue-on-error'], 'true')
        self.assertEqual(forensic['with']['include-hidden-files'], 'true')
        self.assertEqual(forensic['uses'], 'actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02')
        self.assertEqual(forensic['with']['name'], 'release-a-bootstrap-checkout-diagnostic-${{ github.run_id }}-${{ github.run_attempt }}-${{ github.sha }}')
        self.assertEqual((forensic['with']['compression-level'], forensic['with']['if-no-files-found'],
                          forensic['with']['overwrite']), ('0', 'warn', 'false'))
        call_files = ('argv.json', 'stdout.bin', 'stderr.bin', 'exit.json', 'error.json', 'executable.bin', 'main-module.bin')
        expected = ['release-a-bootstrap-error.json'] + ['release-a-initialize/' + name for name in call_files]
        payload = ['provenance/state.json', 'provenance/checkout.json', 'setup/host.json']
        payload += [f'setup/calls/{call}/{name}' for call in ('initialize', 'checkout') for name in call_files]
        payload += ['checkout-inputs/' + name for name in ('checkout-commands.json', 'git-executable.bin',
                    'blob-identities.json', 'attributes-before.bin', 'attributes.bin', 'check-attr.bin')]
        fixed_sources = ('.gitattributes', '.github/workflows/qualify-windows-h264-recipe.yml',
            'tools/qualify-windows-h264-recipe-A.ps1', 'tools/windows-h264-recipe-transport.py',
            'tools/build-ffmpeg-windows.ps1', 'tools/desktop-ffmpeg-source.py', 'tools/ffmpeg-source-recipe.py',
            'tools/windows-ffmpeg-bundle.py', 'tools/windows-ffmpeg-build-capture.py', 'tools/windows-ffmpeg-kernel.py',
            'tools/ffmpeg-build-common.psm1', 'third_party/ffmpeg/8.1.2/h264-luma-dc-aarch64-recipe.json',
            'third_party/ffmpeg/8.1.2/patches/0001-aarch64-h264-luma-dc-neon.patch',
            'third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1', 'tools/tests/test_windows_h264_recipe_transport.py',
            'tools/tests/windows-h264-recipe-ci.Tests.ps1')
        payload += ['checkout-inputs/' + name for name in fixed_sources]
        payload += ['toolchain/windows/catalog.json']
        payload += ['toolchain/windows/' + name + '.bin' for name in ('pwsh', 'python', 'wsl.exe', 'git', 'gpg.exe', 'curl.exe')]
        expected += ['frd-release-a-evidence/' + name for name in payload]
        actual = forensic['with']['path'].splitlines()
        self.assertEqual(actual[:54], ['${{ runner.temp }}/' + name for name in expected])
        t = load()
        self.assertEqual(set(call_files), t.CALL_FILES)
        # linux-status和payload pack是保留项；实际pack/final-reader均在payload外固定目录。
        calls = [name for name in t.CALLS if name not in ('initialize', 'checkout', 'linux-status', 'pack')]
        self.assertEqual(len(calls), 16)
        added = [f'frd-release-a-evidence/setup/calls/{call}/{name}' for call in calls for name in call_files]
        added += [folder + '/' + name for folder in ('release-a-pack', 'release-a-final-verify') for name in call_files]
        added += ['frd-release-a-evidence/provenance/' + name for name in sorted(t.PROVENANCE - {'state.json', 'checkout.json'})]
        added += ['frd-release-a-evidence/setup/' + name for name in sorted(t.SETUP_FILES - {'host.json', 'lifecycle-error.json', 'transport-error.json'})]
        added += [f'frd-release-a-evidence/setup/groups/{kind}/{name}' for kind in ('setup', 'collect', 'control-normal', 'control-halt')
                  for name in ('scope.json', 'halt.json', 'stopped.json')]
        self.assertEqual(len(added), 154)
        expected += added
        self.assertEqual(actual[:208], ['${{ runner.temp }}/' + name for name in expected])
        expected += ['release-a-failed-wsl/' + name for name in (
            'selection.json','wsl.json','native.argv.json','native.exit.json','native.stdout.bin','native.stderr.bin',
            'native.executable.bin','supervisor.py.bin','lifecycle.intent.json','lifecycle.scope.json','lifecycle.halt.json',
            'lifecycle.stopped.json','call.pending.json','call.start.json','call.end.json','call.observations.json')]
        expected += ['release-a-failed-wsl-export.py']
        expected += ['release-a-failed-wsl-export-call/' + name for name in call_files]
        expected += ['release-a-collect-phases.json']
        self.assertEqual(actual, ['${{ runner.temp }}/' + name for name in expected])
        self.assertEqual(len(actual), 233)
        self.assertEqual(len(actual), len(set(actual)))
        self.assertTrue(all(not any(char in path for char in '*?[]') for path in actual))

    def test_static_prefix_gate_reuses_execute_after_fresh_check_before_first_wsl(self):
        import re
        # 源结构控制，不冒充实际PowerShell AST或Windows运行时执行。
        source = (ROOT / 'tools/qualify-windows-h264-recipe-A.ps1').read_text()
        execute = source.split('function Invoke-ReleaseAExecute($Context) {', 1)[1].split('function Invoke-ReleaseACollect', 1)[0]
        gate = "if ($Context.ContainsKey('bootstrapCheckoutDiagnostic') -and $Context.bootstrapCheckoutDiagnostic) { return }"
        self.assertEqual(execute.count(gate), 1)
        self.assertLess(execute.index('Save-ReleaseAWindowsTools $Context'), execute.index(gate))
        self.assertLess(execute.index('if (Test-Path -LiteralPath $fresh)'), execute.index(gate))
        self.assertLess(execute.index(gate), execute.index("Invoke-ReleaseARaw -Exe 'wsl.exe'"))
        self.assertIn('finally { Write-ReleaseAJson $statePath $state }', execute)
        cli = source.split('# Dot-source', 1)[1]
        self.assertEqual(cli.count('$context.bootstrapCheckoutDiagnostic = $true'), 1)
        self.assertRegex(cli, re.compile(r"elseif \(\$Action -ceq 'BootstrapCheckoutDiagnostic'\) \{\s*"
                                        r"\$context.bootstrapCheckoutDiagnostic = \$true\s*"
                                        r"Invoke-ReleaseAExecute \$context\s*\}"))


class InlineSupervisorBoundary(unittest.TestCase):
    def setUp(self):
        import re
        import types
        builder = (ROOT / 'tools/build-ffmpeg-windows.ps1').read_text()
        code = re.findall(r"(?s)\$lifecycleSource = @'\n(.*?)\n'@", builder)
        self.assertEqual(len(code), 1)
        self.module = types.ModuleType('inline_release_a_supervisor')
        self.module.__file__ = str(ROOT / 'tools/build-ffmpeg-windows.ps1')
        exec(compile(code[0], '<actual inline supervisor>', 'exec'), self.module.__dict__)
        parent = os.environ.get('FRD_WINDOWS_EXECUTION_TEST_ARTIFACTS')
        self.work = Path(tempfile.mkdtemp(prefix='supervisor-', dir=parent))

    def tearDown(self):
        if not os.environ.get('FRD_WINDOWS_EXECUTION_TEST_ARTIFACTS'):
            shutil.rmtree(self.work)

    def test_actual_inline_parser_preserves_original_inner_argv_and_command(self):
        import sys
        command = 'printf "space $dollar \\backslash"; printf "\\000\\377" >&2'
        captured = []
        with mock.patch.object(self.module, 'run', side_effect=lambda *args: captured.append(args) or 7), \
             mock.patch.object(sys, 'argv', ['inline', '--scope', str(self.work), '--nonce', 'a' * 32,
                 '--call-id', 'b' * 32, 'run', '--', 'bash', '-lc', command]):
            self.assertEqual(self.module.main(), 7)
        self.assertEqual(captured[0][3], ['bash', '-lc', command])

    @unittest.skipUnless(hasattr(os, 'fork') and hasattr(os, 'setsid'), '需要本机POSIX fork/session子层')
    def test_actual_gated_session_records_before_go_and_preserves_large_binary_streams(self):
        import sys
        import time
        script = self.work / 'child.py'
        marker = self.work / 'executed'
        script.write_text('import os,sys\nfrom pathlib import Path\nPath(sys.argv[1]).touch()\n'
                          'for _ in range(262144):\n os.write(1,b"\\x00\\xffA\\n");os.write(2,b"\\x00\\xfeB\\r")\n'
                          'sys.exit(7)\n')
        with (self.work / 'stdout.bin').open('wb') as out, (self.work / 'stderr.bin').open('wb') as err:
            pid, go = self.module.spawn_gated([sys.executable, str(script), str(marker)], self.work,
                                              out.fileno(), err.fileno())
            self.assertFalse(marker.exists())
            self.assertEqual(os.getpgid(pid), pid)
            self.assertEqual(os.getsid(pid), pid)
            os.write(go, b'G')
            os.close(go)
            deadline = time.monotonic() + 15
            while True:
                got, status = os.waitpid(pid, os.WNOHANG)
                if got:
                    break
                if time.monotonic() > deadline:
                    os.killpg(pid, 9)
                    os.waitpid(pid, 0)
                    self.fail('真实gated child超时')
                time.sleep(.01)
        self.assertEqual(os.waitstatus_to_exitcode(status), 7)
        self.assertEqual((self.work / 'stdout.bin').read_bytes(), b'\x00\xffA\n' * 262144)
        self.assertEqual((self.work / 'stderr.bin').read_bytes(), b'\x00\xfeB\r' * 262144)

    @unittest.skipUnless(hasattr(os, 'fork') and hasattr(os, 'setsid'), '需要本机POSIX fork/session子层')
    def test_timeout_control_signals_only_its_unreaped_session(self):
        import signal
        import sys
        import time
        script = self.work / 'waiting.py'
        script.write_text('import os,time\nos.write(1,b"READY\\n")\ntime.sleep(60)\n')
        with (self.work / 'stdout.bin').open('wb') as out:
            pid, go = self.module.spawn_gated([sys.executable, str(script)], self.work, out.fileno())
            os.write(go, b'G'); os.close(go)
            deadline = time.monotonic() + 5
            while (self.work / 'stdout.bin').read_bytes() != b'READY\n':
                if time.monotonic() > deadline:
                    os.killpg(pid, 9); os.waitpid(pid, 0); self.fail('child无READY')
                time.sleep(.01)
            # 该POSIX子层尚未reap leader；真实LinuxWNOWAIT/subreaper证明另列未运行。
            self.assertEqual(os.getpgid(pid), pid)
            self.assertEqual(os.getsid(pid), pid)
            os.killpg(pid, signal.SIGTERM)
            _, status = os.waitpid(pid, 0)
        self.assertEqual(os.waitstatus_to_exitcode(status), -signal.SIGTERM)

    def test_external_stop_cannot_signal_from_a_saved_foreign_supervisor_identity(self):
        # 真实本进程PID与旧/外部helper PID不同；guard必须在任何/proc或signal调用前拒绝。
        start = {'supervisor': {'pid': os.getpid() + 1000000}, 'leader': {'pgid': os.getpid()}}
        with mock.patch.object(os, 'killpg') as signal_group:
            with self.assertRaisesRegex(RuntimeError, '原supervisor'):
                self.module.signal_registered(start, 15)
            signal_group.assert_not_called()

    @unittest.skipUnless(hasattr(os, 'fork') and hasattr(os, 'setsid'), '需要本机POSIX fork/session子层')
    def test_missing_go_closes_child_without_executing_original_command(self):
        import sys
        marker = self.work / 'must-not-execute'
        with (self.work / 'stderr.bin').open('wb') as err:
            pid, go = self.module.spawn_gated([sys.executable, '-c',
                'from pathlib import Path; Path(__import__("sys").argv[1]).touch()', str(marker)],
                self.work, stderr_fd=err.fileno())
            os.close(go)
            _, status = os.waitpid(pid, 0)
        self.assertEqual(os.waitstatus_to_exitcode(status), 126)
        self.assertFalse(marker.exists())
        self.assertIn('缺少登记后的 GO'.encode(), (self.work / 'stderr.bin').read_bytes())

    @unittest.skipIf(os.sys.platform == 'linux', '本拒绝控制仅用于非Linux宿主')
    def test_non_linux_cannot_claim_proc_subreaper_or_stable_scope(self):
        with self.assertRaisesRegex(RuntimeError, '真实 Linux'):
            self.module.run(self.work / 'scope', 'a' * 32, 'b' * 32, ['bash', '-lc', 'true'])
        self.assertFalse((self.work / 'scope').exists())


class FinalVerificationBoundary(unittest.TestCase):
    setUp = TransportBoundary.setUp
    tearDown = TransportBoundary.tearDown
    seal = TransportBoundary.seal
    verify = TransportBoundary.verify

    def complete_pair(self):
        path = 'tools/windows-h264-recipe-transport.py'
        data = (ROOT / path).read_bytes()
        (self.root / 'checkout-inputs' / path).write_bytes(data)
        self.expected[path] = digest(data)
        self.t.write_json(self.root / 'checkout-inputs/blob-identities.json', self.expected)
        self.t.write_json(self.root / 'provenance/paths.json', dict(windows='D:\\work\\repo', wsl='/mnt/d/work/repo',
            evidence_windows='D:\\temp\\evidence', evidence_wsl='/mnt/d/temp/evidence'))
        archive, receipt = self.seal('final-pair')
        value = self.t.read_json(receipt)
        import base64
        raw = {'argv.json': self.t.encoded(dict(argv=['fixture-pack'], cwd='/fixture')),
            'stdout.bin': b'pack\0\xff', 'stderr.bin': b'pack-err\0\xfe',
            'exit.json': self.t.encoded(dict(argv=['fixture-pack'], cwd='/fixture', exit=0, error=None,
                timed_out=False, terminated=True, capture_complete=True))}
        value['packing'] = {name: dict(size=len(data), sha256=digest(data), base64=base64.b64encode(data).decode()) for name,data in raw.items()}
        self.t.write_json(receipt, value)
        return archive, receipt

    def test_pair_reader_rejects_post_append_raw_exit_exe_and_identity_changes(self):
        archive, receipt = self.complete_pair()
        original = receipt.read_bytes()
        checked = self.t.verify(archive, receipt, source='a'*40, run_id='1234', run_attempt='1',
            expected_inputs=self.expected, require_packing=True)
        self.assertEqual(checked['receipt_sha256'], digest(original))
        self.assertEqual(checked['reader_sha256'], self.expected['tools/windows-h264-recipe-transport.py'])
        import base64
        for label in ('packing-none','missing-field','raw','exit','exe'):
            value = self.t.decode_json(original)
            if label == 'packing-none': value['packing'] = None
            elif label == 'missing-field': value['packing'].pop('stderr.bin')
            elif label == 'raw': value['packing']['stdout.bin']['base64'] = base64.b64encode(b'forged').decode()
            elif label == 'exit':
                result = self.t.decode_json(base64.b64decode(value['packing']['exit.json']['base64'])); result['exit']=7
                data=self.t.encoded(result);value['packing']['exit.json']=dict(size=len(data),sha256=digest(data),base64=base64.b64encode(data).decode())
            else:
                result=self.t.decode_json(base64.b64decode(value['packing']['exit.json']['base64']))
                result['executable']=dict(before_sha256='0'*64,after_sha256='0'*64,saved_sha256='0'*64,final_sha256='0'*64)
                data=self.t.encoded(result);value['packing']['exit.json']=dict(size=len(data),sha256=digest(data),base64=base64.b64encode(data).decode())
                value['packing']['executable.bin']=dict(size=1,sha256=digest(b'x'),base64=base64.b64encode(b'x').decode())
            self.t.write_json(receipt,value)
            with self.subTest(label=label),self.assertRaises(ValueError):
                self.t.verify(archive,receipt,source='a'*40,run_id='1234',run_attempt='1',expected_inputs=self.expected,require_packing=True)
        receipt.write_bytes(original)
        for source,run,expected in [('b'*40,'1234',self.expected),('a'*40,'999',self.expected),('a'*40,'1234',dict(self.expected, **{'tools/windows-h264-recipe-transport.py':'0'*64}))]:
            with self.assertRaises(ValueError):
                self.t.verify(archive,receipt,source=source,run_id=run,run_attempt='1',expected_inputs=expected,require_packing=True)

    def test_third_document_binds_full_pair_reader_and_rejects_forged_transcript(self):
        import base64
        archive, receipt = self.complete_pair()
        before = {'tar': self.t.file_identity(archive), 'receipt': self.t.file_identity(receipt)}
        checked = self.t.verify(archive,receipt,source='a'*40,run_id='1234',run_attempt='1',expected_inputs=self.expected,require_packing=True)
        paths = self.t.read_json(self.root / 'provenance/paths.json')
        argv = self.t.final_reader_argv(paths, 'a'*40, '1234', '1')
        raw = {'argv.json':self.t.encoded(dict(argv=argv,cwd=paths['windows'])),
            'stdout.bin':self.t.encoded(checked),'stderr.bin':b'',
            'exit.json':self.t.encoded(dict(argv=argv,cwd=paths['windows'],exit=0,error=None,timed_out=False,
                terminated=True,capture_complete=True))}
        exe=b'inert verification exe fixture'
        result=self.t.decode_json(raw['exit.json'])
        result['executable']=dict(invoked_name='wsl.exe',before_sha256=digest(exe),after_sha256=digest(exe),saved_sha256=digest(exe),final_sha256=digest(exe))
        result['main_module']=dict(observed=False,reason='data fixture; no Windows execution claimed')
        raw['exit.json']=self.t.encoded(result);raw['executable.bin']=exe
        document = dict(schema=self.t.VERIFICATION_SCHEMA,source='a'*40,run_id='1234',run_attempt='1',
            reader_sha256=self.expected['tools/windows-h264-recipe-transport.py'],inputs_before=before,inputs_after=before,producer_error=None,
            files={name:dict(size=len(data),sha256=digest(data),base64=base64.b64encode(data).decode()) for name,data in raw.items()})
        third=self.work/'release-a.verification.json'; self.t.write_json(third,document)
        self.t.verify(archive,receipt,source='a'*40,run_id='1234',run_attempt='1',expected_inputs=self.expected,
            require_packing=True,verification=third)
        for label in ('stream','structured','exit','input','reader','source','run','no-eof','argv','exe','missing-image'):
            changed=copy.deepcopy(document)
            if label=='stream':changed['files']['stdout.bin']['base64']=base64.b64encode(b'forged').decode()
            elif label=='input':changed['inputs_after']=copy.deepcopy(before);changed['inputs_after']['receipt']['sha256']='0'*64
            elif label=='reader':changed['reader_sha256']='0'*64
            elif label=='source':changed['source']='b'*40
            elif label=='run':changed['run_id']='999'
            elif label=='exe':
                data=b'replaced-inert-exe';changed['files']['executable.bin']=dict(size=len(data),sha256=digest(data),base64=base64.b64encode(data).decode())
            elif label=='structured':
                value=self.t.decode_json(base64.b64decode(changed['files']['stdout.bin']['base64']));value['tar_sha256']='0'*64
                data=self.t.encoded(value);changed['files']['stdout.bin']=dict(size=len(data),sha256=digest(data),base64=base64.b64encode(data).decode())
            else:
                name='argv.json' if label=='argv' else 'exit.json'
                value=self.t.decode_json(base64.b64decode(changed['files'][name]['base64']))
                if label=='argv':value['argv'][-1]='wrong-attempt'
                elif label=='exit':value['exit']=7
                elif label=='missing-image':value['main_module']=dict(observed=True,path='C:\\fixture\\missing.exe')
                else:value['capture_complete']=False
                data=self.t.encoded(value);changed['files'][name]=dict(size=len(data),sha256=digest(data),base64=base64.b64encode(data).decode())
            self.t.write_json(third,changed)
            with self.subTest(label=label),self.assertRaises(ValueError):
                self.t.verify(archive,receipt,source='a'*40,run_id='1234',run_attempt='1',expected_inputs=self.expected,
                    require_packing=True,verification=third)
        self.assertEqual(self.t.file_identity(archive),before['tar'])
        self.assertEqual(self.t.file_identity(receipt),before['receipt'])


class FinalReaderCliBoundary(unittest.TestCase):
    def test_actual_pair_cli_requires_completed_packing_and_trio_cli_requires_third(self):
        import base64
        import sys
        t=load()
        parent=os.environ.get('FRD_WINDOWS_EXECUTION_TEST_ARTIFACTS')
        if parent:Path(parent).mkdir(parents=True,exist_ok=True)
        work=Path(tempfile.mkdtemp(prefix='final-reader-cli-',dir=parent))
        try:
            payload=work/'payload';archive=work/'release-a.tar';receipt=work/'release-a.receipt.json'
            t.initialize(payload,'a'*40,'1234','1')
            expected={name:digest((ROOT/name).read_bytes()) for name in t.CHECKOUT}
            expected_file=work/'expected.json';t.write_json(expected_file,expected)
            t.seal(payload,archive,receipt,expected)
            core=t.read_json(receipt)
            raw={'argv.json':t.encoded(dict(argv=['inert-pack-data-fixture'],cwd=str(work))),
                 'stdout.bin':b'pack\0\xff','stderr.bin':b'pack\0\xfe',
                 'exit.json':t.encoded(dict(argv=['inert-pack-data-fixture'],cwd=str(work),exit=0,error=None,
                    terminated=True,capture_complete=True,timed_out=False))}
            core['packing']={name:dict(size=len(data),sha256=digest(data),base64=base64.b64encode(data).decode()) for name,data in raw.items()}
            t.write_json(receipt,core)
            before=(t.file_identity(archive),t.file_identity(receipt))
            common=['--output',str(archive),'--receipt',str(receipt),'--expected-inputs',str(expected_file),
                '--source','a'*40,'--run-id','1234','--run-attempt','1']
            def call(label,action,args):
                argv=[sys.executable,str(ROOT/'tools/windows-h264-recipe-transport.py'),action,*args]
                result=subprocess.run(argv,cwd=work,stdout=subprocess.PIPE,stderr=subprocess.PIPE,
                    env=dict(os.environ,PYTHONDONTWRITEBYTECODE='1'))
                t.write_json(work/(label+'.argv.json'),dict(argv=argv,cwd=str(work),exit=result.returncode))
                (work/(label+'.stdout.bin')).write_bytes(result.stdout);(work/(label+'.stderr.bin')).write_bytes(result.stderr)
                return result
            result=call('actual-complete-pair','verify-pair',common)
            self.assertEqual(result.returncode,0,result.stderr)
            checked=t.decode_json(result.stdout)
            self.assertTrue(checked['packing_complete']);self.assertFalse(checked['verification_complete'])
            self.assertEqual(checked['receipt_sha256'],before[1]['sha256'])
            self.assertEqual(checked['reader_sha256'],expected['tools/windows-h264-recipe-transport.py'])
            self.assertEqual((t.file_identity(archive),t.file_identity(receipt)),before)
            self.assertNotEqual(call('missing-third','verify',common).returncode,0)
            incomplete=copy.deepcopy(core);incomplete['packing']=None
            bad=work/'incomplete.receipt.json';t.write_json(bad,incomplete)
            bad_args=list(common);bad_args[3]=str(bad)
            self.assertNotEqual(call('incomplete-packing','verify-pair',bad_args).returncode,0)
        finally:
            if not parent:shutil.rmtree(work)


class StableOwnershipBoundary(unittest.TestCase):
    tearDown = InlineSupervisorBoundary.tearDown

    def setUp(self):
        parent = os.environ.get('FRD_WINDOWS_EXECUTION_TEST_ARTIFACTS')
        if parent:
            Path(parent).mkdir(parents=True, exist_ok=True)
        InlineSupervisorBoundary.setUp(self)

    def graph(self):
        supervisor = dict(pid=10, ppid=1, pgid=2, sid=2, starttime=100, uid=0, uid_values=[0]*4, state='S')
        leader = dict(pid=20, ppid=10, pgid=20, sid=20, starttime=110, uid=0, uid_values=[0]*4, state='S')
        child = dict(pid=21, ppid=20, pgid=20, sid=20, starttime=111, uid=123, uid_values=[123]*4, state='S')
        grandchild = dict(pid=22, ppid=21, pgid=20, sid=20, starttime=111, uid=456, uid_values=[456]*4, state='S')
        return {10: supervisor, 20: leader, 21: child, 22: grandchild}, leader, supervisor

    def test_real_predicate_accepts_only_owned_uid_differences_and_adoption(self):
        graph, leader, supervisor = self.graph()
        group = self.module.stable_ownership(graph, graph, {20}, {20}, leader, supervisor)
        self.assertEqual({item['uid'] for item in group}, {0, 123, 456})
        changed_uid = copy.deepcopy(graph)
        changed_uid[21].update(uid=789, uid_values=[789]*4)
        self.module.stable_ownership(graph, changed_uid, {20}, {20}, leader, supervisor)
        adopted = copy.deepcopy(graph)
        adopted[21]['ppid'] = 10
        self.module.stable_ownership(adopted, adopted, {20,21}, {20,21}, leader, supervisor)

    def test_real_predicate_rejects_foreign_missing_reused_wrong_session_and_unstable_edges(self):
        graph, leader, supervisor = self.graph()
        mutations = {
            'foreign-parent': lambda g: g[21].update(ppid=1),
            'missing-ancestor': lambda g: g.pop(21),
            'parent-younger': lambda g: g[21].update(starttime=999),
            'wrong-sid': lambda g: g[21].update(sid=999),
            'wrong-pgid': lambda g: g[21].update(pgid=999),
            'leader-uid': lambda g: g[20].update(uid=999),
            'leader-reused': lambda g: g[20].update(starttime=999),
            'supervisor-reused': lambda g: g[10].update(starttime=999),
            'cycle': lambda g: (g[21].update(ppid=22), g[22].update(ppid=21)),
        }
        for label, mutate in mutations.items():
            changed = copy.deepcopy(graph); mutate(changed)
            with self.subTest(label=label), self.assertRaises(RuntimeError):
                self.module.stable_ownership(changed, changed, {20}, {20}, leader, supervisor)
        changed = copy.deepcopy(graph); changed[21]['ppid'] = 10
        with self.assertRaises(RuntimeError):
            self.module.stable_ownership(graph, changed, {20}, {20,21}, leader, supervisor)
        changed = copy.deepcopy(graph); changed.pop(22)
        with self.assertRaises(RuntimeError):
            self.module.stable_ownership(graph, changed, {20}, {20}, leader, supervisor)
        changed = copy.deepcopy(graph); changed[22]['starttime'] += 1
        with self.assertRaises(RuntimeError):
            self.module.stable_ownership(graph, changed, {20}, {20}, leader, supervisor)


class OwnershipEvidenceBoundary(unittest.TestCase):
    setUp = StableOwnershipBoundary.setUp
    tearDown = StableOwnershipBoundary.tearDown
    graph = StableOwnershipBoundary.graph

    def test_original_rejection_retains_both_actual_graphs_and_candidates(self):
        first, leader, supervisor = self.graph()
        second = copy.deepcopy(first)
        first[22].update(pgid=900, sid=900)
        second[21].update(pgid=901)
        originals = copy.deepcopy((first, second))
        with self.assertRaisesRegex(RuntimeError, '发现逃逸SID/PGID后代') as rejected:
            self.module.stable_ownership(first, second, {20}, {20}, leader, supervisor)
        evidence = rejected.exception.ownership_failure
        self.assertEqual(evidence['checking_snapshot'], 'first')
        self.assertEqual(evidence['first'], {'processes': first, 'children': [20], 'outside_pgid_descendants': [22]})
        self.assertEqual(evidence['second'], {'processes': second, 'children': [20], 'outside_pgid_descendants': [21]})
        self.assertEqual((evidence['leader'], evidence['supervisor']), (leader, supervisor))
        self.assertEqual((first, second), originals)
        # 附件不把未验证的第一图升级为稳定证据，也不改变纯predicate。
        self.assertFalse(evidence['ownership_validated'])
        with self.assertRaisesRegex(RuntimeError, '发现逃逸SID/PGID后代'):
            self.module.owned_graph(first, {20}, leader, supervisor)

    def test_evidence_failure_does_not_replace_original_ownership_error(self):
        first, leader, supervisor = self.graph(); first[22]['pgid'] = 900
        with mock.patch.object(self.module, 'ownership_evidence', side_effect=OSError('fixture-evidence-failure')):
            with mock.patch('sys.stderr', new=io.StringIO()) as error:
                with self.assertRaisesRegex(RuntimeError, '^发现逃逸SID/PGID后代$'):
                    self.module.stable_ownership(first, first, {20}, {20}, leader, supervisor)
                self.assertIn('fixture-evidence-failure', error.getvalue())

    def test_second_snapshot_failure_is_labelled_without_claiming_stability(self):
        first, leader, supervisor = self.graph()
        second = copy.deepcopy(first); second[22]['pgid'] = 900
        with self.assertRaisesRegex(RuntimeError, '发现逃逸SID/PGID后代') as rejected:
            self.module.stable_ownership(first, second, {20}, {20}, leader, supervisor)
        self.assertEqual(rejected.exception.ownership_failure['checking_snapshot'], 'second')
        self.assertEqual(rejected.exception.ownership_failure['first']['outside_pgid_descendants'], [])
        self.assertEqual(rejected.exception.ownership_failure['second']['outside_pgid_descendants'], [22])

    def test_typed_post_observation_requires_both_identities_and_never_guesses_exe(self):
        graph, _, _ = self.graph(); expected = graph[22]
        # 这是纯typed控制，不运行Linux、不读取/proc。
        changed = dict(expected, starttime=999)
        with mock.patch.object(self.module, 'identity', return_value=changed):
            with mock.patch.object(self.module.os, 'readlink') as readlink:
                result = self.module.observe_process(expected)
                self.assertEqual(result['status'], 'unknown'); readlink.assert_not_called()
        with mock.patch.object(self.module, 'identity', side_effect=[expected, changed]):
            with mock.patch.object(self.module.os, 'readlink', return_value='/fixture/dpkg'):
                with mock.patch.object(Path, 'read_text', return_value='fixture-comm\n'):
                    result = self.module.observe_process(expected)
                    self.assertEqual(result['status'], 'unknown')
                    self.assertEqual(result['before'], expected); self.assertEqual(result['after'], changed)
        with mock.patch.object(self.module, 'identity', side_effect=FileNotFoundError('gone')):
            result = self.module.observe_process(expected)
            self.assertEqual(result['status'], 'unknown'); self.assertNotIn('exe', result)
        with mock.patch.object(self.module, 'identity', return_value=expected):
            with mock.patch.object(self.module.os, 'readlink', return_value='/fixture/dpkg'):
                with mock.patch.object(Path, 'read_text', return_value='fixture-comm\n'):
                    self.assertEqual(self.module.observe_process(expected)['status'], 'identity-bound-observation')

    def test_failed_end_preserves_original_raw_graph_and_reports_serialization_failure(self):
        first, leader, supervisor = self.graph(); first[22]['pgid'] = 900
        with self.assertRaises(RuntimeError) as rejected:
            self.module.stable_ownership(first, first, {20}, {20}, leader, supervisor)
        scope = self.work / 'scope'; scope.mkdir()
        failure = rejected.exception
        # 纯graph控制：不读/proc，不把模拟typed observations称作Linux实测。
        with mock.patch.object(self.module, 'observe_process', return_value={'status': 'unknown', 'error': 'gone'}):
            self.module.preserve_failed_end(scope, 'a'*32, 'b'*32, False, failure)
        end = json.loads((scope / ('b'*32 + '.end.json')).read_bytes())
        self.assertFalse(end['quiescent']); self.assertFalse(end['reaped'])
        self.assertEqual(end['error'], str(failure))
        self.assertEqual(end['ownership_failure']['first']['outside_pgid_descendants'], [22])
        self.assertEqual(end['ownership_failure']['process_observations'][0]['expected'], first[22])
        self.assertEqual(end['ownership_failure']['process_observations'][0]['observation']['status'], 'unknown')
        with mock.patch.object(self.module, 'write', side_effect=OSError('fixture-write-failure')):
            with mock.patch('sys.stderr', new=io.StringIO()) as error:
                self.module.preserve_failed_end(scope, 'a'*32, 'c'*32, False, failure)
                self.assertIn('fixture-write-failure', error.getvalue())
        self.assertEqual(str(failure), '发现逃逸SID/PGID后代')


class AptPrefixDiagnosticBoundary(unittest.TestCase):
    def test_actual_generated_executable_reader_preserves_binary_hash_and_non_linux_refuses(self):
        import ast, re, sys, types
        source = (ROOT / 'tools/qualify-windows-h264-recipe-A.ps1').read_text()
        code = re.findall(r"(?s)\$aptExecutables = @'\n(.*?)\n'@", source)
        self.assertEqual(len(code), 1)
        ast.parse(code[0])
        module = types.ModuleType('actual_apt_executable_reader')
        exec(compile(code[0], '<actual generated apt identity reader>', 'exec'), module.__dict__)
        parent = os.environ.get('FRD_WINDOWS_EXECUTION_TEST_ARTIFACTS')
        if parent:
            Path(parent).mkdir(parents=True, exist_ok=True)
        work = tempfile.mkdtemp(prefix='apt-identity-', dir=parent)
        try:
            path = Path(work) / 'fixture.bin'; raw = b'\x00\xff\r\nactual executable bytes\x00'; path.write_bytes(raw)
            result = module.executable_identity(path)
            self.assertEqual((result['size'], result['before_sha256'], result['after_sha256']), (len(raw), digest(raw), digest(raw)))
            self.assertTrue(result['observed_read_consistent'])
        finally:
            if not parent:
                shutil.rmtree(work)
        if sys.platform != 'linux':
            with mock.patch.object(module.shutil, 'which') as which:
                with self.assertRaisesRegex(RuntimeError, '真实Linux'):
                    module.main()
                which.assert_not_called()

    def test_source_gate_uses_original_prepare_prefix_and_returns_before_all_build_work(self):
        import re
        source = (ROOT / 'tools/qualify-windows-h264-recipe-A.ps1').read_text()
        prepare = re.findall(r"(?s)\$prepare = @'\n(.*?)\n'@", source)
        self.assertEqual(len(prepare), 1)
        marker = "\npython3 -c 'import sys; assert sys.version_info[:2] == (3,12)'"
        self.assertEqual(prepare[0].count(marker), 1)
        prefix = prepare[0].split(marker)[0]
        install = 'DEBIAN_FRONTEND=noninteractive apt-get -o Dpkg::Use-Pty=0 install --no-install-recommends -y python3 build-essential make patch pkg-config xz-utils tar curl ca-certificates procps util-linux'
        self.assertEqual(prefix.splitlines()[-2:], ['apt-get update', install])
        self.assertEqual(prefix.count('-o Dpkg::Use-Pty=0'), 1)
        self.assertNotIn('Use-Pty', prefix.replace('-o Dpkg::Use-Pty=0', '')); self.assertNotIn('/opt/', prefix)
        execute = source.split('function Invoke-ReleaseAExecute($Context) {', 1)[1].split('function Invoke-ReleaseACollect', 1)[0]
        self.assertIn('$prepare = $prepare.Substring(0, $boundaryAt)', execute)
        self.assertLess(execute.index('$prepare = $prepare.Substring'), execute.index("Write-ReleaseALf (Join-Path $Context.root 'setup/prepare.sh')"))
        gate = 'if ($aptDiagnostic) { $state.apt_prefix_completed = $true; return }'
        self.assertEqual(execute.count(gate), 1)
        self.assertLess(execute.index("Assert-ReleaseAResult $record '固定环境准备'"), execute.index(gate))
        self.assertLess(execute.index(gate), execute.index('Invoke-ReleaseALinuxControls $Context'))
        self.assertLess(execute.index(gate), execute.index('$state.setup_ready = $true'))
        self.assertLess(execute.index(gate), execute.index('Invoke-ReleaseABuilderOnce'))
        self.assertNotIn('$state.quiescent = $true', execute.split('finally {', 1)[0])
        stop = source.split('function Stop-ReleaseAScopes', 1)[1].split('function Invoke-ReleaseALinuxControls', 1)[0]
        self.assertIn("if ($SetupOnly) { @('setup') }", stop)
        self.assertIn('if (-not $ControlsOnly -and -not $SetupOnly)', stop)
        cli = source.split('# Dot-source', 1)[1]
        self.assertEqual(cli.count('$context.bootstrapAptDiagnostic = $true'), 1)
        self.assertRegex(cli, re.compile(r"elseif \(\$Action -ceq 'BootstrapAptDiagnostic'\) \{\s*"
                                        r"\$context.bootstrapAptDiagnostic = \$true\s*Invoke-ReleaseAExecute \$context\s*\}"))


class FailedWslExportBoundary(unittest.TestCase):
    def setUp(self):
        import re, types
        source = (ROOT / 'tools/qualify-windows-h264-recipe-A.ps1').read_text()
        code = re.findall(r"(?s)\$failedWslExporter = @'\n(.*?)\n'@", source)
        self.assertEqual(len(code), 1)
        parent = os.environ.get('FRD_WINDOWS_EXECUTION_TEST_ARTIFACTS')
        if parent:
            Path(parent).mkdir(parents=True, exist_ok=True)
        self.work = Path(tempfile.mkdtemp(prefix='failed-wsl-', dir=parent))
        script = self.work / 'actual-exporter.py'; script.write_text(code[0] + '\n')
        self.module = types.ModuleType('actual_failed_wsl_exporter'); self.module.__file__ = str(script)
        exec(compile(code[0], '<actual inline failed wsl exporter>', 'exec'), self.module.__dict__)

    tearDown = InlineSupervisorBoundary.tearDown

    def fixture(self, name):
        work = self.work / name; repo = work / 'repo'; evidence = work / 'evidence'
        configuration = repo / '.codex-target/ffmpeg-8.1.2/build-side-frd-h264dc-aarch64-v1/windows-aarch64/Release'
        attempt = configuration / ('.attempt-431-' + 'a'*32)
        native = attempt / ('native-supervised-wsl-' + 'b'*32)
        scope = attempt / 'lifecycle'; native.mkdir(parents=True); scope.mkdir()
        source, nonce, call = 'd'*40, 'c'*32, 'e'*32
        def put(path, value):
            path.parent.mkdir(parents=True, exist_ok=True); path.write_text(json.dumps(value))
        put(evidence / 'provenance/state.json', dict(source=source, builder_exit=1))
        put(evidence / 'provenance/paths.json', dict(wsl='/fixture/repo'))
        put(evidence / 'setup/calls/builder/exit.json', dict(windows_child=dict(pid=431), exit=1, terminated=True, capture_complete=True))
        supervisor = attempt / 'release-a-supervisor.py'; supervisor.write_bytes(b'fixture source bytes only\n')
        sha = digest(supervisor.read_bytes())
        linux_attempt = '/fixture/repo/' + attempt.relative_to(repo).as_posix()
        intent = dict(nonce=nonce, scope_linux=linux_attempt+'/lifecycle', supervisor_linux=linux_attempt+'/release-a-supervisor.py', source_sha256=sha)
        put(attempt / 'lifecycle-intent.json', intent)
        put(scope / 'scope.json', dict(nonce=nonce, source_path=intent['supervisor_linux'], source_sha256=sha))
        put(scope / 'halt.json', dict(nonce=nonce)); put(scope / 'stopped.json', dict(nonce=nonce, quiescent=True, new_launches_blocked=True))
        for kind in ('pending', 'start', 'end', 'observations'):
            value = dict(nonce=nonce, call_id=call)
            if kind in ('pending','start'): value.update(source_sha256=sha, source_path=intent['supervisor_linux'])
            put(scope / (call+'.'+kind+'.json'), value)
        argv = ['wsl.exe','--scope',intent['scope_linux'],'--nonce',nonce,'--call-id',call,'run','--','bash','-lc','make -j4']
        exe_sha = digest(b'MZ fixture data only')
        native_record = dict(argv=argv, cwd=str(repo), stdout=str(native/'stdout.bin'), stderr=str(native/'stderr.bin'), receipt=str(native/'exit.json'), exit=2, terminated=True, capture_complete=True,
                             executable=dict(before_sha256=exe_sha, saved_sha256=exe_sha, after_sha256=exe_sha))
        put(native / 'argv.json', native_record); put(native / 'exit.json', native_record)
        (native / 'stdout.bin').write_bytes(b'raw stdout\x00\r\n'); (native / 'stderr.bin').write_bytes(b'\x1b[31mfirst tool error\x00\xff\r\n')
        (native / 'executable.bin').write_bytes(b'MZ fixture data only'); (native / 'unlisted.bin').write_bytes(b'untouched')
        wsl = dict(argv=argv, cwd=str(repo), inner_argv=['bash','-lc','make -j4'], action='FFmpeg build', exit=2, capture_error=None,
                   raw_receipt=str(native/'exit.json'), stdout=str(native/'stdout.bin'), stderr=str(native/'stderr.bin'),
                   lifecycle=dict(nonce=nonce, scope=intent['scope_linux'], supervisor=intent['supervisor_linux'], source_sha256=sha))
        put(attempt / ('wsl-'+call+'.json'), wsl)
        put(evidence / 'setup/calls/linux-cleanup-builder/exit.json', dict(exit=0, error=None, timed_out=False, terminated=True, capture_complete=True,
            argv=['wsl.exe','--scope',intent['scope_linux'],'--nonce',nonce,'stop']))
        return dict(repo=repo, evidence=evidence, configuration=configuration, attempt=attempt, native=native, scope=scope, call=call, source=source, wsl=wsl, put=put)

    def run_fixture(self, case):
        return self.module.export_failed(case['repo'], case['evidence'], case['source'])

    def windows_observations(self, mutate=None):
        # 明确的host合成API语义；实际Path/open/read/close和源Reader保持真实。
        import contextlib, sys
        from types import SimpleNamespace
        original_regular, original_fstat = self.module.regular, self.module.os.fstat
        counts = dict(path=0, fd=0)
        fields = ('st_dev','st_ino','st_mode','st_nlink','st_size','st_mtime_ns','st_ctime_ns')
        def observation(info, kind):
            values = {name: getattr(info, name) for name in fields}
            values.update(st_file_attributes=32, st_reparse_tag=0, st_birthtime_ns=info.st_ctime_ns)
            if kind == 'fd': values['st_ctime_ns'] += 2000000
            counts[kind] += 1
            if mutate is not None: mutate(kind, counts[kind], values)
            return SimpleNamespace(**values)
        def regular(root, path, directory=False):
            info = original_regular(root, path, directory)
            return info if directory else observation(info, 'path')
        def fstat(fd): return observation(original_fstat(fd), 'fd')
        stack = contextlib.ExitStack()
        stack.enter_context(mock.patch.object(self.module, 'sys', SimpleNamespace(platform='win32', stderr=sys.stderr)))
        stack.enter_context(mock.patch.object(self.module, 'regular', side_effect=regular))
        stack.enter_context(mock.patch.object(self.module.os, 'fstat', side_effect=fstat))
        return stack, counts

    def test_windows_stable_creation_vs_change_time_reads_without_weakening_fd_identity(self):
        directory = self.work/'windows-stable'; directory.mkdir()
        path = directory/'input.bin'; path.write_bytes(b'committed stable payload\x00')
        stack, counts = self.windows_observations()
        with stack:
            raw = self.module.Reader().read(directory, path, 1024)
        self.assertEqual(raw, b'committed stable payload\x00')
        self.assertGreaterEqual(counts['fd'], 6)

    def test_windows_all_seven_fields_reject_at_path_and_fd_read_copy_boundaries(self):
        fields = ('st_dev','st_ino','st_mode','st_nlink','st_size','st_mtime_ns','st_ctime_ns')
        controls = (('guard', 'read', 'path', (2,)), ('metadata-end', 'read', 'fd', (2,)),
                    ('read-open', 'read', 'fd', (3,)), ('read-end', 'read', 'fd', (4,)),
                    ('copy-open', 'copy', 'fd', (3,)), ('copy-end', 'copy', 'fd', (4,)),
                    ('reread-open', 'copy', 'fd', (5,)), ('reread-end', 'copy', 'fd', (6,)),
                    ('read-final', 'read', 'fd', (5,6)), ('copy-final', 'copy', 'fd', (7,8)))
        for label, method, kind, numbers in controls:
            for field in fields:
                with self.subTest(stage=label, field=field):
                    directory = self.work/(label+'-'+field); directory.mkdir()
                    path = directory/'input.bin'; path.write_bytes(b'identity matrix payload')
                    def mutate(got, number, values):
                        if got == kind and number in numbers:
                            values[field] += 2 if field == 'st_mode' else 1
                    stack, counts = self.windows_observations(mutate)
                    streams = []; original_open = Path.open
                    def opening(candidate, *args, **kwargs):
                        stream = original_open(candidate, *args, **kwargs); streams.append(stream); return stream
                    with stack, mock.patch.object(Path, 'open', new=opening):
                        with self.assertRaises(ValueError):
                            reader = self.module.Reader()
                            if method == 'read': reader.read(directory, path, 1024)
                            else: reader.copy(directory, path, directory/'copy.bin', 1024)
                    self.assertTrue(streams)
                    self.assertTrue(all(stream.closed for stream in streams))

    def test_windows_bridge_rejects_each_comparable_field_and_missing_actual_birth(self):
        controls = [(name, 'fd', 'change') for name in ('st_dev','st_ino','st_mode','st_nlink','st_size','st_mtime_ns')]
        controls += [('st_birthtime_ns', kind, change) for kind in ('path','fd') for change in ('change','missing')]
        for field, kind, change in controls:
            with self.subTest(field=field, side=kind, change=change):
                directory = self.work/(field+'-'+kind+'-'+change); directory.mkdir()
                path = directory/'input.bin'; path.write_bytes(b'bridge payload')
                def mutate(got, number, values):
                    if got == kind and number == 1:
                        if change == 'missing': values.pop(field)
                        else: values[field] += 2 if field == 'st_mode' else 1
                stack, counts = self.windows_observations(mutate)
                with stack:
                    with self.assertRaises(ValueError) as caught:
                        self.module.Reader().read(directory, path, 1024)
                if hasattr(caught.exception, 'identity_diagnostic'):
                    detail = caught.exception.identity_diagnostic
                    self.assertNotIn('ctime_ns', detail['failed_fields'])
                    self.assertEqual(detail['operation'], 'path-handle-open')
                    self.assertLessEqual(len(json.dumps(detail).encode()), 4096)
                self.assertEqual(counts['fd'], 1)

    def test_windows_second_bridge_uses_fresh_fstat_after_second_guard(self):
        directory = self.work/'fresh-F1'; directory.mkdir()
        path = directory/'input.bin'; path.write_bytes(b'fresh observation')
        def mutate(kind, number, values):
            if kind == 'fd' and number == 2: values['st_birthtime_ns'] += 1
        stack, counts = self.windows_observations(mutate)
        with stack:
            with self.assertRaisesRegex(ValueError, 'path handle binding changed') as caught:
                self.module.Reader().read(directory, path, 1024)
        detail = caught.exception.identity_diagnostic
        self.assertEqual(detail['operation'], 'path-handle-guard-end')
        self.assertEqual(detail['failed_fields'], ['birthtime_ns'])
        self.assertEqual(counts, dict(path=2, fd=2))

    def test_windows_temporary_handle_for_other_object_is_rejected_when_path_is_restored(self):
        directory = self.work/'actual-A-B-A'; directory.mkdir()
        path = directory/'input.bin'; other = directory/'other.bin'; hold = directory/'held.bin'
        path.write_bytes(b'same bytes'); other.write_bytes(b'same bytes')
        original_open = Path.open; streams = []
        def opening(candidate, *args, **kwargs):
            if candidate == path and not streams:
                os.replace(path, hold); os.replace(other, path)
                stream = original_open(candidate, *args, **kwargs); streams.append(stream)
                os.replace(path, other); os.replace(hold, path)
                return stream
            return original_open(candidate, *args, **kwargs)
        stack, counts = self.windows_observations()
        with stack, mock.patch.object(Path, 'open', new=opening):
            with self.assertRaisesRegex(ValueError, 'path handle binding changed') as caught:
                self.module.Reader().read(directory, path, 1024)
        self.assertIn('ino', caught.exception.identity_diagnostic['failed_fields'])
        self.assertTrue(streams[0].closed)
        self.assertEqual(counts['path'], 1)  # 绑定失败不以末尾path恢复为理由继续。

    def test_windows_same_size_restored_mtime_replace_rejected_at_actual_open_reread_and_final(self):
        for method, opened_at in (('read',2),('read',3),('copy',2),('copy',3),('copy',4)):
            with self.subTest(method=method, opened_at=opened_at):
                directory = self.work/(method+'-replace-'+str(opened_at)); directory.mkdir()
                path = directory/'input.bin'; replacement = directory/'replacement.bin'
                path.write_bytes(b'fixed-length-old'); replacement.write_bytes(b'fixed-length-new')
                info = path.stat(); os.utime(replacement, ns=(info.st_atime_ns, info.st_mtime_ns))
                original_open = Path.open; streams = []; calls = []
                def opening(candidate, *args, **kwargs):
                    if candidate == path:
                        calls.append(candidate)
                        if len(calls) == opened_at: os.replace(replacement, path)
                    stream = original_open(candidate, *args, **kwargs); streams.append(stream); return stream
                stack, counts = self.windows_observations()
                with stack, mock.patch.object(Path, 'open', new=opening):
                    with self.assertRaises(ValueError):
                        reader = self.module.Reader()
                        if method == 'read': reader.read(directory, path, 1024)
                        else: reader.copy(directory, path, directory/'copy.bin', 1024)
                self.assertEqual(len(calls), opened_at)
                self.assertTrue(all(stream.closed for stream in streams))
                self.assertEqual(path.stat().st_mtime_ns, info.st_mtime_ns)
                self.assertEqual(path.stat().st_size, info.st_size)

    def test_windows_canonical_budget_cap_and_handle_type_guards_fail_closed(self):
        directory = self.work/'canonical-bounds'; directory.mkdir()
        path = directory/'input.bin'; path.write_bytes(b'bounded file')
        stack, counts = self.windows_observations()
        with stack:
            with self.assertRaisesRegex(ValueError, 'file byte bound'):
                self.module.Reader().read(directory, path, 1)
        self.assertEqual(counts['fd'], 0)
        for field, value, message in (('st_file_attributes',0x400,'reparse'),('st_mode',0o040700,'file type'),('st_nlink',2,'hardlink')):
            def mutate(kind, number, values):
                if kind == 'fd' and number == 1: values[field] = value
            stack, counts = self.windows_observations(mutate)
            with stack:
                with self.assertRaisesRegex(ValueError, message):
                    self.module.Reader().read(directory, path, 1024)
        stack, counts = self.windows_observations()
        calls = []; streams = []; original_open = Path.open
        def budget():
            calls.append(1)
            if len(calls) == 2: raise ValueError('export time bound')
        def opening(candidate, *args, **kwargs):
            stream = original_open(candidate, *args, **kwargs); streams.append(stream); return stream
        with stack, mock.patch.object(Path, 'open', new=opening):
            with self.assertRaisesRegex(ValueError, 'export time bound'):
                self.module.path_identity(directory, path, 1024, budget)
        self.assertEqual(len(streams), 1)
        self.assertTrue(streams[0].closed)
        reader = self.module.Reader(); reader.began -= 21
        with self.assertRaisesRegex(ValueError, 'export time bound'): reader.read(directory, path, 1024)
        second = directory/'second.bin'; second.write_bytes(b'additional file')
        stack, counts = self.windows_observations()
        with stack, mock.patch.object(self.module, 'TOTAL_LIMIT', 20):
            reader = self.module.Reader(); reader.read(directory, path, 1024)
            with self.assertRaisesRegex(ValueError, 'total byte bound'): reader.read(directory, second, 1024)

    def test_posix_reader_keeps_original_provider_without_metadata_open(self):
        from types import SimpleNamespace
        directory = self.work/'posix-provider'; directory.mkdir()
        path = directory/'input.bin'; path.write_bytes(b'posix bytes')
        original_open = Path.open; streams = []
        def opening(candidate, *args, **kwargs):
            stream = original_open(candidate, *args, **kwargs); streams.append(stream); return stream
        with mock.patch.object(self.module, 'sys', SimpleNamespace(platform='linux')), mock.patch.object(Path, 'open', new=opening):
            raw = self.module.Reader().read(directory, path, 1024)
        self.assertEqual(raw, b'posix bytes')
        self.assertEqual(len(streams), 1)
        self.assertTrue(streams[0].closed)

    def host_probe_definitions(self):
        import types
        path = ROOT/'tools/windows-file-identity-probe.py'
        module = types.ModuleType('host_only_probe_function_fixture'); module.__file__ = str(path)
        # 只装入定义并调用有限host helper；不运行main/run或伪造Windows平台probe。
        exec(compile(path.read_bytes(), '<host probe helper definitions>', 'exec'), module.__dict__)
        return module

    def test_probe_fixed_loader_reads_current_qualifier_bytes_without_import_cache(self):
        probe = self.host_probe_definitions()
        exporter, source = probe.trusted_exporter(ROOT)
        self.assertEqual(source['qualifier_source_relative_path'], 'tools/qualify-windows-h264-recipe-A.ps1')
        self.assertEqual(source['qualifier_source_sha256'], digest((ROOT/source['qualifier_source_relative_path']).read_bytes()))
        directory = self.work/'fixed-loader'; directory.mkdir()
        path = directory/'input.bin'; path.write_bytes(b'real fixed-source Reader')
        self.assertEqual(exporter['Reader']().read(directory, path, 1024), b'real fixed-source Reader')

    def test_probe_canonical_host_controls_use_actual_exporter_and_reject_actual_read_replace(self):
        import time
        probe = self.host_probe_definitions()
        for name, payload in (('stable-direct',probe.DIRECT),('stable-replaced-before-observation',probe.COMMITTED),('replace-before-open-control',probe.AFTER)):
            with self.subTest(name=name):
                directory = self.work/name; directory.mkdir()
                path = directory/'input.bin'; path.write_bytes(payload)
                row = dict(name=name, measurement_complete=False)
                stack, counts = self.windows_observations()
                with stack:
                    probe.canonical_observe(directory, path, row, payload, time.monotonic(), self.module.__dict__)
                self.assertTrue(row['measurement_complete'])
                self.assertEqual(set(row['canonical_path_snapshot']['fields']), {'dev','ino','mode','nlink','size','mtime_ns','ctime_ns'})
                if name == 'replace-before-open-control':
                    self.assertFalse(row['canonical_reader']['accepted'])
                    self.assertTrue(row['canonical_reader']['expected_replacement_rejected'])
                    self.assertEqual(row['canonical_reader']['identity_diagnostic']['operation'], 'read-open')
                    self.assertIn('ino', row['canonical_reader']['identity_diagnostic']['differences'])
                    self.assertEqual(path.read_bytes(), probe.BEFORE)
                else:
                    self.assertTrue(row['canonical_reader']['accepted'])
                    self.assertEqual(row['canonical_reader']['read_payload_sha256'], digest(payload))

    def test_windows_export_success_and_bridge_write_failure_preserve_original_status(self):
        import contextlib, io
        valid = self.fixture('windows-export-valid'); stack, counts = self.windows_observations()
        with stack: result = self.run_fixture(valid)
        self.assertTrue(result['complete'], result)
        self.assertEqual(len(result['files']), 15)
        self.assertEqual(len(list((valid['evidence'].parent/'release-a-failed-wsl').iterdir())), 16)
        failed = self.fixture('windows-bridge-write-failed')
        def mutate(kind, number, values):
            if kind == 'fd' and number == 1: values['st_birthtime_ns'] += 1
        stack, counts = self.windows_observations(mutate)
        original_open = Path.open; streams = []; stderr = io.StringIO()
        def opening(path, *args, **kwargs):
            if path.name == 'selection.json': raise OSError('injected selection write failure')
            stream = original_open(path, *args, **kwargs); streams.append(stream); return stream
        with stack, mock.patch.object(Path, 'open', new=opening), contextlib.redirect_stderr(stderr):
            self.module.sys.stderr = stderr
            result = self.run_fixture(failed)
        self.assertFalse(result['complete'])
        self.assertEqual(result['error'], 'path handle binding changed')
        self.assertEqual(result['identity_diagnostic']['failed_fields'], ['birthtime_ns'])
        self.assertTrue(result['identity_diagnostic_write_failed'])
        self.assertIn('original strict rejection retained', stderr.getvalue())
        self.assertTrue(all(stream.closed for stream in streams))

    def test_host_injected_identity_rejection_preserves_named_relative_diagnostic(self):
        from types import SimpleNamespace
        case = self.fixture('identity-field-diagnostic')
        state_path = case['evidence']/'provenance/state.json'
        state_before = state_path.read_bytes()
        original = self.module.os.fstat
        calls = []
        def changed(fd):
            info = original(fd); calls.append(fd)
            values = {name: getattr(info, name) for name in (
                'st_dev','st_ino','st_mode','st_nlink','st_size','st_mtime_ns','st_ctime_ns')}
            values['st_ctime_ns'] += 1
            return SimpleNamespace(**values)
        with mock.patch.object(self.module.os, 'fstat', side_effect=changed):
            summary = self.run_fixture(case)
        self.assertFalse(summary['complete'])
        self.assertEqual(summary['error'], 'open identity changed')
        self.assertIn('identity_diagnostic', summary)
        detail = summary['identity_diagnostic']
        self.assertEqual(detail['operation'], 'read-open')
        self.assertEqual(detail['relative_path'], 'provenance/state.json')
        self.assertEqual(detail['cap'], 512*1024)
        names = {'dev','ino','mode','nlink','size','mtime_ns','ctime_ns'}
        self.assertEqual(set(detail['before']), names)
        self.assertEqual(set(detail['observed']), names)
        self.assertEqual(detail['differences'], ['ctime_ns'])
        self.assertEqual(detail['observed']['ctime_ns'], detail['before']['ctime_ns']+1)
        self.assertNotIn(str(case['evidence']), json.dumps(detail))
        self.assertLessEqual(len(json.dumps(detail).encode()), 4096)
        self.assertEqual(len(calls), 1)  # open拒绝之后没有读取或伪造read-end/path-after。
        self.assertEqual(state_path.read_bytes(), state_before)
        saved = json.loads((case['evidence'].parent/'release-a-failed-wsl/selection.json').read_bytes())
        self.assertEqual(saved, summary)

    def test_all_reader_observation_boundaries_keep_original_seven_field_comparison(self):
        from types import SimpleNamespace
        controls = (
            ('read-open', 'read', 1, None, 'open identity changed'),
            ('read-end', 'read', 2, None, 'read changed'),
            ('read-path-after', 'read', None, 2, 'read identity changed'),
            ('copy-open', 'copy', 1, None, 'copy open changed'),
            ('copy-read-end', 'copy', 2, None, 'copy changed'),
            ('copy-reread-open', 'copy', 3, None, 'reread open changed'),
            ('copy-reread-end', 'copy', 4, None, 'reread changed'),
            ('copy-path-after', 'copy', None, 2, 'copy identity/bytes changed'))
        fields = ('st_dev','st_ino','st_mode','st_nlink','st_size','st_mtime_ns','st_ctime_ns')
        def changed(info):
            values = {name: getattr(info, name) for name in fields}
            values['st_ctime_ns'] += 1
            # 合成host失败注入，只验证已有属性的保存，不伪装Windows观测。
            values.update(st_file_attributes=32, st_reparse_tag=0)
            return SimpleNamespace(**values)
        for operation, method, fstat_number, path_number, message in controls:
            directory = self.work/operation; directory.mkdir()
            path = directory/'input.bin'; path.write_bytes(b'fixed host Reader fixture\x00')
            original_fstat = self.module.os.fstat; original_regular = self.module.regular
            counts = dict(fstat=0, path=0)
            def fstat(fd):
                info = original_fstat(fd); counts['fstat'] += 1
                return changed(info) if counts['fstat'] == fstat_number else info
            def regular(root, candidate, directory=False):
                info = original_regular(root, candidate, directory)
                counts['path'] += 1
                return changed(info) if counts['path'] == path_number else info
            with self.subTest(operation=operation):
                with mock.patch.object(self.module.os, 'fstat', side_effect=fstat), mock.patch.object(self.module, 'regular', side_effect=regular):
                    with self.assertRaisesRegex(ValueError, message) as caught:
                        reader = self.module.Reader()
                        if method == 'read': reader.read(directory, path, 1024)
                        else: reader.copy(directory, path, directory/'copy.bin', 1024)
                detail = caught.exception.identity_diagnostic
                self.assertEqual(detail['operation'], operation)
                self.assertEqual(detail['relative_path'], 'input.bin')
                self.assertEqual(detail['differences'], ['ctime_ns'])
                self.assertEqual(detail['attributes']['observed'], dict(st_file_attributes=32, st_reparse_tag=0))
                self.assertEqual(set(detail['before']), {'dev','ino','mode','nlink','size','mtime_ns','ctime_ns'})
                if fstat_number: self.assertEqual(counts['fstat'], fstat_number)
                if path_number: self.assertEqual(counts['path'], path_number)

    def test_real_host_replace_before_open_is_rejected_without_followup_reads(self):
        directory = self.work/'real-replace'; directory.mkdir()
        path = directory/'input.bin'; replacement = directory/'replacement.bin'
        path.write_bytes(b'old payload'); replacement.write_bytes(b'new payload')
        original_open = Path.open; replacements = []
        def open_after_replace(candidate, *args, **kwargs):
            if candidate == path and not replacements:
                os.replace(replacement, path); replacements.append(candidate)
            return original_open(candidate, *args, **kwargs)
        with mock.patch.object(Path, 'open', new=open_after_replace):
            with self.assertRaisesRegex(ValueError, 'open identity changed') as caught:
                self.module.Reader().read(directory, path, 1024)
        detail = caught.exception.identity_diagnostic
        self.assertEqual(detail['operation'], 'read-open')
        self.assertIn('ino', detail['differences'])
        self.assertEqual(detail['relative_path'], 'input.bin')
        self.assertEqual(path.read_bytes(), b'new payload')
        self.assertEqual(len(replacements), 1)

    def test_diagnostic_build_encode_and_write_failures_never_replace_identity_error(self):
        import contextlib, io
        from types import SimpleNamespace
        original_fstat = self.module.os.fstat
        def changed(fd):
            info = original_fstat(fd)
            fields = {name: getattr(info, name) for name in ('st_dev','st_ino','st_mode','st_nlink','st_size','st_mtime_ns','st_ctime_ns')}
            fields['st_ctime_ns'] += 1
            return SimpleNamespace(**fields)
        for mode in ('build', 'encode', 'write'):
            case = self.fixture('diagnostic-fails-'+mode)
            original_dumps = self.module.json.dumps; original_open = Path.open
            def encoding(value, *args, **kwargs):
                if isinstance(value, dict) and 'identity_diagnostic' in value:
                    raise ValueError('injected diagnostic encoding failure')
                return original_dumps(value, *args, **kwargs)
            attempts = []
            def opening(path, *args, **kwargs):
                if path.name == 'selection.json':
                    attempts.append(path); raise OSError('injected selection write failure')
                return original_open(path, *args, **kwargs)
            patch = (mock.patch.object(self.module, 'identity_diagnostic', side_effect=OSError('injected diagnostic build failure')) if mode == 'build' else
                     mock.patch.object(self.module.json, 'dumps', side_effect=encoding) if mode == 'encode' else
                     mock.patch.object(Path, 'open', new=opening))
            stderr = io.StringIO()
            with patch, mock.patch.object(self.module.os, 'fstat', side_effect=changed), contextlib.redirect_stderr(stderr):
                result = self.run_fixture(case)
            with self.subTest(mode=mode):
                self.assertFalse(result['complete'])
                self.assertEqual(result['error'], 'open identity changed')
                if mode == 'write':
                    self.assertTrue(result['identity_diagnostic_write_failed'])
                    self.assertEqual(len(attempts), 1)
                    self.assertIn('original strict rejection retained', stderr.getvalue())
                else:
                    self.assertNotIn('identity_diagnostic', result)

    def test_actual_exporter_keeps_original_bytes_paths_and_untouched_files(self):
        case = self.fixture('valid'); before = (case['native']/'unlisted.bin').read_bytes()
        summary = self.run_fixture(case)
        self.assertTrue(summary['complete'], summary)
        self.assertNotIn('identity_diagnostic', summary)
        destination = case['evidence'].parent / 'release-a-failed-wsl'
        self.assertEqual((destination/'native.stderr.bin').read_bytes(), (case['native']/'stderr.bin').read_bytes())
        self.assertEqual((destination/'wsl.json').read_bytes(), (case['attempt']/('wsl-'+case['call']+'.json')).read_bytes())
        self.assertEqual(len(list(destination.iterdir())), 16)
        self.assertEqual((case['native']/'unlisted.bin').read_bytes(), before)
        for item in summary['files'].values():
            if item['status'] == 'copied':
                self.assertEqual(item['source_sha256'], item['saved_sha256'])
        with self.assertRaises(FileExistsError):
            self.run_fixture(case)

    def test_actual_exporter_refuses_ambiguous_escape_links_missing_and_limits(self):
        for label in ('no-attempt', 'two-attempts', 'two-failed', 'escape', 'link', 'hardlink', 'missing', 'oversize', 'total-cap', 'entry-cap', 'stop-failed', 'bad-nonce'):
            case = self.fixture(label)
            if label == 'no-attempt':
                case['attempt'].rename(case['configuration']/'unlisted-directory')
            elif label == 'two-attempts':
                (case['configuration']/('.attempt-432-'+'f'*32)).mkdir()
            elif label == 'two-failed':
                case['put'](case['attempt']/('wsl-'+'f'*32+'.json'), case['wsl'])
            elif label == 'escape':
                case['wsl']['raw_receipt'] = str(case['configuration'].parent/'outside.json')
                case['put'](case['attempt']/('wsl-'+case['call']+'.json'), case['wsl'])
            elif label in ('link', 'hardlink'):
                path = case['native']/'stderr.bin'; path.unlink()
                if label == 'link': path.symlink_to(case['native']/'unlisted.bin')
                else: os.link(case['native']/'unlisted.bin', path)
            elif label == 'missing':
                (case['native']/'exit.json').unlink()
            elif label == 'oversize':
                with (case['native']/'stdout.bin').open('wb') as stream: stream.truncate(16*1024*1024+1)
            elif label == 'entry-cap':
                for number in range(257): (case['attempt']/('extra-'+str(number))).touch()
            elif label == 'stop-failed':
                path = case['evidence']/'setup/calls/linux-cleanup-builder/exit.json'
                value = json.loads(path.read_bytes()); value['exit'] = 1; case['put'](path, value)
            elif label == 'bad-nonce':
                case['wsl']['lifecycle']['nonce'] = '0'*32
                case['put'](case['attempt']/('wsl-'+case['call']+'.json'), case['wsl'])
            if label == 'total-cap':
                with mock.patch.object(self.module, 'TOTAL_LIMIT', 8): summary = self.run_fixture(case)
            else: summary = self.run_fixture(case)
            with self.subTest(label=label): self.assertFalse(summary['complete'], summary)

    def test_optional_missing_and_real_file_change_are_not_fabricated(self):
        case = self.fixture('optional')
        (case['scope']/(case['call']+'.observations.json')).unlink()
        summary = self.run_fixture(case)
        self.assertTrue(summary['complete'], summary)
        self.assertEqual(summary['files']['call.observations.json']['status'], 'missing-optional')
        root = self.work / 'changed'; root.mkdir(); original = root/'input.bin'; original.write_bytes(b'original')
        guard = self.module.regular; calls = []
        def changed_guard(base, path, directory=False):
            if path == original:
                calls.append(path)
                if len(calls) == 2: original.write_bytes(b'changed actual file')
            return guard(base, path, directory)
        with mock.patch.object(self.module, 'regular', side_effect=changed_guard):
            with self.assertRaisesRegex(ValueError, 'copy identity/bytes changed'):
                self.module.Reader().copy(root, original, root/'copy.bin', 1024)


class CollectPhaseBoundary(unittest.TestCase):
    def setUp(self):
        import re, types
        source = (ROOT/'tools/qualify-windows-h264-recipe-A.ps1').read_text()
        code = re.findall(r'(?s)# FRD_PHASE_BEGIN\n(.*?)\n# FRD_PHASE_END', source)
        self.assertEqual(len(code), 1)
        self.module = types.ModuleType('actual_collect_phase_writer')
        exec(compile(code[0], '<actual inline phase writer>', 'exec'), self.module.__dict__)
        parent = os.environ.get('FRD_WINDOWS_EXECUTION_TEST_ARTIFACTS')
        if parent: Path(parent).mkdir(parents=True, exist_ok=True)
        self.work = Path(tempfile.mkdtemp(prefix='collect-phase-', dir=parent))

    tearDown = InlineSupervisorBoundary.tearDown

    def test_three_fixed_slots_preserve_failure_and_interruption(self):
        path = self.work/'phases.json'; update = self.module.phase_update
        update(path, 'llvm-copy', 'start')
        self.assertIsNone(json.loads(path.read_bytes())['phases']['llvm-copy']['end_ns'])
        update(path, 'llvm-copy', 'end', 7)
        self.assertEqual(json.loads(path.read_bytes())['phases']['llvm-copy']['exit'], 7)
        with self.assertRaises(ValueError): update(path, 'system-tools', 'start')
        success = self.work/'success.json'
        for name in ('llvm-copy', 'system-tools', 'build-root-copy'):
            update(success, name, 'start'); update(success, name, 'end', 0)
        self.assertEqual(len(json.loads(success.read_bytes())['phases']), 3)
        self.assertLessEqual(success.stat().st_size, 4096)
        with self.assertRaises(ValueError): update(success, 'llvm-copy', 'start')
        with self.assertRaises(ValueError): update(success, 'arbitrary-command', 'start')

    def test_marker_refuses_links_and_large_existing_data(self):
        target = self.work/'untouched'; target.write_bytes(b'keep')
        link = self.work/'link.json'; link.symlink_to(target)
        with self.assertRaises(ValueError): self.module.phase_update(link, 'llvm-copy', 'start')
        self.assertEqual(target.read_bytes(), b'keep')
        large = self.work/'large.json'; large.write_bytes(b'x'*4097)
        with self.assertRaises(ValueError): self.module.phase_update(large, 'llvm-copy', 'start')

    def test_actual_shell_wrapper_preserves_bulk_exit_when_end_marker_also_fails(self):
        import re
        source = (ROOT/'tools/qualify-windows-h264-recipe-A.ps1').read_text()
        collect = re.findall(r"(?s)\$collect = @'\n(.*?)\n'@", source)
        self.assertEqual(len(collect), 1)
        # 只保留实际shell包装；不执行Python marker或三条bulk命令。
        body = collect[0].split('FRD_PHASE\n}\n', 1)[1]
        commands = (
            ('llvm-copy', 'if [[ -e /opt/frd-llvm-mingw-20250709 ]]; then cp -a /opt/frd-llvm-mingw-20250709 "$root/toolchain/install"; fi'),
            ('system-tools', 'python3 "$repo/tools/windows-h264-recipe-transport.py" collect-tools --root "$root"'),
            ('build-root-copy', 'if [[ -d "$repo/.codex-target/ffmpeg-8.1.2" ]]; then cp -a "$repo/.codex-target/ffmpeg-8.1.2/." "$root/build-root/"; fi'))
        for phase, command in commands:
            self.assertEqual(body.count(command), 1)
            body = body.replace(command, 'fixture_bulk ' + phase)
        prefix = '''set -euo pipefail
selected=$1
bulk_result=$2
marker_result=$3
fixture_bulk() {
    printf 'fixture-bulk:%s\\n' "$1"
    if [[ $1 == "$selected" ]]; then return "$bulk_result"; fi
    return 0
}
mark_phase() {
    if [[ $1 == "$selected" && $2 == end && $marker_result -ne 0 ]]; then
        printf 'fixture-marker-error:%s:%s\\n' "$1" "$marker_result" >&2
        return "$marker_result"
    fi
    return 0
}
'''
        script = self.work/'actual-shell-wrapper.fixture.sh'
        script.write_text(prefix + body + "\nprintf 'fixture-complete\\n'\n")
        for phase, _ in commands:
            for bulk, marker, expected in ((7,1,7), (7,13,7), (0,13,13), (7,0,7), (0,0,0)):
                with self.subTest(phase=phase, bulk=bulk, marker=marker):
                    argv = ['bash', str(script), phase, str(bulk), str(marker)]
                    result = subprocess.run(argv, capture_output=True, timeout=3)
                    name = phase+'-'+str(bulk)+'-'+str(marker)
                    (self.work/(name+'.stdout')).write_bytes(result.stdout)
                    (self.work/(name+'.stderr')).write_bytes(result.stderr)
                    (self.work/(name+'.json')).write_text(json.dumps(dict(argv=argv, exit=result.returncode, timeout_seconds=3))+'\n')
                    self.assertEqual(result.returncode, expected)
                    if marker:
                        self.assertIn(b'fixture-marker-error:', result.stderr)
                    if expected == 0:
                        self.assertIn(b'fixture-complete', result.stdout)
                        self.assertEqual(result.stdout.count(b'fixture-bulk:'), 3)
                    else:
                        self.assertNotIn(b'fixture-complete', result.stdout)

    def test_static_collect_keeps_stop_order_original_commands_and_one_budget(self):
        source = (ROOT/'tools/qualify-windows-h264-recipe-A.ps1').read_text()
        body = source.split('function Invoke-ReleaseACollect($Context) {', 1)[1].split('# Dot-source', 1)[0]
        self.assertEqual(body.count('$Context.collectDeadline = [DateTime]::UtcNow.AddSeconds(300)'), 1)
        self.assertLess(body.index('Stop-ReleaseAScopes $Context'), body.index('Invoke-ReleaseAFailedWslExport $Context'))
        self.assertLess(body.index('Invoke-ReleaseAFailedWslExport $Context'), body.index("'全量原件收集'"))
        self.assertIn('-TimeoutSeconds (Get-ReleaseACollectBudget $Context)', body)
        self.assertNotIn('-TimeoutSeconds 300', body)
        self.assertIn('-TimeoutSeconds (Get-ReleaseACollectBudget $Context 20)', source)
        commands = ['if [[ -e /opt/frd-llvm-mingw-20250709 ]]; then cp -a /opt/frd-llvm-mingw-20250709 "$root/toolchain/install"; fi',
                    'python3 "$repo/tools/windows-h264-recipe-transport.py" collect-tools --root "$root"',
                    'if [[ -d "$repo/.codex-target/ffmpeg-8.1.2" ]]; then cp -a "$repo/.codex-target/ffmpeg-8.1.2/." "$root/build-root/"; fi']
        self.assertEqual([source.index(command) for command in commands], sorted(source.index(command) for command in commands))
        for command in commands:
            self.assertEqual(source.count(command), 1)


class PreparedLinuxControlBoundary(unittest.TestCase):
    def test_exact_control_namespaces_accept_only_the_two_released_scopes(self):
        t=load()
        for kind in ('control-normal','control-halt'):
            self.assertTrue(t.allowed('setup/groups/'+kind, True))
            self.assertTrue(t.allowed('setup/'+kind+'-intent.json'))
            self.assertTrue(t.allowed('setup/calls/linux-'+kind, True))
            self.assertTrue(t.allowed('setup/calls/linux-cleanup-'+kind, True))
            self.assertTrue(t.allowed('setup/groups/'+kind+'/'+'a'*32+'.observations.json'))
        self.assertFalse(t.allowed('setup/groups/control-other',True))
        self.assertFalse(t.allowed('setup/control-other-intent.json'))
        self.assertFalse(t.allowed('setup/calls/linux-control-other',True))

    def test_actual_generated_worker_is_syntax_valid_and_non_linux_refuses_before_launch(self):
        import ast,re,types,sys
        source=(ROOT/'tools/qualify-windows-h264-recipe-A.ps1').read_text()
        workers=re.findall(r"(?s)\$worker = @'\n(.*?)\n'@",source)
        self.assertEqual(len(workers),1)
        ast.parse(workers[0])
        if sys.platform != 'linux':
            worker=types.ModuleType('actual_generated_linux_worker')
            exec(compile(workers[0],'<actual generated worker>','exec'),worker.__dict__)
            with self.assertRaisesRegex(RuntimeError,'真实Linux'):
                worker.main()


if __name__ == '__main__':
    unittest.main()
