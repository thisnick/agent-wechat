---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
---

Complete the recorded WeChatOut, UpdateWxaUsageListNotify and deleteMessages sync controls so they no longer hold a sync batch after the other commands have applied.

After recovery, `wx store usage` counts only the commands that still block a batch.
