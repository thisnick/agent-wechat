---
"@agent-wechat/agent-wechat": minor
"@agent-wechat/cli": minor
"@agent-wechat/shared": minor
---

Return the requested image quality without silently substituting a thumbnail. The CLI requests full resolution by default; use --thumbnail for the cached preview. HTTP clients that omit quality retain the legacy thumbnail-first behavior.

Validate cached file attachments against their message's size and content hash before returning them, and reject titles containing filesystem paths.
