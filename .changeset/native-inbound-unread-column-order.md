---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

Validate supported official table columns by name instead of physical order. Previously, an unread table with `create_time` before `local_id` could let a new message reach history but fail the thread update, preventing OpenClaw from receiving its committed event. Preserve strict column attributes, keys and indexes, and recover retained messages without another network send or duplicate event.
