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
def materials(recipe="upstream"):
    return ffmpeg.files_for_recipe(recipe) - set(ffmpeg.LIBS)


def raw_files(recipe="upstream"):
    return {RAW_PREFIX + name for name in materials(recipe)}
# 逐项来自桥/Rust现有实际链接的公开 NDK 库；新依赖必须单独审核。
PUBLIC = {'libicu.so', 'libc.so', 'libace_napi.z.so', 'libace_ndk.z.so', 'libnative_window.so',
          'libhilog_ndk.z.so', 'libhuks_ndk.z.so', 'libohcrypto.so', 'libability_runtime.so'}
BASE_DEPS = {
    'libentry.so': {'libfrd_shell_ohos_ffi.so', 'libace_napi.z.so', 'libace_ndk.z.so',
                    'libnative_window.so', 'libhilog_ndk.z.so', 'libc++_shared.so', 'libc.so', 'libicu.so'},
    'libfrd_shell_ohos_ffi.so': {'libhilog_ndk.z.so', 'libnative_window.so', 'libhuks_ndk.z.so', 'libohcrypto.so', 'libc.so'},
    'libc++_shared.so': {'libc.so'},
}


FDK_PREFIX = 'resources/rawfile/fdk-aac/'
FDK_SOURCE = Path(__file__).resolve().parents[1] / 'apps/freeremotedesk-ohos/entry/src/main/resources/rawfile/fdk-aac'


def fdk_materials():
    return {FDK_PREFIX + name: (FDK_SOURCE / name).read_bytes() for name in ('NOTICE', 'SOURCE.json')}


def check_fdk_materials(data, apple_mvs=False):
    present = {name for name in data if name.startswith(FDK_PREFIX) and not name.endswith('/')}
    if apple_mvs or present:
        expected = fdk_materials()
        require(present == set(expected), 'FDK 来源/许可材料集合不匹配')
        require(all(data[name] == content for name, content in expected.items()), 'FDK 来源/许可材料字节改变')


def check_native_dependencies(basename, actual, apple_mvs=False, rdp_validation=False):
    allowed = BASE_DEPS[basename]
    if apple_mvs and basename == 'libfrd_shell_ohos_ffi.so':
        # 实际标准 MVS+RDP 最终 FFI 的新增依赖：FDK C++ 与 flate2/libz-sys。
        allowed = allowed | {'libc++_shared.so'}
        if rdp_validation:
            allowed = allowed | {'libz.so'}
    require(actual == allowed or (basename == 'libfrd_shell_ohos_ffi.so' and
            actual == allowed | {'libability_runtime.so'}), f'既有 native 依赖合同改变：{basename}')


def accepted_bundle(bundle: Path, sdk: Path, manifest_sha256: str):
    require(re.fullmatch('[0-9a-f]{64}', manifest_sha256) is not None, '需要独立接收的 manifest SHA256')
    require(ffmpeg.sha(bundle / 'manifest.json') == manifest_sha256, '接收的 manifest SHA256 不匹配')
    return ffmpeg.verify_bundle(bundle, sdk)


def entries(package, recipe="upstream", apple_mvs=False):
    raw = raw_files(recipe)
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
    require(raw <= result.keys(), 'HAP 缺少 FFmpeg 来源/许可材料')
    require({n for n in result if n.startswith(RAW_PREFIX) and not n.endswith('/')} == raw,
            'FFmpeg rawfile 文件集合不匹配')
    check_fdk_materials(result, apple_mvs)
    return result


def relevant_bytes(hap, recipe="upstream"):
    with zipfile.ZipFile(hap) as package:
        data = entries(package, recipe)
        selected = NATIVE | raw_files(recipe) | {'module.json'} | (set(data) & set(fdk_materials()))
        return {name: data[name] for name in selected}


def compare_signed(unsigned, signed, recipe="upstream"):
    require(relevant_bytes(unsigned, recipe) == relevant_bytes(signed, recipe), '签名改变了 native/来源材料/模块清单字节')


def check(hap: Path, readelf: Path, bundle: Path, manifest_sha256: str, apple_mvs=False, rdp_validation=False) -> dict:
    sdk = readelf.parent.parent.parent.parent
    expected = accepted_bundle(bundle, sdk, manifest_sha256)
    recipe = ffmpeg.recipes.identify(ffmpeg.read_json(bundle / "manifest.json"))
    public = PUBLIC | ({'libz.so'} if apple_mvs and rdp_validation else set())
    with zipfile.ZipFile(hap) as package, tempfile.TemporaryDirectory(prefix='frd-hap-check-') as temporary:
        data = entries(package, recipe, apple_mvs)
        module = json.loads(data['module.json'])
        require(module['app']['bundleName'] == 'org.freeremotedesk.client', '应用标识不符')
        require(module['module']['deviceTypes'] == ['phone', '2in1'], '需要真正 phone/2in1 清单')
        require(module['module'].get('extractNativeLibs') is True and
                module['module'].get('compressNativeLibs') is False, '需要显式普通 native extraction 模式')
        permissions = module['module'].get('requestPermissions', [])
        require(permissions == [{'name': 'ohos.permission.INTERNET'}], '权限集合改变')
        for name in materials(recipe):
            require(data[RAW_PREFIX + name] == (bundle / name).read_bytes(), f'来源材料字节不匹配：{name}')
        dependencies = {}
        summaries = {}
        for name in sorted(NATIVE):
            basename = PurePosixPath(name).name
            extracted = Path(temporary) / basename
            extracted.write_bytes(data[name])
            info = ffmpeg.elf(extracted)
            require(info['soname'] == basename, f'SONAME 不匹配：{name}')
            require(set(info['needed']) <= BASE_LIBS | set(ffmpeg.LIBS) | public, f'未批准依赖：{name}')
            if basename in ffmpeg.LIBS:
                require(data[name] == (bundle / basename).read_bytes(), f'已接收 FFmpeg artifact 字节改变：{name}')
                require(info == expected[basename], f'FFmpeg ELF 合同不匹配：{name}')
            else:
                check_native_dependencies(basename, set(info['needed']), apple_mvs, rdp_validation)
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
            for dependency in set(info['needed']) & public:
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
                'ffmpegManifestSha256': manifest_sha256, 'permissions': permissions,
                'appleMvsValidation': apple_mvs, 'rdpValidation': rdp_validation}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--hap', type=Path, required=True)
    parser.add_argument('--readelf', type=Path, required=True)
    parser.add_argument('--ffmpeg-bundle', type=Path, required=True, help='显式接收的 OHOS 三库 bundle 目录')
    parser.add_argument('--ffmpeg-manifest-sha256', required=True, help='独立接收报告中的 manifest SHA256')
    parser.add_argument('--normal-hap-rdp-validation', action='store_true', help='RDP 普通验证；与 Apple 同开时要求实际 zlib 精确依赖')
    parser.add_argument('--normal-hap-apple-mvs-validation', action='store_true', help='显式标准 MVS 的精确 C++/zlib 依赖及 FDK 材料合同')
    parser.add_argument('--unsigned-hap', type=Path, help='签名后同时比较此原包的相关条目字节')
    args = parser.parse_args()
    print(json.dumps(check(args.hap, args.readelf, args.ffmpeg_bundle, args.ffmpeg_manifest_sha256, args.normal_hap_apple_mvs_validation, args.normal_hap_rdp_validation), ensure_ascii=False, indent=2))
    if args.unsigned_hap:
        compare_signed(args.unsigned_hap, args.hap, ffmpeg.recipes.identify(ffmpeg.read_json(args.ffmpeg_bundle / "manifest.json")))


if __name__ == '__main__':
    main()
