#!/usr/bin/env python3
"""Actual shipping session/graphics harness; no manifest/lock changes or scalar replacement."""
import argparse,json,subprocess,os
from pathlib import Path
from rfx_support import ROOT,G,S,artifacts,flags,session_source

def source(base=S):
    value=session_source(base)
    # Existing Stage/RFX tests remain present; new tests append to the actual image module.
    wrapper=ROOT/('target/task-4a27b-'+str(os.getpid())+'-image-module.rs')
    wrapper.write_text('include!('+json.dumps(str(base/'src/image.rs'))+');\n#[cfg(test)] mod image_consumers_tests { include!('+json.dumps(str(S/'tests/image_consumers_native.rs'))+'); }')
    value=value.replace('#[path='+json.dumps(str(base/'src/image.rs'))+'] pub mod image;', '#[path='+json.dumps(str(wrapper))+'] pub mod image;')
    return value

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--release',action='store_true');p.add_argument('--clippy',action='store_true');p.add_argument('--source',type=Path);a=p.parse_args()
    libs=artifacts(release=a.release);mode='release' if a.release else 'debug';path=ROOT/f'target/task-4a27b-session-{mode}.rs';path.write_text(source(a.source or S));out=path.with_suffix('')
    cmd=['clippy-driver' if a.clippy else 'rustc','--test',str(path),'-o',str(out)]+flags(libs,a.release)
    if a.clippy:cmd+=['-D','warnings','-A','clippy::collapsible_if']
    subprocess.run(cmd,check=True);subprocess.run([str(out),'--nocapture'],check=True)
    qoi_wrapper=ROOT/('target/task-4a27b-'+str(os.getpid())+'-qoi-image-module.rs')
    qoi_wrapper.write_text('include!('+json.dumps(str((a.source or S)/'src/image.rs'))+');\n#[cfg(test)] mod qoi_helper_tests { include!('+json.dumps(str(S/'tests/image_qoi_helper_native.rs'))+'); }')
    qoi_source=ROOT/f'target/task-4a27b-qoi-helper-{mode}.rs'
    qoi_source.write_text('pub use ironrdp_session::{SessionResult,SessionError,SessionErrorExt,custom_err,ActiveStageOutput,fast_path};\n#[allow(dead_code)] #[path='+json.dumps(str(qoi_wrapper))+'] mod image;')
    qoi_output=qoi_source.with_suffix('')
    qoi_cmd=['clippy-driver' if a.clippy else 'rustc','--test',str(qoi_source),'--cfg','feature="qoi"','-o',str(qoi_output)]+flags(libs,a.release)
    if a.clippy:qoi_cmd+=['-D','warnings']
    subprocess.run(qoi_cmd,check=True);subprocess.run([str(qoi_output),'--nocapture'],check=True)
if __name__=='__main__':main()
