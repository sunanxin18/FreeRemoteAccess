#!/usr/bin/env python3
"""RELEASE A 封闭材料运输；只读核对归档，不执行其内容，不授予 shipping 资格。"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import posixpath
import re
import shutil
import stat
import subprocess
import tarfile

SCHEMA = 'freeremotedesk.windows.h264.release-a.transport.v1'
VERIFICATION_SCHEMA = 'freeremotedesk.windows.h264.release-a.verification.v1'
LLVM_SHA = '422f13915a41acd8b93566e590531f55d3de6e531575197d51756ff8e8d16525'
PREFIX = '/opt/frd-llvm-mingw-20250709'
BASE = 'build-side-frd-h264dc-aarch64-v1/windows-aarch64/Release'
TOPS = ('provenance', 'checkout-inputs', 'setup', 'toolchain', 'build-root')
ERRORS = ('checkout', 'setup', 'builder', 'capture', 'transport')
CHECKOUT = ('.gitattributes', '.github/workflows/qualify-windows-h264-recipe.yml',
    'tools/qualify-windows-h264-recipe-A.ps1', 'tools/windows-h264-recipe-transport.py',
    'tools/build-ffmpeg-windows.ps1', 'tools/desktop-ffmpeg-source.py', 'tools/ffmpeg-source-recipe.py',
    'tools/windows-ffmpeg-bundle.py', 'tools/windows-ffmpeg-build-capture.py',
    'tools/windows-ffmpeg-kernel.py', 'tools/ffmpeg-build-common.psm1',
    'third_party/ffmpeg/8.1.2/h264-luma-dc-aarch64-recipe.json',
    'third_party/ffmpeg/8.1.2/patches/0001-aarch64-h264-luma-dc-neon.patch',
    'third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1',
    'tools/tests/test_windows_h264_recipe_transport.py', 'tools/tests/windows-h264-recipe-ci.Tests.ps1')
TOOL_LINKS = ('aarch64-w64-mingw32-clang', 'aarch64-w64-mingw32-strip', 'aarch64-w64-mingw32-ld')
WINDOWS_TOOLS = ('pwsh', 'python', 'wsl.exe', 'git', 'gpg.exe', 'curl.exe')
SYSTEM_TOOLS = ('python3', 'env', 'bash', 'sh', 'make', 'patch', 'tar', 'xz', 'curl', 'sha256sum',
    'setsid', 'ps', 'kill', 'sleep', 'stat', 'readlink', 'chmod', 'mkdir', 'sed', 'grep', 'head',
    'cat', 'dpkg-query', 'apt-get', 'find', 'cp', 'ln', 'rm', 'touch', 'pkg-config', 'gcc', 'g++',
    'ld', 'ar', 'ranlib', 'strip', 'nm', 'awk')
CALLS = ('wsl-install', 'wsl-version', 'path-to-wsl', 'path-to-windows', 'setup', 'builder',
         'linux-control-normal', 'linux-control-halt', 'linux-cleanup-control-normal', 'linux-cleanup-control-halt',
         'path-repo', 'path-repo-reverse', 'initialize', 'checkout', 'linux-status', 'linux-cleanup-setup', 'linux-cleanup-builder', 'linux-cleanup-collect', 'collect', 'pack')
CALL_FILES = {'argv.json', 'stdout.bin', 'stderr.bin', 'exit.json', 'error.json', 'executable.bin', 'main-module.bin'}
PROVENANCE = {'state.json', 'checkout.json', 'paths.json', 'requirements.json', 'manifest.json'}
SETUP_FILES = {'prepare.sh', 'collect.sh', 'os-release', 'apt-packages.txt', 'tools.json',
               'host.json', 'lifecycle.json', 'lifecycle-error.json', 'transport-error.json',
               'lifecycle-supervisor.py', 'setup-intent.json', 'collect-intent.json', 'linux-control-worker.py',
               'control-normal-intent.json', 'control-halt-intent.json', 'control-results.json'}
INPUT_META = {'attributes.bin', 'attributes-before.bin', 'blob-identities.json', 'check-attr.bin',
              'checkout-commands.json', 'git-executable.bin'}
OBJECTS = ('libavcodec/h264dsp.o', 'libavcodec/h264idct.o', 'libavcodec/aarch64/h264dsp_init_aarch64.o',
           'libavcodec/aarch64/h264dsp_neon.o', 'libavcodec/aarch64/h264idct_neon.o', 'libavcodec/startcode.o')
MANIFEST = 'provenance/manifest.json'


def require(value, message):
    if not value:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def stream_hash(stream):
    value, size = hashlib.sha256(), 0
    while True:
        block = stream.read(1024 * 1024)
        if not block:
            return size, value.hexdigest()
        value.update(block)
        size += len(block)


def file_hash(path):
    with Path(path).open('rb') as stream:
        return stream_hash(stream)[1]


def file_identity(path):
    path = Path(path)
    before = path.stat()
    value = {'size': before.st_size, 'sha256': file_hash(path)}
    after = path.stat()
    require((before.st_size, before.st_mtime_ns, before.st_ctime_ns) ==
            (after.st_size, after.st_mtime_ns, after.st_ctime_ns), '输入hash读取期间变化')
    return value


def final_reader_argv(paths, source, run_id, run_attempt):
    return ['wsl.exe', '--distribution', 'Ubuntu-24.04', '--user', 'root', '--exec', 'python3',
        paths['wsl'] + '/tools/windows-h264-recipe-transport.py', 'verify-pair',
        '--output', paths['evidence_wsl'] + '/../release-a.tar',
        '--receipt', paths['evidence_wsl'] + '/../release-a.receipt.json',
        '--expected-inputs', paths['evidence_wsl'] + '/checkout-inputs/blob-identities.json',
        '--source', source, '--run-id', str(run_id), '--run-attempt', str(run_attempt)]


def verify_document(document, pair, paths, expected_inputs):
    require(set(document) == {'schema', 'source', 'run_id', 'run_attempt', 'reader_sha256',
            'inputs_before', 'inputs_after', 'files', 'producer_error'} and document['schema'] == VERIFICATION_SCHEMA,
            'verification固定schema字段错误')
    require(all(document[key] == pair[key] for key in ('source', 'run_id', 'run_attempt', 'reader_sha256')),
            'verification source/run/reader身份错误')
    require(pair['reader_sha256'] == expected_inputs['tools/windows-h264-recipe-transport.py'], 'reader源码不等于批准blob')
    identities = {'tar': {'size': pair['tar_size'], 'sha256': pair['tar_sha256']},
                  'receipt': {'size': pair['receipt_size'], 'sha256': pair['receipt_sha256']}}
    require(document['inputs_before'] == identities == document['inputs_after'], 'verification两输入前后身份不同')
    require(document['producer_error'] is None and 'error.json' not in document['files'], 'verification producer/原ErrorRecord失败')
    packing_valid(document['files'])
    files = {name: base64.b64decode(item['base64'], validate=True) for name, item in document['files'].items()}
    call = decode_json(files['argv.json'])
    require(call == {'argv': final_reader_argv(paths, pair['source'], pair['run_id'], pair['run_attempt']),
                    'cwd': paths['windows']}, 'verification原verify argv/cwd不匹配')
    result = decode_json(files['exit.json'])
    require('executable' in result and 'executable.bin' in files and
            result['executable']['invoked_name'] == 'wsl.exe', 'verification原exe缺失/调用名错误')
    module = result.get('main_module')
    require(isinstance(module, dict) and type(module.get('observed')) is bool, 'verification image观察事实缺失')
    if module['observed']:
        require('image' in module and 'main-module.bin' in files, 'verification实际image bytes缺失')
    else:
        require(type(module.get('reason')) is str and module['reason'], 'verification未观察image原因缺失')
    require(decode_json(files['stdout.bin']) == pair, 'verification原reader结构化stdout不绑定最终两输入')


def encoded(value):
    return (json.dumps(value, sort_keys=True, indent=2, ensure_ascii=False) + '\n').encode()


def read_json(path):
    return decode_json(Path(path).read_bytes())


def decode_json(data):
    def unique(pairs):
        require(len(dict(pairs)) == len(pairs), 'JSON 重复字段')
        return dict(pairs)
    return json.loads(data, object_pairs_hook=unique)


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + '.writing')
    with temporary.open('xb') as stream:
        stream.write(encoded(value))
    temporary.replace(path)


def initialize(root, source, run_id, run_attempt):
    require(re.fullmatch('[0-9a-f]{40}', source) and str(run_id).isdigit() and str(run_attempt).isdigit(),
            'source/run 身份格式错误')
    root = Path(root)
    root.mkdir()
    for name in TOPS:
        (root / name).mkdir()
    write_json(root / 'provenance/state.json', dict(schema=SCHEMA, source=source, run_id=str(run_id),
        run_attempt=str(run_attempt), stage='checkout', checkout_ready=False, setup_ready=False,
        builder_started=False, builder_exit=None, quiescent=True, attempt=None, build_origin=None,
        first_error=None, errors={name: [] for name in ERRORS}))


def note_error(state, stage, message):
    require(stage in ERRORS and isinstance(message, str), '错误阶段无效')
    state['errors'][stage].append(message)
    if state['first_error'] is None:
        state['first_error'] = {'stage': stage, 'message': message}


def safe_name(name):
    path = PurePosixPath(name)
    require(name and '\\' not in name and '\0' not in name and not path.is_absolute() and
            '..' not in path.parts and '.' not in path.parts and str(path) == name,
            '运输成员绝对/逃逸/非规范路径')
    return path


def allowed(name, directory=False):
    path = safe_name(name)
    parts = path.parts
    require(parts[0] in TOPS and '.git' not in parts and 'CREDENTIALS.local.md' not in parts,
            '运输未知顶层/禁止材料')
    if len(parts) == 1:
        return directory
    relative = '/'.join(parts[1:])
    if parts[0] == 'provenance':
        return relative in PROVENANCE and not directory
    if parts[0] == 'checkout-inputs':
        files = set(CHECKOUT) | INPUT_META
        return relative in files if not directory else any(p.startswith(relative + '/') for p in files)
    if parts[0] == 'setup':
        if relative in SETUP_FILES:
            return not directory
        if parts[1] == 'groups':
            return (directory and len(parts) == 2) or (len(parts) >= 3 and parts[2] in ('setup', 'collect', 'control-normal', 'control-halt') and
                ((directory and len(parts) == 3) or (len(parts) == 4 and not directory and
                  (parts[3] in ('scope.json', 'control.lock', 'halt.json', 'stopped.json') or
                   re.fullmatch(r'[a-f0-9]{32}\.(?:pending|start|end|observations)\.json', parts[3])))))
        if parts[1] == 'calls':
            return (directory and len(parts) == 2) or (len(parts) >= 3 and parts[2] in CALLS and
                ((len(parts) == 3 and directory) or (len(parts) == 4 and parts[3] in CALL_FILES and not directory)))
        return False
    if parts[0] == 'toolchain':
        if parts[1] == 'windows':
            return (len(parts) == 2 and directory) or (len(parts) == 3 and not directory and
                parts[2] in {'catalog.json', *(name + '.bin' for name in WINDOWS_TOOLS)})
        return parts[1] in ('install', 'system') or (len(parts) == 2 and parts[1] in
            ('archive.tar.xz', 'archive.tar.xz.partial', 'system-tools.json') and not directory)
    if parts[1] in ('source-cache', 'gnupg'):
        return True
    recipe = 'build-root/' + BASE
    return (directory and recipe.startswith(name + '/')) or name == recipe or name.startswith(recipe + '/')


def state_valid(state):
    require(set(state) == {'schema', 'source', 'run_id', 'run_attempt', 'stage', 'checkout_ready',
        'setup_ready', 'builder_started', 'builder_exit', 'quiescent', 'attempt', 'build_origin',
        'first_error', 'errors'} and state['schema'] == SCHEMA, 'state unknown/missing 字段')
    require(re.fullmatch('[0-9a-f]{40}', state['source']) and state['run_id'].isdigit() and
            state['run_attempt'].isdigit() and state['stage'] in ERRORS, 'state source/stage 格式错误')
    require(all(type(state[k]) is bool for k in ('checkout_ready', 'setup_ready', 'builder_started', 'quiescent')),
            'state 布尔值错误')
    require(set(state['errors']) == set(ERRORS) and all(isinstance(v, list) and
            all(type(s) is str for s in v) for v in state['errors'].values()), '独立错误字段错误')
    require(not state['setup_ready'] or state['checkout_ready'], 'setup 不得先于 checkout')
    require(not state['builder_started'] or state['setup_ready'], 'builder 不得先于 setup')
    require(state['builder_exit'] is None or (state['builder_started'] and type(state['builder_exit']) is int),
            'builder 退出与执行阶段不一致')
    first = state['first_error']
    require(first is None or (set(first) == {'stage', 'message'} and first['stage'] in ERRORS and
            first['message'] in state['errors'][first['stage']]), 'first-error 不属于已记录阶段')
    require(state['attempt'] is None or re.fullmatch(r'\.attempt-[A-Za-z0-9-]+', state['attempt']), 'attempt 身份错误')
    origin = state['build_origin']
    require(origin is None or (origin.startswith('/mnt/') and
            origin.endswith('/.codex-target/ffmpeg-8.1.2') and '..' not in PurePosixPath(origin).parts),
            'build 原路径不在批准根')


def link_target(name, target, state):
    require('\0' not in target and '\\' not in target, 'symlink target 无效')
    if target.startswith('/'):
        maps = [(PREFIX, 'toolchain/install'), ('/usr/bin', 'toolchain/system/usr/bin'),
                ('/bin', 'toolchain/system/bin'), ('/etc/alternatives', 'toolchain/system/etc/alternatives')]
        if state['build_origin']:
            maps.append((state['build_origin'], 'build-root'))
        matches = [(a, b) for a, b in maps if target == a or target.startswith(a + '/')]
        require(len(matches) == 1, 'symlink 绝对目标不在批准前缀')
        source, destination = matches[0]
        result = destination + target[len(source):]
    else:
        result = posixpath.normpath(posixpath.join(posixpath.dirname(name), target))
    safe_name(result)
    require(result == 'build-root' or result.startswith('build-root/') or
            result.startswith('toolchain/install/') or result.startswith('toolchain/system/'),
            'symlink 逃逸批准工具/构建根')
    return result


def bind_links(entries, state):
    bases = {name: dict(item) for name, item in entries.items()}
    copy_entries = {name: dict(item) for name, item in entries.items()}
    for name, item in entries.items():
        if item['type'] != 'symlink':
            continue
        pending = link_target(name, item['target'], state)
        chain, seen = [name], set()
        for _ in range(128):
            require(pending not in seen, 'symlink 环路')
            seen.add(pending)
            parts = pending.split('/')
            found = False
            for count in range(1, len(parts) + 1):
                prefix = '/'.join(parts[:count])
                if prefix not in entries:
                    require(not state['setup_ready'] and name.startswith('toolchain/install/'),
                            'symlink 目标缺失')
                    copy_entries[name] = dict(item, resolved=pending, chain=chain, target_sha256=None,
                                              target_absent_reason='setup未完成，原partial工具目标尚未产生')
                    found = None
                    break
                node = entries[prefix]
                if node['type'] == 'symlink':
                    chain.append(prefix)
                    pending = link_target(prefix, node['target'], state)
                    if count < len(parts):
                        pending = posixpath.join(pending, *parts[count:])
                    found = True
                    break
            if not found:
                break
        else:
            raise ValueError('symlink 链过长')
        if found is None:
            continue
        final = entries[pending]
        target_hash = (final['sha256'] if final['type'] == 'file' else digest(encoded({
            key: value for key, value in bases.items() if key == pending or key.startswith(pending + '/')})))
        copy_entries[name] = dict(item, resolved=pending, chain=chain, target_sha256=target_hash)
    for name in entries:
        parent = posixpath.dirname(name)
        while parent:
            require(parent in entries and entries[parent]['type'] == 'dir', '成员祖先目录缺失/type不符')
            parent = posixpath.dirname(parent)
    return copy_entries


def scan(root, *, include_manifest=False):
    root = Path(root)
    state = read_json(root / 'provenance/state.json')
    state_valid(state)
    entries = {}
    def walk(directory):
        for path in sorted(directory.iterdir()):
            name = path.relative_to(root).as_posix()
            if name == MANIFEST and not include_manifest:
                continue
            info = path.lstat()
            mode = stat.S_IMODE(info.st_mode)
            require(allowed(name, stat.S_ISDIR(info.st_mode)), '运输未知成员：' + name)
            if stat.S_ISDIR(info.st_mode):
                entries[name] = {'type': 'dir', 'mode': mode}
                walk(path)
            elif stat.S_ISREG(info.st_mode):
                with path.open('rb') as stream:
                    size, sha = stream_hash(stream)
                after = path.lstat()
                require((info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns) ==
                        (after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns), '读取中材料变化')
                entries[name] = {'type': 'file', 'mode': mode, 'size': size, 'sha256': sha}
            elif stat.S_ISLNK(info.st_mode):
                entries[name] = {'type': 'symlink', 'mode': mode, 'target': os.readlink(path)}
            else:
                raise ValueError('运输拒绝 special file：' + name)
    walk(root)
    return bind_links(entries, state)


def check_install_archive(stream, entries):
    """逐项只读原归档；不解压执行，匹配--strip-components=1后的原安装bytes/link/mode。"""
    original, links, prefix = {}, {}, None
    with stream, tarfile.open(fileobj=stream, mode='r|xz') as package:
        for item in package:
            parts = safe_name(item.name).parts
            if prefix is None:
                prefix = parts[0]
            require(parts[0] == prefix, 'LLVM archive 不止一个工具前缀')
            if len(parts) == 1:
                require(item.isdir(), 'LLVM archive 根必须为目录')
                continue
            name = 'toolchain/install/' + '/'.join(parts[1:])
            require(name not in original and name not in links, 'LLVM archive 重复安装成员')
            node = {'mode': item.mode}
            if item.isdir():
                node['type'] = 'dir'
            elif item.issym():
                node.update(type='symlink', target=item.linkname)
            elif item.islnk():
                target = safe_name(item.linkname).parts
                require(len(target) > 1 and target[0] == prefix, 'LLVM archive hardlink逃逸')
                links[name] = 'toolchain/install/' + '/'.join(target[1:])
                continue
            elif item.isfile():
                with package.extractfile(item) as content:
                    size, sha = stream_hash(content)
                node.update(type='file', size=size, sha256=sha)
            else:
                raise ValueError('LLVM archive special安装成员')
            original[name] = node
    for name, target in links.items():
        require(target in original and original[target]['type'] == 'file', 'LLVM archive hardlink未指向原file')
        original[name] = dict(original[target])
    installed = {name: {k: v for k, v in node.items() if k in ('type', 'mode', 'size', 'sha256', 'target')}
                 for name, node in entries.items() if name.startswith('toolchain/install/')}
    require(original == installed, 'LLVM archive与实际安装树的bytes/link/mode不匹配')


def packing_valid(packing):
    # seal时尚无自身退出；调用者完成原pack后只在外部收据附上固定四份原bytes。
    if packing is None:
        return
    require(isinstance(packing, dict) and {'argv.json', 'stdout.bin', 'stderr.bin', 'exit.json'} <= set(packing) <=
            {'argv.json', 'stdout.bin', 'stderr.bin', 'exit.json', 'executable.bin', 'main-module.bin'},
            'pack 原stream字段不完整/未知')
    decoded = {}
    for name, item in packing.items():
        require(set(item) == {'size', 'sha256', 'base64'}, 'pack 原stream身份字段错误')
        try:
            data = base64.b64decode(item['base64'], validate=True)
        except (ValueError, TypeError):
            raise ValueError('pack 原stream base64错误') from None
        require(type(item['size']) is int and item['size'] == len(data) and item['sha256'] == digest(data),
                'pack 原stream bytes/hash不匹配')
        decoded[name] = data
    call, result = decode_json(decoded['argv.json']), decode_json(decoded['exit.json'])
    require(set(call) == {'argv', 'cwd'} and type(call['argv']) is list and type(call['cwd']) is str and
            all(type(arg) is str for arg in call['argv']), 'pack argv/cwd缺失')
    require(isinstance(result, dict) and {'exit', 'terminated', 'capture_complete', 'timed_out', 'error', 'argv', 'cwd'} <= set(result), 'pack退出字段缺失')
    require(result['exit'] == 0 and result['terminated'] is True and result['capture_complete'] is True and result['timed_out'] is False and
            result['error'] is None and result['argv'] == call['argv'] and result['cwd'] == call['cwd'],
            'pack 未成功完整退出')
    for key, field in (('executable.bin', result.get('executable')), ('main-module.bin',
                       result.get('main_module', {}).get('image'))):
        if field is not None:
            require(key in decoded and digest(decoded[key]) == field['before_sha256'] == field['saved_sha256'] ==
                    field['after_sha256'] == field['final_sha256'], 'pack Windows exe原bytes前后身份不匹配')


def requirements(entries, read, state, expected, open_member):
    require(set(expected) == set(CHECKOUT) and all(re.fullmatch('[0-9a-f]{64}', h) for h in expected.values()),
            '需要调用者独立传入 exact checkout 身份')
    required = set(TOPS) | {'provenance/state.json'}
    if state['checkout_ready']:
        required |= {'checkout-inputs/' + p for p in CHECKOUT}
        required |= {'checkout-inputs/' + p for p in ('attributes.bin', 'blob-identities.json', 'check-attr.bin',
                                                     'checkout-commands.json', 'git-executable.bin')}
        required |= {'provenance/checkout.json', 'provenance/paths.json'}
    if state['setup_ready']:
        required |= {'toolchain/windows/catalog.json'}
        windows = decode_json(read('toolchain/windows/catalog.json'))
        require(set(windows) == set(WINDOWS_TOOLS), 'Windows工具原bytes清单缺失/未知')
        for name, record in windows.items():
            material = 'toolchain/windows/' + name + '.bin'
            required.add(material)
            require(record['invoked_name'] == name and re.match(r'^[A-Za-z]:\\', record['path']), 'Windows工具原调用名/路径错误')
            require(material in entries and entries[material]['type'] == 'file' and
                    entries[material]['size'] == record['size'] and entries[material]['sha256'] == record['before_sha256'] ==
                    record['after_sha256'] == record['saved_sha256'] == record['final_sha256'], 'Windows工具原bytes前后身份不匹配')
        required |= {'setup/' + p for p in ('prepare.sh', 'collect.sh', 'os-release', 'apt-packages.txt',
                                           'tools.json', 'host.json', 'control-results.json')}
        required |= {'toolchain/archive.tar.xz', 'toolchain/system-tools.json', 'toolchain/install'}
        required |= {'toolchain/install/bin/' + p for p in TOOL_LINKS}
        require(entries.get('toolchain/archive.tar.xz', {}).get('sha256') == LLVM_SHA, '固定 LLVM archive hash 不匹配')
        check_install_archive(open_member('toolchain/archive.tar.xz'), entries)
        catalog = decode_json(read('toolchain/system-tools.json'))
        require(set(catalog) == set(SYSTEM_TOOLS), '系统工具 bytes 清单不完整/未知')
        for name, target in catalog.items():
            require(target.startswith('toolchain/system/') and target in entries, '系统工具 bytes 缺失')
            node = entries[target]
            require(node['type'] in ('file', 'symlink') and
                    (node['type'] == 'symlink' or node['mode'] & 0o111), '系统工具 mode/type 无效')
    for path in CHECKOUT:
        name = 'checkout-inputs/' + path
        if name in entries:
            require(entries[name]['type'] == 'file' and entries[name]['sha256'] == expected[path],
                    'checkout 原 blob/材料身份不匹配：' + path)
    if 'checkout-inputs/blob-identities.json' in entries:
        require(decode_json(read('checkout-inputs/blob-identities.json')) == expected, 'blob 身份清单不匹配')
    if 'checkout-inputs/checkout-commands.json' in entries:
        commands = decode_json(read('checkout-inputs/checkout-commands.json'))
        if commands:
            material = entries.get('checkout-inputs/git-executable.bin', {})
            require(material.get('type') == 'file', 'checkout Git原exe缺失')
            for call in commands:
                identity = call['executable']
                require(call['argv'][0] == 'git' and identity['invoked_name'] == 'git' and
                        identity['before_sha256'] == identity['after_sha256'] == material['sha256'],
                        'checkout Git原exe前后身份不匹配')
    if 'checkout-inputs/attributes.bin' in entries:
        require(read('checkout-inputs/attributes.bin') == b'* -text\n', 'checkout 属性优先级不匹配')
    if state['builder_started']:
        required |= {'setup/lifecycle.json'}
        required |= {'setup/calls/builder/' + p for p in ('argv.json', 'stdout.bin', 'stderr.bin', 'exit.json')}
        require(decode_json(read('setup/calls/builder/exit.json'))['exit'] == state['builder_exit'] and
                (state['builder_exit'] is not None or state['first_error'] is not None), 'builder 原退出事实缺失/不匹配')
    attempts = {name.split('/')[4] for name in entries if name.startswith('build-root/' + BASE + '/.attempt-')}
    require(attempts == ({state['attempt']} if state['attempt'] else set()), 'attempt 缺失/多个/身份不匹配')
    optional = {'build-root/source-cache/' + p for p in
                ('ffmpeg-8.1.2.tar.xz', 'ffmpeg-8.1.2.tar.xz.asc', 'ffmpeg-devel.asc')}
    if state['attempt']:
        prefix = 'build-root/' + BASE + '/' + state['attempt'] + '/'
        stages = {}
        for name, node in entries.items():
            if name.startswith(prefix) and re.fullmatch(r'wsl-[a-f0-9]{32}\.json', name[len(prefix):]):
                call = decode_json(read(name))
                if call.get('exit') == 0:
                    stages[call.get('action')] = True
        if stages:
            required |= optional
        groups = {
            '固定 Windows source replay': ['source/ffmpeg-8.1.2/configure', 'source/ffmpeg-8.1.2/ffbuild/library.mak'],
            'Windows 对应源码材料': ['FreeRemoteDesk-ffmpeg-8.1.2-windows-aarch64-frd-h264dc-aarch64-v1-corresponding-source.zip'],
            'FFmpeg configure': ['source/ffmpeg-8.1.2/' + p for p in
                                 ('config.h', 'config_components.h', 'ffbuild/config.mak', 'ffbuild/config.log')],
            'FFmpeg build': ['source/ffmpeg-8.1.2/' + p for p in (*OBJECTS, 'libavcodec/avcodec-62.dll',
                               'libavcodec/avcodec-62.dll.map', 'libavutil/avutil-60.dll')],
            'FFmpeg install': ['dist/bin/avcodec-62.dll', 'dist/bin/avutil-60.dll',
                              'dist/lib/libavcodec.dll.a', 'dist/lib/libavutil.dll.a']}
        for action, paths in groups.items():
            target = {prefix + p for p in paths}
            optional |= target
            if action in stages or state['builder_exit'] == 0:
                required |= target
    missing = required - set(entries)
    require(not missing, '阶段 required 材料缺失：' + ','.join(sorted(missing)))
    return {name: {'required': name in required, 'present': name in entries,
                   'absent_reason': None if name in entries else '该阶段未成功完成；保留 first-error/partial'}
            for name in sorted(required | optional)}


def seal(root, output, receipt, expected_inputs, *, after_scan=None):
    root, output, receipt = map(Path, (root, output, receipt))
    require(not output.exists() and not receipt.exists() and not output.resolve().is_relative_to(root.resolve()),
            '运输只允许 fresh 外部输出')
    state = read_json(root / 'provenance/state.json')
    state_valid(state)
    require(state['quiescent'], '未证明写入停止；拒绝稳定 seal')
    entries = scan(root)
    inventory = requirements(entries, lambda p: (root / p).read_bytes(), state, expected_inputs, lambda p: (root / p).open('rb'))
    write_json(root / 'provenance/requirements.json', inventory)
    entries = scan(root)
    manifest = {'schema': SCHEMA, 'state': state, 'entries': entries}
    write_json(root / MANIFEST, manifest)
    if after_scan:
        after_scan()
    require(scan(root) == entries, '封装前材料变化')
    with tarfile.open(output, 'x', format=tarfile.PAX_FORMAT) as package:
        for name, node in sorted({**entries, MANIFEST: {'type': 'file', 'mode': stat.S_IMODE((root / MANIFEST).stat().st_mode),
               'size': (root / MANIFEST).stat().st_size, 'sha256': digest((root / MANIFEST).read_bytes())}}.items()):
            info = tarfile.TarInfo(name)
            info.mode = node['mode']
            if node['type'] == 'dir':
                info.type = tarfile.DIRTYPE
                package.addfile(info)
            elif node['type'] == 'symlink':
                info.type = tarfile.SYMTYPE
                info.linkname = node['target']
                package.addfile(info)
            else:
                require(file_hash(root / name) == node['sha256'], 'tar 读取中材料变化')
                info.size = node['size']
                with (root / name).open('rb') as stream:
                    package.addfile(info, stream)
    require(scan(root) == entries, '封装后材料变化')
    envelope = {'schema': SCHEMA, 'source': state['source'], 'run_id': state['run_id'],
                'run_attempt': state['run_attempt'], 'tar_size': output.stat().st_size,
                'tar_sha256': file_hash(output), 'manifest_sha256': file_hash(root / MANIFEST), 'packing': None}
    write_json(receipt, envelope)
    verify(output, receipt, source=state['source'], run_id=state['run_id'], run_attempt=state['run_attempt'],
           expected_inputs=expected_inputs)
    return envelope


def verify(archive, receipt, *, source, run_id, run_attempt, expected_inputs, require_packing=False, verification=None):
    archive = Path(archive)
    inputs_before = {'tar': file_identity(archive), 'receipt': file_identity(receipt)}
    envelope = read_json(receipt)
    require(set(envelope) == {'schema', 'source', 'run_id', 'run_attempt', 'tar_size', 'tar_sha256',
            'manifest_sha256', 'packing'} and envelope['schema'] == SCHEMA and envelope['source'] == source and
            envelope['run_id'] == str(run_id) and envelope['run_attempt'] == str(run_attempt) and
            envelope['tar_size'] == archive.stat().st_size and envelope['tar_sha256'] == file_hash(archive),
            '运输外部收据不匹配')
    packing_valid(envelope['packing'])
    require(not require_packing or envelope['packing'] is not None, 'pack原始外部收据尚未完成')
    entries = {}
    content_end = 0
    with tarfile.open(archive, 'r:') as package:
        for item in package:
            name = item.name
            require(name not in entries and allowed(name, item.isdir()), '归档重复/未知成员：' + name)
            require(not item.pax_headers.keys() - {'path', 'linkpath'} and not item.islnk(), '归档扩展/hardlink 不支持')
            node = {'mode': item.mode}
            if item.isdir():
                node['type'] = 'dir'
            elif item.issym():
                node.update(type='symlink', target=item.linkname)
            elif item.isfile():
                with package.extractfile(item) as content:
                    size, sha = stream_hash(content)
                require(size == item.size, '归档成员短读')
                node.update(type='file', size=size, sha256=sha)
            else:
                raise ValueError('归档 special member')
            entries[name] = node
            content_end = max(content_end, item.offset_data + ((item.size + 511) // 512) * 512)
        # tarfile在首个结束块停止；显式拒绝结束后隐藏的第二份archive/任意非零bytes。
        require(archive.stat().st_size % tarfile.RECORDSIZE == 0 and
                archive.stat().st_size - content_end >= 1024, 'tar尾块长度错误')
        with archive.open('rb') as trailing:
            trailing.seek(content_end)
            while True:
                block = trailing.read(1024 * 1024)
                if not block:
                    break
                require(not any(block), 'tar尾部存在未列成员bytes')
        def read(name):
            require(name in entries and entries[name]['type'] == 'file' and entries[name]['size'] <= 32 * 1024 * 1024,
                    '元数据读取缺失/过大')
            with package.extractfile(name) as content:
                return content.read()
        require(MANIFEST in entries and entries[MANIFEST]['sha256'] == envelope['manifest_sha256'], 'manifest 身份缺失/篡改')
        manifest = decode_json(read(MANIFEST))
        require(set(manifest) == {'schema', 'state', 'entries'} and manifest['schema'] == SCHEMA, 'manifest 字段无效')
        state = decode_json(read('provenance/state.json'))
        state_valid(state)
        require(state == manifest['state'] and state['source'] == source and state['run_id'] == str(run_id) and
                state['run_attempt'] == str(run_attempt) and state['quiescent'], '运输 state 不一致/未稳定')
        del entries[MANIFEST]
        entries = bind_links(entries, state)
        require(entries == manifest['entries'], 'tar 完整成员/type/mode/bytes 与 manifest 不匹配')
        computed = requirements(entries, read, state, expected_inputs, package.extractfile)
        require(decode_json(read('provenance/requirements.json')) == computed, '阶段 required/absent 清单不匹配')
        original_paths = decode_json(read('provenance/paths.json')) if verification is not None else None
    inputs_after = {'tar': file_identity(archive), 'receipt': file_identity(receipt)}
    require(inputs_before == inputs_after, '完整reader读取期间两输入变化')
    pair = dict(source=source, run_id=str(run_id), run_attempt=str(run_attempt),
        builder_exit=state['builder_exit'], first_error=state['first_error'],
        packing_complete=envelope['packing'] is not None, verification_complete=False,
        tar_size=inputs_after['tar']['size'], tar_sha256=inputs_after['tar']['sha256'],
        receipt_size=inputs_after['receipt']['size'], receipt_sha256=inputs_after['receipt']['sha256'],
        reader_sha256=file_hash(__file__), expected_inputs_sha256=digest(encoded(expected_inputs)),
        build_identity_passed=False, qualification='raw build-side evidence only')
    if verification is not None:
        require(envelope['packing'] is not None, 'verification不能替代完整packing')
        verify_document(read_json(verification), pair, original_paths, expected_inputs)
        pair['verification_complete'] = True
    return pair


def windows_process_image(process):
    if os.name != 'nt':
        return {'observed': False, 'reason': '非Windows宿主；未冒充MainModule观察'}
    try:
        import ctypes
        from ctypes import wintypes
        query = ctypes.WinDLL('kernel32', use_last_error=True).QueryFullProcessImageNameW
        query.argtypes = (wintypes.HANDLE, wintypes.DWORD, wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD))
        query.restype = wintypes.BOOL
        size = wintypes.DWORD(32768)
        buffer = ctypes.create_unicode_buffer(size.value)
        require(query(wintypes.HANDLE(int(process._handle)), 0, buffer, ctypes.byref(size)),
                '快速进程image不可观察 WinError=' + str(ctypes.get_last_error()))
        return {'observed': True, 'path': buffer.value}
    except (OSError, RuntimeError, ValueError, AttributeError) as error:
        return {'observed': False, 'reason': str(error)}


def canonical_checkout(repo, source, output):
    repo, output = Path(repo).resolve(), Path(output)
    require(re.fullmatch('[0-9a-f]{40}', source), 'checkout 只接受固定 commit SHA')
    output.mkdir(parents=True, exist_ok=True)
    require(output.is_dir() and not output.is_symlink() and not any(output.iterdir()),
            'checkout输出必须是空真实目录；禁止复用旧材料')
    log = []
    def git(*args):
        argv = ['git', *args]
        location = shutil.which('git')
        require(location is not None, '缺少固定Git executable')
        location = str(Path(location).absolute())
        before = file_hash(location)
        material = output / 'git-executable.bin'
        if not material.exists():
            shutil.copyfile(location, material)
            material.chmod(stat.S_IMODE(Path(location).stat().st_mode))
        require(file_hash(material) == before, 'checkout Git原exe在各调用间变化')
        record = {'argv': argv, 'cwd': str(repo), 'exit': None, 'stdout_hex': '', 'stderr_hex': '',
                  'executable': {'invoked_name': 'git', 'path': location, 'before_sha256': before},
                  'main_module': {'observed': False, 'reason': '尚未启动'}, 'error': None}
        log.append(record)
        try:
            with subprocess.Popen(argv, cwd=repo, stdout=subprocess.PIPE, stderr=subprocess.PIPE) as process:
                record['main_module'] = windows_process_image(process)
                stdout, stderr = process.communicate()
                record.update(exit=process.returncode, stdout_hex=stdout.hex(), stderr_hex=stderr.hex())
            record['executable']['after_sha256'] = file_hash(location)
            require(before == record['executable']['after_sha256'], 'checkout Git原exe调用前后变化')
            if record['main_module']['observed']:
                require(os.path.normcase(record['main_module']['path']) == os.path.normcase(location),
                        'Git实际image与原exe不同；保留partial拒绝身份')
            require(record['exit'] == 0, '隔离 checkout Git 失败')
            return stdout
        except BaseException as error:
            record['error'] = str(error)
            raise
    try:
        require(git('rev-parse', 'HEAD').decode().strip() == source, 'checkout HEAD 不等于批准 source')
        require(git('write-tree').strip() == git('rev-parse', source + '^{tree}').strip(), 'index 与批准 commit 不同')
        attributes = repo / '.git/info/attributes'
        if attributes.exists():
            (output / 'attributes-before.bin').write_bytes(attributes.read_bytes())
        attributes.parent.mkdir(parents=True, exist_ok=True)
        attributes.write_bytes(b'* -text\n')
        (output / 'attributes.bin').write_bytes(attributes.read_bytes())
        # --force仍会跳过stat缓存未变的文件；先全量预检，再只移除固定批准源。
        for name in CHECKOUT:
            path = repo / name
            require(path.is_file() and not path.is_symlink(), 'checkout固定源必须是普通文件且非symlink：' + name)
        for name in CHECKOUT:
            (repo / name).unlink()
        git('-c', 'core.autocrlf=false', '-c', 'core.eol=lf', 'checkout-index', '--force', '--all')
        attrs = git('check-attr', '-z', 'text', 'eol', '--', *CHECKOUT)
        (output / 'check-attr.bin').write_bytes(attrs)
        facts = attrs.split(b'\0')
        triples = [facts[i:i + 3] for i in range(0, len(facts) - 1, 3)]
        require(all(value == b'unset' for _, attribute, value in triples if attribute == b'text'), '优先级 -text 未生效')
        expected = {}
        for name in CHECKOUT:
            blob = git('cat-file', 'blob', source + ':' + name)
            path = repo / name
            require(path.is_file() and not path.is_symlink() and path.read_bytes() == blob,
                    'checkout 物理 bytes 不等于批准 blob：' + name)
            expected[name] = digest(blob)
            dest = output / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(blob)
        write_json(output / 'blob-identities.json', expected)
        return expected
    finally:
        write_json(output / 'checkout-commands.json', log)


def collect_system(root):
    root = Path(root)
    catalog = {}
    def copy_chain(path):
        path = Path(path).absolute()
        require(str(path).startswith(('/usr/bin/', '/bin/', '/etc/alternatives/')), '系统工具 symlink 不在批准前缀')
        dest = root / 'toolchain/system' / str(path).lstrip('/')
        if dest.exists() or dest.is_symlink():
            return
        dest.parent.mkdir(parents=True, exist_ok=True)
        if path.is_symlink():
            target = os.readlink(path)
            dest.symlink_to(target)
            copy_chain(Path(os.path.normpath(path.parent / target if not target.startswith('/') else target)))
        else:
            shutil.copy2(path, dest)
    for name in SYSTEM_TOOLS:
        location = shutil.which(name, path='/usr/bin:/bin')
        require(location is not None, '缺少必要系统工具：' + name)
        location = Path(os.path.normpath(location))
        copy_chain(location)
        catalog[name] = 'toolchain/system/' + str(location).lstrip('/')
    write_json(root / 'toolchain/system-tools.json', catalog)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('initialize', 'checkout', 'collect-tools', 'seal', 'verify', 'verify-pair'))
    parser.add_argument('--root', type=Path)
    parser.add_argument('--repo', type=Path)
    parser.add_argument('--source')
    parser.add_argument('--run-id')
    parser.add_argument('--run-attempt')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--receipt', type=Path)
    parser.add_argument('--expected-inputs', type=Path)
    parser.add_argument('--verification', type=Path)
    args = parser.parse_args()
    if args.action == 'initialize':
        initialize(args.root, args.source, args.run_id, args.run_attempt)
    elif args.action == 'checkout':
        canonical_checkout(args.repo, args.source, args.output)
    elif args.action == 'collect-tools':
        collect_system(args.root)
    elif args.action == 'seal':
        seal(args.root, args.output, args.receipt, read_json(args.expected_inputs))
    else:
        require((args.action == 'verify' and args.verification is not None) or
                (args.action == 'verify-pair' and args.verification is None), 'verify需第三固定件；verify-pair不自证第三件')
        print(json.dumps(verify(args.output, args.receipt, source=args.source, run_id=args.run_id,
            run_attempt=args.run_attempt, expected_inputs=read_json(args.expected_inputs), require_packing=True, verification=args.verification), sort_keys=True))


if __name__ == '__main__':
    main()
