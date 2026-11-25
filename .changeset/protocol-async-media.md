---
"@agent-wechat/rust-crates": minor
---

Add asynchronous CDN and standalone media transfer APIs (`CdnClient::*_async` including `download_voice_with_progress_async`, `AsyncDownloadSink`, `PreparedAsyncUpload` / `AsyncUploadSource`, `download_advertised_avatar_async`, `fetch_sticker_async`, `fetch_sticker_original_async`, `fetch_link_thumbnail_async`, `fetch_tp_media_async`, `fetch_tp_media_to_async`, `fetch_channels_video_async`, `Clock::sleep`, `HttpResponse::header_values`, and `TransportError::ResponseTooLarge`) backed by `HttpTransport` and `Clock` for native and `wasm32-unknown-unknown` targets.
