---
"@agent-wechat/cli": minor
"@agent-wechat/agent-wechat": minor
"@agent-wechat/desktop": minor
"@agent-wechat/rust-crates": minor
---

Add headless, automation-friendly `wx calls session` commands with synthetic audio/video sources, live FFT and video metrics, frame snapshots, WAV/video recording, and event/media wait predicates. Negotiate Calls Protocol v4 on `/calls/socket` (with backward-compatible v3 fallback) to emit direct-call `call.peer_status` signaling updates, propagate direct voice call mute/unmute state over `SwitchAV`, and ignore unknown future post-handshake call server event types in Rust and TypeScript clients.
