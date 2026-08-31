---
"@agent-wechat/agent-wechat": minor
---

Rename the OpenClaw plugin/channel id from `wechat` to `agent-wechat` and restore compatibility with OpenClaw 2026.8.x.

OpenClaw's bundled official plugin catalog now reserves `wechat` (and `weixin`) as aliases of Tencent's `@tencent-weixin/openclaw-weixin` plugin, and the catalog is compiled into the openclaw JS bundle — so the id `wechat` gets hijacked: `plugins install` writes `plugins.entries.openclaw-weixin`, and `channels add --channel wechat` tries to install the Tencent plugin instead of this one. Patching `dist/channel-catalog.json` no longer helps.

Changes:

- Plugin id and channel id are now `agent-wechat`; config lives under `channels.agent-wechat`.
- Dropped the `weixin` alias (catalog-reserved).
- Imports moved from the removed bare `openclaw/plugin-sdk` export to `openclaw/plugin-sdk/core` (works on hosts >=2026.5.12, required on 2026.8.x).
- `channelConfigs` metadata added to `package.json#openclaw` so 2026.8.x setup surfaces get the config schema.
- `wechat:`-prefixed targets and allowlist entries are still accepted.

Migration: rename the `channels.wechat` key to `channels.agent-wechat` and `plugins.entries.wechat` (or a stray `plugins.entries.openclaw-weixin`) to `plugins.entries.agent-wechat` in `openclaw.json`, then restart the gateway. Session/routing keys change with the channel id, so active conversation sessions reset.
