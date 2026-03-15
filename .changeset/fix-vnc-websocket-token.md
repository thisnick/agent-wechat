---
"@agent-wechat/agent-server": patch
---

Fix VNC WebSocket auth: keep token embedded in the noVNC `path` query param so it is passed to the WebSocket connection, and remove it from the visible URL for security
