---
"@agent-wechat/agent-wechat": minor
---

Request missing files and full images through WeChat's native queue from the media endpoint on supported AMD64 and ARM64 builds, preserving sender identity for groups, self-sent messages, and File Transfer. Reuse a process-scoped helper, suppress duplicate submissions, and return data only after cache validation. Preserve legacy image selection for clients that omit quality; OpenClaw requests full images explicitly. Voice and video retain cached retrieval without native triggering.
