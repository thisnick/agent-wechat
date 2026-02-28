---
"@agent-wechat/wechaty-puppet": patch
---

Clarify that WeChat login can be completed through the puppet QR flow without requiring a separate CLI login step.

Also improve login websocket behavior for puppet clients:

- Remove noisy unknown-UI waiting status messages during login (`Unknown UI state ({}s), waiting...`) while keeping the existing hard-coded unknown-state timeout behavior.
- Ensure the server sends a terminal login event (`login_success`, `login_timeout`, or `error`) before closing the login websocket, instead of closing without a final event.
