---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
"@agent-wechat/shared": patch
---

Make whole-thread marking the only public read operation. Remove message-selection flags and request fields, keep expected-version checks, and mark a thread after confirmed OpenClaw model handoff unless reads are disabled. Add durable native cross-device read notifications that retain uncertain outcomes without automatic resubmission.
