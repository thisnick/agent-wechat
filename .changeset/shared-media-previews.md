---
"@agent-wechat/client-ui": patch
---

Share browser media preview bytes and blob URLs across chat messages, the media viewer and its thumbnail strip. Deduplicate concurrent reads, retain recent previews within a memory budget, and discard recalled or signed-out content. Use “Compressed” for smaller downloads and “Make original available for download” for the retrieval hint. Separate the QR countdown and sign-in hint so their text cannot overlap.

Clear the current thread’s unread count when focusing or clicking the message composer, using the same version-checked read acknowledgement as replies.
