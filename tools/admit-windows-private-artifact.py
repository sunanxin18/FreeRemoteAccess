#!/usr/bin/env python3
"""同 run 私有编译产物静态准入；只处理数据，从不启动 PE/工具或联网。

admit_artifact(outer_zip, checkout_root, expected, metadata, destination) 的 expected
来自可信 runner/workflow，metadata 是独立 Actions API 观察。此模块不认证 GitHub，
不接收 token，也不以 ZIP/receipt 自授 source/run 权限。返回 data-only seal；调用方
必须保存返回 seal 的独立身份并在每次使用前 revalidate(seal)，本模块不自动执行。
文件 AA64/compiled-only 不代表 Windows native、进程、SEH 或产品资格。
"""
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
from types import ModuleType
import zipfile

REPOSITORY = 'sunanxin18/FreeRemoteAccess'
REPOSITORY_ID = 1338203019
SCHEMA = 'freeremotedesk.windows.private-artifact-admission.v1'
CONTRACT_SCHEMA = 'freeremotedesk.windows.private-contract-compile.v1'
TERMINAL_AUTHORITY = 'terminal-status.json only; absent/pending is never success; terminal-precommit.json is not authority'
COMPILER = 'tools/compile-windows-contract.py'
CHECKOUT_PINS = {
    COMPILER: '8d98d17ea4aa3f5c2781a76494cd0635435b3a2130f729e0facf2441aaadc0ee',
    'tools/compile-windows-private-abi.py': 'ada9eb8a3d714ffaee4396eb19e183bb2f2baf533da9970970043368112fe4ec',
    'tools/tests/test_compile_windows_private_abi.py': '9b19621a194d95c947adc86f1fd12c9419aa2244f75e333ada9f19a097a94a41',
    'tools/tests/test_compile_windows_contract.py': '56cf7e57aad2fb0974ff4b4e857a7f8dd59507ef352968744d30137d2205b91c',
    '.github/workflows/qualify-windows-private-abi.yml': '3a2318bf94819b5c34c5aec0084441c33da997cf07393218d14e81a15d9429c2',
}
MAX_JSON = 8 * 1024 * 1024
MAX_FILE = 64 * 1024 * 1024
MAX_TRANSPORT = 96 * 1024 * 1024
MAX_TOTAL = 256 * 1024 * 1024
MAX_MEMBERS = 512
SEAL_FILE = 'admission-seal.json'


class AdmissionError(Exception):
    """首个拒绝原因；失败目录和原始输入不会删除或重试。"""


def need(condition, message):
    if not condition:
        raise AdmissionError(message)


def same(actual, expected):
    """严格递归类型比较；JSON false/true 不能冒充整数 0/1。"""
    if type(actual) is not type(expected):
        return False
    if type(expected) is dict:
        return actual.keys() == expected.keys() and all(same(actual[k], expected[k]) for k in expected)
    if type(expected) is list:
        return len(actual) == len(expected) and all(same(a, b) for a, b in zip(actual, expected))
    return actual == expected


def hashed(data):
    return hashlib.sha256(data).hexdigest()


def uint(value, label, *, positive=False):
    need(type(value) is int and (1 if positive else 0) <= value < 2**63, label + ': canonical uint63 required')
    return value


def hexvalue(value, width, label):
    need(type(value) is str and re.fullmatch('[0-9a-f]{%d}' % width, value) is not None, label + ': lowercase hex required')
    return value


def strict_json(data, *, object_only=True):
    need(type(data) is bytes and len(data) <= MAX_JSON, 'JSON size/type')
    def pairs(items):
        result = {}
        for key, value in items:
            need(key not in result, 'JSON duplicate key')
            result[key] = value
        return result
    def integer(text):
        value = int(text)
        need(-2**63 <= value < 2**63, 'JSON integer width')
        return value
    def forbidden(text):
        raise AdmissionError('JSON noninteger/invalid number')
    try:
        value = json.loads(data.decode('utf-8'), object_pairs_hook=pairs, parse_int=integer, parse_float=forbidden, parse_constant=forbidden)
    except (ValueError, UnicodeError, RecursionError) as error:
        raise AdmissionError('invalid JSON: ' + str(error)) from error
    need(not object_only or type(value) is dict, 'JSON object required')
    return value


def member_name(name):
    need(type(name) is str and 0 < len(name) <= 240 and name.isascii(), 'member name encoding/length')
    need(not any(ord(c) < 32 or same(ord(c), 127) or c in '\\:' for c in name), 'member control/backslash/ADS')
    parts = name.split('/')
    need(all(p not in ('', '.', '..') and not p.endswith(('.', ' ')) for p in parts), 'member noncanonical/alias')
    for part in parts:
        need(not re.fullmatch(r'(?:CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\..*)?', part, re.I), 'Windows device member')
        need(not any(c in '<>"|?*' for c in part), 'Windows illegal member')
    return name


def read_zip(data, expected, *, large=frozenset()):
    """先验证全部中央目录，再读取所有 bytes/CRC；调用方尚未写入文件。"""
    need(type(data) is bytes and len(data) <= MAX_TRANSPORT, 'ZIP raw size/type')
    try:
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            infos = archive.infolist()
            need(0 < len(infos) <= MAX_MEMBERS, 'ZIP member count')
            names, folded, total = set(), set(), 0
            for info in infos:
                name = member_name(info.filename)
                need(same(info.orig_filename, name) and name not in names and name.casefold() not in folded, 'ZIP duplicate/alias/NUL')
                need(not info.is_dir() and stat.S_IFMT(info.external_attr >> 16) in (0, stat.S_IFREG), 'ZIP special/directory/symlink')
                need(info.create_system in (0, 3) and not info.external_attr & (0x10 | 0x400), 'ZIP unsupported platform/reparse attributes')
                need(not info.extra and not info.comment, 'ZIP unsupported extra metadata')
                need(not info.flag_bits & ~0x808 and info.compress_type in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED), 'ZIP encrypted/unsupported compression')
                cap = MAX_TRANSPORT if name in large else MAX_JSON if name.endswith('.json') else MAX_FILE
                need(0 <= info.file_size <= cap and 0 <= info.compress_size <= MAX_TRANSPORT, 'ZIP member size')
                need(info.file_size <= max(1024 * 1024, info.compress_size * 200), 'ZIP compression ratio')
                total += info.file_size
                need(total <= MAX_TOTAL, 'ZIP total expanded size')
                names.add(name); folded.add(name.casefold())
            need(same(names, set(expected)), 'ZIP exact allowed members mismatch')
            for name in names:
                need(all(str(p).casefold() not in folded for p in PurePosixPath(name).parents if str(p) != '.'), 'ZIP file/ancestor alias')
            result = {}
            for info in infos:
                with archive.open(info) as stream:
                    value = stream.read(info.file_size + 1)
                    need(same(len(value), info.file_size) and same(stream.read(1), b''), 'ZIP actual read length')
                result[info.filename] = value
            return result
    except (zipfile.BadZipFile, RuntimeError, NotImplementedError, EOFError, OSError, ValueError) as error:
        raise AdmissionError('ZIP read/CRC: ' + str(error)) from error


def bundle(raw, record_raw, allowed, schema, *, large=frozenset()):
    record = strict_json(record_raw)
    need(same(set(record), {'schema', 'sha256', 'members_sha256', 'strict_members'}) and same(record['schema'], schema + '.archive'), 'archive schema')
    need(same(record['sha256'], hashed(raw)), 'archive full bytes digest')
    files = read_zip(raw, set(allowed) | {'members.json'}, large=large)
    manifest = strict_json(files['members.json'])
    need(same(set(manifest), {'schema', 'files'}) and same(manifest['schema'], schema + '.members') and type(manifest['files']) is dict, 'members schema')
    need(same(set(manifest['files']), set(allowed)), 'manifest cannot authorize missing/extra member')
    need(same(record['members_sha256'], hashed(files['members.json'])) and same(record['strict_members'], sorted(allowed) + ['members.json']), 'archive strict_members/manifest binding')
    for name in allowed:
        need(same(hexvalue(manifest['files'][name], 64, 'member hash'), hashed(files[name])), 'member full bytes hash')
    for name, data in files.items():
        if name.endswith('.json'):
            strict_json(data, object_only=not name.endswith('.argv.json'))
    return files


def validate_context(expected, metadata):
    need(type(expected) is dict and type(metadata) is dict, 'independent context/metadata required')
    need(same(set(expected), {'repository', 'repository_id', 'source_sha', 'run_id', 'run_attempt', 'compile_job_result', 'artifact'}), 'expected context fields')
    need(same(expected['repository'], REPOSITORY) and type(expected['repository_id']) is int and same(expected['repository_id'], REPOSITORY_ID), 'fixed repository identity')
    hexvalue(expected['source_sha'], 40, 'source SHA')
    run = uint(expected['run_id'], 'run', positive=True)
    attempt = uint(expected['run_attempt'], 'attempt', positive=True)
    need(same(expected['compile_job_result'], 'success'), 'compile job must independently succeed')
    artifact = expected['artifact']
    need(type(artifact) is dict and same(set(artifact), {'id', 'name', 'size_in_bytes', 'digest'}), 'artifact expected fields')
    uint(artifact['id'], 'artifact id', positive=True)
    need(same(artifact['name'], 'task4c5c3b2f2-input-%d-%d' % (run, attempt)), 'fixed upload artifact name')
    need(uint(artifact['size_in_bytes'], 'artifact size', positive=True) <= MAX_TRANSPORT, 'outer size limit')
    need(type(artifact['digest']) is str and artifact['digest'].startswith('sha256:'), 'artifact sha256 algorithm')
    hexvalue(artifact['digest'][7:], 64, 'artifact digest')
    for key, value in artifact.items():
        need(type(metadata.get(key)) is type(value) and same(metadata.get(key), value), 'artifact API mismatch: ' + key)
    need(metadata.get('expired') is False, 'artifact expired/unknown')
    workflow = metadata.get('workflow_run')
    need(type(workflow) is dict, 'artifact workflow_run metadata missing')
    for key, value in {'id': run, 'repository_id': REPOSITORY_ID, 'head_repository_id': REPOSITORY_ID, 'head_sha': expected['source_sha']}.items():
        need(type(workflow.get(key)) is type(value) and same(workflow.get(key), value), 'workflow_run mismatch: ' + key)


def no_alias(path, *, directory=False):
    need(type(path) is Path or isinstance(path, Path), 'Path required')
    need(path.is_absolute() and same(path, path.resolve(strict=True)), 'exact absolute ordinary path required')
    for component in (path, *path.parents):
        info = component.lstat()
        need(not stat.S_ISLNK(info.st_mode) and not getattr(info, 'st_file_attributes', 0) & 0x400, 'symlink/reparse path refused')
    mode = path.lstat().st_mode
    need(stat.S_ISDIR(mode) if directory else stat.S_ISREG(mode), 'ordinary path type')
    if not directory:
        need(same(path.stat().st_nlink, 1), 'hardlink file refused')
    return path


def fresh_destination(path):
    need(path.is_absolute() and same(path, path.parent.resolve(strict=True) / path.name) and path.name not in ('', '.', '..'), 'fresh destination canonical path')
    no_alias(path.parent, directory=True)
    need(not path.exists() and not path.is_symlink(), 'destination already exists')
    path.mkdir()
    no_alias(path, directory=True)


def identity(path):
    no_alias(path)
    with path.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    return {'sha256': digest, 'size': path.stat().st_size}


def _pinned_source_module(root, name, module_name):
    """直接执行通过固定 pin 的同一源码 bytes；不读写任何 bytecode cache。"""
    path = no_alias(root / name)
    source = path.read_bytes()
    need(same(hashed(source), CHECKOUT_PINS[name]), 'reviewed module source bytes pin drift: ' + name)
    module = ModuleType(module_name)
    module.__file__ = str(path)
    exec(compile(source, str(path), 'exec'), module.__dict__)
    need(same(identity(path)['sha256'], CHECKOUT_PINS[name]), 'module source changed during execution: ' + name)
    return module


def reviewed(root):
    no_alias(root, directory=True)
    for name, pin in CHECKOUT_PINS.items():
        need(same(identity(root / name)['sha256'], pin), 'reviewed checkout pin drift: ' + name)
    module = _pinned_source_module(root, COMPILER, 'frd_admission_reviewed_compiler')
    # 不调用冻结 producer 的 pinned_support()：其 loader 仍可能读取未校验 pyc。
    support = _pinned_source_module(root, module.SUPPORT, 'frd_admission_reviewed_support')
    snapshot = module.source_snapshot(root, support)
    for name, item in snapshot.items():
        need(same(identity(root / name)['sha256'], item['sha256']), 'checkout source pin drift')
    for name, pin in CHECKOUT_PINS.items():
        need(same(identity(root / name)['sha256'], pin), 'checkout import drift')
    return module, support, snapshot


def source_copy(files, snapshot, producer_root):
    expected = {name: {'path': producer_root + '/' + name, 'sha256': value['sha256']} for name, value in snapshot.items()}
    need(same(strict_json(files['source-inputs.json']), expected), 'relative source-inputs binding')
    if 'source-inputs-after.json' in files:
        need(same(strict_json(files['source-inputs-after.json']), expected), 'source after identity drift')
    for name, value in snapshot.items():
        need(same(hashed(files['inputs/' + name]), value['sha256']), 'checkout input copy hash drift')


def member_sets(compiler, support, old_snapshot, snapshot):
    common = {'summary.json', 'source-inputs.json', 'source-authorization.json', 'installation.json', 'extracted-inventory.json'} | {'inputs/' + n for n in old_snapshot}
    install = common | {support.ARCHIVE_NAME}
    source = common | {'allowed-commands.json'} | {case + '.obj' for case in compiler.OBJECTS}
    def logs(labels):
        return {label + suffix for label in labels for suffix in ('.stdout', '.stderr', '.receipt.json')}
    version = ('cc-version', 'readobj-version', 'objdump-version')
    install |= logs(version)
    labels = set(support.allowed_commands(PurePosixPath('/source'), PurePosixPath('/output')))
    labels -= {case + suffix for case in ('bad-unwind-5',) for suffix in ('-readobj', '-objdump')}
    source |= logs(labels)
    contract = (compiler.FIXED_MEMBERS - {'failure-output-identities.json'}) | {'inputs/' + n for n in snapshot}
    contract |= logs(compiler.commands(PurePosixPath('/source'), PurePosixPath('/objects'), PurePosixPath('/output'), support))
    for variant in range(5):
        contract |= {'driver-%d.obj' % variant, 'abi-contract-%d.exe' % variant, 'abi-contract-%d.map' % variant, 'variant-%d-pe-bindings.json' % variant}
        # 固定 O2 producer 的可见 compiled symbols；isolated_child 内联是已裁定事实。
        names = {'main', 'native_admission', 'linked_admission', 'child_main', 'isolated_suite', 'frd_h264_dc_check_abi', 'frd_h264_dc_outer_probe', 'frd_h264_dc_wrong_return'}
        if variant in (0, 4):
            names.add('capture_callback')
        for name in names:
            label = 'variant-%d-disassemble-%s' % (variant, name)
            contract |= logs((label,)) | {label + '.argv.json'}
    return install, source, contract


def terminal_authority(terminal_raw, summary_raw, archive):
    summary, terminal = strict_json(summary_raw), strict_json(terminal_raw)
    need(same(summary.get('schema'), CONTRACT_SCHEMA) and same(summary.get('status'), 'packaging-pending') and same(summary.get('build_status'), 'compiled-only') and same(summary.get('failures'), []) and same(summary.get('terminal_authority'), TERMINAL_AUTHORITY), 'contract summary pending/build authority')
    need(same(terminal, {'schema': CONTRACT_SCHEMA + '.terminal', 'status': 'compiled-only', 'archive_verified': True, 'archive': archive, 'summary_sha256': hashed(summary_raw), 'failures': [], 'first_failure': None, 'authority_filename': 'terminal-status.json', 'scope': summary.get('scope')}) and terminal.get('archive_verified') is True, 'terminal-status exact compiled-only authority')


def same_run(summary, expected):
    github = summary.get('github')
    need(type(github) is dict and same(github.get('GITHUB_SHA'), expected['source_sha']) and same(github.get('GITHUB_RUN_ID'), str(expected['run_id'])) and same(github.get('GITHUB_RUN_ATTEMPT'), str(expected['run_attempt'])), 'same-run SHA/run/attempt mismatch')
    need(same(summary.get('failures'), []) and same(summary.get('not_attempted'), []) and summary.get('first_failure') is None, 'producer incomplete/failed')


def record(files, label, argv, producer_root, *, bad5=False, support=None):
    item = strict_json(files[label + '.receipt.json'])
    need(same(item.get('argv'), argv) and same(item.get('cwd'), producer_root), 'stored compile command/cwd mismatch: ' + label)
    need(same(item.get('environment'), {'PATH': '/opt/frd-llvm-mingw-20250709/bin:/usr/bin:/bin', 'LC_ALL': 'C', 'LANG': 'C', 'HOME': '/tmp'}), 'stored compile environment')
    uint(item.get('pid'), 'compiler PID', positive=True)
    uint(item.get('elapsed_ns'), 'compiler elapsed')
    need(item.get('failure') is None and same(item.get('wait_terminal'), 'exited') and type(item.get('returncode')) is int, 'compile receipt failed/unknown')
    for stream in ('stdout', 'stderr'):
        need(same(item.get(stream), {'name': label + '.' + stream, 'sha256': hashed(files[label + '.' + stream])}), 'compile raw stream identity')
    if bad5:
        need(same(support.candidate_outcome(item, files[label + '.stderr'].decode('utf-8'), 'bad-unwind-5'), 'rejected'), 'bad5 exact compiler refusal')
    else:
        need(same(item['returncode'], 0), 'compile receipt nonzero: ' + label)
    return item


def embedded_tree(files, raw, archive_raw):
    result = {name: {'kind': 'file', 'size': len(data), 'sha256': hashed(data)} for name, data in {**files, 'evidence.zip': raw, 'archive.json': archive_raw}.items()}
    for name in list(result):
        for parent in PurePosixPath(name).parents:
            if str(parent) != '.':
                result[str(parent)] = {'kind': 'directory'}
    return result


def original_identity(files, name, folder):
    return {'path': str(folder / name), 'sha256': hashed(files[name]), 'size': len(files[name])}


def check_prepared(files_by_role, outer, prefixes, compiler, support, snapshot, expected):
    install, source, contract = files_by_role
    summaries = [strict_json(files['summary.json']) for files in files_by_role]
    producer = summaries[2].get('source_root')
    need(type(producer) is str and producer.startswith('/') and same(str(PurePosixPath(producer)), producer) and '..' not in PurePosixPath(producer).parts, 'producer source root provenance')
    proot = PurePosixPath(producer)
    ifold, sfold, cfold = [proot / 'target' / prefix for prefix in prefixes]
    old_snapshot = {name: value for name, value in snapshot.items() if name not in {compiler.FIXTURE + '/' + n for n in compiler.PINS} | set(compiler.OWN)}
    for index, (files, summary) in enumerate(zip(files_by_role, summaries)):
        same_run(summary, expected)
        need(same(summary.get('source_root'), producer), 'producer roots differ')
        source_copy(files, snapshot if same(index, 2) else old_snapshot, producer)
        if index < 2:
            need(same(summary.get('schema'), support.SCHEMA) and same(summary.get('mode'), 'install' if index == 0 else 'probe') and same(summary.get('status'), 'installed-only' if index == 0 else 'compiled-only') and same(summary.get('build_target'), support.TARGET) and same(summary.get('host', {}).get('system'), 'Linux') and same(summary.get('host', {}).get('machine'), 'x86_64'), 'installer/source successful schema')
            need(same(strict_json(files['source-authorization.json']), {**support.AUTHORIZATION_PROVENANCE, 'reviewed_source_pins': support.SOURCE_PINS}), 'source authorization changed')
    need(same(summaries[2].get('compile_input'), str(sfold)) and same(summaries[2].get('installation'), str(ifold)), 'compiled input directory/run mismatch')
    terminal_authority(outer[prefixes[2] + '/terminal-status.json'], contract['summary.json'], strict_json(outer[prefixes[2] + '/archive.json']))
    for name in ('installation.json', 'extracted-inventory.json'):
        need(same(source[name], install[name]), 'source/install identity mismatch')
    install_record = strict_json(install['installation.json'])
    need(same(install_record, {'schema': support.SCHEMA + '.installation', 'archive_url': support.ARCHIVE_URL, 'archive_sha256': support.ARCHIVE_SHA, 'tool_root': str(support.TOOL_ROOT), 'inventory_sha256': hashed(install['extracted-inventory.json']), 'scope': 'pinned extracted files/symlinks; host dynamic loader and OS libraries outside inventory'}), 'installer fixed archive identity')
    need(same(hashed(install[support.ARCHIVE_NAME]), support.ARCHIVE_SHA), 'LLVM-MinGW fixed archive pin')
    for files, folder in ((install, ifold), (source, sfold)):
        commands = support.allowed_commands(proot, folder)
        if files is source:
            need(same(strict_json(files['allowed-commands.json']), commands), 'source allowed command table')
        for label in ('cc-version', 'readobj-version', 'objdump-version'):
            record(files, label, commands[label], producer)
    commands = support.allowed_commands(proot, sfold)
    record(source, 'macros', commands['macros'], producer)
    support.macro_gate(source['macros.stdout'].decode('utf-8'))
    items = summaries[1].get('objects')
    need(type(items) is list and same([item.get('case') for item in items], list(support.CASES)), 'exact eight compile cases')
    for item in items:
        case = item['case']
        need(item.get('mandatory') is (case in support.CASES[:3]), 'source mandatory boolean schema')
        record(source, case + '-expansion', commands[case + '-expansion'], producer)
        receipt = record(source, case + '-compile', commands[case + '-compile'], producer, bad5=same(case, 'bad-unwind-5'), support=support)
        need(item.get('failure') is None and same(item.get('compile_receipt'), case + '-compile.receipt.json') and same(item.get('compile_terminal'), {key: receipt[key] for key in ('pid', 'wait_terminal', 'returncode', 'failure')}), 'source item terminal binding')
        if same(case, 'bad-unwind-5'):
            need(same(item.get('outcome'), 'rejected') and item.get('object') is None and case + '.obj' not in source and same(item.get('rejection_category'), 'llvm-mc-helper-epilogue-size-mismatch'), 'bad5 no-object/refusal')
        else:
            need(same(item.get('outcome'), 'compiled') and same(item.get('object'), original_identity(source, case + '.obj', sfold)), 'seven COFF identities')
            for suffix in ('-readobj', '-objdump'):
                record(source, case + suffix, commands[case + suffix], producer)
            support.readobj_agrees(source[case + '-readobj.stdout'].decode('utf-8'), sfold / (case + '.obj'))
    commands = compiler.commands(proot, sfold, cfold, support)
    need(same(strict_json(contract['allowed-commands.json']), commands), 'contract fixed compile/link command table')
    for label, argv in commands.items():
        record(contract, label, argv, producer)
    same_inputs = strict_json(contract['same-run-inputs.json'])
    need(same(set(same_inputs), {'installation_directory', 'object_directory', 'objects'}), 'same-run input schema')
    for role, key in ((0, 'installation_directory'), (1, 'object_directory')):
        prefix = prefixes[role]
        need(same(same_inputs[key], embedded_tree(files_by_role[role], outer[prefix + '/evidence.zip'], outer[prefix + '/archive.json'])), 'same-run original inner tree identity')
    authorization = strict_json(contract['authorization.json'])
    need(same(authorization, {'base': '122f44e9a1c691a723cdb0229d7aa40b41ae123b', 'driver_review_sha256': 'cf32dcd01c86d2ec9b1525d94635756355e03bff951a3511beb7d390c1ce239c', 'driver_pins': compiler.PINS, 'support_sha256': compiler.SUPPORT_SHA, 'scope': 'root embedded source pins; no SDD runtime/discovered authorization; native OPEN'}), 'contract frozen source authorization')
    return proot, ifold, sfold, cfold


def static_files(destination, files_by_role, prefixes, compiler, support, original_folders, snapshot):
    install, source, contract = files_by_role
    proot, ifold, sfold, cfold = original_folders
    idir, sdir, cdir = [destination / 'admitted' / prefix for prefix in prefixes]
    # tar 仅作固定字节的数据检查，从不解压或执行工具链；内部 symlink 仅记录。
    inventory = compiler.archive_inventory(idir / support.ARCHIVE_NAME, support)
    nondirs = {key: value for key, value in inventory.items() if value['kind'] != 'directory'}
    need(same(strict_json(install['extracted-inventory.json']), nondirs), 'pinned tar full inventory')
    for name in ('tool-inputs.json', 'tool-inputs-after.json'):
        need(same(strict_json(contract[name]), inventory), 'compiled tool before/after inventory')
    objects = {}
    for case in compiler.OBJECTS:
        actual = support.coff_admit(sdir / (case + '.obj'))
        actual['path'] = str(sfold / (case + '.obj'))
        item = next(item for item in strict_json(source['summary.json'])['objects'] if same(item['case'], case))
        need(same(item.get('container'), actual), 'raw source COFF/container binding')
        objects[case] = actual
    need(same(strict_json(contract['same-run-inputs.json'])['objects'], objects), 'same-run seven actual COFF')
    toolroot = PurePosixPath('/opt/frd-llvm-mingw-20250709')
    def tool_identity(path):
        need(type(path) is str, 'tool dependency path type')
        relative = PurePosixPath(path).relative_to(toolroot).as_posix()
        member_name(relative)
        value = inventory.get(relative)
        need(value is not None and same(value.get('kind'), 'file'), 'dependency not fixed tool file')
        return {'path': path, 'sha256': value['sha256'], 'size': value['size']}
    def resolved_tool(path):
        parts = list(PurePosixPath(path).relative_to(toolroot).parts)
        seen = set()
        for _ in range(32):
            key = '/'.join(parts)
            need(key not in seen, 'tool dependency cycle')
            seen.add(key)
            entry = inventory.get(key)
            need(entry is not None, 'tool dependency missing')
            if same(entry['kind'], 'file'):
                return str(toolroot / key)
            need(same(entry['kind'], 'symlink'), 'tool dependency not file')
            target = list(PurePosixPath(key).parent.parts)
            for component in entry['target'].split('/'):
                if same(component, '..'):
                    need(bool(target), 'tool dependency escape'); target.pop()
                elif component not in ('', '.'):
                    target.append(component)
            parts = target
        raise AdmissionError('tool dependency link depth')
    deps = strict_json(contract['dependency-inputs.json'])
    need(same(set(deps), set(compiler.CRT)), 'fixed CRT dependency set')
    for name in compiler.CRT:
        text = contract['dependency-' + name + '.stdout'].decode('utf-8').strip()
        need(same(PurePosixPath(text).name, name), 'compiler dependency basename')
        need(same(deps[name], tool_identity(resolved_tool(text))), 'full CRT dependency identity')
    variants = strict_json(contract['summary.json']).get('variants')
    need(type(variants) is list and same([item.get('variant') for item in variants], list(range(5))) and all(type(item.get('variant')) is int for item in variants), 'exact five variants')
    for variant, item in enumerate(variants):
        need(same(item.get('status'), 'compiled-only') and same(item.get('native'), 'OPEN') and same(item.get('bindings'), 'variant-%d-pe-bindings.json' % variant), 'variant static status')
        objname, exename, mapname = 'driver-%d.obj' % variant, 'abi-contract-%d.exe' % variant, 'abi-contract-%d.map' % variant
        for key, name in (('object', objname), ('pe', exename), ('map', mapname)):
            need(same(item.get(key), original_identity(contract, name, cfold)), 'variant original output identity')
        obj = support.coff_admit(cdir / objname); obj['path'] = str(cfold / objname)
        need(same(item.get('object_container'), obj) and len(contract[mapname]) > 0, 'driver COFF/map binding')
        for label, name in (('object-readobj', objname), ('pe-readobj', exename)):
            support.readobj_agrees(contract['variant-%d-%s.stdout' % (variant, label)].decode('utf-8'), cfold / name)
        pe = compiler.pe_admit(cdir / exename, variant=variant)
        helper_name = ('abi' if same(variant, 0) else 'bad-unwind-%d' % variant) + '.obj'
        owned, ranges = compiler.bindings(pe, sdir / helper_name, sdir / 'negative.obj')
        need(set(owned['labels']) == set(compiler.LABELS), 'all frozen helper/outer labels required')
        expected_open = ['compiler omitted/inlined symbol: isolated_child']
        if variant in (1, 2, 3):
            expected_open.insert(0, 'compiler omitted/inlined symbol: capture_callback')
        if variant == 1:
            expected_open.insert(0, 'no owned pdata (bad1 expected helper omission): frd_h264_dc_check_abi')
        need(same(owned['open'], expected_open), 'only frozen frame omissions permitted')
        need(same(item.get('open'), owned['open']), 'owned frame diagnostics changed')
        # callback 实际 frame 在需要 live walk 的两个 variant 中必须归属唯一。
        if variant in (0, 4):
            need('capture_callback' in ranges, 'capture_callback independent owned frame missing')
        stored = strict_json(contract['variant-%d-pe-bindings.json' % variant])
        material = {key: value for key, value in pe.items() if key != 'at'}
        material['path'] = str(cfold / exename)
        need(set(stored) == set(material) | {'owned_bindings', 'actual_linker_dependencies', 'fixed_dependency_diagnostics', 'scope'}, 'PE binding exact schema')
        need(stored.get('scope') == 'actual raw PE/table/import/text ownership; compiler frame/native review OPEN', 'PE binding static scope')
        for key, value in material.items():
            need(same(stored.get(key), value), 'raw PE static material mismatch: ' + key)
        need(same(stored.get('owned_bindings'), owned) and same(stored.get('fixed_dependency_diagnostics'), deps), 'PE full object/text/table/dependency binding')
        closure = []
        for text in (contract['variant-%d-link-expansion.stderr' % variant].decode('utf-8'), contract[mapname].decode('utf-8')):
            for name in re.findall(r"(/[^\s\"'()]+\.(?:a|o|obj))(?=[\s\"'():]|$)", text):
                path = PurePosixPath(name)
                if same(path, cfold / objname):
                    value = original_identity(contract, objname, cfold)
                elif path in (sfold / helper_name, sfold / 'negative.obj'):
                    value = original_identity(source, path.name, sfold)
                else:
                    value = tool_identity(resolved_tool(name))
                if value not in closure:
                    closure.append(value)
        need(bool(closure) and same(stored.get('actual_linker_dependencies'), closure), 'actual fixed linker dependency closure')
        actual_labels = set()
        for name, (begin, end) in ranges.items():
            label = 'variant-%d-disassemble-%s' % (variant, name)
            actual_labels.add(label + '.argv.json')
            argv = [str(support.OBJDUMP), '--disassemble', '--reloc', '--start-address=0x%x' % (pe['image_base'] + begin), '--stop-address=0x%x' % (pe['image_base'] + end), str(cfold / exename)]
            need(same(strict_json(contract[label + '.argv.json'], object_only=False), argv), 'derived disassembly argv')
            record(contract, label, argv, str(proot))
        need(same(actual_labels, {name for name in contract if name.startswith('variant-%d-disassemble-' % variant) and name.endswith('.argv.json')}), 'raw PE actual disassembly range/member set')
    # Producer identities are relative to original host only as provenance; bytes bind here.
    declared = strict_json(contract['actual-output-identities.json']).get('files')
    need(type(declared) is dict, 'actual-output identities missing')
    required = {str(proot / name): {'sha256': value['sha256'], 'size': len(contract['inputs/' + name])} for name, value in snapshot.items()}
    required.update({str(sfold / (case + '.obj')): {'sha256': hashed(source[case + '.obj']), 'size': len(source[case + '.obj'])} for case in compiler.OBJECTS if case != 'platform'})
    required.update({str(cfold / name): {'sha256': hashed(data), 'size': len(data)} for name, data in contract.items() if name not in ('summary.json', 'members.json', 'actual-output-identities.json')})
    for name, value in required.items():
        need(same(declared.get(name), value), 'original completed identity missing/mutated: ' + name)
    for name, value in declared.items():
        if name not in required:
            actual = tool_identity(name)
            need(same(value, {key: actual[key] for key in ('sha256', 'size')}), 'original extra identity outside tool inventory')
    return [str(cdir / ('abi-contract-%d.exe' % n)) for n in range(5)]


def snapshot_tree(root):
    no_alias(root, directory=True)
    result = {}
    aliases = set()
    for path in sorted(root.rglob('*')):
        name = member_name(path.relative_to(root).as_posix())
        need(name.casefold() not in aliases, 'output casefold alias')
        aliases.add(name.casefold())
        if path.is_dir():
            no_alias(path, directory=True)
            result[name] = {'kind': 'directory'}
        else:
            result[name] = {'kind': 'file', **identity(path)}
    return result


def revalidate(seal):
    """seal 必须由可信 caller 保存，不能从可写目录重新学习期望身份。"""
    need(type(seal) is dict and same(seal.get('schema'), SCHEMA) and same(seal.get('status'), 'admitted-static-only'), 'admission seal schema/status')
    root, destination, archive = (Path(seal[key]) for key in ('checkout_root', 'destination', 'outer_zip'))
    reviewed(root)  # 重验冻结来源及 include 目录，新增 shadow header 也拒绝。
    need(same(identity(archive), seal['outer_identity']), 'raw outer mutated after admission')
    for name, item in seal['checkout'].items():
        member_name(name)
        need(same(identity(root / name), item), 'checkout mutated after admission')
    current = snapshot_tree(destination)
    need(same(current.pop(SEAL_FILE, None), {'kind': 'file', 'sha256': hashed(json_bytes(seal)), 'size': len(json_bytes(seal))}), 'stored admission seal changed')
    need(same(current, seal['output_tree']), 'admitted output mutated/extra/alias')
    return True


def json_bytes(value):
    return (json.dumps(value, indent=2, sort_keys=True) + '\n').encode('utf-8')


def admit_artifact(outer_zip, checkout_root, expected, metadata, destination):
    """Artifact-only API，失败抛 AdmissionError 并保留首错/部分输入；不清理复用目录。"""
    outer_zip, root, destination = map(Path, (outer_zip, checkout_root, destination))
    created = False
    try:
        validate_context(expected, metadata)
        no_alias(outer_zip)
        need(same(outer_zip.stat().st_size, expected['artifact']['size_in_bytes']), 'raw outer full size mismatch')
        raw = outer_zip.read_bytes()
        outer_identity = {'sha256': hashed(raw), 'size': len(raw)}
        need(same(outer_identity['sha256'], expected['artifact']['digest'][7:]), 'raw outer full digest mismatch')
        compiler, original_support, snapshot = reviewed(root)
        # 固定 Linux producer 的 argv 作为 POSIX 数据重算，当前 Windows Path 不参与。
        support = original_support
        support.TOOL_ROOT = PurePosixPath('/opt/frd-llvm-mingw-20250709')
        for name, basename in (('CC', 'aarch64-w64-mingw32-clang'), ('READOBJ', 'llvm-readobj'), ('OBJDUMP', 'llvm-objdump')):
            setattr(support, name, support.TOOL_ROOT / 'bin' / basename)
        before = {name: identity(root / name) for name in snapshot}
        before['tools/admit-windows-private-artifact.py'] = identity(root / 'tools/admit-windows-private-artifact.py')
        run, attempt = expected['run_id'], expected['run_attempt']
        prefixes = ['task4c5c3b2c-install-%d-%d' % (run, attempt), 'task4c5c3b2c-compile-%d-%d' % (run, attempt), 'task4c5c3b2e-contract-%d-%d' % (run, attempt)]
        outer_names = {prefix + '/' + name for prefix in prefixes for name in ('evidence.zip', 'archive.json')} | {prefixes[2] + '/terminal-status.json'}
        outer = read_zip(raw, outer_names, large={prefixes[0] + '/evidence.zip'})
        old_snapshot = {name: value for name, value in snapshot.items() if name not in {compiler.FIXTURE + '/' + n for n in compiler.PINS} | set(compiler.OWN)}
        sets = member_sets(compiler, support, old_snapshot, snapshot)
        files_by_role = [bundle(outer[prefix + '/evidence.zip'], outer[prefix + '/archive.json'], allowed, support.SCHEMA if index < 2 else compiler.SCHEMA, large={support.ARCHIVE_NAME} if same(index, 0) else set()) for index, (prefix, allowed) in enumerate(zip(prefixes, sets))]
        need(sum(len(data) for files in files_by_role for data in files.values()) + sum(map(len, outer.values())) <= MAX_TOTAL, 'combined expanded ZIP bytes exceed budget')
        original_folders = check_prepared(files_by_role, outer, prefixes, compiler, support, snapshot, expected)
        # 完整 namespace/schema/expected-set/hash 之后才创建输出树。
        fresh_destination(destination)
        created = True
        for name, data in outer.items():
            path = destination / 'transport' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open('xb') as stream:
                stream.write(data)
        for prefix, files in zip(prefixes, files_by_role):
            for name, data in files.items():
                path = destination / 'admitted' / prefix / name
                path.parent.mkdir(parents=True, exist_ok=True)
                with path.open('xb') as stream:
                    stream.write(data)
        paths = static_files(destination, files_by_role, prefixes, compiler, support, original_folders, snapshot)
        need(same({name: identity(root / name) for name in before}, before), 'checkout changed during admission')
        need(same(identity(outer_zip), outer_identity), 'outer changed during admission')
        tree = snapshot_tree(destination)
        # 同原 bytes 比较，不能把刚被改写的落盘文件重新采纳为 seal 期望。
        expected_files = {'transport/' + name: {'kind': 'file', 'sha256': hashed(data), 'size': len(data)} for name, data in outer.items()}
        expected_files.update({'admitted/' + prefix + '/' + name: {'kind': 'file', 'sha256': hashed(data), 'size': len(data)} for prefix, files in zip(prefixes, files_by_role) for name, data in files.items()})
        need(same({name: item for name, item in tree.items() if item['kind'] == 'file'}, expected_files), 'extracted bytes changed during static inspection')
        seal = {'schema': SCHEMA, 'status': 'admitted-static-only', 'scope': 'data-only; GitHub authentication supplied by caller; Windows native/process/SEH/qualification OPEN', 'checkout_root': str(root), 'destination': str(destination), 'outer_zip': str(outer_zip), 'outer_identity': outer_identity, 'expected': expected, 'metadata': metadata, 'checkout': before, 'output_tree': tree, 'pe_paths': paths}
        pending = destination / '.admission-seal.pending'
        with pending.open('xb') as stream:
            stream.write(json_bytes(seal))
            stream.flush()
            os.fsync(stream.fileno())
        os.rename(pending, destination / SEAL_FILE)  # 最后发布；失败前无成功 seal。
        return seal
    except Exception as error:
        failure = AdmissionError(str(error))
        if created:
            try:
                with (destination / 'admission-failure.json').open('xb') as stream:
                    stream.write(json_bytes({'schema': SCHEMA + '.failure', 'first_failure': {'type': type(error).__name__, 'message': str(error)}, 'status': 'failed-static-admission'}))
            except OSError:
                pass  # 第一失败不会被保存失败覆盖；不清除 partial 文件。
        raise failure from error
