"""Revised narrative timing, split media cards, 1.5x typing, file reveal dissolve."""
from pathlib import Path
from PIL import Image, ImageDraw, ImageFilter
import subprocess,json,sys,itertools,array,wave,math,random,hashlib
import media as src
import titles

E=src.E/'base';E.mkdir(exist_ok=True)
O=src.E
W,H,FPS=1920,1080,30
BG=titles.BACKGROUND
VW,VH,VX,VY=1792,880,64,100
PHONE=(640,1480,880/2620)
WINDOWS=(640,400,880/800)
COMPOSER=(650,630,3.7)
TERMINAL=(960,540,880/1080)
PHONE_ROI=(0,170,1280,2790)
WIN_ROI=(0,0,1280,800)
CARDS=titles.CARDS
CARD_BY_ID={c['id']:c for c in CARDS}
LAST_TERMINAL=None

render_card=titles.render_card

# name, source, source start, duration, playback speed, decoder dimensions
def card(i):
 c=CARDS[i];return ('card_'+c['id'],None,0,c['duration'],0,(W,H))
shots=[
 card(0),
 ('phone_context',src.P0,2.8,3,1,(1280,2856)),
 ('phone_typing',src.P0,5.8,7,1,(1280,2856)),
 ('phone_sent',src.R/'phone-sent.png',0,2,0,(1280,2856)),
 card(1),
 ('claw_receives',src.CLAW,107,8,1,(537,650)),
 card(2),
 # Entire return is from the second upload. The quiet reading portion is
 # extended to four seconds; the voice gesture and send then play at 1x.
 ('phone_reply',src.P1,0,4,.6,(1280,2856)),
 ('phone_voice',src.P1,2.4,10.4,1,(1280,2856)),
 card(3),
 ('windows_handoff',src.WIN,422.8,3.6,1,(1280,800)),
 ('windows_voice',src.WIN,426.6,8.8,1,(1280,800)),
 ('windows_typing',src.WIN,468.5,10/1.5,1.5,(1280,800)),
 ('windows_sent',src.WIN,520.8,2,1,(1280,800)),
 card(4),
 ('windows_image',src.WIN,576.5,5,1,(1280,800)),
 card(5),
 ('cli_download',src.CLI,.3,7/1.5,1.5,(1920,1080)),
 ('cli_saved',src.CLI,23.3,2.5,1,(1920,1080)),
 ('saved_picture',src.R/'walk.jpg',0,3,0,(640,429)),
 ('conclusion',None,0,6.2,0,(W,H)),
]

def mix(a,b,p):return tuple(src.lerp(x,y,p) for x,y in zip(a,b))
def move(t,start,end,a,b):return mix(a,b,src.smooth((t-start)/(end-start)))

def safe_win(im,source_time):
 # Conversation-list protection is baked into the capture. Toast protection
 # is bounded to the observed notification intervals, not an always-on
 # mask across the composer. The moving toast remains inside this rectangle.
 if 431.4<=source_time<=438.8 or 470.4<=source_time<=485.5:
  im=im.copy();b=(901,610,1280,744)
  im.paste(im.crop(b).filter(ImageFilter.GaussianBlur(24)),b)
 return im

def view(im,pose,roi=None):
 cx,cy,s=pose
 if roi:im=im.crop(roi);cx-=roi[0];cy-=roi[1]
 pane=im.transform((VW,VH),Image.Transform.AFFINE,(1/s,0,cx-VW/(2*s),0,1/s,cy-VH/(2*s)),Image.Resampling.BICUBIC,fillcolor=BG)
 out=Image.new('RGB',(W,H),BG);out.paste(pane,(VX,VY));return out

def render(name,im,t,dur,source_time=0):
 if name.startswith('card_'):return render_card(CARD_BY_ID[name[5:]],t)
 if name=='conclusion':return titles.render_end(t)
 roi=PHONE_ROI if name.startswith('phone_') else None
 if name.startswith('windows_'):im=safe_win(im,source_time);roi=WIN_ROI
 if name=='phone_context':pose=PHONE
 elif name=='phone_typing':
  pose=move(t,0,1.2,(640,1640,.92),(640,1640,1.12))
  if t>5.8:pose=move(t,5.8,7,(640,1640,1.12),(640,1420,1.1))
 elif name=='phone_sent':pose=(640,1420,1.1)
 elif name=='claw_receives':
  if t<1.6:pose=(352,310,2.8)
  elif t<3.6:pose=move(t,1.6,3.6,(352,310,2.8),(268.5,325,880/650))
  else:pose=move(t,3.6,4.9,(268.5,325,880/650),(268.5,425,2.7))
 elif name=='phone_reply':pose=move(t,2.2,4,(640,2280,.92),PHONE)
 elif name=='phone_voice':
  # The recording overlay, release, upload indicator, and settled voice note
  # all remain a single uninterrupted source take.
  pose=move(t,.8,2.3,PHONE,(640,2050,.58))
 elif name=='windows_handoff':pose=move(t,1.2,3.6,(1040,330,2),WINDOWS)
 elif name=='windows_voice':
  pose=move(t,.5,2.3,WINDOWS,(650,460,3.1))
  if t>=7:pose=move(t,7,8.8,(650,460,3.1),COMPOSER)
 elif name=='windows_typing':pose=COMPOSER
 elif name=='windows_sent':pose=move(t,.3,1.5,COMPOSER,(980,543,2.6))
 elif name=='windows_image':
  pose=move(t,.3,2.1,WINDOWS,(735,390,1.85))
 elif name=='cli_download':pose=move(t,.3,1.35,TERMINAL,(850,290,1.15))
 elif name=='cli_saved':pose=(850,290,1.15)
 elif name=='saved_picture':
  pose=move(t,0,3,(320,214.5,1.95),(320,214.5,2.04))
  photo=view(im,pose)
  if t<.7 and LAST_TERMINAL is not None:return Image.blend(LAST_TERMINAL,photo,src.smooth(t/.7))
  return photo
 else:raise ValueError(name)
 return view(im,pose,roi)

def frames(path,start,dur,speed,size):
 if speed==0:
  im=Image.open(path).convert('RGB') if path else None
  for _ in range(round(dur*FPS)):yield im
 else:yield from src.frames(path,start,dur,speed,size)

def sound(timeline,total):
 sr=48000;rng=random.Random(28)
 data=array.array('f',[0])*round(total*sr)
 # Quiet editorial typing texture only; no music or decorative transition tones.
 byname={s['name']:s for s in timeline}
 for name,a,b,step in [('phone_context',.2,8.9,.17),('windows_typing',.8/1.5,9.3/1.5,.17/1.5),('cli_download',.4/1.5,6.4/1.5,.16/1.5)]:
  at=byname[name]['start']+a;end=byname[name]['start']+b
  while at<end:
   first=round(at*sr)
   for j in range(round(.023*sr)):
    u=j/sr;k=first+j
    if k<len(data):data[k]+=(rng.uniform(-1,1)*.012+math.sin(2*math.pi*1900*u)*.006)*math.exp(-u*210)
   at+=step+rng.uniform(-.035,.04)
 pcm=array.array('h',(round(max(-1,min(1,v))*32767) for v in data))
 with wave.open(str(E/'typing.wav'),'wb')as w:
  w.setnchannels(1);w.setsampwidth(2);w.setframerate(sr);w.writeframes(pcm.tobytes())
 return byname['phone_voice']['start']+.4,byname['windows_voice']['start']

if __name__=='__main__':
 required=[src.P0,src.P1,src.WIN,src.CLAW,src.CLI,src.R/'walk.jpg',src.R/'human-voice.wav',src.R/'agent-voice.wav']
 missing=[str(p) for p in required if not p.is_file()]
 if missing:raise SystemExit('Missing local inputs:\n'+'\n'.join(missing))
 # A single source frame freezes the sent-message beat for two seconds.
 subprocess.run([src.F,'-v','error','-y','-ss','13','-i',str(src.P0),'-frames:v','1','-update','1',str(E/'phone-sent.png')],check=True)
 shots=[(name,E/'phone-sent.png' if name=='phone_sent' else path,start,dur,speed,size) for name,path,start,dur,speed,size in shots]
 timeline=[];elapsed=0
 enc=subprocess.Popen([src.F,'-v','error','-y','-f','rawvideo','-pix_fmt','rgb24','-s','1920x1080','-r','30','-i','pipe:0','-an','-c:v','libx264','-preset','fast','-crf','18','-pix_fmt','yuv420p','-movflags','+faststart',str(E/'picture.mp4')],stdin=subprocess.PIPE)
 for name,path,start,dur,speed,size in shots:
  nframes=round(dur*FPS)
  timeline.append(dict(name=name,start=round(elapsed,3),duration=nframes/FPS,source=path.name if path else 'motion design',source_start=start,speed=speed))
  print(name,round(elapsed,3),flush=True)
  shotframes=frames(path,start,dur,speed,size)
  if name=='windows_sent':shotframes=itertools.chain(frames(path,520.8,.7,1,size),frames(path,526.5,1.3,1,size))
  for n,im in enumerate(shotframes):
   t=n/FPS;stime=start+t*speed
   if name=='windows_sent' and t>=.7:stime=526.5+t-.7
   out=render(name,im,t,dur,stime);enc.stdin.write(out.tobytes())
   if name=='cli_saved':LAST_TERMINAL=out.copy()
   if n in {0,min(40,nframes-1),nframes-1}:out.save(E/f'{name}-{n}.jpg',quality=94)
   if name=='phone_voice' and n in {0,6,15,252,264,276,294,306,311}:
    out.save(E/f'voice-send-review-{n}.jpg',quality=94)
  elapsed+=nframes/FPS
 enc.stdin.close();assert enc.wait()==0
 (E/'timeline.json').write_text(json.dumps(timeline,indent=2))
 human_at,bot_at=sound(timeline,elapsed)
 output=O/'demo-base.mp4'
 graph=f'[1:a]loudnorm=I=-17:TP=-2:LRA=7,aresample=48000,adelay={round(human_at*1000)}:all=1[h];[2:a]loudnorm=I=-17:TP=-2:LRA=7,aresample=48000,adelay={round(bot_at*1000)}:all=1[b];[3:a][h][b]amix=inputs=3:duration=longest:normalize=0,alimiter=limit=.95[a]'
 print('Finishing audio',human_at,bot_at,'duration',elapsed,flush=True)
 subprocess.run([src.F,'-v','error','-y','-i',str(E/'picture.mp4'),'-i',str(src.R/'human-voice.wav'),'-i',str(src.R/'agent-voice.wav'),'-i',str(E/'typing.wav'),'-filter_complex',graph,'-map','0:v','-map','[a]','-t',str(elapsed),'-c:v','copy','-c:a','aac','-b:a','192k','-ar','48000','-movflags','+faststart',str(output)],check=True)
 (O/'timeline-base.json').write_text(json.dumps(timeline,indent=2))
 manifest=dict(output=output.name,duration_seconds=round(elapsed,3),resolution=[W,H],fps=FPS,voice_offsets_seconds=dict(human=human_at,agent=bot_at),design='Inter Medium, orange #bd5721 squiggly underlines, charcoal #171819, full-screen intercuts',editorial_changes=['Combined AI-agent receipt and reply title','OpenClaw reply zoom starts at 22.0 seconds and holds the response','Voice-note title precedes phone reply, zoom-out, and continuous voice send','Cut shortly after voice note settles in history, without final note zoom','Media title separates image request from image arrival','Separate CLI title after image arrival','Desktop and CLI typing at 1.5x; voice audio remains at original speed','0.7-second dissolve from terminal result to downloaded image','Full bot voice continues over desktop typing and send','No always-on composer blur'],privacy='Source conversation-list blur retained. Toast mask only during observed notifications; other chat surfaces are never shown.',sha256=hashlib.sha256(output.read_bytes()).hexdigest())
 (O/'manifest-base.json').write_text(json.dumps(manifest,indent=2));print('DONE',output,flush=True)
