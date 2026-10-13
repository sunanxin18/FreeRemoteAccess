"""Scoped actual-source harness support; artifacts come from Cargo JSON, never glob hashes."""
import json
from pathlib import Path
import subprocess

VENDOR = Path(__file__).resolve().parents[1]
ROOT = VENDOR.parent.parent
ORIGINAL = Path('/Users/sun/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ironrdp-graphics-0.9.0')


def artifacts(original=False, release=False):
    assert subprocess.check_output(['rustc', '--version'], text=True).startswith('rustc 1.96.0 ')
    command = ['cargo', 'build', '--offline', '--locked', '--message-format=json', '--manifest-path',
               str(ROOT / ('tests/legacy-oracle/Cargo.toml' if original else 'Cargo.toml'))]
    if not original:
        command += ['-p', 'frd-protocol-rdp']
    if release:
        command += ['--release']
    output = subprocess.check_output(command, text=True)
    result = {}
    for line in output.splitlines():
        if not line.startswith('{'):
            continue
        item = json.loads(line)
        if item.get('reason') != 'compiler-artifact':
            continue
        files = [p for p in item['filenames'] if p.endswith('.rlib')]
        if files:
            name = item['target']['name']
            if name not in ['ironrdp_graphics', 'ironrdp_core', 'ironrdp_pdu', 'byteorder', 'frd_rdp_legacy_kernels']:
                continue
            assert name not in result or result[name] == files[0], name
            result[name] = files[0]
    return result


def compiler_args(libraries, release=False):
    result = ['--edition=2024', '-L', 'dependency=' + str(Path(libraries['ironrdp_graphics']).parent)]
    for name in ['ironrdp_graphics', 'ironrdp_core', 'ironrdp_pdu', 'byteorder', 'frd_rdp_legacy_kernels']:
        if name in libraries:
            result += ['--extern', name + '=' + libraries[name]]
    if release:
        result += ['-C', 'opt-level=3', '-C', 'lto=thin', '-C', 'embed-bitcode=yes']
    return result


def source_modules(base, snapshot=False):
    rle = json.dumps(str(base / 'src/rdp6/rle.rs'))
    decoder = json.dumps(str(base / 'src/rdp6/bitmap_stream/decoder.rs'))
    accessor = '''
pub fn snapshot(wire: &[u8], initial: &[u8], w: usize, h: usize) -> (Vec<u8>, String, Vec<u8>) {
 let mut decoder=BitmapStreamDecoder::default(); let mut dst=initial.to_vec();
 let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decoder.decode_bitmap_stream_to_rgb24(wire,&mut dst,w,h)));
 let result=match result { Ok(r)=>format!("{r:?}"), Err(_)=>"Panic geometry".to_owned() };
 (dst,result,decoder.planes_buffer)
}
''' if snapshot else ''
    return f'''pub use ironrdp_graphics::color_conversion;
mod rdp6 {{
 #[path={rle}] pub mod rle;
 pub mod bitmap_stream {{ pub mod decoder {{ include!({decoder}); {accessor} }} pub use decoder::*; }}
}}
'''
