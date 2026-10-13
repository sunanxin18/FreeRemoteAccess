"""核验独立 Cargo 图中实际源文件以及四份缓存 archive；不修改 registry。"""
import hashlib
import json
import pathlib
from check_isolation import metadata, check_oracle, ROOT

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

if __name__ == '__main__':
    graph = metadata(ROOT / 'tests/legacy-oracle/Cargo.toml')
    check_oracle(graph)
    inventory = json.loads((ROOT / 'tests/legacy-oracle/SOURCE-INVENTORY.json').read_text())
    packages = {p['name'] + '-' + p['version']:p for p in graph['packages']}
    for name, source in inventory.items():
        base = pathlib.Path(packages[name]['manifest_path']).parent
        archive = base.parents[2] / 'cache' / base.parent.name / (name + '.crate')
        assert digest(archive) == source['archive_sha256'], str(archive)
        for relative, expected in source['files'].items():
            assert digest(base / relative) == expected, str(base / relative)
    print('PASS: all pinned original source files and four archives match inventory')
