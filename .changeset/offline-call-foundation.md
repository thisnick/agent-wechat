---
"@agent-wechat/cli": minor
"@agent-wechat/agent-wechat": minor
---

Add disabled-by-default direct voice calling: authenticated control/media WebSockets, single-call ownership, PCM SDK connections, CLI monitor/dial/answer controls, and an optional OpenClaw OpenAI audio bridge with separate calling allowlists. Add the opt-in native worker and internal call-media/audio libraries. Use one `calls` API token scope for call state, control and owner-bound audio; messaging grants do not permit calls. Native phone validation and media security lifetime checks remain incomplete; the native worker is restricted to expiring isolated tests. Video and conference calling remain unavailable.
