---
"@agent-wechat/cli": patch
---

Reduce the time message sync holds the account lock on profiles with many conversations by checking the target conversation table's schema directly.
