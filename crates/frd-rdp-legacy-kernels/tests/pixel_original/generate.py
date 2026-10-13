#!/usr/bin/env python3
"""独立 rustc / 原版 process；复用已经冻结的独立原版 rlib，不修改其workspace。"""
import gzip
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
RUSTC = Path('/Users/sun/.rustup/toolchains/1.96.0-aarch64-apple-darwin/bin/rustc')
CACHE = Path('/Users/sun/.cargo/registry/cache/index.crates.io-1949cf8c6b5b557f')
DEPS = ROOT / 'target/legacy-oracle/debug/deps'
OUT = ROOT / 'target/pixel-kernels'
FILES = {
    'ironrdp-session-0.11.0': ('3f1a5a20c24d8ebf47477f9f513201aeb894e7c7f779ea633ece65e42a9f0652', ['image.rs']),
    'ironrdp-graphics-0.9.0': ('c7493e426b6a8104cd497e518ba7781a9c7fc9a5f58a9cc0c1111e6d66f63bcc', ['image_processing.rs', 'color_conversion.rs']),
}
def sha(data):
    return hashlib.sha256(data).hexdigest()

metadata = {'archives': {}, 'sources': {}, 'dependencies': {}, 'wrapper': {}}
for package, (digest, names) in FILES.items():
    archive = CACHE / (package + '.crate')
    assert sha(archive.read_bytes()) == digest
    metadata['archives'][package] = digest
    with tarfile.open(archive) as tar:
        for name in names:
            data = tar.extractfile(package + '/src/' + name).read()
            assert data == (HERE / 'source' / name).read_bytes()
            metadata['sources'][name] = sha(data)

args = [str(RUSTC), '--edition=2021', str(HERE / 'generator.rs'), '-L', 'dependency=' + str(DEPS)]
for name in ['ironrdp_session', 'ironrdp_graphics', 'ironrdp_core', 'ironrdp_pdu', 'tracing', 'yuv']:
    matches = list(DEPS.glob('lib' + name + '-*.rlib'))
    assert len(matches) == 1, matches
    rlib = matches[0]
    depfile = DEPS / (rlib.stem[3:] + '.d')
    text = depfile.read_text()
    assert '/.cargo/registry/src/' in text
    assert 'third_party/' not in text and 'frd-rdp-legacy-kernels' not in text
    metadata['dependencies'][name] = {'rlib_sha256': sha(rlib.read_bytes()), 'depfile_sha256': sha(depfile.read_bytes()), 'path': str(rlib.relative_to(ROOT))}
    args += ['--extern', name + '=' + str(rlib)]
OUT.mkdir(parents=True, exist_ok=True)
args += ['-o', str(OUT / 'original-generator')]
subprocess.run(args, check=True)
subprocess.run([str(OUT / 'original-generator'), str(OUT / 'original-pixel.bin')], check=True)
raw = (OUT / 'original-pixel.bin').read_bytes()
compressed = gzip.compress(raw, mtime=0)
(HERE / 'original-pixel.bin.gz').write_bytes(compressed)
metadata['wrapper']['generator.rs'] = sha((HERE / 'generator.rs').read_bytes())
metadata['wrapper']['generate.py'] = sha(Path(__file__).read_bytes())
metadata['rustc'] = subprocess.check_output([str(RUSTC), '-Vv'], text=True)
metadata['expected_sha256'] = sha(raw)
metadata['gzip_sha256'] = sha(compressed)
metadata['expected_bytes'] = len(raw)
(HERE / 'provenance.json').write_text(json.dumps(metadata, indent=2) + '\n')
print('Original source/include and archive hashes verified; fixture', len(raw), 'bytes', sha(raw))
