---
"@agent-wechat/rust-crates": minor
---

Deprecate `wechat-protocol`'s blocking APIs ahead of their removal in 4.0: `mmtls::Session` and `short_exchange`, `AccountPushConnection` and `open_push_connection_blocking`, `AccountRequest` and `request_blocking`, `renew_session_blocking`, `logout_blocking`, `shutdown_blocking`, `PendingLogout::submit_blocking` / `submit_guarded_blocking`, the blocking QR and saved-device login methods, the blocking `CdnClient` transfer methods, `PreparedFileUpload`, and the blocking avatar, sticker, link-thumbnail, Enterprise WeChat and Channels downloaders. Each deprecation note names its async replacement.
