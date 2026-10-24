---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

Use only the quoted content embedded in replies delivered by WeChat's server protocol. Include parsed quote text, link-card fields, image availability and sender metadata in normal message responses without substituting a stored original. Remove `GET /messages/{id}/quote`, the SDK's `quotedMessage` method and the separate quote-resolution response types; read quotes through the normal message endpoint and message lists. Keep local recall checks and quoted-author admission. Retrieve images using quote-owned metadata; preserve comments and unavailable status for other quoted media. Sending quoted replies continues through the normal send endpoint with the stored target's message ID and server ID.
