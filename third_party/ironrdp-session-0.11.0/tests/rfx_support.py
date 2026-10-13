"""Actual source harnesses use Cargo JSON artifacts; original oracle has a separate graph."""
import json,os
from pathlib import Path
import subprocess
ROOT=Path(__file__).resolve().parents[3]
G=ROOT/'third_party/ironrdp-graphics-0.9.0'
S=ROOT/'third_party/ironrdp-session-0.11.0'

def artifacts(original=False,release=False):
    assert subprocess.check_output(['rustc','--version'],text=True).startswith('rustc 1.96.0 ')
    cmd=['cargo','build','--offline','--locked','--message-format=json','--manifest-path',str(ROOT/('tests/legacy-oracle/Cargo.toml' if original else 'Cargo.toml'))]
    if not original: cmd+=['-p','frd-protocol-rdp']
    if release: cmd+=['--release']
    result={}
    for line in subprocess.check_output(cmd,text=True).splitlines():
        if not line.startswith('{'): continue
        v=json.loads(line)
        if v.get('reason')=='compiler-artifact':
            files=[p for p in v['filenames'] if p.endswith('.rlib')]
            if files:
                name=v['target']['name']
                if not (name.startswith('ironrdp_') or name in ['frd_rdp_legacy_kernels','tracing','bitvec','byteorder','yuv']): continue
                assert name not in result or result[name]==files[0],name
                result[name]=files[0]
    return result

def flags(libs,release=False):
    out=['--edition=2024','-L','dependency='+str(Path(libs['ironrdp_graphics']).parent)]
    for name,path in libs.items(): out+=['--extern',name+'='+path]
    if release: out+=['-C','opt-level=3','-C','lto=thin','-C','embed-bitcode=yes']
    return out

def session_source(base=S):
    # Module wrappers append focused private tests to the actual shipping sources.
    source=(base/'src/lib.rs').read_text()
    source=source.replace('#![cfg_attr(doc, doc = include_str!("../README.md"))]','')
    for module in ['macros','fast_path','image','pointer','rfx','x224','active_stage','palette']:
        declaration=('pub ' if module in ['fast_path','image','pointer','rfx','x224'] else '')+'mod '+module+';'
        prefix='pub ' if declaration.startswith('pub ') else ''
        if module in ['rfx','active_stage']:
            wrapper=ROOT/'target'/('task-4a26-'+str(os.getpid())+'-'+module+'.rs')
            wrapper.write_text('include!('+json.dumps(str(base/'src'/f'{module}.rs'))+');\n#[cfg(test)] mod native_tests { include!('+json.dumps(str(S/'tests'/f'{module}_native.rs'))+'); }\n')
            path=wrapper
        else:
            path=base/'src'/f'{module}.rs'
            if not path.exists(): path=base/'src'/module/'mod.rs'
        source=source.replace(declaration,'#[path='+json.dumps(str(path))+'] '+prefix+'mod '+module+';')
    return source
