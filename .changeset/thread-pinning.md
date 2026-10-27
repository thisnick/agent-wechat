---
"@agent-wechat/cli": minor
"@agent-wechat/shared": minor
---

Add backend thread pinning for ordinary direct chats and active groups. Chat responses include nullable saved pin state; new lists place pinned chats first. Add admin-only pin/unpin API, SDK and CLI operations using the existing idempotency and recovery journal. Recognized incoming pin changes update the official profile and emit thread.updated. Built-in peers and unknown updates remain unsupported; the frontend is unchanged.
