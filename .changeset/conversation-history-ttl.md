---
"@agent-wechat/client-ui": patch
"@agent-wechat/desktop": patch
"@agent-wechat/agent-wechat": patch
"@agent-wechat/shared": patch
---

Speed up chat opening with reusable read-only database connections for daemon-owned profiles, batched attachment metadata and an immediate event-stream heartbeat. Keep offline inspection on source-preserving snapshots.

Reuse authenticated conversation caches by last message ID with a five-minute history validation limit, retain hidden cached history across sign-out, catch up missed changes before restoring messages, and avoid redundant startup and foreground reloads. Cancel obsolete history requests when switching chats.
