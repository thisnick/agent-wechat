---
"@agent-wechat/agent-wechat": patch
---

Add `calls.voice.realtime.fastContext`, off by default, matching OpenClaw's phone plugin. When enabled, each voice-call consultation first searches the agent's indexed memory and session context and answers from matching snippets without running the agent. Set `fallbackToConsult: true` to run the agent when nothing is found. Matching snippets also cut short owner actions such as hang-up.
