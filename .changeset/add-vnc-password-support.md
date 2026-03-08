---
"@agent-wechat/cli": patch
---

Secure noVNC with VNC password authentication. x11vnc now requires a password (set via VNC_PASSWORD or AGENT_WECHAT_TOKEN env var, min 6 chars). noVNC shows a password prompt on connect, or pass ?password=xxx in the URL. VNC and websockify listen on localhost only.
