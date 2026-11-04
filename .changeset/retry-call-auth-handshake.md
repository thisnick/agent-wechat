---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

Retry the connection handshake once if a cryptographic error occurs before sending the call authentication request. Keep the original handshake timeout and send the call request only after a verified connection.

Keep the OpenClaw call listener available for the next caller when the daemon withdraws an unanswered offer.

Keep the voice connection active with paced silence when the speech model has no audio ready. Track how much of each reply played so an interruption can discard the rest. Let the voice model acknowledge a consultation while the OpenClaw agent works.

Run every call consultation as a normal session turn of the selected agent, so it uses the agent's model, CLI runtime and fallbacks. Callers other than the verified owner stay limited to read-only tools. Remove `calls.voice.consultation.model`; delete it from your configuration if you set it.

Log a warning when an incoming call is not answered, naming the step that failed. Also warn when a call ends after a failure, when calling stops until the channel restarts, and when an incoming call is ignored for that reason.

Raise the default call limit (`maxDurationSeconds`) from 5 minutes to 1 hour, and log a warning when the limit ends a call. The native daemon still ends calls after 20 minutes.

Keep calling available when the plugin ends a call after a failure and the WeChat hangup takes longer than two seconds. Previously the plugin treated the slow hangup as an unknown outcome and stopped answering calls until the channel restarted.
