---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
"@agent-wechat/client-ui": patch
"@agent-wechat/desktop": patch
---

Restrict group-wide mentions to the group owner and administrators. Refresh the administrator list from WeChat into the official profile and expose administrator IDs and mention eligibility through group info. Check permission before queuing and submitting a new message, while retaining existing send receipts and idempotency.

Show Everyone in the composer when the server permits it, preserve drafts after permission changes, and retain the existing behavior with older servers that omit the permission field.
