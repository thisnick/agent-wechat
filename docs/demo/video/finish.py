"""Trim the image-in-chat hold by 1.5s and mix the original music bed."""
from pathlib import Path
import subprocess,json,hashlib
from config import OUTPUT, FFMPEG
O=OUTPUT;E=O/'music'
F=FFMPEG
source=O/'demo-base.mp4';output=O/'agent-wechat-demo.mp4'
old=json.loads((O/'timeline-base.json').read_text())
image=next(s for s in old if s['name']=='windows_image')
image_start_frame=round(image['start']*30)
cut_end_frame=image_start_frame+round(image['duration']*30)
cut_start_frame=cut_end_frame-45
cut_start=cut_start_frame/30;cut_end=cut_end_frame/30
duration=(sum(round(s['duration']*30) for s in old)-45)/30
graph=(f'[0:v]split=2[vx][vy];'
       f'[vx]trim=end_frame={cut_start_frame},setpts=PTS-STARTPTS[vp];'
       f'[vy]trim=start_frame={cut_end_frame},setpts=PTS-STARTPTS[vq];'
       '[vp][vq]concat=n=2:v=1:a=0[v];'
       '[0:a]asplit=2[ax][ay];'
       f'[ax]atrim=end={cut_start},asetpts=PTS-STARTPTS[ap];'
       f'[ay]atrim=start={cut_end},asetpts=PTS-STARTPTS[aq];'
       '[ap][aq]concat=n=2:v=0:a=1[demo];'
       '[demo]aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo[d];'
       '[1:a]aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo[m];'
       '[d][m]amix=inputs=2:duration=first:normalize=0,alimiter=limit=.95:level=0:latency=1[a]')
subprocess.run([F,'-v','error','-y','-i',str(source),'-i',str(E/'music-ducked.wav'),'-filter_complex',graph,'-map','[v]','-map','[a]','-r','30','-c:v','libx264','-preset','fast','-crf','18','-pix_fmt','yuv420p','-c:a','aac','-b:a','256k','-ar','48000','-t',str(duration),'-movflags','+faststart',str(output)],check=True)
timeline=[]
for s in old:
 s=s.copy()
 if s['name']=='windows_image':s['duration']-=1.5
 elif round(s['start']*30)>=cut_end_frame:s['start']=round(s['start']-1.5,3)
 timeline.append(s)
(O/'timeline.json').write_text(json.dumps(timeline,indent=2))
manifest=json.loads((O/'manifest-base.json').read_text())
manifest.update(output=output.name,duration_seconds=duration,sha256=hashlib.sha256(output.read_bytes()).hexdigest(),music=json.loads((E/'music.json').read_text()))
manifest['editorial_changes']+=['Trimmed 1.5 seconds from the end of the image-in-chat shot','Added original 96 BPM music, ducked under both voice notes']
(O/'manifest.json').write_text(json.dumps(manifest,indent=2))
print('DONE',output,'duration',duration,'removed',cut_start,cut_end,flush=True)
