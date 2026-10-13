#!/usr/bin/env python3
"""仅管理脚本拥有的 OHOS FFmpeg 暂存文件；不覆盖现有未知文件。"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile

spec = importlib.util.spec_from_file_location('hap_check', Path(__file__).with_name('check-harmony-hap.py'))
hap_check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hap_check)


def destinations():
    return {**{f'libs/arm64-v8a/{n}': n for n in hap_check.ffmpeg.LIBS},
            **{f'src/main/resources/rawfile/ffmpeg-ohos/{n}': n for n in hap_check.MATERIALS}}


def safe_path(entry, path):
    hap_check.require(entry.is_dir() and not entry.is_symlink(), 'entry 必须为普通目录')
    for parent in path.parents:
        hap_check.require(not parent.is_symlink(), f'暂存目录拒绝 symlink：{parent}')
        if parent == entry:
            break


def clean(entry):
    marker = entry / '.frd-ffmpeg-stage.json'
    safe_path(entry, marker)
    if not marker.exists():
        return
    hap_check.ffmpeg.regular(marker)
    recorded = json.loads(marker.read_text())
    hap_check.require(set(recorded) == set(destinations()), '暂存 ownership 清单不匹配')
    for relative, digest in recorded.items():
        path = entry / relative
        safe_path(entry, path)
        if path.exists() or path.is_symlink():
            hap_check.ffmpeg.regular(path)
            hap_check.require(hap_check.ffmpeg.sha(path) == digest, f'暂存文件被更改，保留：{path}')
    for relative in recorded:
        (entry / relative).unlink(missing_ok=True)
    marker.unlink()


def stage(entry, bundle, sdk, digest):
    hap_check.accepted_bundle(bundle, sdk, digest)
    clean(entry)
    files = destinations()
    for relative in files:
        path = entry / relative
        hap_check.require(not path.exists() and not path.is_symlink(), f'拒绝覆盖未知暂存文件：{path}')
        # 拒绝目录 symlink，防止在用户目录外写入。
        safe_path(entry, path)
    content = {relative: (bundle / name).read_bytes() for relative, name in files.items()}
    marker = entry / '.frd-ffmpeg-stage.json'
    with marker.open('x') as output:
        json.dump({relative: hashlib.sha256(data).hexdigest() for relative, data in content.items()}, output)
    try:
        for relative, data in content.items():
            path = entry / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            # 完整 bytes 先写私有临时文件，再用不覆盖目标的 hard link 原子发布。
            # 磁盘/写入失败不会留下半截 owned stage，也不会删除未知用户文件。
            with tempfile.TemporaryDirectory(prefix='.frd-stage-', dir=path.parent) as temporary:
                private = Path(temporary) / 'complete'
                private.write_bytes(data)
                os.link(private, path)
    except Exception:
        clean(entry)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--entry', type=Path, required=True)
    parser.add_argument('--clean', action='store_true')
    parser.add_argument('--ffmpeg-bundle', type=Path)
    parser.add_argument('--ffmpeg-manifest-sha256')
    parser.add_argument('--sdk', type=Path)
    args = parser.parse_args()
    if args.clean:
        clean(args.entry)
    else:
        if not all((args.ffmpeg_bundle, args.ffmpeg_manifest_sha256, args.sdk)):
            parser.error('暂存需要显式 bundle、manifest SHA256 和 SDK')
        stage(args.entry, args.ffmpeg_bundle, args.sdk, args.ffmpeg_manifest_sha256)


if __name__ == '__main__':
    main()
