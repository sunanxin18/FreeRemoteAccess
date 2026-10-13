"""校验冻结 wire hash 与原生产清单；仅允许精确记录的 Bitmap/RDP6 consumer/manifest 差异。"""
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
        assert set(modifications) == {'Cargo.toml', 'src/rle.rs', 'src/rdp6/rle.rs', 'src/rdp6/bitmap_stream/decoder.rs', 'src/rlgr.rs', 'src/lib.rs'}, set(modifications)
    elif name == 'ironrdp-session-0.11.0':
        inventory = json.loads((base / 'FRD-MODIFICATIONS.json').read_text())
        assert inventory['schema'] == 1
        assert inventory['upstream_archive_sha256'] == '3f1a5a20c24d8ebf47477f9f513201aeb894e7c7f779ea633ece65e42a9f0652'
        modifications = inventory['modified_originals']
        assert set(modifications) == {'src/rfx.rs', 'src/fast_path.rs', 'src/active_stage.rs', 'src/image.rs'}, set(modifications)
        assert inventory['image_consumer_fixture_sha256'] == 'c90e24ae846f89aabe58eebbfd2b9daa552b6787506c95a4db9d3fdb9743ec80'
        assert digest((base / 'tests/fixtures/image-consumers-original.bin').read_bytes()) == inventory['image_consumer_fixture_sha256']
        for relative, expected in inventory['added_files'].items():
            assert digest((base / relative).read_bytes()) == expected, relative
    elif name == 'ironrdp-pdu-0.9.0':
        inventory = json.loads((base / 'FRD-MODIFICATIONS.json').read_text())
        assert inventory['schema'] == 1
        assert inventory['upstream_archive_sha256'] == '4ccd1179a4d106df1930347701388b5c79bc3725fa7dab4438d57db0d9d19347'
        # 原四包清单保持只读；生产豁免不能重写或删减126项原始身份。
        frozen = json.loads((ROOT / 'tests/legacy-oracle/SOURCE-INVENTORY.json').read_text())[name]['files']
        assert originals == frozen and len(originals) == 126
        modifications = inventory['modified_originals']
        assert set(modifications) == {'src/basic_output/surface_commands/mod.rs'}, set(modifications)
        assert modifications['src/basic_output/surface_commands/mod.rs']['post_sha256'] == 'f4a8760276195eef7cff2ced8f6558e6450553ea24c4bf92fb2d8d49e9eb2220'
        assert {str(p.relative_to(base)) for p in (base / 'src').rglob('*.rs')} == {p for p in frozen if p.startswith('src/') and p.endswith('.rs')}
        assert set(inventory['added_files']) == {'tests/README.md', 'tests/surface_public.rs', 'tests/surface_session.rs', 'tests/surface_support.py', 'tests/freeze_surface.py', 'tests/run_surface_tests.py', 'tests/test_surface_mutations.py', 'tests/fixtures/surface-original-64.txt', 'tests/fixtures/SURFACE-PROVENANCE.json'}
        assert inventory['oracle_surface_sha256'] == '32aa7d326240ed7bf8ba9b897b8ee5bee5163a4bb180f132ca30b9948ecd84e6'
        assert inventory['added_files']['tests/fixtures/surface-original-64.txt'] == inventory['oracle_surface_sha256']
        for relative, expected in inventory['added_files'].items():
            assert relative.startswith('tests/'), relative
            assert digest((base / relative).read_bytes()) == expected, relative
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
    for name in ['ironrdp-graphics-0.9.0', 'ironrdp-session-0.11.0', 'ironrdp-pdu-0.9.0']:
        validate_production_copy(ROOT / 'third_party' / name, name)
    print('PASS:', len(entries), 'wire records', dict(collections.Counter(e['codec'] for e in entries)), '; exact documented production modifications only')
