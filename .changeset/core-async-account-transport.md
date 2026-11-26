---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
---

The daemon's account requests, QR and saved-device login (including LAN risk challenges), session renewal, logout, long-link notifications and call signaling now use the native async transport (Tokio sockets and a pooled HTTP/1.1 client) instead of blocking sockets. Request, receipt and retry behaviour is unchanged.
