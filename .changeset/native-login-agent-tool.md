---
"@agent-wechat/agent-wechat": patch
---

Register wechat_login explicitly for native account login from any OpenClaw channel. Return the daemon's QR as a direct image, reuse pending login operations, and support bounded start/wait polling, status, disconnect, logout and explicitly confirmed credential forgetting. Preserve invocation cancellation and account/configuration boundaries.
