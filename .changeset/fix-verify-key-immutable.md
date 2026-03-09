---
"@agent-wechat/cli": patch
---

Fix verify_key to use immutable=1 to avoid acquiring locks on WeChat databases
