---
"@agent-wechat/wechat": minor
---

Update for openclaw 2026.5+ compatibility:

- Add `channelConfigs` metadata to `openclaw.plugin.json` so the gateway can validate config and load setup surfaces before the plugin runtime imports (silences the "channel plugin manifest declares wechat without channelConfigs metadata" warning).
- Replace deprecated `runtime.config.loadConfig()` calls with `runtime.config.current()`.
- Add a `message` adapter via `createChannelMessageAdapterFromOutbound` from `openclaw/plugin-sdk/channel-message`. The legacy `outbound` adapter is kept for older openclaw versions.
- Bump the `openclaw` peer dependency floor to `^2026.5.12`.

The deprecated `outbound` adapter and `dispatchReplyWithBufferedBlockDispatcher` ingest flow continue to work via openclaw's compat shims; a follow-up release will migrate the monitor's dispatch path to `core.channel.turn.runPrepared(...)`.
