#!/usr/bin/env python3
"""桌面对应源码材料：固定配方、明确 profile，不承载平台编译策略。"""
import argparse
import importlib.util
import json
from pathlib import Path
import zipfile

REPO = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('desktop_fixed_recipe', REPO / 'tools/ffmpeg-source-recipe.py')
recipes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(recipes)
PROFILE = 'macos-aarch64'
PROFILES = {'macos-aarch64': 'aarch64', 'linux-aarch64': 'aarch64',
            'linux-x86': 'x86', 'linux-x86_64': 'x86_64', 'windows-aarch64': 'aarch64'}


def metadata(recipe, profile=PROFILE, repo=REPO):
    recipes.require(profile in PROFILES, '未知或不支持的 desktop source profile；macOS 仅 ARM64')
    return recipes.metadata(recipe, PROFILES[profile], repo)


def prepare(archive, destination, recipe, profile=PROFILE, repo=REPO):
    metadata(recipe, profile, repo)  # 下载/创建源码目录前验证。
    return recipes.prepare(archive, destination, recipe, PROFILES[profile], repo)


def asset_name(recipe, profile=PROFILE):
    metadata(recipe, profile)
    return f'FreeRemoteDesk-ffmpeg-8.1.2-{profile}-{recipe}-corresponding-source.zip'


def asset_files(archive, recipe, profile=PROFILE, repo=REPO):
    info = metadata(recipe, profile, repo)
    data = recipes.regular(archive)
    recipes.require(recipes.digest(data) == recipes.ARCHIVE_SHA, 'desktop 源码 archive SHA256 不匹配')
    members = {'ffmpeg-8.1.2.tar.xz': data,
               'LICENSE.LGPLv2.1': recipes.regular(repo / recipes.BASE / 'LICENSE.LGPLv2.1')}
    paths = [Path('tools/ffmpeg-source-recipe.py'), Path('tools/desktop-ffmpeg-source.py'),
             Path('tools/macos-ffmpeg-bundle.py'), Path('tools/build-ffmpeg-macos.sh')]
    if profile.startswith('linux-'):
        paths = [Path('tools/ffmpeg-source-recipe.py'), Path('tools/desktop-ffmpeg-source.py'),
                 Path('tools/linux-ffmpeg-bundle.py'), Path('tools/build-ffmpeg-linux.sh'),
                 Path('tools/verify-linux-ffmpeg-bundle.sh')]
    if profile == 'windows-aarch64':
        paths = [Path('tools') / name for name in (
            'ffmpeg-source-recipe.py', 'desktop-ffmpeg-source.py', 'build-ffmpeg-windows.ps1',
            'ffmpeg-build-common.psm1', 'windows-ffmpeg-bundle.py',
            'windows-ffmpeg-build-capture.py', 'windows-ffmpeg-kernel.py')]
    if recipe == recipes.MODIFIED:
        paths += [recipes.PIN, recipes.PATCH]
    for path in paths:
        members[str(path)] = recipes.regular(repo / path)
    instructions = (f'Profile: {profile}\nRecipe: {recipe}\n'
                    'Base archive SHA256 authenticates the pinned upstream archive only.\n'
                    'The local patch is not an upstream-signed release.\n'
                    'Publish this ZIP beside the matching binary on the same download page.\n'
                    'Extract into the matching FreeRemoteDesk checkout (Cargo.lock/rust-toolchain.toml).\n'
                    'Use the macOS ARM64 Apple toolchain and Rust 1.96.0; run:\n'
                    f'FRD_MACOS_ARCH=arm64 FRD_FFMPEG_SOURCE_RECIPE={recipe} '
                    'FRD_FFMPEG_ARCHIVE="$PWD/ffmpeg-8.1.2.tar.xz" '
                    'bash tools/build-ffmpeg-macos.sh\n'
                    'Fresh replay validates all pre/post hashes and leaves the C oracle unchanged.\n')
    if profile.startswith('linux-'):
        instructions = (f'Profile: {profile}\nRecipe: {recipe}\n'
                        'Base archive SHA256 authenticates the pinned upstream archive only.\n'
                        'The local patch is not an upstream-signed release.\n'
                        'Publish this ZIP beside the matching binary on the same download page.\n'
                        'Extract into the matching FreeRemoteDesk checkout (Cargo.lock/rust-toolchain.toml).\n'
                        'Use a Linux toolchain for the exact target and Rust 1.96.0; run:\n'
                        f'FRD_FFMPEG_ARCH={PROFILES[profile]} FRD_FFMPEG_SOURCE_RECIPE={recipe} '
                        'FRD_FFMPEG_ARCHIVE="$PWD/ffmpeg-8.1.2.tar.xz" '
                        'bash tools/build-ffmpeg-linux.sh\n'
                        'Cross builds require FRD_FFMPEG_CC, FRD_FFMPEG_CROSS_PREFIX and FRD_CARGO_TARGET.\n'
                        'x86/i686 and AMD64 require NASM; ARM64 requires HAVE_NEON.\n'
                        'Fresh replay validates all pre/post hashes and leaves the C oracle unchanged.\n'
                        'Static cross checks do not execute the target.\n')
    if profile == 'windows-aarch64':
        instructions = (f'Profile: {profile}\nRecipe: {recipe}\n'
                        'Base archive hash and upstream signature authenticate the original archive only.\n'
                        'The fixed local patch is not an upstream-signed release.\n'
                        'Extract into the matching FreeRemoteDesk checkout; keep all included helpers unchanged.\n'
                        'Use Windows, WSL Ubuntu 24.04 and fixed llvm-mingw 20250709 in /opt, VS2022 and Rust 1.96.0.\n'
                        f'pwsh -File tools/build-ffmpeg-windows.ps1 -Architecture arm64 -SourceRecipe {recipe}\n'
                        'Builder verifies the pinned archive and upstream signature before fixed fresh-source replay.\n'
                        'Modified attempts keep source/config/objects/raw tool receipts and failed partial files.\n'
                        'This is build-side evidence only: old stage/installer contracts do not admit modified output.\n'
                        'Numeric, native guard/qmul/QPC, modified package and live qualification remain OPEN.\n'
                        'Publish corresponding source beside a matching binary only after separate qualification.\n')
    members['REBUILD.txt'] = instructions.encode()
    manifest = {'schema': 'freeremotedesk.desktop.ffmpeg-source.v1', 'profile': profile,
                'source': info, 'recipe': recipe,
                'files': {name: recipes.digest(data) for name, data in members.items()}}
    members['SOURCE-MANIFEST.json'] = (json.dumps(manifest, sort_keys=True, indent=2) + '\n').encode()
    return members


def write_asset(archive, output, recipe, profile=PROFILE, repo=REPO):
    members = asset_files(archive, recipe, profile, repo)
    with zipfile.ZipFile(output, 'x', compression=zipfile.ZIP_STORED) as package:
        for name, data in sorted(members.items()):
            item = zipfile.ZipInfo(name, date_time=(2026, 10, 14, 0, 0, 0))
            item.external_attr = 0o100644 << 16
            package.writestr(item, data)
    verify_asset(archive, output, recipe, profile, repo)


def verify_asset(archive, output, recipe, profile=PROFILE, repo=REPO):
    members = asset_files(archive, recipe, profile, repo)
    recipes.regular(output)
    with zipfile.ZipFile(output) as package:
        recipes.require(len(package.infolist()) == len(members) and set(package.namelist()) == set(members),
                        'desktop 源码 asset 文件集合不匹配')
        recipes.require(all(item.external_attr >> 16 == 0o100644 for item in package.infolist()),
                        'desktop 源码 asset 必须为普通文件')
        recipes.require(all(package.read(name) == data for name, data in members.items()),
                        'desktop 源码 asset bytes 不匹配')


def verify_embedded_asset(output, recipe, profile=PROFILE, repo=REPO):
    """随包 ZIP 仍由受信仓库 helper bytes 决定材料，不信任 ZIP 的自写 manifest。"""
    metadata(recipe, profile, repo)
    recipes.regular(output)
    with zipfile.ZipFile(output) as package:
        recipes.require(package.namelist().count('ffmpeg-8.1.2.tar.xz') == 1, 'desktop 源码 archive 成员缺失/重复')
        archive = package.read('ffmpeg-8.1.2.tar.xz')
    # asset_files 仅需 archive 普通文件；不解压 ZIP、不执行材料。
    import tempfile
    with tempfile.TemporaryDirectory(prefix='frd-source-verification-') as work:
        path = Path(work) / 'ffmpeg-8.1.2.tar.xz'
        path.write_bytes(archive)
        verify_asset(path, output, recipe, profile, repo)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('metadata', 'prepare', 'asset', 'verify-asset', 'asset-name'))
    parser.add_argument('--profile', default=PROFILE)
    parser.add_argument('--source-recipe', default='upstream')
    parser.add_argument('--archive', type=Path)
    parser.add_argument('--destination', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    info = metadata(args.source_recipe, args.profile)
    if args.action == 'metadata':
        print(json.dumps(info, sort_keys=True))
    elif args.action == 'asset-name':
        print(asset_name(args.source_recipe, args.profile))
    elif args.action == 'prepare':
        recipes.require(args.archive is not None and args.destination is not None, 'prepare 需要 archive/destination')
        root, _ = prepare(args.archive, args.destination, args.source_recipe, args.profile)
        print(root)
    else:
        recipes.require(args.archive is not None and args.output is not None, 'asset 需要 archive/output')
        action = write_asset if args.action == 'asset' else verify_asset
        action(args.archive, args.output, args.source_recipe, args.profile)


if __name__ == '__main__':
    main()
