---
"@agent-wechat/client-ui": patch
"@agent-wechat/desktop": patch
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

When signing in to an existing account fails, or its phone prompt goes unanswered or is declined, without showing a QR code, the sign-in screen now offers **Sign in with a QR code instead** under **Try again**. It skips the saved sign-in and shows a QR code for the same account, so the account keeps its chats, like `wx auth login --force-qr`. Before, **Try again** only repeated the saved sign-in, and **Add WeChat account** created a new, empty account.

A failed sign-in's result now names the step WeChat refused and the code it returned, in `service` and `serviceCode`, when WeChat returned one. The sign-in screen shows them on a **Details** line.
