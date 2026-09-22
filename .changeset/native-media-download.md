---
"@agent-wechat/agent-wechat": minor
---

Request missing incoming DM files and full images through WeChat's native queue from the media endpoint on supported AMD64 and ARM64 builds. Reuse a process-scoped helper, suppress duplicate submissions, and return data only after cache validation. Preserve legacy image selection for clients that omit quality; OpenClaw requests full images explicitly.
