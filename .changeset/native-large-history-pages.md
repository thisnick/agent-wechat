---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

Resolve continued native message pages from their retained message IDs instead of decoding the entire thread again for every page. Preserve stable membership, current edits, deletion tombstones and account/query checks. Allow a bounded thirty-minute history view so large downloads can finish without the former five-minute cursor expiry.
