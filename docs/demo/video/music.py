"""Original, locally synthesized 96 BPM instrumental for the demo.

No samples or existing recordings: electric-piano-like FM chords, soft
four-on-the-floor kick, bass, filtered hats and a restrained rim sound.
"""
from pathlib import Path
import json, math, wave, subprocess
import numpy as np

from config import OUTPUT, FFMPEG
E=OUTPUT/'music';E.mkdir(exist_ok=True)
O=OUTPUT
F=FFMPEG
SR=48000
DURATION=2776/30
BPM=96
BEAT=60/BPM
N=round(DURATION*SR)
rng=np.random.default_rng(281026)
mix=np.zeros((N,2),dtype=np.float32)

def hz(m):return 440*2**((m-69)/12)

def add(at,signal,amp=1,pan=0):
 start=round(at*SR)
 if start>=N:return
 signal=signal[:N-start]
 left=math.cos((pan+1)*math.pi/4);right=math.sin((pan+1)*math.pi/4)
 mix[start:start+len(signal),0]+=signal*amp*left
 mix[start:start+len(signal),1]+=signal*amp*right

def times(duration):return np.arange(round(duration*SR),dtype=np.float32)/SR

def finish_env(t,length,attack=.01,release=.08):
 return np.minimum(1,t/attack)*np.minimum(1,np.maximum(0,(length-t)/release))

def noise_band(length,lo,hi):
 n=round(length*SR)
 x=rng.standard_normal(n)
 freqs=np.fft.rfftfreq(n,1/SR)
 shape=(1-np.exp(-(freqs/lo)**4))*np.exp(-(freqs/hi)**6)
 x=np.fft.irfft(np.fft.rfft(x)*shape,n)
 return x/(np.std(x)+1e-8)

def kick():
 t=times(.34)
 # The pitch settles quickly; no subsonic rumble or hard transient.
 phase=2*np.pi*(48*t+62*.018*(1-np.exp(-t/.018)))
 return (.8*np.sin(phase)*np.exp(-t/ .085)+.035*noise_band(.34,700,2200)*np.exp(-t/.009))*finish_env(t,.34,.002,.04)

def hat(opened=False):
 length=.16 if opened else .065;t=times(length)
 return noise_band(length,2700,7200)*np.exp(-t/(.036 if opened else .014))*finish_env(t,length,.001,.02)

def rim():
 t=times(.11)
 return (np.sin(2*np.pi*820*t)+.35*np.sin(2*np.pi*1540*t)+.22*noise_band(.11,850,3500))*np.exp(-t/.014)*finish_env(t,.11,.001,.02)

def bass(m):
 t=times(.46);f=hz(m)
 wave=np.sin(2*np.pi*f*t)+.12*np.sin(2*np.pi*f*2*t)+.025*np.sin(2*np.pi*f*3*t)
 return wave*np.exp(-t/.23)*finish_env(t,.46,.012,.1)

def keys(m,length=4.4):
 t=times(length);f=hz(m)
 phase=2*np.pi*f*t+.52*np.exp(-t/.9)*np.sin(2*np.pi*f*2*t)
 tone=np.sin(phase)+.12*np.sin(2*np.pi*f*2.003*t)+.04*np.sin(2*np.pi*f*3*t)
 return tone*np.exp(-t/1.65)*finish_env(t,length,.014,.4)

def pad(m,length=5.3):
 t=times(length);f=hz(m)
 tone=(np.sin(2*np.pi*f*.999*t)+np.sin(2*np.pi*f*1.001*t))*.5
 return tone*finish_env(t,length,.7,.9)

# Two bars per chord: Dmaj7, Bm7, Gmaj7, Aadd9. Voicings stay in the
# midrange to leave room for intelligible speech.
chords=[([50,57,61,66],38),([47,54,57,62],35),([43,54,59,62],31),([45,52,59,61],33)]
k=kick();r=rim()
for beat in range(140):
 at=.08+beat*BEAT
 if at>=87.5:break
 add(at,k,.19*(1 if beat%4==0 else .85))
 if beat%4 in (1,3):add(at+.009,r,.035,pan=.15)
 for sub in range(2):
  tick=at+sub*BEAT/2+.012
  add(tick,hat(opened=(beat%8==7 and sub==1)),.014*(.72 if sub==0 else 1.0),pan=(-.18 if sub==0 else .22))
 chord,root=chords[(beat//8)%4]
 if beat%2==0:add(at+.025,bass(root),.10)
 if beat%8==6:add(at+BEAT*.75,bass(root+7),.045)

for block,at in enumerate(np.arange(.08,87.5,5.0)):
 notes,root=chords[block%4]
 for i,m in enumerate(notes):
  add(float(at)+i*.009,keys(m),.037,pan=-.4+i*.27)
  add(float(at),pad(m),.009,pan=.35-i*.23)
 # One quiet chord response in the second bar, not a lead melody.
 for i,m in enumerate(notes[1:]):add(float(at)+3.125+i*.006,keys(m,1.8),.012,pan=.25-i*.25)

# A short tonic resolution supports the closing card and then fades out.
for i,m in enumerate([50,57,61,66]):add(87.5+i*.012,keys(m,4.2),.035,pan=-.35+i*.23)
add(87.5,bass(38),.07)

# Small diffuse echoes soften the synthetic keys/percussion without a large
# reverb tail. Their levels stay far below the direct signal.
dry=mix.copy()
for delay,gain in [(.093,.07),(.151,.045),(.227,.025)]:
 n=round(delay*SR);mix[n:]+=dry[:-n,::-1]*gain
del dry
t=np.arange(N,dtype=np.float32)/SR
fade=np.minimum(1,t/.9)*np.minimum(1,np.maximum(0,(DURATION-t)/2.6))
mix*=fade[:,None]
peak=float(np.max(np.abs(mix)))
mix*=.65/max(peak,1e-9)
pcm=(np.clip(mix,-1,1)*32767).astype('<i2')
with wave.open(str(E/'original-instrumental.wav'),'wb')as w:
 w.setnchannels(2);w.setsampwidth(2);w.setframerate(SR);w.writeframes(pcm.tobytes())
subprocess.run([F,'-v','error','-y','-i',str(E/'original-instrumental.wav'),'-af','loudnorm=I=-26:TP=-6:LRA=6,aresample=48000','-c:a','pcm_s16le',str(E/'music-levelled.wav')],check=True)

# Exact, gently ramped dialogue ducking. Voice notes remain at their original
# speed and level. Attack starts ahead of speech; release is gradual.
with wave.open(str(E/'music-levelled.wav'),'rb')as w:
 music=np.frombuffer(w.readframes(w.getnframes()),dtype='<i2').reshape(-1,2).astype(np.float32)/32768
music=music[:N]
tt=np.arange(len(music),dtype=np.float32)/SR
envelope=np.ones(len(music),dtype=np.float32)
for start,end in [(33.0,33.0+8.180),(49.8,49.8+15.960)]:
 attack=np.clip((tt-(start-.4))/.4,0,1);attack=attack*attack*(3-2*attack)
 release=np.clip((tt-(end+.1))/.85,0,1);release=release*release*(3-2*release)
 envelope=np.minimum(envelope,1-.70*attack*(1-release))
music*=envelope[:,None]
with wave.open(str(E/'music-ducked.wav'),'wb')as w:
 w.setnchannels(2);w.setsampwidth(2);w.setframerate(SR);w.writeframes((np.clip(music,-1,1)*32767).astype('<i2').tobytes())
subprocess.run([F,'-v','error','-y','-i',str(E/'music-levelled.wav'),'-c:a','libmp3lame','-b:a','192k',str(O/'demo-background-music.mp3')],check=True)
(E/'music.json').write_text(json.dumps(dict(bpm=BPM,key='D major',duration_seconds=DURATION,instruments=['soft kick','filtered hats','rim','sine bass','FM electric piano','quiet pad'],origin='Locally synthesized original composition; no sampled recordings',music_loudness_target_lufs=-26,speech_duck_db=round(20*math.log10(.30),2),speech_windows=[[33,41.18],[49.8,65.76]]),indent=2))
print('DONE music',DURATION,flush=True)
