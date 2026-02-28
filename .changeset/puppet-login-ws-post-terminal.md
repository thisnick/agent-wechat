---
"@agent-wechat/wechaty-puppet": patch
---

Harden Wechaty puppet login websocket handling to avoid false bot errors after successful login.

- Treat websocket `error`/`close` callbacks as non-fatal once a terminal login event has been seen.
- Normalize empty websocket error messages to a stable fallback.
- Close the login subscription handle immediately after `login_success` to reduce late transport noise.
