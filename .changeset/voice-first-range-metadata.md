---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

Correct metadata for voice notes uploaded in multiple requests by including the client-ID buffer only in the first request. Notes continue to support up to 60 seconds.
