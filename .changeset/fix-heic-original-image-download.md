---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
---

Accept server-transcoded CDN original images (such as iPhone HEIC photos delivered as larger JPEGs) without failing with `media_integrity_failed`, and write/read their `_h.dat` cache entries in official profiles via the exact `MessageResourceDetail` (`type = 65537`) resource join.
