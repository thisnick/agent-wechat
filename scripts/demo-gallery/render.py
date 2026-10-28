"""Approved launch gallery: archived screenshots, Inter, charcoal and orange dividers."""
from pathlib import Path
from PIL import Image,ImageDraw,ImageFont
import argparse,json
HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[1]
parser=argparse.ArgumentParser(description='Render the approved Product Hunt gallery from archived capture inputs.')
parser.add_argument('--output-dir',type=Path,default=ROOT/'docs/public/demo/gallery')
parser.add_argument('--captures-dir',type=Path,default=HERE/'captures')
args=parser.parse_args()
OUT=args.output_dir.resolve();OUT.mkdir(parents=True,exist_ok=True)
CAP=args.captures_dir.resolve()
FONT=HERE.parent/'demo-video/fonts/Inter.ttf'
BG=(23,24,25);WHITE=(255,255,255);ORANGE=(189,87,33)
W,H=1270,760

def font(size):
 f=ImageFont.truetype(str(FONT),size);f.set_variation_by_axes([32,500]);return f

def put(im,text,x,y,size=62):
 d=ImageDraw.Draw(im);f=font(size)
 for line in text.split('\n'):
  d.text((x,y),line,font=f,fill=WHITE,anchor='lt');y+=round(size*1.18)
 return y

def divider(im,x,y,width=220):
 ImageDraw.Draw(im).line((x,y,x+width,y),fill=ORANGE,width=3)

def image(im,name,box,crop=None):
 src=Image.open(CAP/name).convert('RGB')
 if crop:src=src.crop(crop)
 x,y,w,h=box;scale=min(w/src.width,h/src.height)
 src=src.resize((round(src.width*scale),round(src.height*scale)),Image.Resampling.LANCZOS)
 xx=x+(w-src.width)//2;yy=y+(h-src.height)//2
 im.paste(src,(xx,yy))
 ImageDraw.Draw(im).rectangle((xx-1,yy-1,xx+src.width,yy+src.height),outline=(61,62,63),width=1)
 return xx,yy,src.width,src.height

def base():return Image.new('RGB',(W,H),BG)

CARDS=[
 dict(id='01-introducing',headline='Agent WeChat',body='An open source WeChat client.\nFor people and AI agents.',visual='Synthetic group conversation with video, mentions, and attachment.'),
 dict(id='02-images-video',headline='Images and video.\nIn the conversation.',body='Download original or compressed\nimages and videos.',visual='Fresh synthetic media viewer showing compressed download and Get original controls.'),
 dict(id='03-voice-files',headline='Voice notes.\nFile attachments.',body='Receive links and forwarded\nconversations, too.',visual='Fresh synthetic shared-link card and forwarded conversation in the client.'),
 dict(id='04-dms-groups',headline='Direct messages.\nGroup conversations.',body='Keep the context.\nQuote messages in your replies.',visual='Fresh group conversation showing an original message and the quoted reply.'),
 dict(id='05-ai-agents',headline='Connect your agent\nto WeChat.',body='Text, voice, and media,\nright in your WeChat thread.',visual='Real question and Nick Bot reply from the approved phone recording.'),
 dict(id='06-terminal',headline='Your WeChat,\nfrom the terminal.',body='Drive your entire WeChat client directly from the command line.',visual='Actual wx command from the demo recording, with no fabricated terminal output.'),
 dict(id='07-open-source',headline='Open source.\nControl it your way.',body='Use the desktop app, CLI, or API.\nChoose how you interact with WeChat.',visual='Typography-only closing card with the source repository.'),
]

for n,c in enumerate(CARDS):
 im=base()
 if n==0:
  put(im,c['headline'],64,170,78)
  divider(im,64,290)
  put(im,c['body'],64,345,28)
  image(im,'group-media.jpg',(738,58,450,640))
 elif n==1:
  put(im,c['headline'],64,150,59)
  divider(im,64,314)
  put(im,c['body'],64,362,26)
  image(im,'video-quality-v2.jpg',(738,58,450,640))
 elif n==2:
  put(im,c['headline'],64,150,62)
  divider(im,64,314)
  put(im,c['body'],64,362,26)
  image(im,'links-forwarded-v2.jpg',(738,58,450,640))
 elif n==3:
  put(im,c['headline'],64,150,58)
  divider(im,64,314)
  put(im,c['body'],64,362,26)
  image(im,'quoted-reply-v2.jpg',(738,58,450,640))
 elif n==4:
  put(im,c['headline'],64,150,62)
  divider(im,64,314)
  put(im,c['body'],64,362,26)
  put(im,'Nick Bot replying in WeChat',708,164,24)
  image(im,'nick-bot-reply-crop.png',(700,229,510,420))
 elif n==5:
  put(im,c['headline'],64,65,62)
  divider(im,64,235)
  put(im,c['body'],64,273,26)
  image(im,'terminal-crop.jpg',(64,340,1142,350))
 else:
  put(im,c['headline'],64,100,78)
  divider(im,64,314)
  put(im,c['body'],64,365,30)
  put(im,'github.com/thisnick/agent-wechat',64,617,30)
 im.save(OUT/(c['id']+'.png'),optimize=True)

board=Image.new('RGB',(1600,2040),(31,32,33));d=ImageDraw.Draw(board)
for n,c in enumerate(CARDS):
 x=24+(n%2)*788;y=52+(n//2)*496
 d.text((x,y-28),f'{n+1:02d}  '+['Introducing','Images + video','Voice + files','DMs + groups','AI agents','Terminal','Open source'][n],font=font(20),fill=WHITE)
 im=Image.open(OUT/(c['id']+'.png')).resize((764,457),Image.Resampling.LANCZOS)
 board.paste(im,(x,y))
board.save(OUT/'preview.jpg',quality=95)
(OUT/'cards.json').write_text(json.dumps(CARDS,indent=2)+'\n')
print('Rendered',len(CARDS),'cards at 1270 x 760 in',OUT)
