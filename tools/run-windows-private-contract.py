#!/usr/bin/env python3
"""同 run Windows ARM64 nativehelper gate；唯一 CLI 不提供 mock/PE/argv 选择。

固定七命令仅证明 ABI-contract-only/nativehelper-control。full contexts、child
模块文件/provider、shipping/qmul/codec/perf/package 仍未授予。调用者认证、原始 ZIP、
F2 seal、真实 Win32 owner、F1 bytes 与封装终态在这里串联。
"""
import argparse
import dataclasses
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import sys
import time
from types import ModuleType
import urllib.error
import urllib.parse
import urllib.request
import zipfile

REPOSITORY = 'sunanxin18/FreeRemoteAccess'
REPOSITORY_ID = 1338203019
API_BASE = 'https://api.github.com/repos/' + REPOSITORY
SCHEMA = 'freeremotedesk.windows.private-native.v1'
WORKFLOW = '.github/workflows/qualify-windows-private-abi.yml'
PINS = {
    'tools/verify-windows-private-transcript.py': '8045ed01f408bd448755e9008783f0dfbda6d0ba27a91d0cb731e351a438e1d2',
    'tools/admit-windows-private-artifact.py': 'bb87afb53d7fad941519470c9be1184576a225a64f37a8dcd7c8bfc833ab7689',
    'tools/windows-private-process.py': 'efe57549a64a3c7418002c97d81ddd9af293b86e4ebaf824bc0a88081d2f5fb0',
}
COMMANDS = (('register', 0), ('unwind', 0), ('isolated', 0), ('unwind', 1), ('unwind', 2), ('unwind', 3), ('unwind', 4))
MAX_DOWNLOAD = 96 * 1024 * 1024
MAX_JSON = 1024 * 1024
MAX_PACKAGE = 768 * 1024 * 1024


class RunnerError(Exception):
    """安全错误类别；网络异常/带签名 URL/headers 永不回显。"""


def need(condition, code):
    if not condition: raise RunnerError(code)


def failure_observation(stage, error):
    result = {'stage': stage, 'type': type(error).__name__}
    if type(error) is RunnerError:
        result['code'] = str(error)  # 仅本模块固定类别；底层异常文本/URL/headers 永不进入。
    return result


def digest(value): return hashlib.sha256(value).hexdigest()


def jsonable(value):
    if dataclasses.is_dataclass(value): return jsonable(dataclasses.asdict(value))
    if isinstance(value, bytes): return {'encoding': 'hex', 'hex': value.hex(), 'size': len(value), 'sha256': digest(value)}
    if isinstance(value, (tuple, list)): return [jsonable(item) for item in value]
    if isinstance(value, dict): return {key: jsonable(item) for key, item in value.items()}
    return value


def encoded(value): return (json.dumps(jsonable(value), indent=2, sort_keys=True) + '\n').encode()


def strict_json(raw):
    def pairs(items):
        result = {}
        for key, value in items:
            need(key not in result, 'api-json-duplicate-key')
            result[key] = value
        return result
    def invalid(value): raise RunnerError('api-json-invalid-number')
    try:
        need(type(raw) is bytes and len(raw) <= MAX_JSON, 'api-json-size')
        result = json.loads(raw.decode('utf-8'), object_pairs_hook=pairs, parse_float=invalid, parse_constant=invalid)
        need(type(result) is dict, 'api-json-object')
        return result
    except (ValueError, UnicodeError, RecursionError): raise RunnerError('api-json-invalid') from None


def ordinary(path, directory=False):
    path = Path(path)
    need(path.is_absolute() and path == path.resolve(strict=True), 'ordinary-canonical-path')
    for parent in (path, *path.parents):
        info = parent.lstat()
        need(not stat.S_ISLNK(info.st_mode) and not getattr(info, 'st_file_attributes', 0) & 0x400, 'symlink-reparse-refused')
    info = path.stat()
    need(stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode), 'ordinary-path-type')
    need(directory or info.st_nlink == 1, 'hardlink-refused')
    return path


def file_identity(path):
    ordinary(path)
    with Path(path).open('rb') as stream: sha = hashlib.file_digest(stream, 'sha256').hexdigest()
    return {'size': Path(path).stat().st_size, 'sha256': sha}


def fresh_directory(path):
    need(path.is_absolute() and path.name not in ('', '.', '..'), 'fresh-absolute-path')
    ordinary(path.parent, True)
    need(path == path.parent.resolve(strict=True) / path.name, 'fresh-canonical-path')
    need(not path.exists() and not path.is_symlink(), 'output-already-exists')
    path.mkdir()
    ordinary(path, True)


def source_module(root, name, pin, label):
    path = ordinary(root / name)
    raw = path.read_bytes()
    need(digest(raw) == pin, 'source-bytes-pin:' + name)
    module = ModuleType(label)
    module.__file__ = str(path)
    # dataclasses 读取 __module__；注册同一直接编译模块，不调用任何缓存 loader。
    sys.modules[label] = module
    exec(compile(raw, str(path), 'exec'), module.__dict__)
    need(file_identity(path)['sha256'] == pin, 'source-import-mutated:' + name)
    return module


def expected_context(environment):
    def get(key):
        value = environment.get(key)
        need(type(value) is str and bool(value), 'missing-context:' + key)
        return value
    def number(key):
        value = get(key)
        need(re.fullmatch('[1-9][0-9]{0,18}', value) is not None and int(value) < 2**63, 'context-integer:' + key)
        return int(value)
    need(get('GITHUB_ACTIONS') == 'true', 'not-github-actions')
    need(get('GITHUB_REPOSITORY') == REPOSITORY and number('GITHUB_REPOSITORY_ID') == REPOSITORY_ID, 'repository-context')
    need(get('RUNNER_OS') == 'Windows' and get('RUNNER_ARCH') == 'ARM64', 'runner-label-context')
    need(get('FRD_COMPILE_RESULT') == 'success', 'compile-needs-failed')
    sha = get('GITHUB_SHA')
    need(re.fullmatch('[0-9a-f]{40}', sha) is not None, 'source-sha-context')
    run, attempt = number('GITHUB_RUN_ID'), number('GITHUB_RUN_ATTEMPT')
    artifact_id, raw_artifact_digest = number('FRD_INPUT_ARTIFACT_ID'), get('FRD_INPUT_ARTIFACT_DIGEST')
    # 固定 upload action 输出裸64hex；只在此入口转换成 REST/F2 的 sha256: 格式。
    need(re.fullmatch('[0-9a-f]{64}', raw_artifact_digest) is not None, 'artifact-digest-context')
    artifact_digest = 'sha256:' + raw_artifact_digest
    ref, event = get('GITHUB_REF'), get('GITHUB_EVENT_NAME')
    need(ref.startswith('refs/heads/') and len(ref) > 11 and not any(ord(c) < 32 for c in ref), 'ref-context')
    need(event in ('push', 'workflow_dispatch'), 'event-context')
    return {'repository': REPOSITORY, 'repository_id': REPOSITORY_ID, 'source_sha': sha, 'run_id': run,
            'run_attempt': attempt, 'compile_job_result': 'success',
            'artifact': {'id': artifact_id, 'digest': artifact_digest, 'name': 'task4c5c3b2f2-input-%d-%d' % (run, attempt)},
            'workflow_context': {'ref': ref, 'event': event, 'runner_os': 'Windows', 'runner_arch': 'ARM64', 'workflow': WORKFLOW,
                                 'upload_artifact_digest': {'raw_action_output': raw_artifact_digest, 'canonical_rest_digest': artifact_digest,
                                                            'boundary': 'fixed upload-artifact output bare SHA256 to REST/F2 sha256 digest'}}}


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl): return None


class Transport:
    """不使用环境代理；拒绝 urllib 自动重定向，header 权限由每跳重新构造。"""
    def __init__(self):
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), _NoRedirect())
    def open(self, url, headers):
        try:
            return self.opener.open(urllib.request.Request(url, headers=headers), timeout=15)
        except urllib.error.HTTPError as response:
            return response
        except Exception:
            raise RunnerError('https-transport-failed') from None


def safe_storage(location):
    try:
        parsed = urllib.parse.urlsplit(location)
        host = parsed.hostname or ''
        need(parsed.scheme == 'https' and parsed.port in (None, 443) and not parsed.username and not parsed.password and not parsed.fragment,
             'artifact-redirect-scheme-authority')
        need(host.endswith('.blob.core.windows.net') or host.endswith('.githubusercontent.com'), 'artifact-redirect-host')
        need(parsed.path.startswith('/') and not any(ord(c) < 33 or ord(c) > 126 for c in location) and '\\' not in location,
             'artifact-redirect-encoding')
        return location
    except (ValueError, TypeError): raise RunnerError('artifact-redirect-invalid') from None


def bounded_read(response, cap, deadline):
    chunks, size = [], 0
    try:
        while True:
            need(time.monotonic() < deadline, 'https-time-bound')
            try: value = response.read(min(65536, cap - size + 1))
            except Exception: raise RunnerError('https-read-failed') from None
            need(type(value) is bytes, 'https-read-type')
            if not value: return b''.join(chunks)
            remaining = cap - size
            chunks.append(value[:remaining])
            size += len(value)
            need(size <= cap, 'https-size-bound')
    except RunnerError as error:
        error.partial = b''.join(chunks)  # 原始响应 body 的有界前缀；绝无 header/Location。
        raise


def request_bytes(transport, url, token, cap):
    need(url.startswith(API_BASE + '/actions/'), 'fixed-api-endpoint')
    headers = {'Authorization': 'Bearer ' + token, 'Accept': 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28', 'User-Agent': 'FreeRemoteDesk-private-native-gate'}
    try:
        with transport.open(url, headers) as response:
            need(response.status == 200, 'api-http-status:%s' % int(response.status))
            return bounded_read(response, cap, time.monotonic() + 60)
    except RunnerError: raise
    except Exception: raise RunnerError('api-request-failed') from None


def fetch_artifact(transport, artifact_id, token, expected_size):
    need(type(artifact_id) is int and 0 < artifact_id < 2**63, 'artifact-id')
    need(type(expected_size) is int and 0 < expected_size <= MAX_DOWNLOAD, 'artifact-size')
    url = API_BASE + '/actions/artifacts/%d/zip' % artifact_id
    deadline = time.monotonic() + 60
    for hop in range(4):
        headers = {'User-Agent': 'FreeRemoteDesk-private-native-gate'}
        if hop == 0:
            headers.update({'Authorization': 'Bearer ' + token, 'Accept': 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28'})
        try:
            with transport.open(url, headers) as response:
                if response.status in (301, 302, 303, 307, 308):
                    need(hop < 3, 'artifact-redirect-limit')
                    # Location 是敏感签名传输数据，仅在内存用于下一跳，不入材料/异常。
                    url = safe_storage(response.headers.get('Location', ''))
                    need(time.monotonic() < deadline, 'https-time-bound')
                    continue
                need(response.status == 200, 'artifact-http-status:%s' % int(response.status))
                raw = bounded_read(response, expected_size, deadline)
                if len(raw) != expected_size:
                    error = RunnerError('artifact-byte-size')
                    error.partial = raw  # HTTP200 提前 EOF 仍须沿 main 的统一路径保存有界 body。
                    raise error
                return raw
        except RunnerError: raise
        except Exception: raise RunnerError('artifact-request-failed') from None
    raise RunnerError('artifact-redirect-limit')


def validate_run(expected, run, jobs):
    def same(a, b): return type(a) is type(b) and a == b
    for key, value in {'id': expected['run_id'], 'run_attempt': expected['run_attempt'], 'head_sha': expected['source_sha'],
                       'event': expected['workflow_context']['event'], 'head_branch': expected['workflow_context']['ref'][11:], 'path': WORKFLOW}.items():
        need(same(run.get(key), value), 'run-api-context:' + key)
    repo = run.get('repository', {})
    need(same(repo.get('id'), REPOSITORY_ID) and same(repo.get('full_name'), REPOSITORY), 'run-api-repository')
    need(type(jobs.get('total_count')) is int and 0 < jobs['total_count'] <= 100 and type(jobs.get('jobs')) is list and len(jobs['jobs']) == jobs['total_count'], 'jobs-api-complete-page')
    matching = [job for job in jobs['jobs'] if job.get('name') == 'compile-objects']
    need(len(matching) == 1, 'compile-job-unique')
    job = matching[0]
    for key, value in {'run_id': expected['run_id'], 'run_attempt': expected['run_attempt'], 'head_sha': expected['source_sha'], 'status': 'completed', 'conclusion': 'success'}.items():
        need(same(job.get(key), value), 'compile-job-api-context:' + key)
    # workflow 此时仍在执行 native job；不要求整个 run 已成功。


def child_environment(environment):
    system = environment.get('SystemRoot') or environment.get('SYSTEMROOT')
    temp = environment.get('TEMP')
    need(type(system) is str and bool(re.fullmatch(r'[A-Za-z]:\\[^\x00\r\n]*', system)), 'systemroot-environment')
    need(type(temp) is str and bool(re.fullmatch(r'[A-Za-z]:\\[^\x00\r\n]*', temp)), 'temp-environment')
    return {'SystemRoot': system, 'WINDIR': system, 'TEMP': temp, 'TMP': temp,
            'PATH': system.rstrip('\\') + '\\System32;' + system}


class Ledger:
    """创建时冻结原 bytes 身份；不会重新学习变更文件作为期望。"""
    def __init__(self, root):
        self.root, self.files = ordinary(root, True), {}
    def member(self, name):
        need(type(name) is str and name and '\\' not in name and ':' not in name and not name.startswith('/'), 'evidence-member-name')
        need(all(part not in ('', '.', '..') for part in name.split('/')), 'evidence-member-traversal')
        path = self.root / name
        need(path.is_relative_to(self.root), 'evidence-member-root')
        return path
    def write(self, name, raw):
        need(name not in self.files and type(raw) is bytes, 'evidence-overwrite-or-type')
        path = self.member(name)
        path.parent.mkdir(parents=True, exist_ok=True)
        ordinary(path.parent, True)
        with path.open('xb') as stream:
            stream.write(raw); stream.flush(); os.fsync(stream.fileno())
        self.files[name] = {'size': len(raw), 'sha256': digest(raw)}
    def write_json(self, name, value): self.write(name, encoded(value))
    def adopt(self, name, identity):
        need(name not in self.files, 'evidence-adopt-duplicate')
        self.member(name)
        need(set(identity) == {'size', 'sha256'}, 'evidence-adopt-identity')
        self.files[name] = dict(identity)
    def check(self, *, extras=()):
        ordinary(self.root, True)
        found = set()
        for path in self.root.rglob('*'):
            name = path.relative_to(self.root).as_posix()
            if path.is_dir():
                ordinary(path, True)
                need(any(n.startswith(name + '/') for n in (*self.files, *extras)), 'extra-evidence-directory')
                continue
            ordinary(path)
            found.add(name)
            if name in self.files: need(file_identity(path) == self.files[name], 'immutable-evidence-mutated:' + name)
        need(found == set(self.files) | set(extras), 'evidence-exact-files')
        return True


def admitted_trap_sites(artifact_root, seal, expected, verify):
    """只读取 caller-held F2 seal 固定的同 run variant0 数据；不解析/执行 PE。"""
    verify()
    root = ordinary(artifact_root, True)
    need(seal['destination'] == str(root), 'trap-binding-destination')
    run, attempt = expected['run_id'], expected['run_attempt']
    need(type(run) is int and run > 0 and type(attempt) is int and attempt > 0, 'trap-binding-run')
    name = 'admitted/task4c5c3b2e-contract-%d-%d/variant-0-pe-bindings.json' % (run, attempt)
    identity = seal['output_tree'].get(name)
    need(type(identity) is dict and set(identity) == {'kind', 'size', 'sha256'} and identity['kind'] == 'file' and
         type(identity['size']) is int and 0 < identity['size'] <= MAX_JSON and
         type(identity['sha256']) is str and re.fullmatch('[0-9a-f]{64}', identity['sha256']) is not None,
         'trap-binding-held-identity')
    pinned = {key: identity[key] for key in ('size', 'sha256')}
    path = ordinary(root / name)
    need(file_identity(path) == pinned, 'trap-binding-before-identity')
    with path.open('rb') as stream: raw = stream.read(pinned['size'] + 1)
    need(len(raw) == pinned['size'] and digest(raw) == pinned['sha256'], 'trap-binding-read-identity')
    need(file_identity(path) == pinned, 'trap-binding-after-identity')
    verify()
    binding = strict_json(raw)
    functions = binding['owned_bindings']['functions']
    return {'image_size': binding['image_size'],
            'functions': {name: {key: functions[name][key] for key in ('rva', 'length')}
                          for name in ('frd_h264_dc_check_abi', 'frd_h264_dc_wrong_return', 'frd_h264_dc_outer_probe')}}


def bound_transcript_parser(transcript, sites):
    # F1 校验并复制纯数据；offset/opcode 只由受信 F1 源码固定。
    held = transcript._trap_sites(sites)
    def parse(stdout, stderr, suite, variant, returncode):
        return transcript.parse_transcript(stdout, stderr, suite, variant, returncode,
                                           expected_trap_sites=held if suite == 'isolated' else None)
    return parse


def orchestrate(pe_paths, execute, verify, parse, save):
    """中立串行核心；不会发布 native authority，注入界面不暴露到 CLI。"""
    need(type(pe_paths) is list and len(pe_paths) == 5, 'five-admitted-pe-paths')
    result = {'commands': [{'suite': suite, 'variant': variant, 'status': 'not_attempted'} for suite, variant in COMMANDS], 'first_failure': None}
    for index, item in enumerate(result['commands']):
        suite, variant = item['suite'], item['variant']
        stage, observation = 'before-identities', None
        try:
            item['identities_before'] = verify()
            item['status'] = 'started'
            argv = [pe_paths[variant], '--suite', suite, '--expect-variant', str(variant)]
            stage = 'process'
            observation = execute(pe_paths[variant], argv, 120 if suite == 'isolated' else 30)
            stage = 'after-identities'
            item['identities_after'] = verify()
            stage = 'process-observations'
            need(observation.get('first_failure') is None and observation.get('wait') == 0 and type(observation.get('wait')) is int and
                 observation.get('exit_code') == 0 and type(observation.get('exit_code')) is int and
                 observation.get('cleanup_confirmed') is True and type(observation.get('creation', {}).get('pid')) is int and observation['creation']['pid'] > 0,
                 'parent-wait-exit-cleanup')
            need(observation['stderr'] == b'', 'parent-stderr')
            stage = 'parser'
            parsed = parse(observation['stdout'], observation['stderr'], suite, variant, observation['exit_code'])
            item['status'] = 'observed-contract'
            item['parser'] = parsed
        except Exception as error:
            item['status'] = 'failed'
            failure = {'command_index': index, **failure_observation(stage, error)}
            if observation and observation.get('first_failure') is not None: failure['process_first_failure'] = observation['first_failure']
            result['first_failure'] = failure
            item['failure'] = failure
        try:
            save(index, item, observation)
        except Exception as error:
            if result['first_failure'] is None:
                result['first_failure'] = {'command_index': index, **failure_observation('save-evidence', error)}
            item['status'] = 'failed'
        if result['first_failure'] is not None: break
    return result


class _NativeAuthority:
    """只由真实入口在七项完成后构造；模型回调返回值无法自行授予终态。"""
    def __init__(self, root, launcher, ledger):
        need(os.name == 'nt', 'native-authority-windows-required')
        module = source_module(root, 'tools/windows-private-process.py', PINS['tools/windows-private-process.py'], 'frd_terminal_native_owner')
        backend = module.Win32()
        try:
            self.observation = backend.admission()
        except Exception as error:
            # API 首错先保存在材料；保存自身失败也不替换 native API 拒绝类别。
            failure = failure_observation('terminal-native-admission', error)
            try:
                ledger.write_json('terminal-native-admission.json', {'observation': getattr(backend, 'admission_observation', None),
                                                                   'api_calls': backend.calls, 'machine_queries': backend.machine_queries, 'first_failure': failure})
            except Exception: pass
            raise RunnerError('terminal-native-admission-rejected:' + type(error).__name__) from None
        ledger.write_json('terminal-native-admission.json', {'observation': self.observation, 'api_calls': backend.calls, 'machine_queries': backend.machine_queries})
        need(self.observation['pe'] == launcher['pe'], 'terminal-launcher-pe-identity')


def finalize(ledger, native_success, first_failure, *, authority=None, check_inputs=None):
    """失败也可封装；成功由 CLI 的真实 native 路径传入。最后一步才 rename authority。"""
    ledger.check()
    if check_inputs is not None: check_inputs()
    need(native_success is False or first_failure is None, 'success-first-failure-conflict')
    original = dict(ledger.files)
    need('summary.json' in original, 'summary-required')
    if native_success:
        need(os.name == 'nt' and type(authority) is _NativeAuthority and callable(check_inputs), 'native-authority-required')
        summary = json.loads((ledger.root / 'summary.json').read_bytes())
        commands = summary.get('commands')
        need(summary.get('schema') == SCHEMA and summary.get('status') == 'packaging-pending' and
             summary.get('native_commands_complete') is True and summary.get('first_failure') is None and
             type(commands) is list and len(commands) == 7 and
             [(item.get('suite'), item.get('variant')) for item in commands] == list(COMMANDS) and
             all(item.get('status') == 'observed-contract' for item in commands), 'native-seven-summary-required')
    ledger.write_json('members.json', {'schema': SCHEMA + '.members', 'files': original})
    members = dict(ledger.files)
    need(sum(item['size'] for item in members.values()) <= MAX_PACKAGE, 'evidence-package-size')
    archive_path = ledger.root / 'evidence.zip'
    with archive_path.open('xb') as stream:
        with zipfile.ZipFile(stream, 'w', zipfile.ZIP_DEFLATED, compresslevel=6, allowZip64=True) as archive:
            for name in sorted(members): archive.write(ledger.root / name, name)
        stream.flush(); os.fsync(stream.fileno())
    ledger.check(extras=('evidence.zip',))
    # 独立重新读每个 archive member，不信写入成功；小块验证，不解压执行。
    with zipfile.ZipFile(archive_path) as archive:
        names = archive.namelist()
        need(len(names) == len(members) and set(names) == set(members), 'archive-exact-members')
        for info in archive.infolist():
            need(not info.is_dir() and info.file_size == members[info.filename]['size'], 'archive-member-size')
            hasher, size = hashlib.sha256(), 0
            with archive.open(info) as stream:
                while True:
                    raw = stream.read(65536)
                    if not raw: break
                    size += len(raw); need(size <= members[info.filename]['size'], 'archive-member-bound')
                    hasher.update(raw)
            need(size == members[info.filename]['size'] and hasher.hexdigest() == members[info.filename]['sha256'], 'archive-member-hash')
    archive_identity = file_identity(archive_path)
    ledger.adopt('evidence.zip', archive_identity)
    ledger.write_json('archive.json', {'schema': SCHEMA + '.archive', **archive_identity,
                                      'members': members, 'members_identity': ledger.files['members.json'], 'archive_verified': True})
    ledger.check()
    if check_inputs is not None: check_inputs()
    terminal = {'schema': SCHEMA + '.terminal', 'status': 'ABI-contract-only/nativehelper-control' if native_success else 'failed',
                'scope': 'C internal oracle + external exact transcript + actual parent wait/jobzero; full contexts/child base/provider/shipping/qmul/codec/perf/package OPEN',
                'archive_verified': True, 'archive': ledger.files['evidence.zip'], 'archive_record': ledger.files['archive.json'],
                'summary': ledger.files['summary.json'], 'members': ledger.files['members.json'],
                'first_failure': first_failure, 'authority_filename': 'terminal-status.json'}
    raw = encoded(terminal)
    pending = ledger.root / '.terminal.pending'
    with pending.open('xb') as stream:
        stream.write(raw); stream.flush(); os.fsync(stream.fileno())
    need(pending.read_bytes() == raw, 'pending-terminal-bytes')
    ordinary(pending)
    ledger.check(extras=('.terminal.pending',))
    # 无 success 后还能失败的检查/写盘。既有文件/fresh gate禁止覆盖。
    need(not (ledger.root / 'terminal-status.json').exists(), 'terminal-already-exists')
    os.rename(pending, ledger.root / 'terminal-status.json')


def main():
    parser = argparse.ArgumentParser(description='同 run Windows ARM64 私有 nativehelper 控制；非 shipping 资格')
    parser.add_argument('--source-root', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    ledger, first_failure, state, owner, expected, authority, final_check = None, None, None, None, None, None, None
    stage = 'context'
    try:
        expected = expected_context(os.environ)
        root = ordinary(args.source_root, True)
        need(Path(__file__).absolute() == root / 'tools/run-windows-private-contract.py', 'actual-runner-source-root')
        ordinary(root / 'target', True)
        need(args.output == root / 'target' / ('task4c5c3b2f3-native-%d-%d' % (expected['run_id'], expected['run_attempt'])), 'fixed-fresh-output')
        fresh_directory(args.output)
        ledger = Ledger(args.output)
        ledger.write_json('caller-context.json', expected)
        stage = 'source-pins'
        source_names = set(PINS) | {'tools/run-windows-private-contract.py', 'tools/tests/test_run_windows_private_contract.py', 'tools/tests/test_windows_private_process.py', WORKFLOW}
        sources = {name: file_identity(root / name) for name in source_names}
        # pin 拒绝前保留固定非秘密源码的实际字节身份及独立预期，不重学/改写源文件。
        ledger.write_json('source-identities.json', sources)
        ledger.write_json('source-expected-pins.json', PINS)
        for name, pin in PINS.items(): need(sources[name]['sha256'] == pin, 'runner-helper-source-pin:' + name)
        for name in source_names:
            raw = (root / name).read_bytes()
            need(digest(raw) == sources[name]['sha256'], 'source-snapshot-race')
            ledger.write('sources/' + name, raw)
        admission = source_module(root, 'tools/admit-windows-private-artifact.py', PINS['tools/admit-windows-private-artifact.py'], 'frd_native_admission')
        transcript = source_module(root, 'tools/verify-windows-private-transcript.py', PINS['tools/verify-windows-private-transcript.py'], 'frd_native_transcript')
        process = source_module(root, 'tools/windows-private-process.py', PINS['tools/windows-private-process.py'], 'frd_native_owner')
        stage = 'launcher-native-admission'
        owner = process.Win32()
        try:
            launcher = owner.admission()
        except Exception as error:
            first_failure = failure_observation(stage, error)
            raise
        finally:
            ledger.write_json('launcher-admission.json', {'observation': getattr(owner, 'admission_observation', None), 'api_calls': owner.calls, 'machine_queries': owner.machine_queries})
        environment = child_environment(os.environ)
        ledger.write_json('child-environment.json', environment)
        stage = 'authenticated-actions-observations'
        token = os.environ.get('GITHUB_TOKEN')
        need(type(token) is str and bool(token), 'automatic-token-missing')
        transport = Transport()
        run_id, attempt = expected['run_id'], expected['run_attempt']
        run_raw = request_bytes(transport, API_BASE + '/actions/runs/%d/attempts/%d' % (run_id, attempt), token, MAX_JSON)
        ledger.write('api-run.json', run_raw)
        jobs_raw = request_bytes(transport, API_BASE + '/actions/runs/%d/attempts/%d/jobs?per_page=100' % (run_id, attempt), token, MAX_JSON)
        ledger.write('api-jobs.json', jobs_raw)
        validate_run(expected, strict_json(run_raw), strict_json(jobs_raw))
        artifact_id = expected['artifact']['id']
        metadata_raw = request_bytes(transport, API_BASE + '/actions/artifacts/%d' % artifact_id, token, MAX_JSON)
        ledger.write('api-artifact.json', metadata_raw)
        metadata = strict_json(metadata_raw)
        size = metadata.get('size_in_bytes')
        need(type(size) is int and 0 < size <= MAX_DOWNLOAD, 'metadata-artifact-size')
        trusted = {key: value for key, value in expected.items() if key != 'workflow_context'}
        trusted['artifact'] = {**expected['artifact'], 'size_in_bytes': size}
        ledger.write_json('expected-artifact-context.json', trusted)
        ledger.write_json('transport-authorities.json', {'id_digest': 'trusted needs.compile-objects outputs; compared with authenticated artifact API',
                                                       'size': 'authenticated artifact metadata API only; independently compared to actual raw byte length',
                                                       'run_attempt': 'trusted workflow context + authenticated run-attempt and jobs API',
                                                       'endpoint': API_BASE + '/actions/artifacts/%d/zip' % artifact_id,
                                                       'upload_artifact_digest': expected['workflow_context']['upload_artifact_digest']})
        admission.validate_context(trusted, metadata)
        stage = 'download-outer-bytes'
        try:
            outer = fetch_artifact(transport, artifact_id, token, size)
        except RunnerError as error:
            first_failure = failure_observation(stage, error)
            partial = getattr(error, 'partial', None)
            if type(partial) is bytes: ledger.write('input-outer.partial', partial)
            raise
        finally:
            token = None
        ledger.write('input-outer.zip', outer)
        need(digest(outer) == trusted['artifact']['digest'][7:], 'outer-upload-api-digest')
        stage = 'f2-static-admission'
        seal = admission.admit_artifact(ledger.root / 'input-outer.zip', root, trusted, metadata, ledger.root / 'artifact')
        seal_bytes = encoded(seal)
        seal_identity = digest(seal_bytes)
        # 从 caller-held seal 的原始身份加入，不从解压后文件重新学习期望。
        for name, value in seal['output_tree'].items():
            if value['kind'] == 'file': ledger.adopt('artifact/' + name, {k: value[k] for k in ('size', 'sha256')})
        ledger.adopt('artifact/admission-seal.json', {'size': len(admission.json_bytes(seal)), 'sha256': digest(admission.json_bytes(seal))})
        ledger.write_json('caller-held-seal.json', seal)
        def verify():
            need(digest(encoded(seal)) == seal_identity, 'caller-held-seal-mutated')
            for name, identity in sources.items(): need(file_identity(root / name) == identity, 'runtime-source-mutated:' + name)
            need(process.python_pe(sys.executable) == launcher['pe'], 'launcher-pe-mutated')
            admission.revalidate(seal)
            ledger.check()
            return {'sources': dict(sources), 'launcher_pe': launcher['pe'],
                    'admitted_pe': {name: file_identity(Path(name)) for name in seal['pe_paths']},
                    'caller_held_seal_sha256': seal_identity, 'evidence_identities_sha256': digest(encoded(ledger.files))}
        def execute(exe, argv, timeout):
            # 唯一实际 CLI 的 executor；不可从 CLI 注入/选 PE、argv、backend。
            backend = process.Win32()
            return process.collect_owned(backend, exe, argv, str(root), environment, timeout)
        def save(index, item, observation):
            prefix = 'commands/%d-%s-%d' % (index, item['suite'], item['variant'])
            if observation is not None:
                for stream in ('stdout', 'stderr'): ledger.write(prefix + '.' + stream, observation[stream])
                receipt = {key: value for key, value in observation.items() if key not in ('stdout', 'stderr')}
                receipt['streams'] = {stream: ledger.files[prefix + '.' + stream] for stream in ('stdout', 'stderr')}
                ledger.write_json(prefix + '.receipt.json', receipt)
            ledger.write_json(prefix + '.parser.json', item)
        final_check = verify
        stage = 'admitted-trap-binding'
        sites = admitted_trap_sites(ledger.root / 'artifact', seal, expected, verify)
        parse = bound_transcript_parser(transcript, sites)
        stage = 'fixed-seven-native-commands'
        state = orchestrate(seal['pe_paths'], execute, verify, parse, save)
        first_failure = state['first_failure']
        stage = 'final-identity-check'
        verify()
        if first_failure is None:
            stage = 'terminal-native-admission'
            authority = _NativeAuthority(root, launcher, ledger)
            verify()
    except Exception as error:
        if first_failure is None: first_failure = failure_observation(stage, error)
    if ledger is None:
        print('私有原生控制在创建本轮输出前拒绝执行', file=sys.stderr)
        return 1
    if state is None:
        state = {'commands': [{'suite': suite, 'variant': variant, 'status': 'not_attempted'} for suite, variant in COMMANDS], 'first_failure': first_failure}
    try:
        ledger.write_json('summary.json', {'schema': SCHEMA, 'status': 'packaging-pending',
                                         'scope': 'ABI-contract-only/nativehelper-control; all shipping qualifications OPEN',
                                         'context': expected, 'commands': state['commands'], 'first_failure': first_failure,
                                         'native_commands_complete': first_failure is None and all(item['status'] == 'observed-contract' for item in state['commands'])})
        native_success = first_failure is None and all(item['status'] == 'observed-contract' for item in state['commands'])
        finalize(ledger, native_success, first_failure, authority=authority, check_inputs=final_check)
        return 0 if native_success else 1
    except Exception as error:
        # 不覆盖首错、不刷新 ledger。封装错误留下 partial；没有成功 authority。
        failure = first_failure or failure_observation('finalization', error)
        try:
            with (ledger.root / 'finalization-failure.json').open('xb') as stream:
                stream.write(encoded({'schema': SCHEMA + '.failure', 'first_failure': failure,
                                      'finalization_error_type': type(error).__name__, 'status': 'failed-no-terminal'}))
                stream.flush(); os.fsync(stream.fileno())
        except OSError: pass
        return 1


if __name__ == '__main__': sys.exit(main())
