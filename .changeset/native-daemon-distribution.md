---
"@agent-wechat/cli": minor
"@agent-wechat/agent-wechat": minor
"@agent-wechat/shared": minor
---

Add native daemon distribution and explicit native OpenClaw transport. The global `wx` entry point now selects a version-matched native executable with npm-bundled SHA-256 verification, offline provisioning and no runtime downloads; the previous container client remains available as `wx-legacy`. Add release/image preparation gates and standalone deployment templates.

Add the native v1 HTTP/SSE SDK, managed-child and external hosting, durable consumer custody and live committed-text dispatch. Native media/voice forwarding remains unsupported. Prepared-model read receipts are tested in a harness and fail closed on released OpenClaw hosts without the required callback; consumer acknowledgement alone never clears unread state.
