"""校验冻结 wire hash 与原生产清单；仅允许精确记录的 Bitmap consumer/manifest 差异。"""
import collections
import gzip
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
def digest(data):
    return hashlib.sha256(data).hexdigest()

def validate(entry):
    for field, key in [('input','input_sha256'), ('initial','initial_sha256'), ('output_or_partial','expected_sha256')]:
        assert digest(bytes(entry[field])) == entry[key], (entry['name'], field)
    assert entry['consumed_bytes'] is None
    assert entry['result'].startswith(('Ok(', 'Err(', 'Panic '))

def validate_production_copy(base, name):
    originals = dict(line.split('  ', 1)[::-1] for line in (base / 'UPSTREAM-SHA256SUMS').read_text().splitlines())
    modifications = {}
    if name == 'ironrdp-graphics-0.9.0':
        inventory = json.loads((base / 'FRD-MODIFICATIONS.json').read_text())
        assert inventory['schema'] == 1
        assert inventory['upstream_archive_sha256'] == 'c7493e426b6a8104cd497e518ba7781a9c7fc9a5f58a9cc0c1111e6d66f63bcc'
        modifications = inventory['modified_originals']
        # 显式窄边界：不能通过向 inventory 添加其他源路径来扩大豁免。
        assert set(modifications) == {'Cargo.toml', 'src/rle.rs'}, set(modifications)
    for relative, expected in originals.items():
        if relative in modifications:
            record = modifications[relative]
            assert record['upstream_sha256'] == expected, relative
            expected = record['post_sha256']
        assert digest((base / relative).read_bytes()) == expected, str(base / relative)


if __name__ == '__main__':
    path = Path(__file__).parent / 'fixtures/wire-catalog.json.gz'
    data = path.read_bytes()
    assert digest(data) == path.with_suffix('.sha256').read_text().strip()
    entries = json.loads(gzip.decompress(data))['entries']
    assert len({e['name'] for e in entries}) == len(entries)
    for entry in entries:
        validate(entry)
    corrupted = dict(entries[0], input=entries[0]['input']+[0])
    try:
        validate(corrupted)
    except AssertionError:
        pass
    else:
        raise AssertionError('corrupted fixture must fail')
    for name in ['ironrdp-graphics-0.9.0', 'ironrdp-session-0.11.0']:
        validate_production_copy(ROOT / 'third_party' / name, name)
    print('PASS:', len(entries), 'wire records', dict(collections.Counter(e['codec'] for e in entries)), '; exact documented production modifications only')
