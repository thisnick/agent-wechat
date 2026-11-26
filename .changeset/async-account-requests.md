---
"@agent-wechat/rust-crates": minor
---

Add async parity for the remaining blocking-only `wechat-protocol` paths: `NativeClient::request` with `AsyncAccountRequest` (the async counterpart of `request_blocking`, keeping a verified reply when the account commit fails), async `upload_sticker_chunk_raw`, `custom_stickers_raw`, `send_sticker_chunk_raw`, `sticker_pack_page_raw`, `sticker_pack_detail_raw`, `sticker_pack_captions_raw` and `sticker_pack_acquisition_raw`, and LAN risk-challenge support in async QR login (`poll_login_guarded_async`, `ProtocolTransport::with_lan_login`, enabled by `ProtocolTransport::native()`). The protocol examples now use the native async transport.
