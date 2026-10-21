---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

Receive incoming work over an authenticated persistent WeChat connection and sync immediately on native push notifications. Preserve server-directed heartbeats and transport keepalives, reconnect with bounded backoff, and fall back to heartbeat polling when the persistent connection is unavailable. Expose the active incoming transport in account status and retain durable sync, account isolation and logout fences.
