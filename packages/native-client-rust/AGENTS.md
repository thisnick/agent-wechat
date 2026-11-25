# Native Rust workspace guide

These instructions apply to `packages/native-client-rust/`. Follow the
repository's root `AGENTS.md` as well.

## Prefer pure-Rust crypto and async transport APIs

`wechat-protocol` currently retains its legacy OpenSSL crypto backend
(`openssl-crypto`) and blocking OS-socket / `reqwest::blocking` I/O backends for
`3.x` backwards compatibility while existing native callers migrate. Both legacy
surfaces are scheduled for retirement in `#701`.

**For all new code, tests, and refactors:**

1. **Use pure-Rust crypto (`pure-rust-crypto`) and portable `crypto` APIs.** Do
   not add new OpenSSL dependencies, `openssl`-typed signatures, or callers of
   the legacy OpenSSL helpers in `wechat-protocol`. (SQLCipher in
   `wechat-profile` and `wechat-store` remains OpenSSL-backed separately.)
2. **Use async `ProtocolTransport` / `HttpTransport` APIs.** Put protocol,
   framing, validation, and state-machine logic in shared sans-I/O cores
   (`mmtls::SessionCore`, `DownloadState`, upload builders) so it works on both
   native and `wasm32-unknown-unknown`. Do not add new features or new callers
   to the legacy blocking `std::net::TcpStream` or `reqwest::blocking` paths.
3. **Keep `wasm32-unknown-unknown` clean.** Verify that `wechat-media` and
   `wechat-protocol --no-default-features --features pure-rust-crypto` pass
   `wasm32-unknown-unknown` Clippy (`-D warnings`) when changing shared code.

## Legacy APIs to avoid in new code

| Legacy (avoid in new code; tracked in `#701`) | Preferred replacement |
| --- | --- |
| `openssl-crypto` feature, `crypto::openssl_backend`, `crypto::Ecdh::generate`, `crypto::Ecdh::from_private`, `crypto::cbc` | `pure-rust-crypto` feature, `crypto::Curve`, `Ecdh::generate_for`, `Ecdh::from_private_for`, `crypto::cbc_encrypt`, `crypto::cbc_decrypt` |
| `mmtls::Session`, `mmtls::short_exchange` | `mmtls::AsyncSession`, `mmtls::short_exchange_async` |
| `push::Connection`, `AccountPushConnection`, `NativeClient::open_push_connection_blocking` | `push::AsyncConnection`, `AsyncPushConnection`, `NativeClient::open_push_connection` |
| Blocking `NativeClient` login and escape hatches (`start_*_login`, `poll_login`, `complete_login`, `refresh_qr_login`, `*_blocking`, `spawn_blocking` fallbacks) | `NativeClient::with_transport`, `from_account_with_transport*`, `*_login_async`, and `NativeClient` async RPCs |
| Blocking `CdnSession`, `CdnClient::{download*, upload*}`, `PreparedFileUpload` | `CdnClient::*_async`, `PreparedAsyncUpload` (`AsyncUploadSource`), `AsyncDownloadSink` |
| Blocking `download_advertised_avatar`, `fetch_sticker*`, `fetch_link_thumbnail`, `fetch_tp_media*`, `fetch_channels_video` | `download_advertised_avatar_async`, `fetch_sticker*_async`, `fetch_link_thumbnail_async`, `fetch_tp_media*_async`, `fetch_channels_video_async` |
