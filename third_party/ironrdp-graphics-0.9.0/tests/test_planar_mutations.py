#!/usr/bin/env python3
"""Only isolated source copies are mutated; each mutant must compile and fail semantic assertions."""
import shutil
import subprocess
import tempfile
from pathlib import Path
from planar_support import ROOT, VENDOR, artifacts, compiler_args
from run_planar_tests import harness_source


def main():
    libs=artifacts()
    mutations=[
        ('discard-rle-alpha-consumption','if self.bitmap.header.use_alpha {','if false && self.bitmap.header.use_alpha {'),
        ('floor-odd-chroma','(image_width.div_ceil(2), image_height.div_ceil(2))','(image_width / 2, image_height / 2)'),
        ('clamp-cll7','color_loss_level,\n                    subsampled:', 'color_loss_level: color_loss_level.min(6),\n                    subsampled:'),
    ]
    with tempfile.TemporaryDirectory(prefix='task-4a25-mutants-',dir=ROOT/'target') as temp:
        for name,before,after in mutations:
            base=Path(temp)/name
            (base/'src/rdp6/bitmap_stream').mkdir(parents=True)
            for relative in ['src/rdp6/rle.rs','src/rdp6/bitmap_stream/decoder.rs']:
                shutil.copyfile(VENDOR/relative,base/relative)
            path=base/'src/rdp6/bitmap_stream/decoder.rs'
            source=path.read_text();assert before in source,name
            path.write_text(source.replace(before,after,1))
            runner=base/'main.rs';runner.write_text(harness_source(base))
            output=base/'mutant'
            subprocess.run(['rustc','--test',str(runner),'-o',str(output)]+compiler_args(libs),check=True)
            result=subprocess.run([str(output)],text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
            assert result.returncode!=0,name
            failures=[line for line in result.stdout.splitlines() if ' ... FAILED' in line]
            assert failures,name
            print('KILLED',name,':', '; '.join(failures))


if __name__=='__main__':
    main()
