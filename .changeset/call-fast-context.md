---
"@agent-wechat/agent-wechat": patch
---

Add `calls.voice.realtime.fastContext`, off by default, based on OpenClaw's phone plugin. When enabled, consultations on the verified owner's direct calls first search the agent's indexed memory and session context and answer from matching snippets without running the agent. The next consultation in the call runs the agent, so asking again gets past snippets that did not answer or a request that needs an action. Other callers and conferences always use the agent. Set `fallbackToConsult: true` to run the agent when nothing is found.
