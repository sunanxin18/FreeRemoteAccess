#!/usr/bin/env python3
"""Isolated mutants must compile and fail semantic or true call-site trace assertions."""
import json,shutil,subprocess,tempfile
from pathlib import Path
from rfx_support import ROOT,G,S,artifacts,flags,session_source

def main():
    libs=artifacts()
    mutations=[
        ('run-zero-hook-bypass','gfx','src/rlgr.rs','materializer.zero(&mut output[..size], ZeroSite::Run);','output[..size].fill(0);'),
        ('pad-zero-hook-bypass','gfx','src/rlgr.rs','materializer.zero(output, ZeroSite::Padding);','output.fill(0);'),
        ('LL3-wrong-range','gfx','src/legacy_native.rs','output[4032..4096]','output[4031..4095]'),
        ('quant-HL-LH-swap','gfx','src/legacy_native.rs','quant.hl1, quant.lh1','quant.lh1, quant.hl1'),
        ('reactivation-policy-loss','session','src/active_stage.rs','            processor.set_native_legacy_kernels(kernels);','            let _ = kernels;'),
        ('RFX-capability-loss','session','src/fast_path.rs','self.rfx_handler.set_native_legacy_kernels(kernels);','let _ = kernels;'),
    ]
    with tempfile.TemporaryDirectory(prefix='task-4a26-mutants-',dir=ROOT/'target') as temp:
        for name,kind,relative,before,after in mutations:
            base=Path(temp)/name;shutil.copytree(G if kind=='gfx' else S,base)
            path=base/relative;source=path.read_text();assert before in source,name;path.write_text(source.replace(before,after,1))
            if kind=='gfx':
                wrapper=base/'rlgr-module.rs';wrapper.write_text('include!('+json.dumps(str(base/'src/rlgr.rs'))+');\n#[cfg(test)] mod native_tests { include!('+json.dumps(str(G/'tests/rlgr_native.rs'))+'); }')
                source='#[allow(dead_code)]\n'+'\n'.join('#[path='+json.dumps(str(path))+'] '+vis+'mod '+mod+';' for mod,path,vis in [('utils',G/'src/utils.rs',''),('rlgr',wrapper,''),('legacy_native',base/'src/legacy_native.rs','pub ')])
            else:source=session_source(base)
            runner=base/'main.rs';runner.write_text(source);output=base/'mutant'
            subprocess.run(['rustc','--test',str(runner),'-o',str(output)]+flags(libs),check=True)
            result=subprocess.run([str(output)],text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
            failures=[v for v in result.stdout.splitlines() if ' ... FAILED' in v]
            assert result.returncode!=0 and failures,name
            print('KILLED',name,':','; '.join(failures),flush=True)
if __name__=='__main__':main()
