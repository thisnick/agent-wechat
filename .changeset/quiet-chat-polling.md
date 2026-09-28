---
"@agent-wechat/agent-wechat": patch
"@agent-wechat/shared": patch
---

Keep slow media retrieval in one chat from blocking other inbound chats. Bound direct and group processing independently, serialize monitor state writes, share a three-request media budget across preparation and refresh, and cancel monitor HTTP requests and retry waits on shutdown.
