---
"@agent-wechat/client-ui": patch
"@agent-wechat/cli": patch
---

Connect the message Reply action and composer to native quoted text sends. Keep quote selections scoped to each chat, preserve exact references through pending and uncertain sends, and reject stale or recalled targets without sending ordinary text instead.

Support quoted replies in the isolated server simulator so native admission, history, event delivery and quote lookup can be tested without an official endpoint.
