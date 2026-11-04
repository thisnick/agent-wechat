---
"@agent-wechat/agent-wechat": minor
---

WeChat voice calls now remember earlier calls with each contact. Consultations for a contact share one OpenClaw session (`calls.voice.sessionScope: "per-contact"`, the new default; `per-call` keeps the old behavior; `chat` shares the caller's WeChat chat session). After an answered call, the agent summarizes the transcript in that session (`consultation.summary`, on by default); calls placed with `wechat_call` also report the summary to the session that placed them. Set `realtime.recentContext.enabled` to start each call with the last few summaries and the latest messages of the chat with the caller.
