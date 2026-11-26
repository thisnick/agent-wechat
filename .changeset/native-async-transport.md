---
"@agent-wechat/rust-crates": minor
---

Add default native async transports to `wechat-protocol`: `ProtocolTransport::native()`, `TokioStreamTransport`, `TokioSpawner` and `NativeHttpTransport` (pooled HTTP/1.1, no proxy or redirects, streamed receive budgets) with `DestinationPolicy::PublicOnly` for message-supplied media URLs. Add `TransportError::DestinationNotAllowed`, which the async standalone downloaders report as their existing `*_dns_not_public` codes, plus `media::FileUploadSource`, `PreparedAsyncUpload::from_file` and an `AsyncDownloadSink` implementation for `tokio::fs::File`.
