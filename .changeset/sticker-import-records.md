---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
"@agent-wechat/rust-crates": patch
---

Fix adding larger custom stickers by splitting encrypted upload requests across transport records. Preserve collection revision headers in the desktop app so moving and removing Saved stickers reaches the daemon with the required precondition.
