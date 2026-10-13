"""生成固定合成 HEVC I/P 与独立像素参考；可用临时输出目录核对既有 pin。"""
from pathlib import Path
import argparse
import hashlib
import json
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        '--output-dir', type=Path, default=Path(__file__).resolve().parent,
        help='合成输出目录；验收时使用临时目录，避免覆盖跟踪夹具',
    )
    root = parser.parse_args().output_dir.resolve()
    root.mkdir(parents=True, exist_ok=True)
    name = 'synthetic-hevc444-256x144-2frames'
    encoded = root / (name + '.hevc')
    raw = root / (name + '.yuv444p')
    version = subprocess.check_output(['ffmpeg', '-version'], text=True).splitlines()[0]
    cmd = [
        'ffmpeg', '-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i',
        'testsrc2=size=256x144:rate=30', '-frames:v', '2', '-c:v', 'libx265',
        '-preset', 'ultrafast', '-tune', 'zerolatency', '-x265-params',
        'pools=none:frame-threads=1:wpp=0:bframes=0:aud=1:keyint=30:scenecut=0:repeat-headers=1:log-level=error',
        '-pix_fmt', 'yuv444p', '-f', 'hevc', '-y', str(encoded),
    ]
    subprocess.run(cmd, check=True)
    decode = [
        'ffmpeg', '-hide_banner', '-loglevel', 'error', '-i', str(encoded),
        '-frames:v', '2', '-pix_fmt', 'yuv444p', '-f', 'rawvideo', '-y', str(raw),
    ]
    subprocess.run(decode, check=True)
    probe = [
        'ffprobe', '-v', 'error', '-show_packets', '-show_streams', '-show_entries',
        'stream=profile,width,height,pix_fmt:packet=pos,size,flags', '-of', 'json', str(encoded),
    ]
    info = json.loads(subprocess.check_output(probe))
    data = encoded.read_bytes()
    pixels = raw.read_bytes()
    frame_size = 256 * 144 * 3
    assert len(pixels) == frame_size * 2 and len(info['packets']) == 2
    assert info['packets'][0]['flags'].startswith('K')
    assert not info['packets'][1]['flags'].startswith('K')
    access_units = []
    for index, item in enumerate(info['packets']):
        pos, size = int(item['pos']), int(item['size'])
        au = data[pos:pos + size]
        path = root / (name + f'-au{index}.hevc')
        path.write_bytes(au)
        reference = root / (name + f'-frame{index}.yuv444p')
        reference.write_bytes(pixels[index * frame_size:(index + 1) * frame_size])
        access_units.append({
            'file': path.name, 'sha256': hashlib.sha256(au).hexdigest(),
            'bytes': len(au), 'flags': item['flags'], 'reference_file': reference.name,
            'reference_sha256': hashlib.sha256(reference.read_bytes()).hexdigest(),
        })
    assert sum(int(item['size']) for item in info['packets']) == len(data)

    def normalized(command):
        # 运行命令仍使用真实路径；序列化只记夹具文件名，与输出目录无关。
        return [Path(arg).name if Path(arg).is_absolute() and Path(arg).parent == root else arg
                for arg in command]

    output = {
        'name': name, 'width': 256, 'height': 144, 'pixel_format': 'yuv444p',
        'ffmpeg_version': version, 'encoder_command': normalized(cmd),
        'oracle_command': normalized(decode), 'probe_command': normalized(probe),
        'stream': info['streams'], 'encoded_sha256': hashlib.sha256(data).hexdigest(),
        'reference_frame_bytes': frame_size, 'access_units': access_units,
        'boundary': 'Synthetic independent host oracle only; not guest decode or HEVC wire support.',
    }
    (root / 'hevc-measurement-provenance.json').write_text(json.dumps(output, indent=2) + '\n')
    print(name, info['streams'], [au['bytes'] for au in access_units])


if __name__ == '__main__':
    main()
