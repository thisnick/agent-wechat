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

## Upgrade compatibility fixtures

`native-compat/consumer-state-1.0.0-beta.13.json` is a consumer state file written
by the 1.0.0-beta.13 extension code itself. It holds an answered mention, a mention
held for review and seven group messages waiting as context (text, three quoted
replies, a link card, a sticker and an image). It was produced by accepting the
messages from `packages/native-client-rust/crates/wechat-core/tests/fixtures/native-compat/messages.json`
in order and dispatching each one with the 1.0.0-beta.13 `dispatchNativeMessage`,
then copying the file the extension wrote. All identifiers and text are synthetic.

Never regenerate or edit this file: it stands for state already on deployed
hosts. `src/native-consumer-compat.test.ts` checks that the current extension
loads it and dispatches its buffered context against the daemon's current
message JSON.
