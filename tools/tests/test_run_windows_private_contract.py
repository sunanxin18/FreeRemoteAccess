"""Host-only runner 拒绝控制；无网络、Win32 或 PE 执行。"""
import dataclasses
import hashlib
import importlib.util
import io
import json
import os
import marshal
import struct
import shutil
import subprocess
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[2]
PATH = ROOT / 'tools/run-windows-private-contract.py'
m = None
if PATH.exists():
    spec = importlib.util.spec_from_file_location('runner_controls', PATH)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)


class Response(io.BytesIO):
    def __init__(self, value, status=200, headers=None):
        super().__init__(value)
        self.status, self.headers = status, headers or {}


class Transport:
    def __init__(self, *responses): self.responses, self.requests = list(responses), []
    def open(self, url, headers):
        self.requests.append((url, headers))
        return self.responses.pop(0)


def context():
    return {'GITHUB_ACTIONS': 'true', 'GITHUB_REPOSITORY': 'sunanxin18/FreeRemoteAccess',
            'GITHUB_REPOSITORY_ID': '1338203019', 'GITHUB_SHA': 'a' * 40,
            'GITHUB_RUN_ID': '123', 'GITHUB_RUN_ATTEMPT': '2', 'GITHUB_REF': 'refs/heads/test',
            'GITHUB_EVENT_NAME': 'workflow_dispatch', 'RUNNER_OS': 'Windows', 'RUNNER_ARCH': 'ARM64',
            'FRD_COMPILE_RESULT': 'success', 'FRD_INPUT_ARTIFACT_ID': '456',
            'FRD_INPUT_ARTIFACT_DIGEST': 'b' * 64}


def successful_observation():
    return {'stdout': b'raw', 'stderr': b'', 'wait': 0, 'exit_code': 0,
            'cleanup_confirmed': True, 'first_failure': None, 'creation': {'pid': 42}}


class RunnerControls(unittest.TestCase):
    def setUp(self): self.assertIsNotNone(m, 'missing native runner implementation')

    def test_admitted_binding_read_is_held_identity_not_current_self_report(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            name = 'admitted/task4c5c3b2e-contract-123-2/variant-0-pe-bindings.json'
            path = root/name; path.parent.mkdir(parents=True)
            functions = {'frd_h264_dc_check_abi': {'rva': 0x1000, 'length': 0x450},
                         'frd_h264_dc_wrong_return': {'rva': 0x3000, 'length': 0x10},
                         'frd_h264_dc_outer_probe': {'rva': 0x2000, 'length': 0x430},
                         'frd_h264_dc_abi_sp_failure_fragment': {'rva': 0x1450, 'length': 32}}
            raw = m.encoded({'image_size': 0x5000, 'owned_bindings': {'functions': functions}})
            path.write_bytes(raw)
            seal = {'destination': str(root), 'output_tree': {name: {'kind': 'file', 'size': len(raw), 'sha256': hashlib.sha256(raw).hexdigest()}}}
            expected = {'run_id': 123, 'run_attempt': 2}
            checks = []
            sites = m.admitted_trap_sites(root, seal, expected, lambda: checks.append(1))
            self.assertEqual(checks, [1, 1])
            self.assertEqual(sites, {'image_size': 0x5000, 'functions': functions})
            path.write_bytes(raw.replace(b'12288', b'12292'))
            with self.assertRaises(m.RunnerError):
                m.admitted_trap_sites(root, seal, expected, lambda: None)
            path.write_bytes(raw)
            with self.assertRaises((m.RunnerError, FileNotFoundError)):
                m.admitted_trap_sites(root, seal, {**expected, 'run_attempt': 3}, lambda: None)
            original = root/'original'; path.rename(original); path.symlink_to(original)
            with self.assertRaises(m.RunnerError):
                m.admitted_trap_sites(root, seal, expected, lambda: None)
            path.unlink(); original.rename(path)
            duplicate = b'{"image_size":20480,"image_size":20480}'
            path.write_bytes(duplicate)
            seal['output_tree'][name] = {'kind': 'file', 'size': len(duplicate), 'sha256': hashlib.sha256(duplicate).hexdigest()}
            with self.assertRaises(m.RunnerError):
                m.admitted_trap_sites(root, seal, expected, lambda: None)

    def test_admitted_binding_read_race_and_post_verify_fail_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            name = 'admitted/task4c5c3b2e-contract-123-2/variant-0-pe-bindings.json'
            path = root/name; path.parent.mkdir(parents=True); path.write_bytes(b'{}')
            seal = {'destination': str(root), 'output_tree': {name: {'kind': 'file', 'size': 2, 'sha256': hashlib.sha256(b'{}').hexdigest()}}}
            expected = {'run_id': 123, 'run_attempt': 2}
            real_identity = m.file_identity
            def mutate_after_identity(p):
                identity = real_identity(p); path.write_bytes(b'[]'); return identity
            with patch.object(m, 'file_identity', side_effect=mutate_after_identity), self.assertRaises(m.RunnerError):
                m.admitted_trap_sites(root, seal, expected, lambda: None)
            path.write_bytes(b'{}'); calls = []
            def verify():
                calls.append(1)
                if len(calls) == 2: raise m.RunnerError('post-read-mutation')
            with self.assertRaisesRegex(m.RunnerError, 'post-read-mutation'):
                m.admitted_trap_sites(root, seal, expected, verify)

    def test_fixed_seven_and_first_failure_stop(self):
        seen = []
        def execute(path, argv, timeout):
            seen.append((path, argv, timeout))
            return successful_observation()
        check = []
        def verify(): check.append(1)
        result = m.orchestrate(['pe0', 'pe1', 'pe2', 'pe3', 'pe4'], execute, verify,
                               lambda *args: {'host': 'parser data only'}, lambda *args: None)
        self.assertIsNone(result['first_failure'])
        self.assertEqual([(r[1][2], r[1][4], r[2]) for r in seen], [('register', '0', 30), ('unwind', '0', 30), ('isolated', '0', 120), ('unwind', '1', 30), ('unwind', '2', 30), ('unwind', '3', 30), ('unwind', '4', 30)])
        self.assertEqual(len(check), 14)
        for stop in range(7):
            calls = []
            def broken(path, argv, timeout):
                calls.append(path)
                result = successful_observation()
                if len(calls) == stop + 1: result['exit_code'] = 1
                return result
            result = m.orchestrate(['pe0', 'pe1', 'pe2', 'pe3', 'pe4'], broken, lambda: None, lambda *a: {}, lambda *a: None)
            self.assertEqual(len(calls), stop + 1)
            self.assertEqual([x['status'] for x in result['commands']][stop + 1:], ['not_attempted'] * (6 - stop))
    def test_wait_cleanup_stderr_and_parser_fail_closed(self):
        for key, value in [('wait', 258), ('exit_code', 2), ('cleanup_confirmed', False), ('stderr', b'error'), ('first_failure', {'stage': 'timeout'})]:
            with self.subTest(key=key):
                calls = []
                def execute(*args):
                    calls.append(1)
                    result = successful_observation(); result[key] = value
                    return result
                result = m.orchestrate(['a'] * 5, execute, lambda: None, lambda *a: {}, lambda *a: None)
                self.assertIsNotNone(result['first_failure'])
                self.assertEqual(len(calls), 1)
        def badparser(*args): raise ValueError('bad transcript')
        result = m.orchestrate(['a'] * 5, lambda *a: successful_observation(), lambda: None, badparser, lambda *a: None)
        self.assertEqual(result['first_failure']['stage'], 'parser')
    def test_identity_mutation_before_and_after_stops(self):
        for index in (1, 2, 3):
            calls, checks = [], []
            def verify():
                checks.append(1)
                if len(checks) == index: raise ValueError('changed evidence/source/PE')
            def execute(*args): calls.append(1); return successful_observation()
            result = m.orchestrate(['a'] * 5, execute, verify, lambda *a: {}, lambda *a: None)
            self.assertIsNotNone(result['first_failure'])
            self.assertEqual(len(calls), {1: 0, 2: 1, 3: 1}[index])
    def test_safe_error_categories_survive_without_exception_secret_text(self):
        first = m.failure_observation('download', m.RunnerError('artifact-http-status:403'))
        self.assertEqual(first['code'], 'artifact-http-status:403')
        unsafe = m.failure_observation('download', OSError('TOKEN SECRET https://signed.invalid/?sig=SECRET'))
        self.assertNotIn('SECRET', str(unsafe))
        self.assertNotIn('TOKEN', str(unsafe))

    def test_fixed_action_bare_digest_normalizes_once_and_matches_real_f2_metadata(self):
        environment = context()
        environment['FRD_INPUT_ARTIFACT_DIGEST'] = 'b' * 64  # 固定 action 的实际裸输出。
        expected = m.expected_context(environment)
        self.assertEqual(expected['artifact']['digest'], 'sha256:' + 'b' * 64)
        self.assertEqual(expected['workflow_context']['upload_artifact_digest'],
                         {'raw_action_output': 'b' * 64, 'canonical_rest_digest': 'sha256:' + 'b' * 64,
                          'boundary': 'fixed upload-artifact output bare SHA256 to REST/F2 sha256 digest'})
        f2 = m.source_module(ROOT, 'tools/admit-windows-private-artifact.py',
                             '6d30078d2a12995788266424201e6431231176b942ff4b76ee15a590e42a1f7b', 'fix1_real_f2_digest')
        trusted = {key: value for key, value in expected.items() if key != 'workflow_context'}
        trusted['artifact'] = {**expected['artifact'], 'size_in_bytes': 10}
        metadata = {**trusted['artifact'], 'expired': False,
                    'workflow_run': {'id': 123, 'repository_id': 1338203019, 'head_repository_id': 1338203019, 'head_sha': 'a' * 40}}
        f2.validate_context(trusted, metadata)
        for key, value in [('digest', 'sha256:' + 'c' * 64), ('id', 457), ('size_in_bytes', 11)]:
            bad = {**metadata, key: value}
            with self.subTest(key=key), self.assertRaises(f2.AdmissionError): f2.validate_context(trusted, bad)
        for value in ('sha256:' + 'b' * 64, 'B' * 64, 'b' * 63, 'b' * 65, 'g' * 64, ' b' * 32):
            bad = {**environment, 'FRD_INPUT_ARTIFACT_DIGEST': value}
            with self.subTest(value=value), self.assertRaises(m.RunnerError): m.expected_context(bad)

    def test_main_short_http_eof_preserves_exact_partial_and_failed_terminal(self):
        # 直接执行真实 main 失败路径，只替换网络与 native admission 外部边界。
        # 不执行真实 Win32/PE，不向 CLI 添加 fake/backend 选项。
        class HostAdmission:
            def __init__(self): self.calls, self.machine_queries = [], []
            def admission(self):
                self.admission_observation = {'scope': 'host-only admission boundary double'}
                return self.admission_observation
        actual_source_module = m.source_module
        f2 = actual_source_module(ROOT, 'tools/admit-windows-private-artifact.py',
                                   '6d30078d2a12995788266424201e6431231176b942ff4b76ee15a590e42a1f7b', 'fix1_main_real_f2')
        modules = {'tools/admit-windows-private-artifact.py': f2,
                   'tools/verify-windows-private-transcript.py': SimpleNamespace(),
                   'tools/windows-private-process.py': SimpleNamespace(Win32=HostAdmission)}
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            names = set(m.PINS) | {'tools/run-windows-private-contract.py', 'tools/tests/test_run_windows_private_contract.py',
                                   'tools/tests/test_windows_private_process.py', m.WORKFLOW}
            for name in names:
                destination = root / name; destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes((ROOT / name).read_bytes())
            (root / 'target').mkdir()
            environment = context()
            environment.update(FRD_INPUT_ARTIFACT_DIGEST='b' * 64, GITHUB_TOKEN='HOST-ONLY-TOKEN',
                               SystemRoot=r'C:\Windows', TEMP=r'C:\Temp')
            run = {'id': 123, 'run_attempt': 2, 'head_sha': 'a' * 40, 'repository': {'id': 1338203019, 'full_name': 'sunanxin18/FreeRemoteAccess'},
                   'event': 'workflow_dispatch', 'head_branch': 'test', 'path': m.WORKFLOW}
            jobs = {'total_count': 1, 'jobs': [{'name': 'compile-objects', 'run_id': 123, 'run_attempt': 2, 'head_sha': 'a' * 40,
                                             'status': 'completed', 'conclusion': 'success'}]}
            metadata = {'id': 456, 'name': 'task4c5c3b2f2-input-123-2', 'size_in_bytes': 20, 'digest': 'sha256:' + 'b' * 64, 'expired': False,
                        'workflow_run': {'id': 123, 'repository_id': 1338203019, 'head_repository_id': 1338203019, 'head_sha': 'a' * 40}}
            transport = Transport(*[Response(json.dumps(value).encode()) for value in (run, jobs, metadata)], Response(b'zip-prefix'))
            output = root / 'target/task4c5c3b2f3-native-123-2'
            argv = ['run-windows-private-contract.py', '--source-root', str(root), '--output', str(output)]
            with patch.object(m, '__file__', str(root / 'tools/run-windows-private-contract.py')), patch.object(m.sys, 'argv', argv), \
                    patch.dict(m.os.environ, environment, clear=True), patch.object(m, 'Transport', lambda: transport), \
                    patch.object(m, 'source_module', lambda root, name, pin, label: modules[name]):
                returncode = m.main()
            self.assertEqual(returncode, 1)
            self.assertEqual(len(transport.requests), 4)
            self.assertTrue((output / 'input-outer.partial').exists(), 'actual main must persist the short body')
            partial = (output / 'input-outer.partial').read_bytes()
            self.assertEqual(partial, b'zip-prefix')
            expected_identity = {'size': 10, 'sha256': hashlib.sha256(b'zip-prefix').hexdigest()}
            manifest = json.loads((output / 'members.json').read_bytes())
            self.assertEqual(manifest['files']['input-outer.partial'], expected_identity)
            summary = json.loads((output / 'summary.json').read_bytes())
            self.assertEqual(summary['first_failure'], {'stage': 'download-outer-bytes', 'type': 'RunnerError', 'code': 'artifact-byte-size'})
            self.assertEqual([item['status'] for item in summary['commands']], ['not_attempted'] * 7)
            terminal = json.loads((output / 'terminal-status.json').read_bytes())
            self.assertEqual(terminal['status'], 'failed')
            self.assertEqual(terminal['first_failure'], summary['first_failure'])
            self.assertFalse((output / 'input-outer.zip').exists())
            with zipfile.ZipFile(output / 'evidence.zip') as archive:
                self.assertEqual(archive.read('input-outer.partial'), b'zip-prefix')
            authority = json.loads((output / 'transport-authorities.json').read_bytes())
            self.assertEqual(authority['upload_artifact_digest']['raw_action_output'], 'b' * 64)
            self.assertEqual(authority['upload_artifact_digest']['canonical_rest_digest'], 'sha256:' + 'b' * 64)

    def test_owned_post_exit_failures_preserve_bytes_and_stop_remaining_commands(self):
        path = ROOT / 'tools/tests/test_windows_private_process.py'
        spec = importlib.util.spec_from_file_location('fix1_owned_boundary_fixture', path)
        fixture = importlib.util.module_from_spec(spec)
        exec(compile(path.read_bytes(), str(path), 'exec'), fixture.__dict__)
        for failure, want in [('overflow', b'x' * (1024 * 1024)), ('stderr', b'initialdrain-prefix')]:
            with self.subTest(failure=failure):
                api = fixture.BoundedPostExitAPI(failure)
                launches, saved = [], []
                def execute(exe, argv, timeout):
                    launches.append(argv)
                    return fixture.m.collect_owned(api, exe, argv, 'host-only', {}, timeout)
                def parse(*args): raise AssertionError('failed owner must not reach parser')
                state = m.orchestrate(['pe0', 'pe1', 'pe2', 'pe3', 'pe4'], execute, lambda: {}, parse,
                                      lambda index, item, observation: saved.append(observation))
                self.assertEqual(len(launches), 1)
                self.assertEqual([item['status'] for item in state['commands']], ['failed'] + ['not_attempted'] * 6)
                observation = saved[0]
                self.assertEqual(len(observation['stdout']), len(want))
                self.assertEqual(observation['stdout'], want)
                self.assertEqual(hashlib.sha256(observation['stdout']).hexdigest(), hashlib.sha256(want).hexdigest())
                self.assertTrue(observation['cleanup_confirmed'])
                self.assertEqual(state['first_failure']['process_first_failure'], observation['first_failure'])
                self.assertEqual(observation['first_failure']['stage'], 'stream-overflow' if failure == 'overflow' else 'read-stderr')

    def test_checkout_process_git_keys_restore_exact_lf_bytes_under_crlf_defaults(self):
        # 读取实际 native checkout env，并用于真实临时 Git 过滤；不是字符串存在检查。
        workflow = (ROOT / m.WORKFLOW).read_text()
        native = workflow.split('  native-helper-contract:\n', 1)[1]
        block = native.split('        env:\n', 1)[1].split('        with:\n', 1)[0]
        checkout_environment = {}
        for line in block.splitlines():
            key, value = line.strip().split(':', 1)
            checkout_environment[key] = value.strip().strip("'")
        git = shutil.which('git')
        self.assertIsNotNone(git, 'host control requires installed Git')
        raw = (ROOT / 'tools/verify-windows-private-transcript.py').read_bytes()
        self.assertEqual(hashlib.sha256(raw).hexdigest(), '9860862c965d7ca11bf8b95d63b52c83a961b240accfa2c1301eec999fd58790')
        with tempfile.TemporaryDirectory() as tmp:
            temporary = Path(tmp).resolve(); repository = temporary / 'repository'; repository.mkdir()
            home = temporary / 'home'; home.mkdir()
            defaults = temporary / 'fixture-defaults.config'
            defaults.write_text('[core]\n    autocrlf = false\n    eol = crlf\n')
            # fixture config 仅模拟 Windows native eol；不读取/改写用户或系统配置。
            environment = {'PATH': os.defpath, 'HOME': str(home), 'XDG_CONFIG_HOME': str(home),
                           'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': str(defaults),
                           'GIT_TERMINAL_PROMPT': '0', 'LC_ALL': 'C', 'LANG': 'C'}
            def run_git(arguments, extra=None):
                result = subprocess.run([git, '-C', str(repository), *arguments], env={**environment, **(extra or {})},
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15, check=False)
                self.assertEqual(result.returncode, 0, result.stderr.decode('utf-8', 'replace'))
                return result.stdout
            run_git(['init', '--quiet'])  # 仅这个新临时fixture允许 Git 写操作。
            name = 'tools/verify-windows-private-transcript.py'
            source = repository / name; source.parent.mkdir(); source.write_bytes(raw)
            (repository / '.gitattributes').write_text('*.py text=auto\n')
            run_git(['add', '--', '.gitattributes', name])
            self.assertEqual(run_git(['cat-file', 'blob', ':' + name]), raw)
            crlf = run_git(['cat-file', '--filters', ':' + name],
                           {'GIT_CONFIG_COUNT': '1', 'GIT_CONFIG_KEY_0': 'core.autocrlf', 'GIT_CONFIG_VALUE_0': 'false'})
            restored = run_git(['cat-file', '--filters', ':' + name], checkout_environment)
            self.assertEqual(hashlib.sha256(crlf).hexdigest(), '3a43c8ef0f6dc44c149e4902448da77e5fc11c2a7ec0aef03593ff628e86a242')
            self.assertEqual(crlf.count(b'\r\n'), 436)
            self.assertEqual(hashlib.sha256(restored).hexdigest(), '9860862c965d7ca11bf8b95d63b52c83a961b240accfa2c1301eec999fd58790')
            self.assertEqual(restored, raw)
            self.assertEqual(source.read_bytes(), raw, 'filter observation must not rewrite fixture source')
            print(json.dumps({'scope': 'host-only real Git filter fixture; not failed Windows bytes',
                              'crlf': {'size': len(crlf), 'sha256': hashlib.sha256(crlf).hexdigest()},
                              'workflow_lf': {'size': len(restored), 'sha256': hashlib.sha256(restored).hexdigest()}}))

    def test_main_pin_mismatch_saves_actual_and_expected_source_identities_before_rejecting(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            names = set(m.PINS) | {'tools/run-windows-private-contract.py', 'tools/tests/test_run_windows_private_contract.py',
                                   'tools/tests/test_windows_private_process.py', m.WORKFLOW}
            for name in names:
                destination = root / name; destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes((ROOT / name).read_bytes())
            (root / 'target').mkdir()
            changed_name = 'tools/verify-windows-private-transcript.py'
            changed = root / changed_name
            raw = changed.read_bytes().replace(b'\n', b'\r\n')  # 仅对测试fixture制造已知失配。
            changed.write_bytes(raw)
            environment = context()
            output = root / 'target/task4c5c3b2f3-native-123-2'
            argv = ['run-windows-private-contract.py', '--source-root', str(root), '--output', str(output)]
            with patch.object(m, '__file__', str(root / 'tools/run-windows-private-contract.py')), patch.object(m.sys, 'argv', argv), \
                    patch.dict(m.os.environ, environment, clear=True), \
                    patch.object(m, 'source_module', side_effect=AssertionError('no private module or Win32 backend may be loaded')) as modules, \
                    patch.object(m, 'Transport', side_effect=AssertionError('no network may start')) as transport:
                returncode = m.main()
            self.assertEqual(returncode, 1)
            modules.assert_not_called()
            transport.assert_not_called()
            self.assertTrue((output / 'source-identities.json').exists(), 'actual main must save actual source bytes identity before pin rejection')
            actual_raw = (output / 'source-identities.json').read_bytes()
            expected_raw = (output / 'source-expected-pins.json').read_bytes()
            actual, expected = json.loads(actual_raw), json.loads(expected_raw)
            self.assertEqual(actual[changed_name], {'size': len(raw), 'sha256': hashlib.sha256(raw).hexdigest()})
            self.assertEqual(expected, m.PINS)
            self.assertEqual(expected[changed_name], '9860862c965d7ca11bf8b95d63b52c83a961b240accfa2c1301eec999fd58790')
            self.assertNotEqual(actual[changed_name]['sha256'], expected[changed_name])
            summary = json.loads((output / 'summary.json').read_bytes())
            first = {'stage': 'source-pins', 'type': 'RunnerError', 'code': 'runner-helper-source-pin:' + changed_name}
            self.assertEqual(summary['first_failure'], first)
            self.assertEqual([command['status'] for command in summary['commands']], ['not_attempted'] * 7)
            self.assertFalse(summary['native_commands_complete'])
            terminal = json.loads((output / 'terminal-status.json').read_bytes())
            self.assertEqual(terminal['status'], 'failed')
            self.assertEqual(terminal['first_failure'], first)
            self.assertFalse((output / 'launcher-admission.json').exists())
            self.assertFalse((output / 'input-outer.zip').exists())
            with zipfile.ZipFile(output / 'evidence.zip') as archive:
                self.assertEqual(archive.read('source-identities.json'), actual_raw)
                self.assertEqual(archive.read('source-expected-pins.json'), expected_raw)
            print(json.dumps({'scope': 'host-only actual-main pin rejection fixture', 'actual': actual[changed_name], 'expected_sha256': expected[changed_name]}))

    def test_joint_collect_transient_completes_seven_real_parser_pairs_without_native_authority(self):
        owner_fixture_path = ROOT / 'tools/tests/test_windows_private_process.py'
        spec = importlib.util.spec_from_file_location('fix3_joint_owner_fixture', owner_fixture_path)
        fixture = importlib.util.module_from_spec(spec)
        exec(compile(owner_fixture_path.read_bytes(), str(owner_fixture_path), 'exec'), fixture.__dict__)
        fixture.m = m.source_module(ROOT, 'tools/windows-private-process.py', m.PINS['tools/windows-private-process.py'], 'fix3_pinned_owner')
        transcript_path = ROOT / 'tools/tests/test_verify_windows_private_transcript.py'
        spec = importlib.util.spec_from_file_location('fix3_joint_transcript_fixture', transcript_path)
        transcripts = importlib.util.module_from_spec(spec)
        exec(compile(transcript_path.read_bytes(), str(transcript_path), 'exec'), transcripts.__dict__)
        parser = m.source_module(ROOT, 'tools/verify-windows-private-transcript.py', m.PINS['tools/verify-windows-private-transcript.py'], 'fix3_actual_parser')
        apis, observations = [], []
        def execute(exe, argv, timeout):
            api = fixture.JointCompletionAPI()
            api.stream = transcripts.transcript(argv[2], int(argv[4]))
            apis.append(api)
            return fixture.m.collect_owned(api, exe, argv, 'host-only', {}, timeout)
        state = m.orchestrate(['pe0', 'pe1', 'pe2', 'pe3', 'pe4'], execute, lambda: {}, m.bound_transcript_parser(parser, transcripts.TRAP_SITES),
                              lambda index, item, observation: observations.append(observation))
        self.assertIsNone(state['first_failure'])
        self.assertEqual([item['status'] for item in state['commands']], ['observed-contract'] * 7)
        self.assertEqual([item['normal_timeout_seconds'] for item in observations], [30, 30, 120, 30, 30, 30, 30])
        for api, observation in zip(apis, observations):
            self.assertNotIn('terminate', api.log)
            self.assertEqual(api.exit_queries, 1)
            self.assertTrue(observation['cleanup_confirmed'])
            self.assertEqual(observation['exit_code'], 0)
            self.assertEqual(observation['normal_active_processes'], 1)
            active = [row['active_processes'] for row in observation['normal_observations']]
            self.assertEqual(active[:2], [1, 0])
            self.assertEqual(active[-1], 0)
        self.assertNotIn('terminal-status.json', state)

    def test_joint_collect_failure_or_unconfirmed_cleanup_stops_after_first_parent(self):
        path = ROOT / 'tools/tests/test_windows_private_process.py'
        spec = importlib.util.spec_from_file_location('fix3_joint_negative_fixture', path)
        fixture = importlib.util.module_from_spec(spec)
        exec(compile(path.read_bytes(), str(path), 'exec'), fixture.__dict__)
        fixture.m = m.source_module(ROOT, 'tools/windows-private-process.py', m.PINS['tools/windows-private-process.py'], 'fix3_negative_pinned_owner')
        cases = [('persistent', {'active': (1,)}, 'timeout', True),
                 ('cleanup-unconfirmed', {'active': (1,), 'terminate_clears': False}, 'timeout', False),
                 ('exit-nonzero', {'code': 7}, 'exit', True),
                 ('exit-unknown', {'code': None}, 'exit', True),
                 ('exit-query-error', {'failure': 'exit'}, 'exit', True),
                 ('active-query-error', {'failure': 'active'}, 'active', True),
                 ('wait-failed', {'waits': (0xffffffff, 0)}, 'wait', True)]
        for label, options, stage, cleanup in cases:
            with self.subTest(label=label):
                api = fixture.JointCompletionAPI(**options)
                launches, observations = [], []
                def execute(exe, argv, timeout):
                    launches.append(argv)
                    return fixture.m.collect_owned(api, exe, argv, 'host-only', {}, timeout)
                def parse(*args): raise AssertionError('failed joint completion cannot reach parser')
                state = m.orchestrate(['pe0', 'pe1', 'pe2', 'pe3', 'pe4'], execute, lambda: {}, parse,
                                      lambda index, item, observation: observations.append(observation))
                self.assertEqual(len(launches), 1)
                self.assertEqual([item['status'] for item in state['commands']], ['failed'] + ['not_attempted'] * 6)
                self.assertEqual(state['first_failure']['process_first_failure'], observations[0]['first_failure'])
                self.assertEqual(observations[0]['first_failure']['stage'], stage)
                self.assertIs(observations[0]['cleanup_confirmed'], cleanup)
                self.assertFalse(api.handles)

    def test_context_cannot_take_identity_from_artifact(self):
        self.assertEqual(m.expected_context(context())['run_attempt'], 2)
        for key in context():
            bad = context(); del bad[key]
            with self.subTest(key=key), self.assertRaises(m.RunnerError): m.expected_context(bad)
        for key, value in [('GITHUB_REPOSITORY_ID', '1'), ('RUNNER_ARCH', 'X64'), ('FRD_COMPILE_RESULT', 'failure'), ('GITHUB_RUN_ID', '0123'), ('GITHUB_EVENT_NAME', 'pull_request')]:
            bad = context(); bad[key] = value
            with self.assertRaises(m.RunnerError): m.expected_context(bad)
    def test_redirect_authorization_never_forwarded(self):
        location = 'https://artifact.blob.core.windows.net/container/a?sig=SECRET'
        transport = Transport(Response(b'', 302, {'Location': location}), Response(b'zip'))
        value = m.fetch_artifact(transport, 456, 'TOKEN', 3)
        self.assertEqual(value, b'zip')
        self.assertEqual(transport.requests[0][1]['Authorization'], 'Bearer TOKEN')
        self.assertNotIn('Authorization', transport.requests[1][1])
        for location in ('http://artifact.blob.core.windows.net/a', 'https://evil.invalid/a', 'https://api.github.com:444/a', 'https://user@artifact.blob.core.windows.net/a', 'https://artifact.blob.core.windows.net.evil.invalid/a', 'https://api.github.com/repos/evil/private'):
            transport = Transport(Response(b'', 302, {'Location': location}))
            with self.subTest(location=location), self.assertRaises(m.RunnerError): m.fetch_artifact(transport, 456, 'TOKEN', 3)
            self.assertEqual(len(transport.requests), 1)
    def test_bounded_download_and_errors_do_not_echo_secrets(self):
        for response in (Response(b'1234'), Response(b'SECRET', 403), Response(b'', 302, {'Location': 'SECRET'})):
            with self.assertRaises(m.RunnerError) as error: m.fetch_artifact(Transport(response), 456, 'TOKEN', 3)
            self.assertNotIn('SECRET', str(error.exception))
            self.assertNotIn('TOKEN', str(error.exception))
        transport = Transport(*[Response(b'', 302, {'Location': 'https://a.blob.core.windows.net/a?sig=SECRET'}) for _ in range(5)])
        with self.assertRaises(m.RunnerError): m.fetch_artifact(transport, 456, 'TOKEN', 3)
        self.assertEqual(len(transport.requests), 4)
    def test_download_read_failure_retains_bounded_raw_partial_bytes(self):
        class BrokenStream(Response):
            def __init__(self): super().__init__(b'zip-prefix'); self.reads = 0
            def read(self, count):
                self.reads += 1
                if self.reads == 2: raise OSError('signed SECRET URL')
                return super().read(min(count, 4))
        with self.assertRaises(m.RunnerError) as error:
            m.fetch_artifact(Transport(BrokenStream()), 456, 'TOKEN', 20)
        self.assertEqual(error.exception.partial, b'zip-')
        self.assertNotIn('SECRET', str(error.exception))

    def test_same_attempt_run_and_job_observations(self):
        expected = m.expected_context(context())
        run = {'id': 123, 'run_attempt': 2, 'head_sha': 'a' * 40, 'repository': {'id': 1338203019, 'full_name': 'sunanxin18/FreeRemoteAccess'}, 'event': 'workflow_dispatch', 'head_branch': 'test', 'path': '.github/workflows/qualify-windows-private-abi.yml'}
        jobs = {'total_count': 1, 'jobs': [{'name': 'compile-objects', 'run_id': 123, 'run_attempt': 2, 'head_sha': 'a' * 40, 'status': 'completed', 'conclusion': 'success'}]}
        m.validate_run(expected, run, jobs)
        for key in ('id', 'run_attempt', 'head_sha', 'event', 'head_branch', 'path'):
            bad = dict(run); bad[key] = 'wrong'
            with self.assertRaises(m.RunnerError): m.validate_run(expected, bad, jobs)
        jobs['jobs'][0]['conclusion'] = 'failure'
        with self.assertRaises(m.RunnerError): m.validate_run(expected, run, jobs)
    def test_exact_source_bytes_ignore_poisoned_pyc(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            source = b'VALUE = 7\n'; (root / 'helper.py').write_bytes(source)
            cache = root / '__pycache__'; cache.mkdir()
            stamp = int((root / 'helper.py').stat().st_mtime)
            pyc = importlib.util.MAGIC_NUMBER + struct.pack('<III', 0, stamp, len(source)) + marshal.dumps(compile(b'VALUE = 99\n', str(root / 'helper.py'), 'exec'))
            Path(importlib.util.cache_from_source(str(root / 'helper.py'))).write_bytes(pyc)
            # 先证明这是真正可替换的有效缓存；普通 loader 会执行99。
            poison_spec = importlib.util.spec_from_file_location('poison_control', root / 'helper.py')
            poison_module = importlib.util.module_from_spec(poison_spec)
            poison_spec.loader.exec_module(poison_module)
            self.assertEqual(poison_module.VALUE, 99)
            module = m.source_module(root, 'helper.py', hashlib.sha256(source).hexdigest(), 'source_bytes_control')
            self.assertEqual(module.VALUE, 7)
            with self.assertRaises(m.RunnerError): m.source_module(root, 'helper.py', '0' * 64, 'refused')
    def test_fresh_output_and_immutable_record(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp).resolve(); out = base / 'fresh'
            m.fresh_directory(out)
            with self.assertRaises(m.RunnerError): m.fresh_directory(out)
            ledger = m.Ledger(out); ledger.write('raw.stdout', b'first')
            (out / 'raw.stdout').write_bytes(b'changed')
            with self.assertRaises(m.RunnerError): ledger.check()
            link = base / 'link'; link.symlink_to(out, target_is_directory=True)
            with self.assertRaises(m.RunnerError): m.fresh_directory(link / 'child')
    def test_archive_and_terminal_never_publish_success_on_finalization_error(self):
        for fault in ('mutation', 'archive', 'rename', 'extra'):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as tmp:
                out = Path(tmp).resolve(); ledger = m.Ledger(out); ledger.write('raw.stdout', b'raw')
                ledger.write_json('summary.json', {'status': 'packaging-pending', 'commands': [1] * 7})
                if fault == 'mutation': (out / 'raw.stdout').write_bytes(b'mutated')
                if fault == 'extra': (out / 'unexpected').write_bytes(b'extra')
                def broken(*args, **kwargs): raise OSError('disk')
                target = 'os.rename' if fault == 'rename' else 'zipfile.ZipFile' if fault == 'archive' else None
                if target:
                    with patch(target, broken), self.assertRaises(Exception): m.finalize(ledger, False, {'stage': 'host-failure'})
                else:
                    with self.assertRaises(Exception): m.finalize(ledger, False, {'stage': 'host-failure'})
                self.assertFalse((out / 'terminal-status.json').exists())
    def test_external_input_mutation_during_archive_blocks_terminal(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp).resolve(); ledger = m.Ledger(out)
            ledger.write_json('summary.json', {'status': 'packaging-pending'})
            checks = []
            def verify_inputs():
                checks.append(1)
                if len(checks) == 2: raise m.RunnerError('source-mutated-during-packaging')
            with self.assertRaises(m.RunnerError):
                m.finalize(ledger, False, {'stage': 'earlier-failure'}, check_inputs=verify_inputs)
            self.assertEqual(len(checks), 2)
            self.assertFalse((out / 'terminal-status.json').exists())

    def test_failed_final_archive_exact_members_and_binding(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp).resolve(); ledger = m.Ledger(out)
            ledger.write('raw.stdout', b'raw\r\n'); ledger.write_json('summary.json', {'status': 'packaging-pending'})
            m.finalize(ledger, False, {'stage': 'host-only-refusal'})
            terminal = json.loads((out / 'terminal-status.json').read_bytes())
            self.assertEqual(terminal['status'], 'failed')
            self.assertEqual(terminal['summary']['sha256'], hashlib.sha256((out / 'summary.json').read_bytes()).hexdigest())
            with zipfile.ZipFile(out / 'evidence.zip') as z:
                self.assertEqual(set(z.namelist()), {'raw.stdout', 'summary.json', 'members.json'})
                self.assertEqual(z.read('raw.stdout'), b'raw\r\n')
    def test_each_command_retains_identity_observations(self):
        identities = {'pe': {'sha256': 'a' * 64}, 'source': {'sha256': 'b' * 64}}
        state = m.orchestrate(['a'] * 5, lambda *a: successful_observation(), lambda: identities,
                              lambda *a: {}, lambda *a: None)
        for command in state['commands']:
            self.assertEqual(command['identities_before'], identities)
            self.assertEqual(command['identities_after'], identities)

    def test_real_f1_parser_and_bytes_dataclasses_integrate_without_native_authority(self):
        fixture_path = ROOT / 'tools/tests/test_verify_windows_private_transcript.py'
        spec = importlib.util.spec_from_file_location('f1_host_fixture_controls', fixture_path)
        fixture = importlib.util.module_from_spec(spec)
        exec(compile(fixture_path.read_bytes(), str(fixture_path), 'exec'), fixture.__dict__)
        parser = m.source_module(ROOT, 'tools/verify-windows-private-transcript.py',
                                 m.PINS['tools/verify-windows-private-transcript.py'], 'f1_direct_bytes_controls')
        def execute(path, argv, timeout):
            result = successful_observation()
            result['stdout'] = fixture.transcript(argv[2], int(argv[4]))
            return result
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            name = 'admitted/task4c5c3b2e-contract-123-2/variant-0-pe-bindings.json'
            path = root/name; path.parent.mkdir(parents=True)
            raw = m.encoded({'image_size': 0x5000, 'owned_bindings': {'functions': fixture.TRAP_SITES['functions']}})
            path.write_bytes(raw)
            seal = {'destination': str(root), 'output_tree': {name: {'kind': 'file', 'size': len(raw), 'sha256': hashlib.sha256(raw).hexdigest()}}}
            sites = m.admitted_trap_sites(root, seal, {'run_id': 123, 'run_attempt': 2}, lambda: {})
            parse = m.bound_transcript_parser(parser, sites)
            sites['functions']['frd_h264_dc_wrong_return']['rva'] = 0x1000
            # closure 持有已检查副本，不能从后来修改的输入学习期望。
            observed = m.orchestrate(['a'] * 5, execute, lambda: {}, parse, lambda *a: None)
        self.assertIsNone(observed['first_failure'])
        rendered = json.loads(m.encoded(observed))
        child = rendered['commands'][2]['parser']['children'][0]
        self.assertEqual(child['stdout']['encoding'], 'hex')
        self.assertEqual(bytes.fromhex(child['stdout']['hex']), fixture.admission(0, b'\r\n') + fixture.line('FRD1 ready case=preserve variant=0') + fixture.line('FRD1 returned case=preserve mask=0000000000000000', b'\r\n') + fixture.line('FRD1 result case=preserve outcome=normal-control'))
        self.assertNotIn('terminal-status.json', observed)

    def test_model_results_cannot_mint_native_success_terminal(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp).resolve(); ledger = m.Ledger(out)
            ledger.write_json('summary.json', {'schema': m.SCHEMA, 'status': 'packaging-pending',
                                             'native_commands_complete': True, 'first_failure': None,
                                             'commands': [{'suite': suite, 'variant': variant, 'status': 'observed-contract'}
                                                          for suite, variant in [('register', 0), ('unwind', 0), ('isolated', 0), ('unwind', 1), ('unwind', 2), ('unwind', 3), ('unwind', 4)]]})
            with self.assertRaises(m.RunnerError): m.finalize(ledger, True, None)
            self.assertFalse((out / 'terminal-status.json').exists())

    def test_success_finalization_refuses_incomplete_or_failed_seven(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp).resolve(); ledger = m.Ledger(out)
            ledger.write_json('summary.json', {'schema': m.SCHEMA, 'status': 'packaging-pending',
                                             'native_commands_complete': True, 'first_failure': None,
                                             'commands': [{'status': 'observed-contract'}]})
            with self.assertRaises(m.RunnerError): m.finalize(ledger, True, None)
            self.assertFalse((out / 'terminal-status.json').exists())

    def test_minimal_child_environment_never_inherits_token(self):
        environment = {'SystemRoot': 'C:\\Windows', 'TEMP': 'C:\\Temp', 'GITHUB_TOKEN': 'SECRET', 'FRD_PASSWORD': 'SECRET', 'PATH': 'evil'}
        child = m.child_environment(environment)
        self.assertEqual(set(child), {'SystemRoot', 'WINDIR', 'TEMP', 'TMP', 'PATH'})
        self.assertNotIn('SECRET', str(child))
        self.assertEqual(child['PATH'], 'C:\\Windows\\System32;C:\\Windows')

if __name__ == '__main__': unittest.main()
