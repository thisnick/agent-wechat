---
"@agent-wechat/rust-crates": minor
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
---

The daemon's CDN uploads and downloads, file-backed uploads, and avatar, sticker, link-thumbnail, Enterprise WeChat and Channels video downloads now use the native async transport; message media URLs go through `NativeHttpTransport` with `DestinationPolicy::PublicOnly`. `wechat-protocol` adds `HttpTransport::send_streaming` (with `HttpBodySink` and `HttpResponseHead`) so long downloads report progress and stop per piece, streams Channels videos through it, and adds `TransportError::Interrupted` so CDN transfers retry the same address after a connection drops mid-request, as the blocking session does.
