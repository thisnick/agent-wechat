---
"@agent-wechat/agent-wechat": patch
---

Declare `wechat_send_share` in the plugin manifest's `contracts.tools`. OpenClaw drops tools a plugin registers without declaring them, so agents never saw the share tool and `openclaw plugins doctor` reported an error.
