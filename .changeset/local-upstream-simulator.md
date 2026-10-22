---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

Publish queued text sends, logout holds and interrupted-send recovery to the durable event stream so multiple hub clients can observe state changes. Introduce an injectable native upstream boundary and a separate local development simulator for API/SDK regression testing with synthetic profiles, persistent upstream state and no WeChat connection.
