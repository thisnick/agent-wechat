---
"@agent-wechat/rust-crates": patch
---

A remark change, from this account or another device, is now confirmed within moments instead of at the account's next scheduled sync, which could be up to 30 seconds away. Queueing the remark lookup wakes the account's worker.
