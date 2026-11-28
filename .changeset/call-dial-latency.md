---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
"@agent-wechat/rust-crates": minor
---

Reduce direct-call setup and media-startup latency by caching lease-fenced `AppSession` identities per account, deferring cold-call `AppAuth` until after invite submission, and reusing ringing-phase UDP relay pre-connection for outgoing calls as well as incoming calls.
