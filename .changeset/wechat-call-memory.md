---
"@agent-wechat/agent-wechat": minor
---

WeChat voice calls now remember earlier calls with each contact. Consultations for a contact share one OpenClaw session (`calls.voice.sessionScope: "per-contact"`, the new default; `per-call` keeps the old behavior). After an answered call, the agent summarizes the transcript in that session (`consultation.summary`, on by default) and the summary is left as a note in your WeChat chat with the caller; calls placed with `wechat_call` also report it to the session that placed them. A call's first consultation gets the latest messages of that chat (`consultation.recentMessages`, 10 by default). Set `realtime.recentContext.enabled` to start each call with the last few summaries and the latest messages of the chat with the caller.
