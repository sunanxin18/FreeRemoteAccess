#!/usr/bin/env python3
"""Windows本机固定file-identity观察；无任意路径入口，不授生产资格。"""
import hashlib
import json
import os
from pathlib import Path
import platform
import stat
import sys
import threading
import time


FIELDS = ('dev', 'ino', 'mode', 'nlink', 'size', 'mtime_ns', 'ctime_ns')
FILE_LIMIT = 128
JSON_LIMIT = 64 * 1024
OUTPUT = 'windows-file-identity-probe'
DIRECT = b'frd-identity-probe/direct-v1\r\n'
COMMITTED = b'frd-identity-probe/replaced-before-observation-v1\r\n'
BEFORE = b'frd-identity-probe/before-open-old-v1\r\n'
AFTER = b'frd-identity-probe/before-open-new-v1\r\n'


def require(value, message):
    if not value:
        raise ValueError(message)


def signature(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_nlink, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def snapshot(info):
    value = dict(fields=dict(zip(FIELDS, signature(info))))
    value['attributes'] = {name: getattr(info, name) for name in ('st_file_attributes', 'st_reparse_tag') if hasattr(info, name)}
    return value


def no_link(info, directory=False):
    require(not stat.S_ISLNK(info.st_mode) and not (getattr(info, 'st_file_attributes', 0) & 0x400), 'link/reparse refused')
    require(stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode), 'unexpected file type')
    if not directory:
        require(info.st_nlink == 1, 'hardlink refused')


def checked_path(workspace, path, directory=False):
    relative = path.relative_to(workspace)
    # 验证批准workspace的父链及其内部固定路径，不用字符串前缀或扫描目录。
    chain = list(reversed(workspace.parents)) + [workspace]
    chain += [workspace.joinpath(*relative.parts[:i]) for i in range(1, len(relative.parts) + 1)]
    for current in chain:
        info = current.lstat()
        no_link(info, current != path or directory)
    return info


def deadline(began):
    require(time.monotonic() - began < 30, 'fixed 30 second budget exhausted')


def write_payload(workspace, path, payload, began):
    deadline(began)
    require(len(payload) <= FILE_LIMIT, 'fixture byte bound')
    checked_path(workspace, path.parent, True)
    with path.open('xb') as output:
        no_link(os.fstat(output.fileno()))
        output.write(payload)
        output.flush()
        os.fsync(output.fileno())
    deadline(began)


def replace_payload(workspace, source, destination, began):
    deadline(began)
    checked_path(workspace, source)
    if os.path.lexists(destination):
        checked_path(workspace, destination)
    os.replace(source, destination)  # 所有fixture handle均关闭，不在Windows开handle时replace。
    checked_path(workspace, destination)
    deadline(began)


def observe(workspace, path, row, expected, began, replacement=None):
    observations = row['observations']
    deadline(began)
    observations['lstat_before'] = snapshot(checked_path(workspace, path))
    info = path.stat()
    observations['stat_before'] = snapshot(info)
    no_link(info)
    if replacement is not None:
        replace_payload(workspace, replacement, path, began)
        row['replacement_completed_before_open'] = True
    with path.open('rb') as stream:
        info = os.fstat(stream.fileno())
        observations['open_fstat'] = snapshot(info)
        no_link(info)
        raw = stream.read(FILE_LIMIT + 1)
        require(len(raw) <= FILE_LIMIT, 'fixture read byte bound')
        info = os.fstat(stream.fileno())
        observations['read_end_fstat'] = snapshot(info)
        no_link(info)
    observations['path_after_lstat'] = snapshot(checked_path(workspace, path))
    info = path.stat()
    observations['path_after_stat'] = snapshot(info)
    no_link(info)
    row['read_payload_sha256'] = hashlib.sha256(raw).hexdigest()
    row['read_bytes'] = len(raw)
    row['payload_matches_fixed_expected'] = raw == expected
    # 原值与旧strict决定分开；记录每一比较，不能把稳定file的不等改写成PASS。
    before = observations['lstat_before']['fields']
    comparisons = {}
    for operation, phase in (('read-open', 'open_fstat'), ('read-end', 'read_end_fstat'), ('path-after', 'path_after_lstat')):
        differences = [name for name in FIELDS if before[name] != observations[phase]['fields'][name]]
        comparisons[operation] = dict(equal=not differences, differences=differences)
    row['old_strict_comparator'] = comparisons
    row['old_reader_first_rejection'] = next((name for name, value in comparisons.items() if not value['equal']), None)
    require(raw == expected, 'fixed fixture payload mismatch')
    deadline(began)
    row['measurement_complete'] = True


def tool_metadata(workspace):
    source = workspace / 'tools/windows-file-identity-probe.py'
    require(Path(__file__).absolute() == source, 'fixed trusted source required')
    info = checked_path(workspace, source)
    require(info.st_size <= 256 * 1024, 'source byte bound')
    with source.open('rb') as stream:
        source_bytes = stream.read(256 * 1024 + 1)
    require(len(source_bytes) <= 256 * 1024, 'source read byte bound')
    # 只读当前已执行的Python exe身份；不输出绝对用户路径或环境全集。
    executable = Path(sys.executable)
    info = executable.lstat()
    no_link(info)
    require(info.st_size <= 8 * 1024 * 1024, 'Python executable byte bound')
    with executable.open('rb') as stream:
        raw = stream.read(8 * 1024 * 1024 + 1)
    require(len(raw) <= 8 * 1024 * 1024, 'Python executable read byte bound')
    version = sys.getwindowsversion()
    windows = {name: getattr(version, name) for name in (
        'major', 'minor', 'build', 'platform', 'service_pack', 'service_pack_major',
        'service_pack_minor', 'suite_mask', 'product_type', 'platform_version') if hasattr(version, name)}
    return dict(python_version=sys.version[:512], python_implementation=platform.python_implementation(),
                python_executable_name=executable.name, python_executable_sha256=hashlib.sha256(raw).hexdigest(),
                python_identity_scope='current executable file only; no DLL or global runtime qualification',
                windows=windows, reported_machine=platform.machine(), source_relative_path='tools/windows-file-identity-probe.py',
                source_sha256=hashlib.sha256(source_bytes).hexdigest())


def save_result(workspace, output, value):
    raw = (json.dumps(value, sort_keys=True, indent=2) + '\n').encode()
    require(len(raw) <= JSON_LIMIT, 'result JSON byte bound')
    path = output / 'result.json'
    exists = os.path.lexists(path)
    if exists:
        checked_path(workspace, path)
    else:
        checked_path(workspace, output, True)
    # fresh目录内唯一自有JSON，最多初始partial和最终结果两次写入；不重试旧目录。
    with path.open('r+b' if exists else 'xb') as stream:
        no_link(os.fstat(stream.fileno()))
        stream.write(raw)
        stream.truncate()
        stream.flush()
        os.fsync(stream.fileno())


def run():
    began = time.monotonic()
    result = dict(schema='freeremotedesk.windows-file-identity-probe.v1', measurement_complete=False,
                  identity_compatibility='UNQUALIFIED', cases=[], total_budget_seconds=30,
                  limits=dict(payload_file_bytes=FILE_LIMIT, final_payload_files=3, peak_payload_files=4,
                              result_files=1, peak_total_files=5, result_json_bytes=JSON_LIMIT))
    output = workspace = None
    def expired():
        try:
            os.write(2, b'FILE_IDENTITY_PROBE_PARTIAL fixed 30 second budget exhausted\n')
        finally:
            os._exit(124)
    timer = threading.Timer(max(0, 30 - (time.monotonic() - began)), expired)
    timer.daemon = True
    timer.start()
    code = 1
    try:
        # 仅固定CI来源关联字段；不读取环境全集，不据此授identity资格。
        for name in ('GITHUB_SHA', 'GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT', 'GITHUB_JOB'):
            value = os.environ.get(name, '')
            require(len(value) <= 256 and all(32 <= ord(char) < 127 for char in value), 'fixed CI provenance bound')
            result[name] = value
        text = os.environ.get('GITHUB_WORKSPACE', '')
        require(0 < len(text) <= 1024 and '\0' not in text and not text.startswith(('\\\\', '//')), 'fixed local workspace required')
        require(not any(part in ('.', '..') for part in text.replace('\\', '/').split('/')), 'workspace path escape')
        workspace = Path(text)
        require(workspace.is_absolute() and workspace.drive and not any(':' in part for part in workspace.parts[1:]), 'workspace drive/ADS refused')
        checked_path(workspace, workspace, True)
        target = workspace / 'target'
        if not os.path.lexists(target):
            target.mkdir()
        checked_path(workspace, target, True)
        candidate = target / OUTPUT
        candidate.mkdir()  # 已有目录直接拒绝，不清空、不覆盖或重试。
        output = candidate
        save_result(workspace, output, result)
        result['tools'] = tool_metadata(workspace)
        for name, file_name, initial, expected in (
                ('stable-direct', 'direct.bin', DIRECT, DIRECT),
                ('stable-replaced-before-observation', 'committed.bin', COMMITTED, COMMITTED),
                ('replace-before-open-control', 'before-open.bin', BEFORE, AFTER)):
            row = dict(name=name, relative_path=file_name, measurement_complete=False, observations={},
                       known_initial_payload_sha256=hashlib.sha256(initial).hexdigest(),
                       expected_read_payload_sha256=hashlib.sha256(expected).hexdigest())
            result['cases'].append(row)
            path = output / file_name
            if name == 'stable-replaced-before-observation':
                staged = output / 'committed.tmp'
                write_payload(workspace, staged, initial, began)
                replace_payload(workspace, staged, path, began)
            else:
                write_payload(workspace, path, initial, began)
            replacement = None
            if name == 'replace-before-open-control':
                replacement = output / 'replacement.tmp'
                write_payload(workspace, replacement, expected, began)
            observe(workspace, path, row, expected, began, replacement)
        deadline(began)
        result['measurement_complete'] = True
        code = 0  # 只表示固定观察完整，不表示strict比较或生产兼容性通过。
    except Exception as error:
        result['error_type'] = type(error).__name__  # 不把OS错误中的绝对用户路径写入结果。
    finally:
        if output is not None:
            try:
                save_result(workspace, output, result)
            except Exception as error:
                result['measurement_complete'] = False
                result['result_write_error_type'] = type(error).__name__
                code = 1
    try:
        print(json.dumps(dict(measurement_complete=result['measurement_complete'], identity_compatibility='UNQUALIFIED', exit=code)))
        return code
    finally:
        timer.cancel()


def main():
    if len(sys.argv) != 1:
        print('Only the fixed no-argument probe is supported', file=sys.stderr)
        return 2
    if os.name != 'nt' or sys.platform != 'win32':
        print('UNAVAILABLE requires=actual-Windows-Python', file=sys.stderr)
        return 77
    return run()


if __name__ == '__main__':
    sys.exit(main())
