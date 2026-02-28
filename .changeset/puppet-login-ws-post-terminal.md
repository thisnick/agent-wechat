---
"@agent-wechat/cli": patch
"@agent-wechat/wechaty-puppet": patch
---

Harden Wechaty puppet login websocket handling and clarify CLI status behavior.

- Treat websocket `error`/`close` callbacks as non-fatal once a terminal login event has been seen.
- Normalize empty websocket error messages to a stable fallback.
- Close the login subscription handle immediately after `login_success` to reduce late transport noise.
- Make `wx status` report explicit container up/down state, and only show server/login details when the container is running.
