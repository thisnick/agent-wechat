---
"@agent-wechat/rust-crates": patch
---

Sticker sends are about three times faster. A sticker is sent in 8 KB pieces, and every piece used to start a new secure connection to WeChat before it was sent; a 370 KB sticker took about 70 seconds. The daemon now reuses the account's current connection ticket until it nears expiry, so the same sticker takes about 25 seconds. Other requests to WeChat skip the same setup step and are faster too.
