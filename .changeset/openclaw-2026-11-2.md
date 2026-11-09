---
"@agent-wechat/agent-wechat": patch
---

Validate the OpenClaw integration against 2026.11.2. On this host, `readPolicy: "on_model_dispatch"` confirms a message when OpenClaw reports the prepared model run for it, without `hooks.allowConversationAccess`; this confirms that the run started, not that a text-only model saw an image. The host's catalog dispatcher is now `dispatch_action` (formerly `tool_call`).
