---
"@agent-wechat/agent-wechat": patch
---

Fix GPT-Live voice calls failing to connect on OpenClaw 2026.11.1. Pass a plain audio format to media workers while preserving the SDK's callback ownership, settings, and call permissions.
