#!/usr/bin/env python3
"""Freeze actual original image source APIs in a registry-only process, no K/vendor graph."""
import argparse,hashlib,json,subprocess,tempfile
from pathlib import Path
from rfx_support import ROOT,S,artifacts,flags

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--original-session',type=Path,required=True);p.add_argument('--verify',action='store_true');p.add_argument('--release',action='store_true');a=p.parse_args();assert not a.release or a.verify, 'release only regenerates frozen expectations'
    subprocess.run(['python3',str(ROOT/'tests/legacy-oracle/verify_sources.py')],check=True)
    subprocess.run(['python3',str(ROOT/'tests/legacy-oracle/check_isolation.py')],check=True)
    inv=json.loads((ROOT/'tests/legacy-oracle/SOURCE-INVENTORY.json').read_text())['ironrdp-session-0.11.0']['files']
    for path,expected in inv.items():assert hashlib.sha256((a.original_session/path).read_bytes()).hexdigest()==expected,path
    libs=artifacts(original=True,release=a.release)
    with tempfile.TemporaryDirectory(prefix='task-4a27b-original-',dir=ROOT/'target') as tmp:
        tmp=Path(tmp);source=tmp/'original.rs';output=tmp/'generator';actual=tmp/'output.bin'
        source.write_text('pub use ironrdp_session::{SessionResult,SessionError,SessionErrorExt,custom_err};\nmod image { include!('+json.dumps(str(a.original_session/'src/image.rs'))+'); }\ninclude!('+json.dumps(str(S/'tests/image_consumers_original_generator.rs'))+');')
        subprocess.run(['rustc',str(source),'-o',str(output)]+flags(libs,a.release),check=True)
        subprocess.run([str(output),str(actual)],check=True)
        dest=S/'tests/fixtures/image-consumers-original.bin'
        if a.verify:assert dest.read_bytes()==actual.read_bytes()
        else:dest.write_bytes(actual.read_bytes())
        identity={'fixture_sha256':hashlib.sha256(actual.read_bytes()).hexdigest(),'original_image_sha256':inv['src/image.rs'],'generator_sha256':hashlib.sha256((S/'tests/image_consumers_original_generator.rs').read_bytes()).hexdigest(),'wrapper_sha256':hashlib.sha256(source.read_bytes()).hexdigest(),'profile':'release' if a.release else 'debug','direct_artifacts':{name:{'path':path,'sha256':hashlib.sha256(Path(path).read_bytes()).hexdigest()} for name,path in libs.items()}}
        if not a.verify:(S/'tests/fixtures/IMAGE-CONSUMERS-PROVENANCE.json').write_text(json.dumps(identity,indent=2)+'\n')
        print(json.dumps(identity,indent=2))
if __name__=='__main__':main()
