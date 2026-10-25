---
"@agent-wechat/cli": patch
---

Sign out locally when WeChat rejects the current push, heartbeat or sync session
with an authentication-required error. Stop automatic reconnect and session-renewal
attempts, retain profiles and history, and publish the failure source and service
code in account events. Late failures cannot sign out a newer login.
