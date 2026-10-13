#!/usr/bin/env python3
"""Freeze original public catalog and actual hash-pinned private plane/scratch snapshots before consumer edits."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile

from planar_support import ROOT, VENDOR, ORIGINAL, artifacts, compiler_args, source_modules


def blob(value):
    return struct.pack('<I', len(value)) + value


def record(name, w, h, wire, initial, output, result, scratch=b'', consumed=b'', mode=0):
    return b''.join(map(blob, [name.encode(), struct.pack('<III', w, h, mode), wire, initial,
                             output, result.encode(), scratch, consumed]))


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--original-source',type=Path,default=ORIGINAL,
                        help='未修改 ironrdp-graphics-0.9.0 crate目录；全部source hashes必须匹配固定inventory')
    original=parser.parse_args().original_source.resolve()
    inventory = json.loads((ROOT / 'tests/legacy-oracle/SOURCE-INVENTORY.json').read_text())['ironrdp-graphics-0.9.0']
    for relative, expected in inventory['files'].items():
        assert hashlib.sha256((original / relative).read_bytes()).hexdigest() == expected, relative
    catalog = ROOT / 'tests/legacy-oracle/fixtures/wire-catalog.json.gz'
    assert hashlib.sha256(catalog.read_bytes()).hexdigest() == catalog.with_suffix('.sha256').read_text().strip()
    entries = [e for e in json.loads(gzip.decompress(catalog.read_bytes()))['entries'] if e['codec'] == 'planar']
    assert len(entries) == 4972
    output = b'FRDPLN02' + struct.pack('<I', len(entries))
    for e in entries:
        output += record(e['name'], e['width'], e['height'], bytes(e['input']), bytes(e['initial']),
                         bytes(e['output_or_partial']), e['result'])
    destination = VENDOR / 'tests/fixtures'
    (destination / 'planar-original-wire.bin').write_bytes(output)
    bmp_records=[]
    for e in entries:
        bmp_path=original / ('src/rdp6/test_assets/'+e['name']+'.bmp')
        if not bmp_path.exists():
            continue
        bmp=bmp_path.read_bytes()
        offset=struct.unpack_from('<I',bmp,10)[0]
        header,w,signed_h,planes,bpp,compression=struct.unpack_from('<IiiHHI',bmp,14)
        assert bmp[:2]==b'BM' and header==40 and planes==1 and bpp==24 and compression==0
        h=abs(signed_h);stride=(w*3+3)&~3
        pixels=bytearray()
        for y in range(h):
            row=offset+(h-1-y if signed_h>0 else y)*stride
            for x in range(w):
                b,g,r=bmp[row+x*3:row+x*3+3];pixels.extend([r,g,b])
        expected=bytes(e['initial'])+pixels
        assert w==e['width'] and h==e['height'] and expected==bytes(e['output_or_partial']),e['name']
        bmp_records.append(record('upstream-BMP-'+e['name'],w,h,bytes(e['input']),bytes(e['initial']),expected,'Ok(())'))
    assert len(bmp_records)==6
    (destination/'planar-original-bmp.bin').write_bytes(b'FRDPLN02'+struct.pack('<I',6)+b''.join(bmp_records))

    # Supplemental inputs, not reconstructed expected values. The unchanged actual source determines every result.
    seeds = []
    for e in entries:
        if e['result'] == 'Ok(())':
            seeds.append((e['name']+'-private-scratch',e['width'],e['height'],0,bytes(e['input']),bytes(e['initial'])))
    for low in [1, 2]:
        for raw in range(16):
            seeds.append((f'plane-special-{raw}-{low}',low*16+raw,2,1,bytes([raw*16+low])*2,b''))
    for w,h,wire in [(8,2,bytes.fromhex('17ff044001020304')), (8,2,bytes.fromhex('17ff18ff')),
                     (8,2,bytes.fromhex('17ff0000')), (8,2,bytes.fromhex('17ff401122')),
                     (8,1,bytes.fromhex('18')), (8,1,b'\0')]:
        seeds.append((f'plane-{w}x{h}-{wire.hex()}',w,h,1,wire,b''))
    for control in range(256):
        low,raw=control&15,control>>4
        count=(16+raw if low==1 else 32+raw if low==2 else low+raw)
        width=max(count,1)
        wire=bytes([control])+ (bytes((i*53+19)%256 for i in range(raw)) if low not in [1,2] else b'')
        for end in range(len(wire)+1):
            seeds.append((f'plane-control-{control:02x}-cut-{end}',width,1,1,wire[:end],b''))
    for length in range(1,8):
        seeds.append((f'plane-short-dst-{length}',8,1,1,bytes.fromhex('17ff'),bytes([0xa7])*length))
    for header in range(64):
        # Complete first color row then malformed next; non-ff alpha is supplied when present.
        w,h=3,3
        if header & 16:
            prefix=bytes([header])+ (bytes.fromhex('301122333044556630778899') if not header&32 else b'')
            wire=prefix+bytes.fromhex('3012345600')
        else:
            wire=bytes([header])+bytes(range(27))+b'\0'
        for end in range(len(wire)+1):
            seeds.append((f'supp-header-{header:02x}-cut-{end}',w,h,0,wire[:end],b'\xa5\x17\x61'))
    for header in [0x20,0x30,0x2f,0x3f]:
        seeds.append((f'unsafe-overflow-{header}',0xffffffff,0xffffffff,2,bytes([header,0]),b'\xa5'))
    for w,h in [(0,1),(1,0),(0,0)]:
        for header in [0x20,0x30,0x21,0x31]:
            seeds.append((f'unsafe-zero-{w}x{h}-{header}',w,h,0,bytes([header,0]),b'\xa5'))
    data = struct.pack('<I',len(seeds))
    for name,w,h,mode,wire,initial in seeds:
        data += blob(name.encode())+blob(struct.pack('<III',w,h,mode))+blob(wire)+blob(initial)
    libs=artifacts(original=True)
    with tempfile.TemporaryDirectory(prefix='frd-planar-original-') as temp:
        temp=Path(temp); (temp/'input.bin').write_bytes(data)
        runner=source_modules(original, snapshot=True)+'''
fn field<'a>(input:&mut &'a [u8])->&'a [u8] {let n=u32::from_le_bytes(input[..4].try_into().unwrap()) as usize;let v=&input[4..4+n];*input=&input[4+n..];v}
fn append(out:&mut Vec<u8>,value:&[u8]){out.extend((value.len() as u32).to_le_bytes());out.extend(value);}
fn main(){std::panic::set_hook(Box::new(|_|{}));let data=std::fs::read(INPUT).unwrap();let mut input=&data[4..];
let mut out=b"FRDPLN02".to_vec();out.extend(&data[..4]);
while !input.is_empty(){let name=field(&mut input);let dims=field(&mut input);let size=|i|u32::from_le_bytes(dims[i..i+4].try_into().unwrap()) as usize;
let wire=field(&mut input);let initial=field(&mut input);let (w,h,mode)=(size(0),size(4),size(8));
let (output,result,scratch,consumed)=if mode!=1 {let(o,r,s)=rdp6::bitmap_stream::snapshot(wire,initial,w,h);(o,r,s,Vec::new())}else{
let mut plane=if initial.is_empty(){vec![0xa7;w*h]}else{initial.to_vec()};let r=rdp6::rle::decompress_8bpp_plane(wire,&mut plane,w,h);
let count=r.as_ref().ok().map(|n|n.to_string()).unwrap_or_default();(Vec::new(),format!("{r:?}"),plane,count.into_bytes())};
for value in [name,dims,wire,initial,&output,result.as_bytes(),&scratch,&consumed]{append(&mut out,value);}}
std::fs::write(OUTPUT,out).unwrap();}
'''
        runner=runner.replace('INPUT',json.dumps(str(temp/'input.bin'))).replace('OUTPUT',json.dumps(str(temp/'output.bin')))
        (temp/'main.rs').write_text(runner)
        subprocess.run(['rustc','-A','dead_code',str(temp/'main.rs'),'-o',str(temp/'oracle')]+compiler_args(libs),check=True)
        subprocess.run([str(temp/'oracle')],check=True)
        supplemental=(temp/'output.bin').read_bytes()
    (destination/'planar-original-supplemental.bin').write_bytes(supplemental)
    for path in sorted(destination.glob('planar-original-*.bin')):
        print(path.name,len(path.read_bytes()),hashlib.sha256(path.read_bytes()).hexdigest())
    print('supplemental entries',len(seeds))


if __name__=='__main__':
    main()
