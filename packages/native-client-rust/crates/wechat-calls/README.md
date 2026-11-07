# wechat-calls

Use `wechat-calls` for voice-session and media building blocks without the HTTP
daemon, local audio devices or OpenClaw. It includes the native direct/conference
session machinery used by the daemon, optional Opus, and portable PCM utilities.
All five public WeChat crates follow the same native release version; pin that
version when combining them.

## Choose a feature set

| Cargo configuration | Available modules | Build requirements |
| --- | --- | --- |
| Default (`native-runtime`) | PCM utilities, Opus, native session/media implementation and `wechat-protocol` | Rust 1.93.1+, C/C++ compiler, CMake, Make and Perl |
| `default-features = false, features = ["native-opus"]` | PCM utilities and Opus, without protocol/account dependencies | Rust 1.93.1+, C/C++ compiler and CMake |
| `default-features = false` | PCM formats, queues, ordering, pacing, tones and mixing | Rust 1.93.1+; also builds for `wasm32-unknown-unknown` |

Native features are unavailable on WebAssembly. No feature opens a microphone or
speaker, loads PulseAudio, or creates a Tokio runtime. Applications supply their
execution, clocks and device adapters. The native media implementation can start
workers when the embedding host requests it.

## Mix PCM tracks

`AudioFrame` owns interleaved signed 16-bit samples. Its `timestamp` is a sample
position per channel on the shared presentation clock; `generation` fences
replaced audio, and `sequence` orders frames within a source. Announce tracks
before submitting frames. Render each due slot after the application's chosen
alignment delay; missing/muted sources contribute silence and late frames do
not accumulate a catch-up backlog. Do not submit your own uplink to the mixer.

```rust
use wechat_calls::{audio::{AudioFormat, AudioFrame}, mixer::PcmMixer};

let format = AudioFormat::new(16000, 1, 20)?;
let mut mixer = PcmMixer::new(format, 2, 3, 0)?;
mixer.add_track(7, 1)?;
mixer.push(7, AudioFrame {
    generation: 1,
    sequence: 0,
    timestamp: 0,
    samples: vec![1000; format.samples_per_frame()],
})?;
let mixed = mixer.render(0)?;
assert_eq!(mixed.samples.len(), 320);
# Ok::<(), wechat_calls::audio::MediaError>(())
```

`AudioQueue` fences interrupted output with a new generation. `PcmOutput` admits
variable-size PCM chunks with backpressure and produces frames. `FramePacer`
avoids bursts after a scheduling stall. These are scheduling tools, not a full
jitter buffer or congestion controller. Clearing application audio never
licenses resetting a transport counter or reusing a key.

## Check examples offline

From a source checkout:

```sh
cargo run -p wechat-calls --no-default-features --example portable_mix
# tracks=2 samples=320 first_sample=3000
cargo run -p wechat-calls --no-default-features --features native-opus --example tone_roundtrip
# frames=50 packet_bytes=... decoded_samples=16000
```

`OpusEncoder` and `OpusDecoder` implement `AudioEncoder` and `AudioDecoder` for
one configured frame per raw Opus packet. Authenticate/admit packets before
decoding them. Neither example accesses accounts, devices or the network.

## Native calling integration

`wechat-calls` depends on `wechat-protocol`; protocol does not depend on calls.
`conference_media` decodes admitted conference sources; `conference_send` handles
one paced uplink. The `session` and `native_media` modules are lower-level host
integration APIs hidden from generated docs while the public account-bound
`CallClient`/`CallHandle` facade is unfinished. There is no `client.calls()` API
in this release. Embedding those modules requires account ownership, permission
checks, request journals, cancellation, event delivery and uncertainty handling.
Do not retry signaling that may already have reached the server.

For an application that needs complete dial/join/invite/leave operations today,
use the daemon's [Calls API](https://thisnick.github.io/agent-wechat/reference/calls-api/).
Its JSON-v2/base64 PCM contract is separate from this crate's typed frames and
WeChat media packets. Native workers currently stop at 20 minutes; key renewal,
long-call acceptance, video and direct-to-conference promotion are not provided.

Our code is MIT-licensed; see [LICENSE](LICENSE). Native Opus distributions must
also retain [NOTICE.md](NOTICE.md), [LICENSE-OPUS](LICENSE-OPUS) and
[LICENSE-OPUSIC](LICENSE-OPUSIC).
