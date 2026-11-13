---
"@agent-wechat/desktop": patch
---

Speed up sticker pack browsing by loading only visible previews, pausing the obscured tray, and removing duplicate collection and post-membership detail reads. Reuse recent catalog and pack views for up to one minute, invalidating them on collection changes, reconnection, event gaps and mutations.
