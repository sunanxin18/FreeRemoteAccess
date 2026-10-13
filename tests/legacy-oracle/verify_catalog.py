"""校验冻结wire目录的逐例输入/初始/输出hash及生产源不变；重算另由Rust verify完成。"""
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
    for name in ['ironrdp-graphics-0.9.0','ironrdp-session-0.11.0']:
        base = ROOT / 'third_party' / name
        for line in (base / 'UPSTREAM-SHA256SUMS').read_text().splitlines():
            expected, relative = line.split('  ',1)
            if relative == 'Cargo.toml' and name.startswith('ironrdp-graphics'):
                continue  # 唯一获准差异：私有kernel manifest依赖。
            assert digest((base / relative).read_bytes()) == expected, str(base / relative)
    print('PASS:', len(entries), 'wire records', dict(collections.Counter(e['codec'] for e in entries)), '; original production source inventory unchanged')
