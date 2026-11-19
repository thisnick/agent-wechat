---
"@agent-wechat/client-ui": patch
---

The contact card keeps contact data and remark and privacy saves in TanStack Query. Behavior is unchanged: a reloading contact stays on screen, edits save one after another, and a save whose outcome is unknown is checked before it is retried.
