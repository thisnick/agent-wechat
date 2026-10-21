---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

Accept the existing official unread-table layout with `create_time` before `local_id`. Previously, a new message could reach the message database but fail the thread update, preventing OpenClaw from receiving its committed event. Preserve strict column attributes, keys and indexes, and recover retained messages without another network send or duplicate event.
