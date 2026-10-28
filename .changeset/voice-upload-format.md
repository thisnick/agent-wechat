---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
"@agent-wechat/desktop": patch
---

Correct the cancellation and codec-format fields in outgoing voice uploads so SILK notes declare format 4 in the transmitted request, including continuation and final ranges.
