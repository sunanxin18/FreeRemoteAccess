#!/usr/bin/env python3
"""Compile actual patched RLGR and session sources plus focused fixture tests."""
import argparse,json,subprocess
from rfx_support import ROOT,G,S,artifacts,flags,session_source

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--release',action='store_true');p.add_argument('--clippy',action='store_true');p.add_argument('--core-only',action='store_true');a=p.parse_args()
    libs=artifacts(release=a.release)
    root=ROOT/'target';mode='release' if a.release else 'debug'
    wrapper=root/'task-4a26-rlgr-module.rs'
    wrapper.write_text('include!('+json.dumps(str(G/'src/rlgr.rs'))+');\n#[cfg(test)] mod native_tests { include!('+json.dumps(str(G/'tests/rlgr_native.rs'))+'); }')
    source='#[allow(dead_code)]\n'+'\n'.join('#[path='+json.dumps(str(path))+'] '+vis+'mod '+name+';' for name,path,vis in [('utils',G/'src/utils.rs',''),('rlgr',wrapper,''),('legacy_native',G/'src/legacy_native.rs','pub ')])
    jobs=[('core',source)]
    if not a.core_only: jobs.append(('session',session_source()))
    for kind,source in jobs:
        path=root/f'task-4a26-{kind}-{mode}.rs';path.write_text(source);output=path.with_suffix('')
        cmd=['clippy-driver' if a.clippy else 'rustc','--test',str(path),'-o',str(output)]+flags(libs,a.release)
        if a.clippy:cmd+=['-D','warnings','-A','unfulfilled_lint_expectations','-A','clippy::collapsible_if']
        subprocess.run(cmd,check=True);subprocess.run([str(output)],check=True)
if __name__=='__main__':main()
