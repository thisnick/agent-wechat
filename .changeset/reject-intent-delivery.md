---
"@agent-wechat/cli": patch
---

Keep an incoming call's actor available when an accepted reject fences an unsent poll before the reject command reaches the backend. Send the reject once, without resending the poll, and retain uncertain rejection receipts.
