---
"@agent-wechat/wechaty-puppet": patch
---

Clarify that WeChat login can be completed through the puppet QR flow without requiring a separate CLI login step.

Also remove noisy unknown-UI waiting status messages during login (`Unknown UI state ({}s), waiting...`) while keeping the existing hard-coded unknown-state timeout behavior.
