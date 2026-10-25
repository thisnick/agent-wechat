---
"@agent-wechat/client-ui": patch
---

Return to account selection when WeChat rejects the active session, stop its
background work, and require sign-in before reopening chats. Preserve drafts and
pending-send receipts while clearing fetched account caches. Recheck account
authentication when returning to the browser.
