#!/usr/bin/env python3
"""Freeze original RLGR/session catalog and additional independent actual source fixtures."""
import argparse,gzip,hashlib,json,struct,subprocess,tempfile
from pathlib import Path
from rfx_support import ROOT,S,artifacts,flags

def blob(v): return struct.pack('<I',len(v))+v

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--original-graphics',type=Path,required=True)
    p.add_argument('--original-session',type=Path,required=True)
    p.add_argument('--verify',action='store_true',help='Compare regenerated original outputs without replacing frozen expectations')
    a=p.parse_args()
    subprocess.run(['python3',str(ROOT/'tests/legacy-oracle/verify_sources.py')],check=True)
    inv=json.loads((ROOT/'tests/legacy-oracle/SOURCE-INVENTORY.json').read_text())
    for name,base in [('ironrdp-graphics-0.9.0',a.original_graphics),('ironrdp-session-0.11.0',a.original_session)]:
        for path,expected in inv[name]['files'].items():
            assert hashlib.sha256((base/path).read_bytes()).hexdigest()==expected,path
    catalog=json.loads(gzip.decompress((ROOT/'tests/legacy-oracle/fixtures/wire-catalog.json.gz').read_bytes()))['entries']
    entries=[e for e in catalog if e['codec'] in ['rlgr','session']]
    assert sum(e['codec']=='rlgr' for e in entries)==634
    assert sum(e['codec']=='session' for e in entries)==10
    data=b'FRDRFX01'+struct.pack('<I',len(entries))
    for e in entries:
        meta=bytes([1 if 'Rlgr1' in e['name'] else 3])+struct.pack('<I',e['width']) if e['codec']=='rlgr' else b'\x00'
        data+=b''.join(blob(v) for v in [e['name'].encode(),meta,bytes(e['input']),bytes(e['initial']),bytes(e['output_or_partial']),e['result'].encode()])
    catalog_path=S/'tests/fixtures/rfx-original-catalog.bin'
    if a.verify: assert catalog_path.read_bytes()==data
    else: catalog_path.write_bytes(data)
    libs=artifacts(original=True)
    for name,base in [('ironrdp_graphics',a.original_graphics),('ironrdp_session',a.original_session)]:
        assert str(base) not in libs[name] # libraries belong to the independent oracle target
    src=S/'tests/rfx_original_generator.rs'
    out=ROOT/'target/task-4a26-original-generator'
    subprocess.run(['rustc',str(src),'-o',str(out)]+flags(libs),check=True)
    with tempfile.TemporaryDirectory(prefix='task-4a26-original-',dir=ROOT/'target') as tmp:
        actual=Path(tmp)/'supplemental.bin'
        subprocess.run([str(out),str(actual)],check=True)
        destination=S/'tests/fixtures/rfx-original-supplemental.bin'
        if a.verify: assert destination.read_bytes()==actual.read_bytes()
        else: destination.write_bytes(actual.read_bytes())
    with tempfile.TemporaryDirectory(prefix='task-4a26-quant-',dir=ROOT/'target') as tmp:
        actual=Path(tmp)/'quant.bin'
        subprocess.run([str(out),str(actual),'quant-order'],check=True)
        destination=S/'tests/fixtures/rfx-original-quant-order.bin'
        if a.verify: assert destination.read_bytes()==actual.read_bytes()
        else: destination.write_bytes(actual.read_bytes())
    for path in sorted((S/'tests/fixtures').glob('*.bin')):
        print(path.name,len(path.read_bytes()),hashlib.sha256(path.read_bytes()).hexdigest())
if __name__=='__main__': main()
