---
"@agent-wechat/rust-crates": patch
---

Sticker sends are about four times faster. A sticker is sent in 8 KB pieces, and every piece used to start a new secure connection to WeChat; a 370 KB sticker took about 70 seconds. Pieces now go over the account's existing long-lived connection when it is connected, as the official client sends them, and otherwise reuse the account's current short-link ticket until it nears expiry. The same sticker now takes about 15 seconds. Other requests to WeChat also skip the per-request connection setup and are faster.
