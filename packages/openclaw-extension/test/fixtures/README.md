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

## Native HTTP acknowledgement fixtures

`native-send-legacy.json` preserves the minimal `{id,state}` acknowledgement
accepted by earlier extension code and the existing Gateway smoke fixtures.
`native-send-current.json` is a separate full send-intent response. Keep both
files unchanged when adding another response shape; add a new fixture instead.

`src/native-send.test.ts` exercises both through submission, queue polling and
lost-response recovery. Missing account/conversation fields use the authenticated
request's scope; a supplied mismatch or null is rejected. Acceptance alone does
not establish delivery: a verified result still requires reading the exact stored
outgoing message and checking its account, conversation and server ID.

These are daemon HTTP responses, separate from the persisted consumer-file
upgrade fixture above. The existing Gateway fixtures keep their original minimal
responses, and `src/native-consumer-compat.test.ts` continues to load the frozen
consumer file without regenerating it.
