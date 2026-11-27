---
"@agent-wechat/cli": minor
"@agent-wechat/agent-wechat": minor
"@agent-wechat/desktop": minor
"@agent-wechat/rust-crates": minor
---

Add headless, automation-friendly `wx calls session` commands with synthetic audio/video sources, live FFT and video metrics, frame snapshots, WAV/video recording, and event/media wait predicates. Emit direct-call `call.peer_status` signaling updates on Calls Protocol v3, propagate direct voice call mute/unmute state over `SwitchAV`, and ignore unknown future post-handshake call server event types in Rust and TypeScript clients.

Because `call.peer_status` is emitted on protocol v3 and older released clients disconnect on unknown event types, upgrade call clients (`wx`, desktop app, OpenClaw plugin, and `@agent-wechat/shared` SDK consumers) together with the daemon.
