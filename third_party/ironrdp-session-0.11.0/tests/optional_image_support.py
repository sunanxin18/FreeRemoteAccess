"""精确可选依赖的正常Cargo artifacts；真实shipping session保持root graph不变。"""
import hashlib,json,subprocess,tomllib,os
from pathlib import Path
from rfx_support import ROOT,S,artifacts,flags
from run_image_consumers_tests import source
WORKSPACE=ROOT/'target/task-4a27b-repair/optional-deps'

def optional_artifacts(release=False):
    expected={p['name']:p for p in tomllib.loads((S/'Cargo.lock').read_text())['package']}
    lock=tomllib.loads((WORKSPACE/'Cargo.lock').read_text())
    for package in lock['package']:
        if 'source' not in package:continue
        pinned=expected[package['name']]
        assert package['version']==pinned['version'] and package['checksum']==pinned['checksum'],package['name']
    command=['cargo','build','--offline','--locked','--manifest-path',str(WORKSPACE/'Cargo.toml'),'--message-format=json']
    if release:command+=['--release']
    process=subprocess.run(command,text=True,stdout=subprocess.PIPE,check=True)
    libraries={};native=[]
    for line in process.stdout.splitlines():
        if not line.startswith('{'):continue
        item=json.loads(line)
        if item.get('reason')=='compiler-artifact':
            for filename in item['filenames']:
                if filename.endswith('.rlib') and item['target']['name'] in ['qoi','zstd_safe']:libraries[item['target']['name']]=filename
        elif item.get('reason')=='build-script-executed':native.extend(item['linked_paths'])
    assert set(libraries)=={'qoi','zstd_safe'}
    cache=Path('/Users/sun/.cargo/registry/cache/index.crates.io-1949cf8c6b5b557f')
    for name in ['qoicoubeh','zstd-safe','zstd-sys']:
        package=expected[name];archive=cache/(name+'-'+package['version']+'.crate')
        assert hashlib.sha256(archive.read_bytes()).hexdigest()==package['checksum'],name
    identity={'lock_sha256':hashlib.sha256((WORKSPACE/'Cargo.lock').read_bytes()).hexdigest(),'packages':lock['package'],'artifacts':{name:{'path':path,'sha256':hashlib.sha256(Path(path).read_bytes()).hexdigest()}for name,path in libraries.items()},'native_search_paths':native}
    (ROOT/'target/task-4a27b-repair'/('dependency-identity-'+('release' if release else 'debug')+'.json')).write_text(json.dumps(identity,indent=2)+'\n')
    return libraries,native

def optional_source(base=S,old_default=False):
    value=source(base)
    wrapper=ROOT/('target/task-4a27b-repair/fast-path-'+str(os.getpid())+'.rs')
    tests=old_default_test_source() if old_default else 'include!('+json.dumps(str(S/'tests/image_optional_wire.rs'))+');'
    wrapper.write_text('include!('+json.dumps(str(base/'src/fast_path.rs'))+');\n#[cfg(test)] mod optional_wire_tests {'+tests+'}')
    value=value.replace('#[path='+json.dumps(str(base/'src/fast_path.rs'))+'] pub mod fast_path;','#[path='+json.dumps(str(wrapper))+'] pub mod fast_path;')
    # 仅harness observer，读取真实image call-site trace；不修改shipping image算法。
    image_wrapper=ROOT/('target/task-4a27b-'+str(os.getpid())+'-image-module.rs')
    observers=''if old_default else '\n#[cfg(test)] pub(crate) fn optional_native_calls_clear() { NATIVE_IMAGE_CALLS.with(|v|v.borrow_mut().clear()); }\n#[cfg(test)] pub(crate) fn optional_native_calls_count() -> usize { NATIVE_IMAGE_CALLS.with(|v|v.borrow().len()) }\n'
    content=image_wrapper.read_text();position=content.index('#[cfg(test)] mod image_consumers_tests')
    image_wrapper.write_text(content[:position]+observers+content[position:])
    return value

def optional_flags(libs,extra,native,release=False):
    result=flags(libs,release)+['--cfg','feature="qoi"','--cfg','feature="qoiz"']
    for name,path in extra.items():result+=['--extern',name+'='+path]
    result+=['-L','dependency='+str(Path(extra['qoi']).parent)]
    for path in native:result+=['-L',path]
    return result

def prepare_dependencies():
    """只在ignored验证workspace正常解析/fetch；之后统一offline locked。"""
    WORKSPACE.mkdir(parents=True,exist_ok=True);(WORKSPACE/'src').mkdir(exist_ok=True)
    manifest='[package]\nname="frd-image-optional-validation"\nversion="0.0.0"\nedition="2024"\npublish=false\n[workspace]\n[dependencies]\nqoicoubeh="=0.5.0"\nzstd-safe={version="=7.2.4",features=["std"]}\n[profile.release]\nlto="thin"\n'
    path=WORKSPACE/'Cargo.toml'
    if path.exists():assert tomllib.loads(path.read_text())==tomllib.loads(manifest)
    else:path.write_text(manifest)
    (WORKSPACE/'src/lib.rs').write_text('//! A2.7B 可选wire验证依赖容器；不是产品manifest。\n')
    if not (WORKSPACE/'Cargo.lock').exists():subprocess.run(['cargo','generate-lockfile','--manifest-path',str(path)],check=True)
    expected={p['name']:p for p in tomllib.loads((S/'Cargo.lock').read_text())['package']}
    # Cargo自身维护独立锁；先固定sys，再收拢它的build依赖，禁止手抄锁。
    names=[p['name'] for p in tomllib.loads((WORKSPACE/'Cargo.lock').read_text())['package']if 'source'in p]
    names.sort(key=lambda name:(name!='zstd-sys',name))
    for name in names:
        current={p['name']:p for p in tomllib.loads((WORKSPACE/'Cargo.lock').read_text())['package']}
        if current[name]['version']!=expected[name]['version']:
            subprocess.run(['cargo','update','--manifest-path',str(path),'-p',name,'--precise',expected[name]['version']],check=True)
    subprocess.run(['cargo','fetch','--locked','--manifest-path',str(path)],check=True)

FIX_BASE='848258c2a9b90f71144f9b5c5f43694a748e614c'
def old_default_snapshot():
    """从固定Git对象读源，不改变共享checkout或root/oracle图。"""
    base=ROOT/'target/task-4a27b-repair/old-default-source';prefix='third_party/ironrdp-session-0.11.0/'
    names=subprocess.check_output(['git','ls-tree','-r','--name-only',FIX_BASE,'--',prefix+'src'],cwd=ROOT,text=True).splitlines()
    identities={}
    for name in names:
        data=subprocess.check_output(['git','show',FIX_BASE+':'+name],cwd=ROOT);path=base/name.removeprefix(prefix);path.parent.mkdir(parents=True,exist_ok=True);path.write_bytes(data);identities[name]=hashlib.sha256(data).hexdigest()
    (base/'source-identity.json').write_text(json.dumps({'base':FIX_BASE,'files':identities},indent=2)+'\n')
    return base

def old_default_test_source():
    value=(S/'tests/image_optional_wire.rs').read_text()
    # 复用同一wire encoder和公开None消费者测试；不带任何native helper调用。
    common=value[value.index('fn processor('):value.index('#[test]\nfn qoi_same_area_')]
    formats=value[value.index('const FORMATS:'):value.index('#[test]\nfn optional_wire_rgb_')]
    test=value[value.index('#[test]\nfn old_default_optional_wire_regression_digest()'):]
    return 'use super::*;\nuse ironrdp_core::{Encode as _,WriteCursor};\nuse ironrdp_pdu::fast_path::{EncryptionFlags,FastPathHeader,FastPathUpdatePdu,Fragmentation,UpdateCode};\nuse ironrdp_pdu::surface_commands::{ExtendedBitmapDataPdu,SurfaceBitsPdu};\n'+common+formats+test
