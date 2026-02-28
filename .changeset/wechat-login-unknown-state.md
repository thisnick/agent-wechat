---
"@agent-wechat/wechaty-puppet": patch
---

Clarify that WeChat login can be completed through the puppet QR flow without requiring a separate CLI login step.

Also harden login behavior so transient unknown UI states during login no longer fail immediately; the flow now keeps waiting for terminal login events instead of surfacing an early unknown-state failure.
