---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

Use only the quoted content embedded in received replies. Include parsed quote text, link-card fields, image availability and sender metadata in normal message responses so OpenClaw does not call the quote-resolution endpoint or substitute a stored original. Keep local recall checks and quoted-author admission. Retrieve images using quote-owned metadata; preserve comments and unavailable status for other quoted media. The existing quote endpoint keeps its URL and parameters and returns a structured embedded message marked source:embedded, without inventing a local original-message ID.
