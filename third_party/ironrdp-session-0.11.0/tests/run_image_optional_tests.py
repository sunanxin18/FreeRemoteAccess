#!/usr/bin/env python3
"""执行实际shipping QOI/QOIZ feature branches，独立锁及Cargo checksum依赖。"""
import argparse,subprocess,re
from optional_image_support import ROOT,S,artifacts,optional_artifacts,optional_source,optional_flags,prepare_dependencies,old_default_snapshot

def compile_harness(release=False,clippy=False,base=S,old_default=False,production=False):
    libs=artifacts(release=release);extra,native=optional_artifacts(release)
    mode=('release' if release else 'debug')+('-old-default' if old_default else '')+('-production' if production else '');source=ROOT/('target/task-4a27b-repair/optional-'+mode+'.rs');source.write_text(optional_source(base,old_default));out=source.with_suffix('')
    command=['clippy-driver' if clippy else 'rustc']+(['--crate-type','lib']if production else ['--test'])+[str(source),'-o',str(out)]+optional_flags(libs,extra,native,release)
    if clippy:command+=['-D','warnings','-A','clippy::collapsible_if']
    subprocess.run(command,check=True);return out

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--release',action='store_true');p.add_argument('--clippy',action='store_true');p.add_argument('--test');p.add_argument('--prepare-deps',action='store_true');p.add_argument('--verify-old-default',action='store_true');p.add_argument('--production',action='store_true');a=p.parse_args()
    if a.prepare_deps:prepare_dependencies()
    out=compile_harness(a.release,a.clippy,production=a.production)
    if a.production:return
    command=[str(out),'--nocapture']
    if a.test:command += [a.test,'--exact']
    subprocess.run(command,check=True)
    if a.verify_old_default:
        test='fast_path::optional_wire_tests::old_default_optional_wire_regression_digest'
        new=subprocess.check_output([str(out),test,'--exact','--nocapture'],text=True)
        old=compile_harness(a.release,base=old_default_snapshot(),old_default=True)
        previous=subprocess.check_output([str(old),test,'--exact','--nocapture'],text=True)
        pattern=r'DEFAULT_DIGEST 64 [0-9a-f]+'
        assert re.search(pattern,new)[0]==re.search(pattern,previous)[0]
        print('fixed848258c old/new None '+re.search(pattern,new)[0],flush=True)
if __name__=='__main__':main()
