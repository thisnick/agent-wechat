---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

Retry the connection handshake once if a cryptographic error occurs before sending the call authentication request. Keep the original handshake timeout and send the call request only after a verified connection.

Keep the OpenClaw call listener available for the next caller when the daemon withdraws an unanswered offer.

Keep the voice connection active with paced silence when the speech model has no audio ready. Track how much of each reply played so an interruption can discard the rest. Let the voice model acknowledge a consultation while the OpenClaw agent works.

Run every call consultation as a normal session turn of the selected agent, so it uses the agent's model, CLI runtime and fallbacks. Callers other than the verified owner stay limited to read-only tools. Remove `calls.voice.consultation.model`; delete it from your configuration if you set it.
