"""使用精确 toolchain、独立原版 Cargo 图和实际 public rlib；不修改原图。"""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
PDU = ROOT / 'third_party/ironrdp-pdu-0.9.0'
SOURCE = 'src/basic_output/surface_commands/mod.rs'
TOOLCHAIN = Path.home() / '.rustup/toolchains/1.96.0-aarch64-apple-darwin/bin'
CARGO = str(TOOLCHAIN / 'cargo')
RUSTC = str(TOOLCHAIN / 'rustc')
ENV = dict(os.environ, RUSTC=RUSTC, RUSTDOC=str(TOOLCHAIN / 'rustdoc'))
ARCHIVE_SHA = '4ccd1179a4d106df1930347701388b5c79bc3725fa7dab4438d57db0d9d19347'

def digest(data):
    return hashlib.sha256(data).hexdigest()

def artifacts(original=False, release=False):
    assert subprocess.check_output([RUSTC, '--version'], text=True).startswith('rustc 1.96.0 ')
    cmd = [CARGO, 'build', '--offline', '--locked', '--message-format=json', '--manifest-path', str(ROOT / ('tests/legacy-oracle/Cargo.toml' if original else 'Cargo.toml'))]
    if not original:
        cmd += ['-p', 'frd-protocol-rdp']
    if release:
        cmd += ['--release']
    result = {}
    completed = subprocess.run(cmd, text=True, env=ENV, stdout=subprocess.PIPE)
    for line in completed.stdout.splitlines():
        if not line.startswith('{'):
            continue
        v = json.loads(line)
        if v.get('reason') == 'compiler-message':
            print(v['message'].get('rendered', v['message']['message']), file=sys.stderr)
        if v.get('reason') == 'compiler-artifact':
            files = [p for p in v['filenames'] if p.endswith('.rlib')]
            if files:
                name = v['target']['name']
                if name.startswith('ironrdp_') or name in ['frd_rdp_legacy_kernels', 'tracing', 'bitvec', 'byteorder', 'yuv']:
                    assert name not in result or result[name] == files[0], name
                    result[name] = files[0]
    completed.check_returncode()
    return result

def flags(libs, release=False):
    out = ['--edition=2024', '-L', 'dependency=' + str(Path(libs['ironrdp_pdu']).parent)]
    for name, path in libs.items():
        out += ['--extern', name + '=' + path]
    out += ['-C', 'panic=unwind', '-C', 'overflow-checks=' + ('off' if release else 'on')]
    if release:
        out += ['-C', 'opt-level=3', '-C', 'lto=thin', '-C', 'embed-bitcode=yes']
    return out

def length_expression(source):
    # 精确抽取实际 decoder 中的 arithmetic leaf，不能用另写的公式替代。
    match = re.search(r'let expected_remaining_size = if flags.contains\(BitmapDataFlags::COMPRESSED_BITMAP_HEADER_PRESENT\) \{(.*?)\} else \{\s*data_length\s*\};', source, re.S)
    assert match, 'length branch not found'
    return match.group(1).strip()

def arithmetic_source(source):
    expression = length_expression(source).replace('BitmapDataHeader::ENCODED_SIZE', '24u32')
    return '''#[allow(unused_imports)]
use ironrdp_core::{DecodeResult, DecodeErrorKind, other_err, invalid_field_err};
fn required(data_length: u32) -> DecodeResult<u32> { Ok(''' + expression + ''') }
#[test] fn extracted_u32_arithmetic_precise_overflow_error() {
    assert_eq!(required(u32::MAX - 24).unwrap(), u32::MAX);
    for length in [u32::MAX - 23, u32::MAX] {
        let error = required(length).unwrap_err();
        assert!(matches!(error.kind(), DecodeErrorKind::Other { description } if *description == "bitmap data length including header exceeds usize"));
    }
}
'''

def observed_session_source(root, source, injected=False):
    # 全部 session module 均来自现有生产源码；只有临时wrapper加入入口计数。
    # 注入仅用于单独命名的u32错误dispatch测试，不用于public PDU parity。
    session = ROOT / 'third_party/ironrdp-session-0.11.0'
    lib = (session / 'src/lib.rs').read_text().replace('#![cfg_attr(doc, doc = include_str!("../README.md"))]', '')
    for module in ['macros', 'fast_path', 'image', 'pointer', 'rfx', 'x224', 'active_stage', 'palette']:
        declaration = ('pub ' if module in ['fast_path', 'image', 'pointer', 'rfx', 'x224'] else '') + 'mod ' + module + ';'
        prefix = 'pub ' if declaration.startswith('pub ') else ''
        path = session / 'src' / (module + '.rs')
        if not path.exists():
            path = session / 'src' / module / 'mod.rs'
        if module in ['fast_path', 'image']:
            data = path.read_text()
            method = 'process_surface_commands' if module == 'fast_path' else 'pointer_rendering_begin'
            marker = re.search(r'fn ' + method + r'\(.*?\) -> [^{]+\{', data, re.S)
            assert marker, method
            counter = 'SURFACE_ENTRIES' if module == 'fast_path' else 'CURSOR_ENTRIES'
            data = data[:marker.end()] + '\n crate::' + counter + '.with(|v| v.set(v.get()+1));\n' + data[marker.end():]
            if injected and module == 'fast_path':
                binding = 'let update = FastPathUpdate::decode_with_code(data.as_slice(), update_code);'
                assert data.count(binding) == 1
                data = data.replace(binding, 'let update = crate::required(u32::MAX).and_then(|_| FastPathUpdate::decode_with_code(data.as_slice(), update_code));')
            path = root / ('observed-' + module + '.rs')
            path.write_text(data)
        lib = lib.replace(declaration, '#[path=' + json.dumps(str(path)) + '] ' + prefix + 'mod ' + module + ';')
    lib += '''
extern crate self as ironrdp_session;
std::thread_local! {
    static SURFACE_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CURSOR_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
fn reset_observations() { SURFACE_ENTRIES.with(|v| v.set(0)); CURSOR_ENTRIES.with(|v| v.set(0)); }
fn observations() -> (usize, usize) { (SURFACE_ENTRIES.with(|v| v.get()), CURSOR_ENTRIES.with(|v| v.get())) }
#[cfg(test)] mod surface_tests { include!(''' + json.dumps(str(PDU / 'tests/surface_session.rs')) + '''); }
'''
    if injected:
        lib += arithmetic_source(source)
    return lib
