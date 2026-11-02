# wechat-calls

Portable call-media interfaces, PCM frame queues, pacing and generated test audio.
The crate is initially unpublished. Its primitives do not establish a live call.

The default feature set has no native dependencies. Hosts supply authenticated
packets, clocks and PCM; device access, account credentials and networking belong
in adapters. `native-opus` provides raw-packet VOIP encoding/decoding through
`opusic-c` and its bundled libopus. Device callbacks must hand samples to a worker
rather than invoking codec or network operations directly.

`AudioQueue` uses a fixed frame capacity and fences interrupted output with a
new generation. Incoming playout may drop old queued frames; outgoing production
reports backpressure. `FramePacer` avoids catch-up bursts after a scheduling stall.
These are local scheduling tools, not a complete jitter buffer or congestion
controller. The host chooses loss-concealment policy and negotiates codec settings.

`PulseTone` produces synthetic PCM without accessing devices or the network.
Playing it to a real participant requires a separately authorized call.

The optional codec contains third-party C code under libopus's BSD license and
patent grant. Preserve its license notices when distributing native artifacts.
See [NOTICE.md](NOTICE.md), the retained license files, and
<https://opus-codec.org/license/>. The
portable crate code is MIT-licensed under the repository license.

The internal daemon PCM envelope codec uses a 36-byte `WXCM` version 1 header with big-endian
track/generation/sequence/timestamp/length fields and little-endian signed PCM.
`MediaFrame::decode` borrows its payload, validates complete frame extent and the
negotiated PCM frame length, and allocates nothing. `MediaStream` separately
rejects wrong tracks, stale generations and replayed/out-of-order frames. This is
an internal representation only. API sockets exchange raw PCM samples and JSON
stream metadata; the daemon constructs these envelopes and handles pacing. This
is also separate from the remote WeChat wire format.

Run the generated-audio check without devices or network access:

```sh
cargo run -p wechat-calls --features native-opus --example tone_roundtrip
```
