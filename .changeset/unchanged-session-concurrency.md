---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
---

Preserve successful operations when concurrent requests re-encrypt identical session credentials. Continue to reject stale credential, cookie, connection and logout state.
