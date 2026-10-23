---
"@agent-wechat/cli": minor
"@agent-wechat/agent-wechat": minor
"@agent-wechat/shared": minor
---

Add admin-scoped native group creation, rename, membership, own display name, announcement clearing, leave and group notification operations with stable idempotency keys and retained uncertain outcomes. Split member list and exact lookup, refresh server group details, and expose matching API/SDK methods.

Add the owner-only `wechat_group_info` OpenClaw tool. Recognized incoming group metadata updates are saved before events; unknown sync controls remain pending. Automatic propagation of every edit made on another device is not established.
