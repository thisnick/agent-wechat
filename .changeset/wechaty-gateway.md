---
"@agent-wechat/wechaty-puppet": minor
---

Add Wechaty gateway package and puppet improvements for gRPC service hosting.

- New `packages/wechaty-gateway/` wraps PuppetAgentWeChat as a standard Wechaty gRPC puppet service
- Snapshot message baseline on connect to prevent historical message replay
- Guard against double login crash on PuppetServer client reconnect
- Emit heartbeat every poll cycle to keep gRPC watchdog alive
- Clear unreads via openChat after processing messages
