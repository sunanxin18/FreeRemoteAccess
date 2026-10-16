#!/usr/bin/env python3
"""Windows ARM64 修改配方的 build-side 收据适配；不授权 stage/install。"""
import argparse
import importlib.util
import json
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, REPO / 'tools' / filename)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


source = load('windows_desktop_source', 'desktop-ffmpeg-source.py')
capture = load('windows_build_capture', 'windows-ffmpeg-build-capture.py')
kernel = load('windows_build_kernel', 'windows-ffmpeg-kernel.py')
require = source.recipes.require
PROFILE = 'windows-aarch64'
OBJECTS = ('libavcodec/h264dsp.o', 'libavcodec/h264idct.o',
           'libavcodec/aarch64/h264dsp_init_aarch64.o', 'libavcodec/aarch64/h264dsp_neon.o',
           'libavcodec/aarch64/h264idct_neon.o', 'libavcodec/startcode.o')


def selection(recipe='upstream', architecture='x86_64'):
    require(recipe in source.recipes.CHOICES, '未知 Windows source recipe')
    require(architecture in ('x86', 'x86_64', 'arm64'), 'Windows target 不支持（包括 ARM64EC）')
    require(recipe == 'upstream' or architecture == 'arm64', '修改 Windows recipe 仅允许 ARM64')
    return {'recipe': recipe, 'architecture': architecture, 'modified': recipe != 'upstream',
            'publishable': recipe == 'upstream'}


def endpoint_request(tree, archive, obj, compile_call, evidence):
    """只分离现有 API 明确规定的固定尾缀；不删除/改写任何原始 flags。"""
    tree, obj = Path(tree).resolve(), Path(obj).resolve()
    argv = compile_call['argv']
    suffix = ['-I', str(tree), '-c', '-x', 'assembler-with-cpp',
              str(tree / kernel.SOURCE), '-o', str(obj)]
    require(len(argv) > len(suffix) and argv[-len(suffix):] == suffix and
            compile_call['cwd'] == str(tree),
            '自然 AS argv/cwd 超出现 endpoint grammar；保留原收据，禁止等价改写/另编 shipping object')
    return {'tree': str(tree), 'archive': str(Path(archive).resolve()),
            'compiler': argv[0], 'flags': argv[1:-len(suffix)], 'evidence_dir': str(evidence)}


def feed(tree, archive, obj, linked, link_map, final, calls, evidence):
    """受信 builder 的固定三收据入口；现 consumer 自行裁定，不授予新 grammar。"""
    require(set(calls) == {'compile', 'link', 'strip'}, 'invocation unknown/missing members')
    compile_call, link_call, strip_call = (capture.load_call(calls[k]) for k in ('compile', 'link', 'strip'))
    require(compile_call['outputs'] == {'module': kernel.artifact(obj)} and
            link_call['outputs'] == {'module': kernel.artifact(linked), 'map': kernel.artifact(link_map)} and
            strip_call['inputs'] and kernel.artifact(linked) in strip_call['inputs'] and
            strip_call['outputs'] == {'module': kernel.artifact(final)},
            '自然 compile/link/strip/final 链不匹配；不重写 installed-copy strip path')
    evidence = Path(evidence)
    evidence.mkdir()  # fresh；包括失败的原 consumer invocation 都留在 attempt。
    _, symbol, _, _ = kernel.object_start(obj)
    endpoint = (None if symbol['naux'] else
                endpoint_request(tree, archive, obj, compile_call, evidence / 'endpoint'))
    invocation = {
        'compile_invocation': {'argv': compile_call['argv'], 'cwd': compile_call['cwd'],
                               'inputs': compile_call['inputs'], 'output': kernel.artifact(obj)},
        'link_invocation': {'argv': link_call['argv'], 'cwd': link_call['cwd'],
                            'inputs': link_call['inputs'],
                            'outputs': {'module': kernel.artifact(linked), 'map': kernel.artifact(link_map)}},
        'strip_invocation': {'argv': strip_call['argv'], 'cwd': strip_call['cwd'], 'input': kernel.artifact(linked)}}
    for name, data in invocation.items():
        capture.write(evidence / (name + '.json'), data)
    inputs = {k: evidence / (k + '.json') for k in invocation}
    inputs.update({'compiler': compile_call['argv'][0], 'linker': link_call['argv'][0],
                   'strip': strip_call['argv'][0], 'config.h': Path(tree) / 'config.h',
                   'config.mak': Path(tree) / 'ffbuild/config.mak'})
    record = kernel.capture_pe_kernel(obj, linked, link_map, endpoint=endpoint, build_inputs=inputs)
    capture.write(evidence / 'kernel-capture.json', record)
    checked = kernel.verify_pe_kernel(final, record, endpoint=endpoint)
    capture.write(evidence / 'kernel-verified.json', checked)
    return record, checked


def link_map_from_receipt(linked, receipt):
    """只选择实际 link 原输出路径；不复制或从最终共享文件重建 map。"""
    linked = Path(linked).resolve()
    call = capture.load_call(receipt)
    require(set(call['outputs']) == {'module', 'map'} and
            call['outputs']['module'] == kernel.artifact(linked), '所选 link module 不匹配')
    original = capture.outputs(call['argv'], Path(call['cwd']))
    expected_map = Path(str(linked) + '.map')
    require(original == {'module': linked, 'map': expected_map} and
            call['outputs']['map'] == kernel.artifact(expected_map),
            '所选自然 link map 原路径/身份不匹配')
    return expected_map


def complete(attempt, tree, archive, dist):
    attempt, tree, dist = Path(attempt).resolve(), Path(tree).resolve(), Path(dist).resolve()
    require(tree.is_relative_to(attempt) and dist.is_relative_to(attempt), 'build 输入必须属于同一 attempt')
    outcome = {'schema': 'freeremotedesk.windows.ffmpeg-build-side.v1', 'recipe': source.recipes.MODIFIED,
               'profile': PROFILE, 'publishable': False, 'status': 'failed', 'error': None}
    try:
        outcome['source'] = source.metadata(source.recipes.MODIFIED, PROFILE)
        require(source.recipes.digest(source.recipes.regular(Path(archive))) == source.recipes.ARCHIVE_SHA,
                'build archive pin 不匹配')
        for relative, hashes in outcome['source']['files'].items():
            require(source.recipes.digest(source.recipes.regular(tree / relative)) == hashes['post'],
                    'build source postimage 不匹配')
        for relative, expected in outcome['source']['unchanged_oracle'].items():
            require(source.recipes.digest(source.recipes.regular(tree / relative)) == expected,
                    'build C oracle bytes 改变')
        outcome['objects'] = {p: kernel.artifact(tree / p) for p in OBJECTS}
        outcome['configuration'] = {p: kernel.artifact(tree / p) for p in ('config.h', 'ffbuild/config.mak')}
        scope = json.loads(source.recipes.regular(attempt / 'tool-capture/scope.json'))
        require(set(scope) == {'schema', 'attempt', 'tree', 'tools', 'capture_source'} and
                scope['schema'] == capture.SCHEMA and scope['attempt'] == str(attempt) and
                scope['tree'] == str(tree) and
                scope['capture_source'] == capture.artifact(REPO / 'tools/windows-ffmpeg-build-capture.py') and
                scope['tools'] == {name: capture.artifact(tool) for name, tool in capture.TOOLS.items()},
                '固定自然工具/scope 身份不匹配')
        calls = list((attempt / 'tool-capture/calls').glob('*/finished.json'))
        records = [(path, json.loads(path.read_bytes())) for path in calls]
        obj, linked = tree / OBJECTS[4], tree / 'libavcodec/avcodec-62.dll'
        final = dist / 'bin/avcodec-62.dll'
        def select(predicate, label):
            matches = [path for path, data in records if predicate(data)]
            require(len(matches) == 1, f'自然 {label} 收据缺失/重复')
            return matches[0]
        chosen = {
            'compile': select(lambda d: d['outputs'].get('module') == kernel.artifact(obj), 'AS'),
            'link': select(lambda d: d['outputs'].get('module') == kernel.artifact(linked), 'link'),
            'strip': select(lambda d: d['outputs'].get('module') == kernel.artifact(final), 'strip')}
        for name, expected in (('compile', 'clang'), ('link', 'clang'), ('strip', 'strip')):
            data = capture.load_call(chosen[name])
            require(data['tool'] == scope['tools'][capture.PREFIX + expected] and
                    data['argv'][0] == str(capture.TOOLS[capture.PREFIX + expected]),
                    '自然工具调用名/固定 bytes 不匹配')
        link_map = link_map_from_receipt(linked, chosen['link'])
        feed(tree, archive, obj, linked, link_map, final, chosen, attempt / 'identity')
        source.verify_asset(archive, attempt / source.asset_name(source.recipes.MODIFIED, PROFILE),
                           source.recipes.MODIFIED, PROFILE)
        outcome['status'] = 'build-side-identity-only'
    except Exception as error:
        outcome['error'] = f'{type(error).__name__}: {error}'
        raise
    finally:
        capture.write(attempt / 'build-side-result.json', outcome)
    return outcome


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('selection', 'complete'))
    parser.add_argument('--source-recipe', default='upstream')
    parser.add_argument('--architecture', default='x86_64')
    parser.add_argument('--attempt', type=Path)
    parser.add_argument('--tree', type=Path)
    parser.add_argument('--archive', type=Path)
    parser.add_argument('--dist', type=Path)
    args = parser.parse_args()
    selected = selection(args.source_recipe, args.architecture)
    if args.action == 'selection':
        print(json.dumps(selected, sort_keys=True))
    else:
        require(selected['modified'], 'complete 仅用于明确修改 ARM64 build-side 配方')
        require(all(p is not None for p in (args.attempt, args.tree, args.archive, args.dist)), 'complete 缺少路径')
        complete(args.attempt, args.tree, args.archive, args.dist)


if __name__ == '__main__':
    main()
