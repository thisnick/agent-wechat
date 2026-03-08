---
"@agent-wechat/cli": patch
---

Add VNC password support via VNC_PASSWORD environment variable. When set, noVNC will prompt for a password before connecting. Without it, behavior is unchanged (no password).
