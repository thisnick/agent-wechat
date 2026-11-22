---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
"@agent-wechat/client-ui": patch
"@agent-wechat/shared": patch
"@agent-wechat/rust-crates": minor
---

Expand oversized merged-forward chat history cards arriving as app message subtype `40` (`49:40`) in place, and show an explicit unavailable fallback when expansion cannot complete.

- Add CGI `221` (`downloadappattach`) chunked download support to `wechat-protocol` (`AppAttachRequest`, `AppAttachChunk`, `download_app_attach_chunk`, `Service::AppAttachment`) and in-place subtype-`40` expansion updates to `wechat-profile` (`AppMessageExpansion`, `apply_app_message_expansion`).
- Automatically download and expand incoming `49:40` messages in `wechat-core`, rewriting the official `Msg_*` row to its inner subtype (such as `49:19`) and emitting `message.updated` if a later retry succeeds after an initial failure.
- Render unexpanded `49:40` forwarded cards with an explicit `[Chat History unavailable]` fallback instead of `body: null` across the daemon API, OpenClaw extension, and browser client UI, and allow quoting expanded `49:19` cards up to the 1 MiB forwarded-XML limit.
