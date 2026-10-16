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
            git('add', '--all')
            git('commit', '-m', 'fixture')
            source = git('rev-parse', 'HEAD').decode().strip()
            t.initialize(output.parent, source, '1234', '1')
            (repo / 'tools/build-ffmpeg-windows.ps1').unlink()
            git('checkout-index', '--force', '--all')
            self.assertIn(b'\r\n', (repo / 'tools/build-ffmpeg-windows.ps1').read_bytes())
            # 仅对该临时仓库运行新函数；实际项目Git从未调用。
            with mock.patch.dict(os.environ, environment, clear=True):
                expected = t.canonical_checkout(repo, source, output)
            self.assertNotIn(b'\r', (repo / 'tools/build-ffmpeg-windows.ps1').read_bytes())
            for name in t.CHECKOUT:
                blob = git('cat-file', 'blob', source + ':' + name)
                self.assertEqual((repo / name).read_bytes(), blob)
                self.assertEqual(expected[name], digest(blob))
            self.assertEqual((repo / '.git/info/attributes').read_bytes(), b'* -text\n')
            commands = t.read_json(output / 'checkout-commands.json')
            saved_git = digest((output / 'git-executable.bin').read_bytes())
            for call in commands:
                self.assertEqual(call['executable']['before_sha256'], saved_git)
                self.assertEqual(call['executable']['after_sha256'], saved_git)
                self.assertIn('observed', call['main_module'])

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
        self.assertEqual(execute['run'], 'pwsh -NoProfile -File tools/qualify-windows-h264-recipe-A.ps1 -Action BootstrapAptDiagnostic')
        self.assertNotIn('${{', execute['run'])
        active_commands = [step['run'] for step in job['steps'] if 'run' in step and step.get('if') != '${{ false }}']
        self.assertEqual(active_commands, [execute['run']])
        self.assertEqual(collect['if'], '${{ false }}')
        self.assertEqual(collect['run'], 'pwsh -NoProfile -File tools/qualify-windows-h264-recipe-A.ps1 -Action Collect')
        self.assertEqual(upload['if'], '${{ false }}')
        self.assertEqual(upload['with']['compression-level'], '0')
        self.assertEqual(upload['with']['if-no-files-found'], 'error')
        self.assertEqual(upload['with']['path'].splitlines(), ['${{ runner.temp }}/release-a.tar',
                                                             '${{ runner.temp }}/release-a.receipt.json',
                                                             '${{ runner.temp }}/release-a.verification.json'])
        self.assertEqual(forensic['if'], 'always()')
        self.assertEqual(forensic['continue-on-error'], 'true')
        self.assertEqual(forensic['with']['include-hidden-files'], 'true')
        self.assertEqual(forensic['uses'], 'actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02')
        self.assertEqual(forensic['with']['name'], 'release-a-apt-prefix-diagnostic-${{ github.run_id }}-${{ github.run_attempt }}-${{ github.sha }}')
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
        # 诊断只运输当前固定apt前缀；不进入正式POSIX transport allowlist。
        calls = ('wsl-install', 'wsl-version', 'path-to-wsl', 'path-to-windows', 'path-repo',
                 'path-repo-reverse', 'apt-version', 'apt-get-version', 'dpkg-version',
                 'apt-use-pty', 'apt-executables', 'setup', 'linux-cleanup-setup')
        added = [f'frd-release-a-evidence/setup/calls/{call}/{name}' for call in calls for name in call_files]
        added += ['frd-release-a-evidence/' + name for name in (
            'provenance/paths.json', 'setup/prepare.sh', 'setup/os-release',
            'setup/lifecycle-supervisor.py', 'setup/setup-intent.json', 'setup/apt-executables.py',
            'setup/apt-diagnostic-receipts.json', 'setup/lifecycle.json',
            'setup/groups/setup/scope.json', 'setup/groups/setup/halt.json', 'setup/groups/setup/stopped.json')]
        expected += added
        self.assertEqual(actual, ['${{ runner.temp }}/' + name for name in expected])
        self.assertEqual(len(actual), 156)
        self.assertFalse(t.allowed('setup/apt-diagnostic-receipts.json'))
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
