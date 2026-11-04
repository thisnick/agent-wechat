---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

Retry the connection handshake once if a cryptographic error occurs before sending the call authentication request. Keep the original handshake timeout and send the call request only after a verified connection.

Keep the OpenClaw call listener available for the next caller when the daemon withdraws an unanswered offer.
