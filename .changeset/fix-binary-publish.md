---
"@agent-wechat/cli": patch
---

Fix binary publish job in release workflow

- Remove read-only flag from Docker source mount that prevented container startup (exit code 125)
