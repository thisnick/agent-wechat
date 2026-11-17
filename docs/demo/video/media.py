"""Frame decoding helpers for the recorded demo."""
from PIL import Image
import subprocess
from config import MEDIA, OUTPUT, FFMPEG

F=FFMPEG
E=OUTPUT
R=MEDIA
P0=MEDIA/'phone-message.mp4'
P1=MEDIA/'phone-voice.mp4'
WIN=MEDIA/'windows.mp4'
CLAW=MEDIA/'openclaw.webm'
CLI=MEDIA/'terminal.webm'
FPS=30

def smooth(t):
 p=max(0,min(1,t))
 return p*p*(3-2*p)

def lerp(a,b,t):return a+(b-a)*t

def frames(path,start,dur,speed,size):
 count=round(dur*FPS)
 if path is None:
  for _ in range(count):yield None
  return
 w,h=size
 args=[F,'-v','error','-threads','2','-ss',str(start),'-i',str(path),'-an','-vf',f'setpts=(PTS-STARTPTS)/{speed},fps={FPS},scale={w}:{h}', '-frames:v',str(count),'-pix_fmt','rgb24','-f','rawvideo','pipe:1']
 proc=subprocess.Popen(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
 last=None
 try:
  for i in range(count):
   data=proc.stdout.read(w*h*3)
   if len(data)!=w*h*3:
    if last is None:raise RuntimeError(f'No frame: {path}: '+proc.stderr.read().decode())
    yield last;continue
   last=Image.frombytes('RGB',(w,h),data);yield last
 finally:
  proc.stdout.close();proc.terminate();proc.wait()
