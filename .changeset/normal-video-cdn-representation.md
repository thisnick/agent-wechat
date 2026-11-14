---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
---

Fix received normal-quality videos that failed to download with `media_integrity_failed` when WeChat served a different playback copy than the message advertised. The daemon now accepts that copy after checking it against the checksum WeChat sends with it, including for videos received before this release. A reply that marks a different copy but has no valid checksum still fails, and original-quality copies must still match the message exactly.
