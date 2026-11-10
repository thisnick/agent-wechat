---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
"@agent-wechat/desktop": patch
---

Expose the sender's supplied display name as optional `quote.senderName` in received quoted replies, including replies quoting OpenIM members. The name comes from the delivered reply payload.
