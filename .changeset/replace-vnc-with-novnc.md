---
"@agent-wechat/cli": minor
---

Replace raw VNC port (5900) with noVNC browser-based viewer on port 6080. x11vnc now listens on 127.0.0.1 only (internal to the container), and websockify serves the noVNC web client. Access the desktop at `http://localhost:6080/vnc.html?autoconnect=true`. No VNC client installation needed.
