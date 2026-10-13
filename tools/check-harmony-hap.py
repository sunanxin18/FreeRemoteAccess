#!/usr/bin/env python3
"""核验普通 phone/2in1 HAP 的精确原生/来源边界；不代表设备加载或解码。"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import tempfile
import zipfile

spec = importlib.util.spec_from_file_location('ohos_ffmpeg_bundle', Path(__file__).with_name('ohos-ffmpeg-bundle.py'))
ffmpeg = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ffmpeg)
require = ffmpeg.require
NATIVE_PREFIX = 'libs/arm64-v8a/'
RAW_PREFIX = 'resources/rawfile/ffmpeg-ohos/'
BASE_LIBS = {'libentry.so', 'libfrd_shell_ohos_ffi.so', 'libc++_shared.so'}
NATIVE = {NATIVE_PREFIX + name for name in BASE_LIBS | set(ffmpeg.LIBS)}
MATERIALS = ffmpeg.FILES - set(ffmpeg.LIBS)
RAW = {RAW_PREFIX + name for name in MATERIALS}
# 逐项来自桥/Rust现有实际链接的公开 NDK 库；新依赖必须单独审核。
PUBLIC = {'libc.so', 'libace_napi.z.so', 'libace_ndk.z.so', 'libnative_window.so',
          'libhilog_ndk.z.so', 'libhuks_ndk.z.so', 'libohcrypto.so', 'libability_runtime.so'}
BASE_DEPS = {
    'libentry.so': {'libfrd_shell_ohos_ffi.so', 'libace_napi.z.so', 'libace_ndk.z.so',
                    'libnative_window.so', 'libhilog_ndk.z.so', 'libc++_shared.so', 'libc.so'},
    'libfrd_shell_ohos_ffi.so': {'libhilog_ndk.z.so', 'libnative_window.so', 'libhuks_ndk.z.so', 'libohcrypto.so', 'libc.so'},
    'libc++_shared.so': {'libc.so'},
}


def accepted_bundle(bundle: Path, sdk: Path, manifest_sha256: str):
    require(re.fullmatch('[0-9a-f]{64}', manifest_sha256) is not None, '需要独立接收的 manifest SHA256')
    require(ffmpeg.sha(bundle / 'manifest.json') == manifest_sha256, '接收的 manifest SHA256 不匹配')
    return ffmpeg.verify_bundle(bundle, sdk)


def entries(package):
    result = {}
    for item in package.infolist():
        name = item.filename
        require(name not in result, f'重复 ZIP 条目：{name}')
        path = PurePosixPath(name)
        require(name and item.orig_filename == name and '\\' not in name and ':' not in name and not path.is_absolute() and
                all(part not in ('', '.', '..') for part in name.rstrip('/').split('/')),
                f'不安全 ZIP 路径：{name}')
        mode = item.external_attr >> 16
        require(not stat.S_ISLNK(mode) and stat.S_IFMT(mode) in (0, stat.S_IFREG, stat.S_IFDIR),
                f'拒绝非普通 ZIP 条目：{name}')
        data = package.read(item)
        if name.startswith('libs/') or data.startswith(b'\x7fELF') or re.search(r'\.so(?:\..*)?$', name):
            if item.is_dir():
                require(name in ('libs/', 'libs/arm64-v8a/'), f'错误 native 目录：{name}')
            else:
                require(name in NATIVE, f'未批准 native/ELF 条目：{name}')
                require(data.startswith(b'\x7fELF'), f'非 ELF native 条目：{name}')
                require(item.compress_type == zipfile.ZIP_STORED, f'native 库必须不压缩：{name}')
        result[name] = data
    require(NATIVE <= result.keys(), 'HAP 缺少固定六个 native 文件')
    require(RAW <= result.keys(), 'HAP 缺少 FFmpeg 来源/许可材料')
    require({n for n in result if n.startswith(RAW_PREFIX) and not n.endswith('/')} == RAW,
            'FFmpeg rawfile 文件集合不匹配')
    return result


def relevant_bytes(hap):
    with zipfile.ZipFile(hap) as package:
        data = entries(package)
        return {name: data[name] for name in NATIVE | RAW | {'module.json'}}


def compare_signed(unsigned, signed):
    require(relevant_bytes(unsigned) == relevant_bytes(signed), '签名改变了 native/来源材料/模块清单字节')


def check(hap: Path, readelf: Path, bundle: Path, manifest_sha256: str) -> dict:
    sdk = readelf.parent.parent.parent.parent
    expected = accepted_bundle(bundle, sdk, manifest_sha256)
    with zipfile.ZipFile(hap) as package, tempfile.TemporaryDirectory(prefix='frd-hap-check-') as temporary:
        data = entries(package)
        module = json.loads(data['module.json'])
        require(module['app']['bundleName'] == 'org.freeremotedesk.client', '应用标识不符')
        require(module['module']['deviceTypes'] == ['phone', '2in1'], '需要真正 phone/2in1 清单')
        require(module['module'].get('extractNativeLibs') is True and
                module['module'].get('compressNativeLibs') is False, '需要显式普通 native extraction 模式')
        permissions = module['module'].get('requestPermissions', [])
        require(permissions == [{'name': 'ohos.permission.INTERNET'}], '权限集合改变')
        for name in MATERIALS:
            require(data[RAW_PREFIX + name] == (bundle / name).read_bytes(), f'来源材料字节不匹配：{name}')
        dependencies = {}
        summaries = {}
        for name in sorted(NATIVE):
            basename = PurePosixPath(name).name
            extracted = Path(temporary) / basename
            extracted.write_bytes(data[name])
            info = ffmpeg.elf(extracted)
            require(info['soname'] == basename, f'SONAME 不匹配：{name}')
            require(set(info['needed']) <= BASE_LIBS | set(ffmpeg.LIBS) | PUBLIC, f'未批准依赖：{name}')
            if basename in ffmpeg.LIBS:
                require(data[name] == (bundle / basename).read_bytes(), f'已接收 FFmpeg artifact 字节改变：{name}')
                require(info == expected[basename], f'FFmpeg ELF 合同不匹配：{name}')
            else:
                allowed = BASE_DEPS[basename]
                # loader composition 可增加唯一已审查的公开 AbilityKit 依赖。
                actual = set(info['needed'])
                require(actual == allowed or (basename == 'libfrd_shell_ohos_ffi.so' and
                        actual == allowed | {'libability_runtime.so'}), f'既有 native 依赖合同改变：{name}')
                if basename == 'libfrd_shell_ohos_ffi.so':
                    require({'frd_ohos_abi_version', 'frd_ohos_create', 'frd_ohos_destroy'} <= set(info['exports']),
                            'Rust FFI 导出身份不匹配')
                if basename == 'libc++_shared.so':
                    sdk_cpp = readelf.parent.parent / 'lib/aarch64-linux-ohos/libc++_shared.so'
                    sdk_info = ffmpeg.elf(sdk_cpp)
                    require(info['build_ids'] and info['build_ids'] == sdk_info['build_ids'], 'C++ SDK Build ID 不匹配')
                    # Hvigor 使用 llvm-strip --strip-unneeded；独立生成所选 SDK 的预期包字节。
                    stripped = Path(temporary) / 'sdk-cpp-stripped.so'
                    subprocess.run([str(readelf.with_name('llvm-strip')), '--strip-unneeded', '-o', str(stripped), str(sdk_cpp)], check=True)
                    require(data[name] == stripped.read_bytes(), 'C++ 运行库不是所选 SDK strip 产物')
            for dependency in set(info['needed']) & PUBLIC:
                sdk_lib = sdk / 'native/sysroot/usr/lib/aarch64-linux-ohos' / dependency
                require(sdk_lib.is_file() and ffmpeg.elf(sdk_lib)['soname'] == dependency,
                        f'所选 SDK 无公开依赖：{dependency}')
                if dependency == 'libability_runtime.so':
                    require('OH_AbilityRuntime_ApplicationContextGetBundleCodeDir' in ffmpeg.elf(sdk_lib)['exports'],
                            '所选 SDK 缺少已审核 AbilityKit API')
            dependencies[basename] = info['needed']
            summaries[basename] = hashlib.sha256(data[name]).hexdigest()
        return {'bundle': module['app']['bundleName'], 'deviceTypes': module['module']['deviceTypes'],
                'nativeDependencies': dependencies, 'nativeSha256': summaries,
                'ffmpegManifestSha256': manifest_sha256, 'permissions': permissions}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--hap', type=Path, required=True)
    parser.add_argument('--readelf', type=Path, required=True)
    parser.add_argument('--ffmpeg-bundle', type=Path, required=True, help='显式接收的 OHOS 三库 bundle 目录')
    parser.add_argument('--ffmpeg-manifest-sha256', required=True, help='独立接收报告中的 manifest SHA256')
    parser.add_argument('--unsigned-hap', type=Path, help='签名后同时比较此原包的相关条目字节')
    args = parser.parse_args()
    print(json.dumps(check(args.hap, args.readelf, args.ffmpeg_bundle, args.ffmpeg_manifest_sha256), ensure_ascii=False, indent=2))
    if args.unsigned_hap:
        compare_signed(args.unsigned_hap, args.hap)


if __name__ == '__main__':
    main()
