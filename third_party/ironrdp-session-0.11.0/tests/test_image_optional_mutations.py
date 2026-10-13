#!/usr/bin/env python3
"""真实shipping可选feature源码mutation；分配guard杀错前不触大内存。"""
import argparse,shutil,subprocess,tempfile
from pathlib import Path
from optional_image_support import ROOT,S,artifacts,optional_artifacts,optional_flags,optional_source
from run_image_optional_tests import compile_harness

def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--release',action='store_true');args=parser.parse_args();mode='release'if args.release else'debug'
    baseline=compile_harness(args.release);subprocess.run([str(baseline)],check=True)
    libs=artifacts(release=args.release);extra,native=optional_artifacts(args.release);compiler_flags=optional_flags(libs,extra,native,args.release)
    text=(S/'src/fast_path.rs').read_text()
    header='    let mut decoder = qoi::Decoder::new(data).map_err(|e| custom_err!("native QOI header", e))?;'
    preflight='    image.native_bitmap_size(format, width, height, stride, &destination)?;'
    mutations=[
        ('qoi-unbounded-vector-before-header',header,'    let _unbounded = qoi::decode_to_vec(data).map_err(|e| custom_err!("mutant QOI decode", e))?;\n'+header,'qoi_oversize_header_rejects_before_any_large_allocation',73),
        ('qoi-declared-shape-bypass','if width != usize::from(declared_width) || height != usize::from(declared_height) {','if false {','qoi_same_area_different_header_shape_is_rejected_before_publication',101),
        ('qoiz-initial-compressed-length','NATIVE_QOIZ_INITIAL_BYTES.min(limit)','compressed.len().min(limit)','qoiz_first_staging_is_bounded_independently_of_compressed_length',73),
        ('qoiz-growth-cap-bypass','    let required = checked_native_staging_size(required, limit)?;','    let _ = limit;','qoiz_real_growth_budget_is_checked_before_reserve_and_decode',73),
        ('qoi-preflight-after-decode',preflight,'','qoi_destination_preflight_precedes_allocation',101),
        ('qoi-reserve-source-erased','custom_err!("native QOI allocation", e)','reason_err!("native QOI allocation", "{}", e)','qoi_allocation_failure_keeps_typed_source_and_visible_cursor',101),
        ('qoiz-reserve-source-erased','custom_err!("native QOIZ allocation", e)','reason_err!("native QOIZ allocation", "{}", e)','qoiz_initial_and_growth_reserve_failures_leave_image_cursor_and_pending_mouse',101),
        ('qoi-native-to-default-scalar','qoi_apply_native(image, destination, ext.data, ext.width, ext.height, kernels)?','{ let mut ignored=None; qoi_apply(image,destination,ext.data,&mut ignored)?; ignored }','optional_wire_rgb_rgba_all_formats_set_stream_default_parity_and_canaries',101),
    ]
    with tempfile.TemporaryDirectory(prefix='task-4a27b-optional-mutants-',dir=ROOT/'target')as folder:
        for name,before,after,test,status in mutations:
            base=Path(folder)/name;shutil.copytree(S/'src',base/'src');value=text;assert value.count(before)==1,name;value=value.replace(before,after,1)
            if name=='qoi-preflight-after-decode':value=value.replace('    if written != size {',preflight+'\n    if written != size {',1)
            (base/'src/fast_path.rs').write_text(value);source=base/'main.rs';source.write_text(optional_source(base));output=base/'mutant'
            subprocess.run(['rustc','--test',str(source),'-o',str(output)]+compiler_flags,check=True)
            result=subprocess.run([str(output),'fast_path::optional_wire_tests::'+test,'--exact','--nocapture'],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
            log=ROOT/'target/task-4a27b-repair'/('mutant-'+name+'-'+mode+'.log');log.write_text(result.stdout+'\nEXIT_STATUS '+str(result.returncode)+'\n')
            assert result.returncode==status,(name,result.returncode,result.stdout)
            if status==101:assert ' ... FAILED'in result.stdout,name
            print('KILLED',name,'exit',result.returncode,'actual source',test,flush=True)
    print('all',len(mutations),'actual optional shipping source mutants compiled and killed',mode)
if __name__=='__main__':main()
