#!/usr/bin/env python3
"""Isolated actual source mutants compile, then fail numerical/publication or real call-site assertions."""
import argparse,shutil,subprocess,tempfile
from pathlib import Path
from rfx_support import ROOT,S,artifacts,flags
from run_image_consumers_tests import source

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--release',action='store_true');a=p.parse_args();libs=artifacts(release=a.release)
    mutations=[
        ('underfill-consumer-bypass','src/fast_path.rs','ironrdp_graphics::rle::decompress_native_complete(', 'ironrdp_graphics::rle::decompress_native('),
        ('source-height-to-destination-height','src/image.rs','source_height - 1 - y','height - 1 - y'),
        ('source-rechunk-by-destination-width','src/image.rs','let start = source_y * row_stride;', 'let start = source_y * source_row_bytes;'),
        ('Region32-to-Raw32-alpha','src/image.rs','PixelRowSource::Region32(native_pixel_format(source_format))','PixelRowSource::Raw32(native_pixel_format(source_format))'),
        ('Set-treated-as-Stream','src/fast_path.rs','native_surface_rectangle(&bits.destination, ext.width, ext.height, set)?','native_surface_rectangle(&bits.destination, ext.width, ext.height, false)?'),
        ('Stream-treated-as-Set','src/fast_path.rs','native_surface_rectangle(&bits.destination, ext.width, ext.height, set)?','native_surface_rectangle(&bits.destination, ext.width, ext.height, true)?'),
        ('outside-empty-sentinel','src/fast_path.rs','if bitmap.rectangle.right >= image.width() || bitmap.rectangle.bottom >= image.height() {\n                continue;\n            }','if bitmap.rectangle.right >= image.width() || bitmap.rectangle.bottom >= image.height() { region = Some(InclusiveRectangle::empty()); continue; }'),
        ('tile-empty-sentinel','src/image.rs','if region.rectangles.is_empty() {\n            return Ok(None);\n        }','if region.rectangles.is_empty() { return Ok(Some(InclusiveRectangle::empty())); }'),
        ('resource-limit-bypass','src/image.rs','&& n <= MAX_NATIVE_BITMAP_BYTES','&& n <= usize::MAX'),
    ]
    # Exact blocks are obtained from current actual source, rather than a second production implementation.
    text=(S/'src/image.rs').read_text();begin='        let state = self.pointer_rendering_begin(rectangle)?;'
    assert text.count(begin)==1
    mutations.append(('cursor-begin-before-preflight','src/image.rs','        let expected = self.native_bitmap_size(source_format, source_width, source_height, row_stride, rectangle)?;',begin+'\n        let expected = self.native_bitmap_size(source_format, source_width, source_height, row_stride, rectangle)?;'))
    text=(S/'src/fast_path.rs').read_text()
    start=text.index('image.apply_bitmap_native(\n                source,')
    stop=text.index(')?',start)+2
    call=text[start:stop]
    mutations.append(('pixel-consumer-return-to-scalar','src/fast_path.rs',call,'if format == PixelRowSource::Packed(PackedDepth::Bgr24) { Some(image.apply_bgr24_bitmap(source, &bitmap.rectangle)?) } else { '+call+' }'))
    mutations.append(('no-tile-RFX-empty-sentinel','src/rfx.rs','let mut final_update_rectangle = if self.native_legacy_kernels.is_some() {\n            None','let mut final_update_rectangle = if self.native_legacy_kernels.is_some() {\n            Some(InclusiveRectangle::empty())'))
    mutations.extend([
        ('pending-mouse-before-failure','src/fast_path.rs','if self.native_legacy_kernels.is_none() {','if true {'),
        ('pending-mouse-overwrites-server-position','src/fast_path.rs','self.mouse_pos_update = None;','let _ = server_position;'),
        ('cache-miss-consumes-pending-mouse','src/fast_path.rs','*flush_pending_mouse = !unmatched_cached;','*flush_pending_mouse = true; let _ = unmatched_cached;'),
        ('hidden-event-before-local-move','src/fast_path.rs','PointerUpdateData::SetHidden => {\n                self.flush_native_mouse(image, &mut processor_updates)?;','PointerUpdateData::SetHidden => {'),
    ])
    # An unchanged baseline is compiled and executed before each profile's mutations.
    baseline=ROOT/'target'/('task-4a27b-mutant-baseline-'+('release' if a.release else 'debug'))
    baseline_source=baseline.with_suffix('.rs');baseline_source.write_text(source())
    subprocess.run(['rustc','--test',str(baseline_source),'-o',str(baseline)]+flags(libs,a.release),check=True)
    subprocess.run([str(baseline)],check=True)
    with tempfile.TemporaryDirectory(prefix='task-4a27b-mutants-',dir=ROOT/'target') as temp:
        for name,relative,before,after in mutations:
            base=Path(temp)/name;shutil.copytree(S/'src',base/'src');path=base/relative;value=path.read_text();assert before in value,name
            if name=='cursor-begin-before-preflight':value=value.replace(begin,'',1)
            path.write_text(value.replace(before,after) if name=='source-height-to-destination-height' else value.replace(before,after,1));runner=base/'main.rs';runner.write_text(source(base));output=base/'mutant'
            subprocess.run(['rustc','--test',str(runner),'-o',str(output)]+flags(libs,a.release),check=True)
            result=subprocess.run([str(output)],text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT);failures=[v for v in result.stdout.splitlines() if ' ... FAILED' in v]
            (ROOT/'target'/('task-4a27b-mutant-'+name+'-'+('release' if a.release else 'debug'))).write_text(result.stdout)
            assert result.returncode!=0 and failures,name
            print('KILLED',name,':','; '.join(failures),flush=True)
    print('all',len(mutations),'actual shipping source mutants compiled and killed')
if __name__=='__main__':main()
