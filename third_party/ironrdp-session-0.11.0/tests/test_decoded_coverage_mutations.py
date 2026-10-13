#!/usr/bin/env python3
"""Executable actual-source faults must compile, then fail coverage/pixel semantics."""
import argparse,shutil,subprocess,tempfile
from pathlib import Path
from rfx_support import ROOT,S,artifacts
from run_decoded_coverage_tests import source,build_flags

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--release',action='store_true');a=p.parse_args()
    libs=artifacts(release=a.release);profile='release' if a.release else 'debug'
    mutations=[
        ('bbox-substitution','src/image.rs','rectangles: core::mem::take(&mut self.pending),',
         'rectangles: if self.pending.is_empty() { Vec::new() } else { vec![InclusiveRectangle::union_all(&core::mem::take(&mut self.pending))] },'),
        ('cursor-counted','src/image.rs','self.reserve_entry()?;\n                self.receipts.push(DecodedCoverageReceipt {',
         'self.reserve_entry()?; if self.pending.is_empty() { if let crate::fast_path::UpdateKind::Region(r) = update { self.pending.push(r.clone()); } }\n                self.receipts.push(DecodedCoverageReceipt {'),
        ('source-SW-to-DW','src/image.rs','let start = sy * stride;','let start = sy * row;'),
        ('source-SH-to-DH','src/image.rs','sh - 1 - y','dh - 1 - y'),
        ('len-as-written','src/fast_path.rs','if progress.written != expected {','if decoded.len() != expected {'),
        ('remove-call-rectangle-cap','src/image.rs','self.total_rectangles\n            .checked_add(count)\n            .is_none_or(|n| n > MAX_COVERAGE_ENTRIES)',
         'self.total_rectangles\n            .checked_add(count)\n            .is_none_or(|n| n > usize::MAX)'),
        ('reset-cap-per-update','src/image.rs','self.update_offset += updates.len();','self.total_rectangles = 0; self.update_offset += updates.len();'),
        ('index-mismap','src/active_stage.rs','graphics_output_index = stage_outputs.len();','graphics_output_index = stage_outputs.len() + 1;'),
        ('lossy-writer-intersection','src/image.rs','self.apply_tile_rows(source, source_format, regions, update, kernels, true)',
         'self.apply_tile_rows(source, source_format, regions, update, kernels, false)'),
    ]
    # Formatting-neutral resource anchor; match the whole exact current predicate once.
    text=(S/'src/image.rs').read_text()
    start=text.index('if self\n            .total_rectangles') if 'if self\n            .total_rectangles' in text else text.index('if self.total_rectangles')
    stop=text.index('\n        {',start)
    condition=text[start:stop]
    for i,item in enumerate(mutations):
        if item[0]=='remove-call-rectangle-cap':
            mutations[i]=(item[0],item[1],condition,condition.replace('n > MAX_COVERAGE_ENTRIES','n > usize::MAX'))
    with tempfile.TemporaryDirectory(prefix='task-4a28b-mutants-',dir=ROOT/'target') as temp:
        for name,relative,before,after in [('baseline',None,None,None)]+mutations:
            base=S
            if relative:
                base=Path(temp)/name;shutil.copytree(S/'src',base/'src');path=base/relative
                value=path.read_text();assert before in value,name;path.write_text(value.replace(before,after,1))
            runner=Path(temp)/(name+'.rs');runner.write_text(source(base));output=Path(temp)/(name+'-bin')
            subprocess.run(['rustc','--test',str(runner),'-o',str(output)]+build_flags(libs,a.release),check=True)
            result=subprocess.run([str(output),'decoded_coverage_tests'],text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
            log=ROOT/'target/task-4a28b'/f'mutant-{name}-{profile}.log';log.write_text(result.stdout)
            if name=='baseline':assert result.returncode==0,result.stdout
            else:
                failures=[line for line in result.stdout.splitlines() if ' ... FAILED' in line]
                assert result.returncode!=0 and failures,(name,result.stdout)
                print('KILLED',name,':','; '.join(failures),flush=True)
    print('PASS',len(mutations),'actual-source mutants compiled and killed',profile)
if __name__=='__main__':main()
