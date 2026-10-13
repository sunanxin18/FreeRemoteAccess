#!/usr/bin/env python3
"""冻结真实 registry 原版 public API，独立 process 输出；禁止从生产图生成 expected。"""
import argparse
import json
import subprocess
import tarfile
import sys
sys.dont_write_bytecode = True

from surface_support import CARGO, RUSTC, ENV, ROOT, PDU, SOURCE, ARCHIVE_SHA, digest, artifacts, flags

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--release', action='store_true')
    args = parser.parse_args()
    graph = json.loads(subprocess.check_output([CARGO, 'metadata', '--offline', '--locked', '--format-version', '1', '--manifest-path', str(ROOT / 'tests/legacy-oracle/Cargo.toml')], text=True, env=ENV))
    for p in graph['packages']:
        if p['name'] != 'frd-legacy-original-oracle':
            assert p['source'] and p['source'].startswith('registry+'), p['name']
    package = next(p for p in graph['packages'] if p['name'] == 'ironrdp-pdu')
    assert package['version'] == '0.9.0'
    registry = ROOT.__class__(package['manifest_path']).parent
    archive = registry.parents[2] / 'cache' / registry.parent.name / 'ironrdp-pdu-0.9.0.crate'
    assert digest(archive.read_bytes()) == ARCHIVE_SHA
    inventory = json.loads((ROOT / 'tests/legacy-oracle/SOURCE-INVENTORY.json').read_text())['ironrdp-pdu-0.9.0']['files']
    with tarfile.open(archive) as tar:
        actual = {}
        for member in tar.getmembers():
            if member.isfile():
                relative = str(ROOT.__class__(member.name).relative_to('ironrdp-pdu-0.9.0'))
                data = tar.extractfile(member).read()
                assert (registry / relative).read_bytes() == data, relative
                actual[relative] = digest(data)
        assert actual == inventory and len(actual) == 126
    libs = artifacts(original=True, release=args.release)
    mode = 'release' if args.release else 'debug'
    binary = ROOT / 'target' / ('task-4a27p-original-' + mode)
    subprocess.run([RUSTC, str(PDU / 'tests/surface_public.rs'), '-o', str(binary)] + flags(libs, args.release), check=True)
    generated = binary.with_suffix('.txt')
    subprocess.run([str(binary), str(generated)], check=True)
    fixture = PDU / 'tests/fixtures/surface-original-64.txt'
    fixture.parent.mkdir(exist_ok=True)
    if fixture.exists():
        assert fixture.read_bytes() == generated.read_bytes(), 'frozen original changed'
    else:
        assert not args.release, 'initial freeze must use original debug process'
        fixture.write_bytes(generated.read_bytes())
    record = {'schema': 1, 'upstream_archive_sha256': ARCHIVE_SHA, 'original_source_sha256': actual[SOURCE], 'records': len(generated.read_text().splitlines()), 'fixture_sha256': digest(generated.read_bytes()), 'pointer_width': 64, 'toolchain': '1.96.0', 'graph': 'independent tests/legacy-oracle registry-only', 'panic': 'unwind', 'overflow_checks': ['on', 'off'] if args.release else ['on'], 'observer_sha256': digest((PDU / 'tests/surface_public.rs').read_bytes())}
    (PDU / 'tests/fixtures/SURFACE-PROVENANCE.json').write_text(json.dumps(record, indent=2) + '\n')
    print('PASS original', mode, record['records'], 'records', record['fixture_sha256'])

if __name__ == '__main__':
    main()
