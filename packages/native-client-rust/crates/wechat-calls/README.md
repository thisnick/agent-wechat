# wechat-calls

Call-session state, protocol orchestration and portable media utilities. This
crate is not published yet. General conference API/CLI integration and acceptance
testing are still in progress.

Default `native-runtime` builds include call sessions and `native-opus`.
`native-opus` alone provides the codec without account/protocol dependencies.
With no default features, PCM queues, ordering, pacing and mixing remain portable
and compile for WebAssembly. Hosts supply authenticated transport, account
authority, journals, clocks and PCM. Device access and credential persistence
belong in adapters. Raw-packet VOIP encoding/decoding uses
`opusic-c` and its bundled libopus. Device callbacks must hand samples to a worker
rather than invoking codec or network operations directly.

`AudioQueue` uses a fixed frame capacity and fences interrupted output with a
new generation. Incoming playout may drop old queued frames; outgoing production
reports backpressure. `FramePacer` avoids catch-up bursts after a scheduling stall.
These are local scheduling tools, not a complete jitter buffer or congestion
controller. The host chooses loss-concealment policy and negotiates codec settings.

`PulseTone` produces synthetic PCM without accessing devices or the network.
Playing it to a real participant requires a separately authorized call.

The native codec contains third-party C code under libopus's BSD license and
patent grant. Preserve its license notices when distributing native artifacts.
See [NOTICE.md](NOTICE.md), the retained license files, and
<https://opus-codec.org/license/>. The
portable crate code is MIT-licensed under the repository license.

This crate depends on `wechat-protocol` for authenticated host transport and
codecs; protocol does not depend on calls. Core uses these same session state
machines while retaining daemon authorization and encrypted operation journals.
Direct and conference media workers, source tracking, interruption gates and
relay setup also run in this crate. Core supplies
permission checks, interruption guards and event delivery through narrow hooks;
no Core, HTTP, persistence or device dependency enters this crate. The current
`session` and `native_media` integration layers are hidden from generated API
docs while the account-bound public service is being completed.

The daemon's private PCM envelope belongs to Core. It is not this crate's public
media interface. API protocol v2 sockets exchange JSON with base64 PCM, separately
from WeChat media packets.

Run the generated-audio check without devices or network access:

```sh
cargo run -p wechat-calls --example tone_roundtrip
```
