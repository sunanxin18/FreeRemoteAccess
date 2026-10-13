"""离线验证真实 Cargo 图；kernel dev 边也必须隔离。"""
import json
import pathlib
import subprocess
import os
import shutil

ROOT = pathlib.Path(__file__).resolve().parents[2]
CARGO = os.environ.get('CARGO') or shutil.which('cargo')
assert CARGO, 'cargo is required on PATH'

def check_oracle(graph):
    for package in graph['packages']:
        if package['name'] == 'frd-legacy-original-oracle':
            continue
        assert package['source'] and package['source'].startswith('registry+'), package['name']
        assert not package['name'].startswith('frd-'), package['name']
    for name, version in [('ironrdp-graphics', '0.9.0'), ('ironrdp-session', '0.11.0'), ('ironrdp-pdu', '0.9.0'), ('yuv', '0.8.17')]:
        assert {p['version'] for p in graph['packages'] if p['name'] == name} == {version}

def check_kernel(graph):
    packages = {p['id']: p for p in graph['packages']}
    nodes = {n['id']: n for n in graph['resolve']['nodes']}
    pending = [p['id'] for p in packages.values() if p['name'] == 'frd-rdp-legacy-kernels']
    assert len(pending) == 1, 'expected exactly one kernel package'
    visited = set()
    while pending:
        ident = pending.pop()
        if ident in visited:
            continue
        visited.add(ident)
        assert packages[ident]['name'] not in ['ironrdp-graphics', 'ironrdp-session', 'ironrdp-pdu', 'yuv'], packages[ident]['name']
        pending.extend(nodes[ident]['dependencies'])

def check_production(graph):
    assert len([p for p in graph['packages'] if p['name'] == 'ironrdp-pdu']) == 1, 'expected exactly one production PDU node'
    packages = {p['name']: p for p in graph['packages']}
    nodes = {n['id']: n for n in graph['resolve']['nodes']}
    graphics = packages['ironrdp-graphics']
    session = packages['ironrdp-session']
    pdu = packages['ironrdp-pdu']
    kernel = packages['frd-rdp-legacy-kernels']
    for package, version in [(graphics, '0.9.0'), (session, '0.11.0'), (pdu, '0.9.0')]:
        expected = ROOT / 'third_party' / (package['name'] + '-' + version) / 'Cargo.toml'
        assert pathlib.Path(package['manifest_path']) == expected
        assert package['source'] is None
        assert package['version'] == version
    assert graphics['id'] in nodes[session['id']]['dependencies']
    assert pdu['id'] in nodes[session['id']]['dependencies']
    assert kernel['id'] in nodes[graphics['id']]['dependencies']

def metadata(manifest):
    return json.loads(subprocess.check_output([CARGO, 'metadata', '--offline', '--locked', '--format-version', '1', '--manifest-path', str(manifest)], cwd=ROOT))

if __name__ == '__main__':
    production = metadata(ROOT / 'Cargo.toml')
    check_kernel(production)
    check_production(production)
    check_oracle(metadata(ROOT / 'tests/legacy-oracle/Cargo.toml'))
    print('PASS: kernel including dev graph has no original decoder; oracle is registry-only')
