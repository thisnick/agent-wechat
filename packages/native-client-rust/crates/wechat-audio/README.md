# Native call audio

`wechat-audio` is the optional native microphone/speaker adapter. Protocol, HTTP
and headless AI clients do not need it. It is not currently published separately.

```rust,no_run
use std::time::Duration;
use wechat_audio::{AudioConfig, AudioSession};

# fn example() -> Result<(), wechat_audio::AudioError> {
let mut session = AudioSession::open(AudioConfig::default())?;
let output = session.playback();
let response = output.token()?;
let pcm = session.recv_timeout(Duration::from_secs(1))?;
output.enqueue(response, &pcm, 16_000)?;
// For an interruption, retain the returned token for the next response only.
let next_response = output.clear()?;
# let _ = next_response;
session.stop()?;
# Ok(())
# }
```

`native-audio` enables platform libraries. Opening a session accesses the microphone
and speaker; no device opens during default unit tests. Input/output are mono f32;
`sample_rate` selects captured PCM rate. `native_sample_rate()` reports the 48 kHz
backend contract. Receive may return an empty chunk while the streaming resampler
accumulates input. Keep receiving to avoid dropping captured chunks.

A playback token belongs to one response generation. After `clear()`, submissions
with earlier tokens fail, including delayed producers. Clearing removes local
queued audio; it cannot retract samples already played. OS buffering adds delay.
Applications should cancel the old producer too. Playback queues reject overflow
rather than accumulating seconds of stale audio.

Linux accepts PulseAudio source/sink names; Windows accepts device endpoint IDs.
macOS currently accepts the system defaults only. Inspect `aec_status()`; a value
of `Requested` records backend selection, not measured cancellation quality. Linux
requires a separately configured echo-cancel source/sink for speakerphone use.

See [THIRD-PARTY.md](THIRD-PARTY.md) for source pins and local changes.

The `isolated_loopback` example is a manual backend check for a disposable sound
server with a null sink and its monitor. It first waits up to three seconds for
actual capture data, since stream creation can precede the sound-server clock. It requires `--isolated-fixture` plus both
source and sink names, generates at most two seconds of quiet pulsed tone, reports
only sample/energy counts, and stops. Run it only in an isolated test environment
without host devices or profiles; it is not a call or an AEC quality test.
