---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
---

Shorten the interactive call prompts by removing the audio device startup message and the microphone status from the waiting prompt. Show which call controls require Enter and let users redisplay the keys with `?`.

Set `WX_CALL_DEBUG=1` to trace call events, audio frame writes and WebSocket closure details on stderr.
