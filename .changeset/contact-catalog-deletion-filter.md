---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
---

Exclude deleted rows from the saved contact catalog when the official profile exposes a deletion marker. Keep nullable markers and readable layouts without that column working, and preserve historical chat labels and group rosters.
