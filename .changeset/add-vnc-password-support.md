---
"@agent-wechat/cli": patch
---

Secure noVNC with full-token auth on the WebSocket proxy (no 8-char VNC limit). Opening /vnc/ shows a login prompt for your token. Direct access via ?token=xxx&autoconnect=true also works. VNC and websockify listen on localhost only.
