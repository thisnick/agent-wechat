---
"@agent-wechat/client-ui": patch
"@agent-wechat/desktop": patch
---

Keep IME candidate confirmation and cancellation inside the message input instead of sending a message or removing a quoted reply. Track composition events, handle WebKit's IME key code, and ignore repeated Enter presses when sending.

Remove the duplicate browser tooltip from conversation previews, keeping the conversation hover card.

Add cancellation for queued text messages and dismissal for uncertain or failed send notices. Dismissal keeps the receipt for recovery and does not claim to cancel a message that may already have been accepted.
