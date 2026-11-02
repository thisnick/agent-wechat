---
"@agent-wechat/cli": minor
"@agent-wechat/agent-wechat": minor
---

Add disabled-by-default direct voice calling: authenticated control/media WebSockets, single-call ownership, PCM SDK connections, CLI monitor/dial/answer controls, and an optional OpenClaw OpenAI audio bridge with separate calling allowlists. Add the opt-in native worker and internal call-media/audio libraries. Expose direct/conference kind and voice/video call type in call metadata, including same-account observer snapshots without ownership or audio access. Select call type when dialing and accept offers by identity/revision; remove redundant media flags and the unimplemented resume command. Use one `calls` API token scope for call state, control and owner-bound audio; messaging grants do not permit calls. Isolated live tests cover personal voice negotiation, duplex PCM and terminal handling. Media security lifetime and physical device validation remain incomplete; the native worker is restricted to expiring isolated tests. Video and conference calling remain unavailable.
