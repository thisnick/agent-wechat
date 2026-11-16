"""Full-screen Inter title cards with orange drawn underlines."""
from pathlib import Path
from functools import lru_cache
import json, math, subprocess, sys
from PIL import Image, ImageDraw, ImageFont

from config import OUTPUT, FFMPEG
ROOT=OUTPUT
OUT=ROOT/'motion-titles'
OUT.mkdir(exist_ok=True)
FONT=Path(__file__).parent/'fonts/Inter.ttf'
W,H,FPS=1920,1080,30
WHITE=(255,255,255)
BACKGROUND=(23,24,25) # #171819, near-neutral charcoal
ACCENT=(189,87,33) # --accent: #bd5721
CARDS=[
 dict(id='01-wechat',duration=3.2,lines=[('Start a conversation',.06,None),('in the WeChat app.',.8,'WeChat app')]),
 dict(id='02-agent-combined',duration=3.2,lines=[('Your AI agent receives',.06,'AI agent'),('and replies in your',.55,None),('WeChat thread.',1.05,None)]),
 dict(id='04-voice',duration=2.2,lines=[('Send a voice note.',.06,'voice note')]),
 dict(id='05-voice-reply',duration=3.2,lines=[('It can hear that',.06,'hear'),('and send one too.',.8,'send')]),
 dict(id='06-media',duration=3.2,lines=[('Access and save your media',.06,None),('in the Agent WeChat desktop app.',.8,'Agent WeChat desktop app')]),
 dict(id='07-terminal',duration=2.2,lines=[('Or from the terminal.',.06,'terminal')]),
]

@lru_cache(None)
def font(size):
 f=ImageFont.truetype(str(FONT),size)
 f.set_variation_by_axes([32,500])
 return f

def ease(t):
 p=max(0,min(1,t));return p*p*(3-2*p)

def reveal(t,at):return ease((t-at)/.17)

def text(im,s,baseline,size,alpha,x=None):
 f=font(size)
 if x is None:x=(W-f.getlength(s))/2
 assert x>=80 and x+f.getlength(s)<=W-80,(s,x,f.getlength(s))
 if alpha>0:
  layer=Image.new('RGBA',(W,H))
  ImageDraw.Draw(layer).text((x,baseline),s,font=f,fill=(*WHITE,round(255*alpha)),anchor='ls')
  im.alpha_composite(layer)
 return x

def squiggle(im,x,y,width,p):
 if p<=0:return
 # A restrained handwritten underline: one draw-on, no looping motion.
 # Supersample only its small region to retain smooth thin curves.
 z=3;pad=10
 layer=Image.new('RGBA',(round(width+2*pad)*z,40*z))
 d=ImageDraw.Draw(layer)
 count=max(2,round(width*min(p,1)*2))
 points=[]
 waves=max(1.25,width/112)
 for i in range(count):
  u=min(p,1)*i/(count-1)
  yy=18+3.7*math.sin(u*waves*math.tau)+1.2*math.sin(u*math.pi)
  points.append(((pad+u*width)*z,yy*z))
 d.line(points,fill=(*ACCENT,255),width=5*z,joint='curve')
 for xx,yy in (points[0],points[-1]):
  r=2.5*z;d.ellipse((xx-r,yy-r,xx+r,yy+r),fill=(*ACCENT,255))
 layer=layer.resize((round(width+2*pad),40),Image.Resampling.LANCZOS)
 im.alpha_composite(layer,(round(x-pad),round(y-18)))

def line(im,s,baseline,size,t,at,under=None,x=None):
 left=text(im,s,baseline,size,reveal(t,at),x)
 if under:
  prefix=s[:s.index(under)]
  ux=left+font(size).getlength(prefix)
  squiggle(im,ux,baseline+21,font(size).getlength(under),ease((t-at-.3)/.5))

def render_card(c,t):
 im=Image.new('RGBA',(W,H),(*BACKGROUND,255))
 size=100 if c['id']=='06-media' else 110
 baselines={1:[571],2:[501,647],3:[425,561,697]}[len(c['lines'])]
 if len(c['lines'])==3:size=104
 for (s,at,under),baseline in zip(c['lines'],baselines):line(im,s,baseline,size,t,at,under)
 return im.convert('RGB')

END_LINES=[
 'Desktop',
 'CLI',
 'AI agents (OpenClaw + others)',
 'Text · Images · Video · Files · Voice notes',
 'DMs & groups',
 'Open source.',
]

def render_end(t):
 im=Image.new('RGBA',(W,H),(*BACKGROUND,255))
 line(im,'Agent WeChat',285,120,t,.06,'Agent WeChat',x=180)
 for i,s in enumerate(END_LINES):
  line(im,s,445+i*84,42,t,1.0+i*.24,x=180)
 return im.convert('RGB')

def styleframes():
 frames=[]
 for c in CARDS:
  im=render_card(c,2.3);im.save(OUT/(c['id']+'.png'));frames.append(im)
 im=render_end(3);im.save(OUT/'07-conclusion.png');frames.append(im)
 board=Image.new('RGB',(1600,1952),(24,24,24))
 ImageDraw.Draw(board).text((32,22),'Client typography / Inter + orange',font=font(28),fill=WHITE)
 for n,im in enumerate(frames):
  board.paste(im.resize((768,432),Image.Resampling.LANCZOS),(24+n%2*792,76+n//2*470))
 board.save(ROOT/'motion-board.jpg',quality=95)
 strip=Image.new('RGB',(1920,360),'black')
 for i,t in enumerate((.4,1.2,1.8)):
  strip.paste(render_card(CARDS[0],t).resize((640,360),Image.Resampling.LANCZOS),(i*640,0))
 strip.save(OUT/'underline-timing.jpg',quality=95)

def encode(name,duration,renderer):
 path=OUT/(name+'.mp4')
 p=subprocess.Popen([FFMPEG,'-v','error','-y','-f','rawvideo','-pix_fmt','rgb24','-s','1920x1080','-r','30','-i','pipe:0','-an','-c:v','libx264','-preset','fast','-crf','18','-pix_fmt','yuv420p','-movflags','+faststart',str(path)],stdin=subprocess.PIPE)
 for n in range(round(duration*FPS)):p.stdin.write(renderer(n/FPS).tobytes())
 p.stdin.close();assert p.wait()==0
 return path

if __name__=='__main__':
 styleframes()
 if '--frames' in sys.argv:sys.exit(0)
 paths=[]
 for c in CARDS:
  print(c['id'],flush=True)
  paths.append(encode(c['id'],c['duration'],lambda t,c=c:render_card(c,t)))
 paths.append(encode('07-conclusion',6.2,render_end))
 listing=OUT/'concat.txt';listing.write_text(''.join("file '"+str(p)+"'\n" for p in paths))
 subprocess.run([FFMPEG,'-v','error','-y','-f','concat','-safe','0','-i',str(listing),'-c','copy','-movflags','+faststart',str(ROOT/'motion-preview.mp4')],check=True)
 (OUT/'design.json').write_text(json.dumps(dict(cards=CARDS,conclusion=END_LINES,conclusion_duration=6.2,font='Inter Medium, optical size 32',accent='#bd5721',background='#171819',source='client-ui onboarding palette and font stack at capture time',final_font_sizes=[120,42],final_text_color='#ffffff',fps=FPS,resolution=[W,H]),indent=2))
 print('DONE',flush=True)
