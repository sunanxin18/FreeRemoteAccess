#!/usr/bin/env python3
"""从独立原版 catalog 提取 fixtures；附加截断用例只执行 hash 固定的原版 rle.rs。"""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile

ORIGINAL_RLE = '4a0a5132460e4890cf272743c0702890172d08a0336cca5f39e3800b44048fcb'


def blob(value):
    return struct.pack('<I', len(value)) + value


def record(entry):
    values = [entry['name'].encode(), struct.pack('<III', entry['width'], entry['height'], entry['bpp']),
              bytes(entry['input']), bytes(entry['initial']), bytes(entry['output_or_partial']), entry['result'].encode()]
    return b''.join(map(blob, values))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--catalog', type=Path, required=True)
    parser.add_argument('--original-rle', type=Path, required=True)
    args = parser.parse_args()
    source = args.original_rle.resolve()
    if hashlib.sha256(source.read_bytes()).hexdigest() != ORIGINAL_RLE:
        raise SystemExit('原版 rle.rs identity 不匹配')
    entries = [entry for entry in json.loads(gzip.decompress(args.catalog.read_bytes()))['entries']
               if entry['codec'] == 'bitmap']
    if len(entries) != 16721:
        raise SystemExit('冻结 catalog entry 数目改变')
    destination = Path(__file__).parent / 'fixtures'
    destination.mkdir(exist_ok=True)
    frozen = b'FRDBMP01' + struct.pack('<I', len(entries)) + b''.join(map(record, entries))
    (destination / 'bitmap-original-wire.bin').write_bytes(frozen)
    # 各 code/depth/first-next/直接扩展 header 的每一个 byte truncation，另用小几何触发输出错误。
    seeds = []
    for entry in entries:
        if not entry['name'].startswith('code-'):
            continue
        for end in range(len(entry['input']) + 1):
            for width, height in [(entry['width'], entry['height']), (1, 1)]:
                seeds.append((f"truncate-{entry['name']}-end-{end}-size-{width}x{height}", width, height,
                              entry['bpp'], bytes(entry['input'][:end]), bytes(entry['initial'])))
    for bpp in [8, 15, 16, 24]:
        for code in range(0xf0, 0x100):
            for wire in [bytes([code]), bytes([code, 0]), bytes([code, 0, 0]), bytes([code, 1, 0])]:
                seeds.append((f'header-{wire.hex()}-bpp-{bpp}', 3, 3, bpp, wire, bytes([0x71] * 17)))
    data = struct.pack('<I', len(seeds))
    for name, width, height, bpp, wire, initial in seeds:
        data += blob(name.encode()) + blob(struct.pack('<III', width, height, bpp)) + blob(wire) + blob(initial)
    # 独立原版 module，完全没有生产 patch 或 kernel dependency。这里只生成 fixture，不作为生产 decoder。
    with tempfile.TemporaryDirectory(prefix='frd-bitmap-original-') as temp:
        temp = Path(temp)
        (temp / 'input.bin').write_bytes(data)
        runner = '''#[path = SOURCE] mod original;
fn field<'a>(input: &mut &'a [u8]) -> &'a [u8] {
 let n=u32::from_le_bytes(input[..4].try_into().unwrap()) as usize;
 let v=&input[4..4+n]; *input=&input[4+n..]; v
}
fn append(out: &mut Vec<u8>, v: &[u8]) {
 out.extend(u32::try_from(v.len()).unwrap().to_le_bytes()); out.extend(v);
}
fn main() {
 let data=std::fs::read(INPUT).unwrap(); let mut input=&data[4..];
 let mut out=b"FRDBMP01".to_vec(); out.extend(&data[..4]);
 while !input.is_empty() {
  let name=field(&mut input); let dims=field(&mut input);
  let size=|i|u32::from_le_bytes(dims[i..i+4].try_into().unwrap()) as usize;
  let wire=field(&mut input); let initial=field(&mut input); let mut dst=initial.to_vec();
  let result=original::decompress(wire,&mut dst,size(0),size(4),size(8));
  for v in [name,dims,wire,initial,&dst,format!("{result:?}").as_bytes()] { append(&mut out,v); }
 }
 std::fs::write(OUTPUT,out).unwrap();
}
'''.replace('SOURCE', json.dumps(str(source))).replace('INPUT', json.dumps(str(temp / 'input.bin'))).replace('OUTPUT', json.dumps(str(temp / 'output.bin')))
        (temp / 'main.rs').write_text(runner)
        subprocess.run(['rustc', '--edition=2024', '-A', 'dead_code', str(temp / 'main.rs'), '-o', str(temp / 'oracle')], check=True)
        subprocess.run([str(temp / 'oracle')], check=True)
        malformed = (temp / 'output.bin').read_bytes()
    (destination / 'bitmap-original-malformed.bin').write_bytes(malformed)
    for name, value in [('bitmap-original-wire.bin', frozen), ('bitmap-original-malformed.bin', malformed)]:
        print(name, len(value), hashlib.sha256(value).hexdigest())
    print('malformed entries', len(seeds))


if __name__ == '__main__':
    main()
