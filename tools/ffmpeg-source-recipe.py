#!/usr/bin/env python3
"""仅接受仓库固定 FFmpeg 来源配方；在私有 fresh 树重放，不接受外部 patch。"""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import struct
import subprocess
import tarfile
import zipfile

REPO = Path(__file__).resolve().parent.parent
BASE = Path('third_party/ffmpeg/8.1.2')
PIN = BASE / 'h264-luma-dc-aarch64-recipe.json'
PIN_SHA = 'd4105ea904a4996615eb8b4952be9aec7ae074de1dadbd58ad71c26d6398ec87'
PATCH = BASE / 'patches/0001-aarch64-h264-luma-dc-neon.patch'
PATCH_SHA = '4527ec807869f2a8ae8c2843ce51c693f3cbb728104e4fb9e617627dec593c7c'
ARCHIVE_SHA = '464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c'
URL = 'https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz'
MODIFIED = 'frd-h264dc-aarch64-v1'
CHOICES = ('upstream', MODIFIED)
EXTRA = ('FFmpeg-CHANGES.diff', 'FFmpeg-SOURCE-RECIPE.json')
ASSET = 'FreeRemoteDesk-ffmpeg-8.1.2-frd-h264dc-aarch64-v1-corresponding-source.zip'


def require(ok, message):
    if not ok:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def regular(path):
    require(path.is_file() and not path.is_symlink(), f'来源要求普通文件：{path}')
    return path.read_bytes()


def trusted(repo=REPO):
    pin_bytes, patch = regular(repo / PIN), regular(repo / PATCH)
    require(digest(pin_bytes) == PIN_SHA, '受信 recipe pin SHA256 不匹配')
    require(digest(patch) == PATCH_SHA, '受信 patch SHA256 不匹配')
    return json.loads(pin_bytes)


def metadata(recipe, arch='aarch64', repo=REPO):
    require(recipe in CHOICES, '未知来源 recipe')
    result = {'url': URL, 'archive_sha256': ARCHIVE_SHA, 'modified': False}
    if recipe == MODIFIED:
        require(arch == 'aarch64', '修改来源 recipe 仅允许 AArch64')
        pin = trusted(repo)
        result.update(modified=True, recipe=MODIFIED, recipe_sha256=PIN_SHA,
                      patch_sha256=PATCH_SHA, files=pin['files'], unchanged_oracle=pin['unchanged_oracle'])
    return result


def identify(manifest):
    schema = manifest.get('schema')
    recipe = 'upstream' if type(schema) is int and schema == 1 else MODIFIED
    source = manifest.get('source')
    require(type(schema) is int and schema in (1, 2) and isinstance(source, dict) and
            type(source.get('modified')) is bool and source == metadata(recipe),
            'FFmpeg 来源/recipe 不匹配')
    return recipe


def tree_hashes(root):
    result = {}
    for path in sorted(root.rglob('*')):
        require(not path.is_symlink(), f'来源树拒绝 symlink：{path}')
        require(path.is_dir() or path.is_file(), '来源树拒绝非普通文件')
        if path.is_file():
            result[str(path.relative_to(root))] = digest(path.read_bytes())
    return result


def prepare(archive, destination, recipe, arch='aarch64', repo=REPO):
    """destination 必须不存在；完整固定归档是 preimage，不信任已有解压树。"""
    expected = metadata(recipe, arch, repo)
    require(digest(regular(archive)) == ARCHIVE_SHA, 'FFmpeg archive SHA256 不匹配')
    require(not destination.exists() and not destination.is_symlink(), '来源只允许 fresh 目录，拒绝重复 apply')
    destination.mkdir()
    with tarfile.open(archive) as package:
        seen = set()
        for item in package.getmembers():
            path = PurePosixPath(item.name)
            require(item.name not in seen and not path.is_absolute() and '\\' not in item.name and
                    path.parts[0] == 'ffmpeg-8.1.2' and '..' not in path.parts and
                    (item.isfile() or item.isdir()), '来源归档拒绝路径逃逸/link/特殊文件/重复成员')
            seen.add(item.name)
        package.extractall(destination, filter='data')
    source = destination / 'ffmpeg-8.1.2'
    before = tree_hashes(source)
    if recipe == MODIFIED:
        pin = trusted(repo)
        for path, hashes in pin['files'].items():
            require(before.get(path) == hashes['pre'], f'源码 preimage 不匹配：{path}')
        require(all(before.get(p) == h for p, h in pin['unchanged_oracle'].items()), 'C oracle hash 不匹配')
        patch = regular(repo / PATCH).decode()
        # 固定 hash 之外也验证目标集合，禁止新建、删除或额外文件。
        require(re.findall(r'^--- a/(.+)$', patch, re.M) == re.findall(r'^\+\+\+ b/(.+)$', patch, re.M)
                and set(re.findall(r'^--- a/(.+)$', patch, re.M)) == set(pin['files']), 'patch 路径集合不匹配')
        for dry in (True, False):
            command = ['patch', '-p1', '--batch', '--forward', '--fuzz=0']
            if dry:
                command.append('--dry-run')
            result = subprocess.run(command, cwd=source, input=patch, text=True, stdout=subprocess.PIPE,
                                    stderr=subprocess.STDOUT, env={'PATH': '/usr/bin:/bin', 'LC_ALL': 'C'})
            require(result.returncode == 0 and not re.search(r'offset|fuzz|FAILED|Reversed', result.stdout, re.I),
                    f'patch 无偏移重放失败：{result.stdout}')
        after = tree_hashes(source)
        require(before.keys() == after.keys(), 'patch 创建/删除未批准文件')
        for path, value in after.items():
            require(value == (pin['files'][path]['post'] if path in pin['files'] else before[path]),
                    f'源码 postimage/未批准文件改变：{path}')
    return source, expected


def materials(repo=REPO):
    trusted(repo)
    return {EXTRA[0]: regular(repo / PATCH), EXTRA[1]: regular(repo / PIN)}


def kernel_identity(path, record=None):
    """用受信数学代码 hash 核对 ELF 可执行 LOAD；记录不能自授权任意 kernel。"""
    pin = trusted()['kernel']
    data = regular(path)
    require(data[:7] == b'\x7fELF\x02\x01\x01', 'kernel 需要 ELF64 little-endian')
    h = struct.unpack_from('<HHIQQQIHHHHHH', data, 16)
    require(h[1] == 183, 'kernel 需要 AArch64')
    if record is None:
        # 构建时从未剥离同源动态库的 .symtab 取得真实符号地址。
        sections = [struct.unpack_from('<IIQQQQIIQQ', data, h[5] + i * 64) for i in range(h[11])]
        matches = []
        for s in sections:
            if s[1] != 2:
                continue
            strings = sections[s[6]]
            names = data[strings[4]:strings[4] + strings[5]]
            for offset in range(s[4], s[4] + s[5], 24):
                name, info, other, index, value, size = struct.unpack_from('<IBBHQQ', data, offset)
                if names[name:names.find(b'\0', name)] == pin['symbol'].encode():
                    matches.append((value, size))
        require(len(matches) == 1 and matches[0][1] == pin['size'], '缺少固定 kernel symbol/size')
        record = {'symbol': pin['symbol'], 'size': pin['size'], 'sha256': pin['sha256'], 'address': matches[0][0]}
    require(set(record) == {'symbol', 'size', 'sha256', 'address'} and
            all(record[k] == pin[k] for k in ('symbol', 'size', 'sha256')) and type(record['address']) is int,
            'kernel 受信代码 identity 不匹配')
    for i in range(h[9]):
        p = struct.unpack_from('<IIQQQQQQ', data, h[4] + i * 56)
        if p[0] == 1 and p[1] & 1 and p[3] <= record['address'] and record['address'] + pin['size'] <= p[3] + p[5]:
            offset = p[2] + record['address'] - p[3]
            require(digest(data[offset:offset + pin['size']]) == pin['sha256'], 'kernel 实际代码 bytes 不匹配')
            return record
    raise ValueError('kernel 不在可执行 LOAD 内')


def asset_files(archive, repo=REPO):
    require(digest(regular(archive)) == ARCHIVE_SHA, '源码 asset archive hash 不匹配')
    trusted(repo)
    files = {'ffmpeg-8.1.2.tar.xz': archive.read_bytes(), 'LICENSE.LGPLv2.1': regular(repo / BASE / 'LICENSE.LGPLv2.1')}
    for name in (PIN, PATCH, Path('tools/ffmpeg-source-recipe.py'), Path('tools/ohos-ffmpeg-bundle.py'),
                 Path('tools/build-ffmpeg-ohos.sh')):
        files[str(name)] = regular(repo / name)
    manifest = {'recipe': metadata(MODIFIED), 'files': {n: digest(b) for n, b in files.items()},
                'distribution': 'Publish this asset beside the binary on the same release/download page.',
                'signatures': 'Base archive SHA256 is pinned. Local patch is not an upstream-signed release.',
                'rebuild': 'Extract asset at repository root; use tools/build-ffmpeg-ohos.sh --archive ffmpeg-8.1.2.tar.xz --source-recipe frd-h264dc-aarch64-v1 with the matching FreeRemoteDesk source, official SDK and coherent Rust 1.96.0.'}
    files['SOURCE-MANIFEST.json'] = (json.dumps(manifest, indent=2, sort_keys=True) + '\n').encode()
    return files


def write_asset(archive, output, repo=REPO):
    files = asset_files(archive, repo)
    with zipfile.ZipFile(output, 'x', compression=zipfile.ZIP_STORED) as package:
        for name, data in sorted(files.items()):
            item = zipfile.ZipInfo(name, date_time=(2026, 10, 13, 0, 0, 0))
            item.external_attr = 0o100644 << 16
            package.writestr(item, data)
    verify_asset(archive, output, repo)


def verify_asset(archive, output, repo=REPO):
    files = asset_files(archive, repo)
    with zipfile.ZipFile(output) as package:
        require(len(package.infolist()) == len(files) and set(package.namelist()) == set(files), '源码 asset 文件集合不匹配')
        require(all(package.read(n) == b for n, b in files.items()), '源码 asset bytes 不匹配')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', type=Path, required=True)
    parser.add_argument('--destination', type=Path, required=True)
    parser.add_argument('--source-recipe', choices=CHOICES, default='upstream')
    parser.add_argument('--arch', default='aarch64')
    args = parser.parse_args()
    source, info = prepare(args.archive, args.destination, args.source_recipe, args.arch)
    print(json.dumps({'source_directory': str(source), 'source': info}, indent=2))


if __name__ == '__main__':
    main()
