---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
---

Fix large file downloads when the CDN omits redundant source-size metadata, while retaining transfer length, decrypted size, and checksum validation.
