# Synthetic native model fixtures

These files contain no account data or captured user content. Both PNGs are
solid green RGB images generated with Python's standard-library `zlib`, PNG
chunks and CRC32: 32×32 and 1800×1300. The MP4 is one second of a solid green 32×32
frame at two frames/second, generated locally with:

```bash
ffmpeg -f lavfi -i color=c=green:s=32x32:r=2 -t 1 -c:v libx264 \
  -pix_fmt yuv420p -movflags frag_keyframe+empty_moov -f mp4 native-model.mp4
```

The media model harness generates silent PCM WAV and plain-text bytes in memory.
It starts only owned temporary Gateways and local fixture providers; it does not
use account credentials or external model services. The transcription CLI emits
deterministic text after validating the synthetic WAV; it is not a recognizer.
