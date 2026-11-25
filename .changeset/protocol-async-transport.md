---
"@agent-wechat/rust-crates": minor
---

Add pluggable async transport abstractions (`wechat_protocol::transport`) and `wasm32-unknown-unknown` target compatibility to `wechat-protocol` and `wechat-media`.

- Add `transport::{AsyncByteStream, Clock, HttpMethod, HttpOverStream, HttpRequest, HttpResponse, HttpTransport, MaybeSend, MaybeSync, ProtocolTransport, ReadOutcome, StreamTransport, SystemClock, TransportError, TransportFuture}`.
- Add `mmtls::AsyncSession`, `mmtls::short_exchange_async`, `AsyncPushConnection`, `NativeClient::with_transport`, `NativeClient::from_account_with_transport`, `NativeClient::from_account_with_transport_and_persistence`, async login methods (`*_login_async`), and async `lookup_openim_contact` / `lookup_openim_contact_raw` / `channels_*` methods.
- Gate blocking OS-socket, `reqwest`, `wechat-platform`, and native FFmpeg/WXGF helpers behind `cfg(not(target_arch = "wasm32"))` so `wechat-media` and `wechat-protocol --no-default-features --features pure-rust-crypto` compile cleanly for `wasm32-unknown-unknown` without adding Cargo features or changing native builds.
