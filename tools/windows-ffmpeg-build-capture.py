#!/usr/bin/env python3
"""固定 Windows ARM64 自然工具调用记录；不是安装或原生资格授权。"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import uuid

TOOL_ROOT = Path('/opt/frd-llvm-mingw-20250709/bin')
PREFIX = 'aarch64-w64-mingw32-'
TOOLS = {PREFIX + name: TOOL_ROOT / (PREFIX + name) for name in ('clang', 'strip')}
SCHEMA = 'freeremotedesk.windows.ffmpeg-spawn.v1'
REJECTION_MAX_BYTES = 32768  # 包含末尾换行；与正常 call 收据无关。
REJECTION_MAX_ARGS = 64
REJECTION_MAX_FIELD_CHARS = 512


def require(ok, message):
    if not ok:
        raise ValueError(message)


def artifact(path):
    path = Path(path).absolute()
    require(path.is_file(), f'工具输入/输出不是文件：{path}')
    return {'path': str(path), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}


def write(path, value):
    with Path(path).open('x', encoding='utf-8') as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write('\n')


def initialize(attempt, tree):
    """仅受信 builder 调用；CLI 不允许选择 compiler、flags 或任意 recipe。"""
    attempt, tree = Path(attempt).resolve(), Path(tree).resolve()
    require(tree.is_relative_to(attempt) and tree.is_dir(), 'capture source 必须属于 fresh attempt')
    root = attempt / 'tool-capture'
    require(not root.exists(), 'capture 必须 fresh')
    identities = {name: artifact(tool) for name, tool in TOOLS.items()}
    root.mkdir()
    (root / 'bin').mkdir()
    (root / 'calls').mkdir()
    write(root / 'scope.json', {'schema': SCHEMA, 'attempt': str(attempt), 'tree': str(tree), 'tools': identities,
                               'capture_source': artifact(Path(__file__).resolve())})
    for name in TOOLS:
        wrapper = root / 'bin' / name
        wrapper.write_bytes(Path(__file__).read_bytes())
        wrapper.chmod(0o755)
    return root / 'bin'


def paths(argv, cwd):
    """记录所有现存显式文件，包括 response/archive；不展开或重写参数。"""
    found = {}
    for arg in argv[1:]:
        for candidate in (arg, arg[1:] if arg.startswith('@') else arg):
            path = Path(candidate)
            path = path if path.is_absolute() else cwd / path
            if path.is_file():
                found[str(path.absolute())] = artifact(path)
    for relative in ('config.h', 'config_components.h', 'ffbuild/config.mak',
                     'libavcodec/aarch64/neon.S', 'libavutil/aarch64/asm.S'):
        path = cwd / relative
        if path.is_file():
            found[str(path.absolute())] = artifact(path)
    return list(found.values())


def outputs(argv, cwd, strip=False):
    values = {}
    for index, arg in enumerate(argv[1:], 1):
        if arg == '-o' and index + 1 < len(argv):
            values['module'] = argv[index + 1]
        elif arg.startswith('/out:'):
            values['module'] = arg[5:]
        elif arg.startswith('/lldmap:'):
            values['map'] = arg[8:]
        elif arg.startswith('--Map='):
            values['map'] = arg[6:]
        elif arg.startswith('-Wl,--Map='):
            values['map'] = arg[len('-Wl,--Map='):]
    if strip and 'module' not in values:
        values['module'] = argv[-1]
    return {name: (Path(value) if Path(value).is_absolute() else cwd / value).absolute()
            for name, value in values.items()}


def emit_map_rejection(argv, cwd, targets):
    """向已有 native stderr 留下有界现场；不读取环境，也不改写自然参数。"""
    reasons = []

    def bounded(value):
        if value is None:
            return None
        value = str(value)
        if len(value) > REJECTION_MAX_FIELD_CHARS:
            if 'field_limit' not in reasons:
                reasons.append('field_limit')
            return value[:REJECTION_MAX_FIELD_CHARS]
        return value

    module = targets.get('module')
    event = {'schema': 'freeremotedesk.windows.ffmpeg-spawn-rejected.v1',
             'reason': 'link-map-ownership', 'argv_count': len(argv),
             'argv': [bounded(arg) for arg in argv[:REJECTION_MAX_ARGS]],
             'cwd': bounded(cwd), 'module': bounded(module), 'map': bounded(targets.get('map')),
             'expected_map': bounded(str(module) + '.map' if module is not None else None),
             'complete': True, 'incomplete_reasons': reasons}
    if len(argv) > REJECTION_MAX_ARGS:
        reasons.append('argv_count_limit')
    while True:
        event['complete'] = not reasons
        encoded = (json.dumps(event, ensure_ascii=True, sort_keys=True, separators=(',', ':')) + '\n').encode('ascii')
        if len(encoded) <= REJECTION_MAX_BYTES:
            break
        if 'event_byte_limit' not in reasons:
            reasons.append('event_byte_limit')
        # 固定字段已各自限长；四个路径即使全为 supplementary Unicode 也小于总预算。
        require(bool(event['argv']), '拒绝诊断固定字段超过总预算')
        event['argv'].pop()
    sys.stderr.buffer.write(encoded)
    sys.stderr.buffer.flush()


def spawn(argv, cwd, directory, *, expected_tool):
    """内部受信入口：实际 argv 不变，stdout/stderr 以原 bytes 保存，失败仍有收据。"""
    cwd, directory = Path(cwd).absolute(), Path(directory)
    require(argv and all(type(arg) is str and '\0' not in arg for arg in argv), 'spawn argv 无效')
    require(Path(argv[0]).is_absolute() and artifact(argv[0]) == expected_tool, '固定工具身份漂移')
    call = directory / uuid.uuid4().hex
    strip = Path(argv[0]).name.endswith('-strip')
    targets = outputs(argv, cwd, strip)
    map_owned = 'map' not in targets or ('module' in targets and
                targets['map'] == Path(str(targets['module']) + '.map'))
    if not map_owned:
        try:
            emit_map_rejection(argv, cwd, targets)
        except Exception:
            pass  # 日志失败不得替代原 guard，或允许工具启动。
    require(map_owned, '自然 link map 必须专属于原 module 路径；拒绝共享/跨调用 map')
    call.mkdir()
    before = paths(argv, cwd)
    saved_before = {}
    for name, path in targets.items():
        if path.is_file():
            saved = call / (name + '.before')
            shutil.copyfile(path, saved)
            saved_before[name] = artifact(saved)
    saved_inputs = {}
    for index, item in enumerate(before):
        saved = call / f'input-{index}.bin'
        shutil.copyfile(item['path'], saved)
        saved_inputs[item['path']] = artifact(saved)
    record = {'schema': SCHEMA, 'argv': argv, 'cwd': str(cwd), 'tool': expected_tool,
              'inputs': before, 'saved_inputs': saved_inputs, 'outputs': {}, 'before': saved_before, 'saved': {},
              'returncode': None, 'error': None}
    write(call / 'started.json', record)
    try:
        with (call / 'stdout.bin').open('xb') as stdout, (call / 'stderr.bin').open('xb') as stderr:
            result = subprocess.run(argv, cwd=cwd, stdout=stdout, stderr=stderr, check=False)
        record['returncode'] = result.returncode
    except OSError as error:
        record['error'] = str(error)
    finally:
        for name, path in targets.items():
            if path.is_file():
                record['outputs'][name] = artifact(path)
                saved = call / (name + '.after')
                shutil.copyfile(path, saved)
                record['saved'][name] = artifact(saved)
        record['tool_after'] = artifact(argv[0])
        record['streams'] = {name: artifact(call / name) for name in ('stdout.bin', 'stderr.bin')
                             if (call / name).is_file()}
        write(call / 'finished.json', record)
    for name, destination in (('stdout.bin', sys.stdout.buffer), ('stderr.bin', sys.stderr.buffer)):
        if (call / name).is_file():
            destination.write((call / name).read_bytes())
            destination.flush()
    return record['returncode'] if record['returncode'] is not None else 127


def load_call(path):
    def unique(pairs):
        require(len(pairs) == len(dict(pairs)), 'capture 重复字段')
        return dict(pairs)
    data = json.loads(Path(path).read_bytes(), object_pairs_hook=unique)
    require(set(data) == {'schema', 'argv', 'cwd', 'tool', 'inputs', 'outputs', 'before', 'saved',
                          'returncode', 'error', 'tool_after', 'streams', 'saved_inputs'} and data['schema'] == SCHEMA,
            'capture unknown/missing fields')
    require(data['returncode'] == 0 and data['error'] is None and data['tool'] == data['tool_after'],
            'capture 失败/工具漂移')
    require(set(data['saved_inputs']) == {i['path'] for i in data['inputs']}, 'capture 输入快照集合不匹配')
    for item in data['inputs']:
        require(data['saved_inputs'][item['path']]['sha256'] == item['sha256'], 'capture 输入快照身份不匹配')
    require(set(data['outputs']) == set(data['saved']) and all(
        data['outputs'][k]['sha256'] == data['saved'][k]['sha256'] for k in data['outputs']),
        'capture 输出快照身份不匹配')
    for item in [data['tool'], *data['saved_inputs'].values(),
                 *data['before'].values(), *data['saved'].values(), *data['streams'].values()]:
        require(item == artifact(item['path']), 'capture 文件身份漂移')
    return data


def main():
    invoked = Path(sys.argv[0]).absolute()
    if invoked.name in TOOLS and invoked.parent.name == 'bin':
        scope = json.loads((invoked.parent.parent / 'scope.json').read_bytes())
        require(set(scope) == {'schema', 'attempt', 'tree', 'tools', 'capture_source'} and
                scope['schema'] == SCHEMA and set(scope['tools']) == set(TOOLS) and
                scope['capture_source'] == artifact(scope['capture_source']['path']) and
                artifact(invoked)['sha256'] == scope['capture_source']['sha256'] and
                Path.cwd().resolve().is_relative_to(Path(scope['tree'])), '固定 capture scope 无效')
        tool = TOOLS[invoked.name]  # 保留实际 driver 调用名，不 resolve symlink。
        require(scope['tools'][invoked.name] == artifact(tool), '受信工具 bytes 漂移')
        code = spawn([str(tool), *sys.argv[1:]], Path.cwd(), invoked.parent.parent / 'calls',
                     expected_tool=scope['tools'][invoked.name])
        if code < 0:
            os.kill(os.getpid(), -code)
        return code
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('initialize',))
    parser.add_argument('--attempt', type=Path, required=True)
    parser.add_argument('--tree', type=Path, required=True)
    args = parser.parse_args()
    print(initialize(args.attempt, args.tree))
    return 0


if __name__ == '__main__':
    sys.exit(main())
