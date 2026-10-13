from pathlib import Path
import subprocess,json,hashlib
root=Path(__file__).resolve().parent; name='synthetic-hevc444-256x144-2frames';encoded=root/(name+'.hevc');raw=root/(name+'.yuv444p')
version=subprocess.check_output(['ffmpeg','-version'],text=True).splitlines()[0]
cmd=['ffmpeg','-hide_banner','-loglevel','error','-f','lavfi','-i','testsrc2=size=256x144:rate=30','-frames:v','2','-c:v','libx265','-preset','ultrafast','-tune','zerolatency','-x265-params','pools=none:frame-threads=1:wpp=0:bframes=0:aud=1:keyint=30:scenecut=0:repeat-headers=1:log-level=error','-pix_fmt','yuv444p','-f','hevc','-y',str(encoded)]
subprocess.run(cmd,check=True)
decode=['ffmpeg','-hide_banner','-loglevel','error','-i',str(encoded),'-frames:v','2','-pix_fmt','yuv444p','-f','rawvideo','-y',str(raw)];subprocess.run(decode,check=True)
probe=['ffprobe','-v','error','-show_packets','-show_streams','-show_entries','stream=profile,width,height,pix_fmt:packet=pos,size,flags','-of','json',str(encoded)]
info=json.loads(subprocess.check_output(probe));data=encoded.read_bytes();pixels=raw.read_bytes();fs=256*144*3
assert len(pixels)==fs*2 and len(info['packets'])==2
assert info['packets'][0]['flags'].startswith('K') and not info['packets'][1]['flags'].startswith('K')
aus=[]
for i,item in enumerate(info['packets']):
 pos,size=int(item['pos']),int(item['size']); au=data[pos:pos+size]; path=root/(name+f'-au{i}.hevc');path.write_bytes(au)
 ref=root/(name+f'-frame{i}.yuv444p');ref.write_bytes(pixels[i*fs:(i+1)*fs])
 aus.append({'file':path.name,'sha256':hashlib.sha256(au).hexdigest(),'bytes':len(au),'flags':item['flags'],'reference_file':ref.name,'reference_sha256':hashlib.sha256(ref.read_bytes()).hexdigest()})
assert sum(int(x['size']) for x in info['packets'])==len(data)
out={'name':name,'width':256,'height':144,'pixel_format':'yuv444p','ffmpeg_version':version,'encoder_command':cmd,'oracle_command':decode,'probe_command':probe,'stream':info['streams'],'encoded_sha256':hashlib.sha256(data).hexdigest(),'reference_frame_bytes':fs,'access_units':aus,'boundary':'Synthetic independent host oracle only; not guest decode or HEVC wire support.'}
(root/'hevc-fixture-manifest.json').write_text(json.dumps(out,indent=2)+'\n');print(name,info['streams'],[a['bytes'] for a in aus])
