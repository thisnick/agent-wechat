---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
"@agent-wechat/shared": patch
---

Expose saved direct-chat and group notification preferences as nullable `muted` values on thread lists and detail. Read each page in a single local query. Apply recognized incoming direct mute-only updates while preserving unrelated flags and retaining unsupported controls and stale pending updates.
