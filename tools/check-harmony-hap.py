#!/usr/bin/env python3
"""核验 phone/2in1 HAP 的原生包边界；不代表设备加载或网络互操作。"""
import argparse
import json
from pathlib import Path
import re
import subprocess
import tempfile
import zipfile


def check(hap: Path, readelf: Path) -> dict:
    with zipfile.ZipFile(hap) as package, tempfile.TemporaryDirectory(prefix='frd-hap-check-') as temporary:
        names = package.namelist()
        module = json.loads(package.read('module.json'))
        assert set(module['module']['deviceTypes']) == {'phone', '2in1'}, '需要真正 phone/2in1 清单'
        libraries = [name for name in names if name.endswith('.so')]
        required = {'libentry.so', 'libfrd_shell_ohos_ffi.so', 'libc++_shared.so'}
        assert required <= {Path(name).name for name in libraries}, 'HAP 缺少 Rust/桥/OHOS C++ 运行库'
        dependencies = {}
        for name in libraries:
            assert name.startswith('libs/arm64-v8a/'), f'未批准的 ABI: {name}'
            extracted = Path(temporary) / Path(name).name
            extracted.write_bytes(package.read(name))
            headers = subprocess.check_output([str(readelf), '-h', str(extracted)], text=True)
            assert 'ELF64' in headers and re.search(r'Machine:\s+AArch64', headers), f'错误的 ELF ABI: {name}'
            dynamic = subprocess.check_output([str(readelf), '-d', str(extracted)], text=True)
            needed = re.findall(r'\(NEEDED\).*?\[(.*?)\]', dynamic)
            assert all('/' not in dependency and '\\' not in dependency for dependency in needed), f'DT_NEEDED 含宿主路径: {name}: {needed}'
            if Path(name).name == 'libc++_shared.so':
                notes = subprocess.check_output([str(readelf), '-n', str(extracted)], text=True)
                sdk_cpp = readelf.parent.parent / 'lib/aarch64-linux-ohos/libc++_shared.so'
                sdk_notes = subprocess.check_output([str(readelf), '-n', str(sdk_cpp)], text=True)
                build_id = re.search(r'Build ID: ([0-9a-f]+)', notes)
                sdk_id = re.search(r'Build ID: ([0-9a-f]+)', sdk_notes)
                assert '.note.ohos.ident' in notes and build_id and sdk_id and build_id[1] == sdk_id[1], 'C++ 运行库不是所选官方 OHOS SDK 的 ARM64 库'
            if Path(name).name == 'libfrd_shell_ohos_ffi.so':
                assert re.search(r'\(SONAME\).*?\[libfrd_shell_ohos_ffi.so\]', dynamic), 'Rust 动态库缺固定 SONAME'
            dependencies[Path(name).name] = needed
        assert 'libfrd_shell_ohos_ffi.so' in dependencies['libentry.so'], '桥未连接正确 Rust 动态库'
        assert 'libc++_shared.so' in dependencies['libentry.so'], '桥未连接 OHOS C++ 运行库'
        return {'bundle': module['app']['bundleName'], 'deviceTypes': module['module']['deviceTypes'], 'nativeDependencies': dependencies, 'permissions': module['module'].get('requestPermissions', [])}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--hap', type=Path, required=True)
    parser.add_argument('--readelf', type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(check(args.hap, args.readelf), ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
