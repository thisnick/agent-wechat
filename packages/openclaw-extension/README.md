# OpenClaw WeChat Extension

OpenClaw channel plugin for WeChat. Polls the agent-wechat REST API for inbound messages and dispatches replies through OpenClaw's agent runtime.

## Prerequisites

- A running agent-wechat container (provides the REST API on port 6174)
- OpenClaw installed and configured

## Install From npm

```bash
openclaw plugins install @agent-wechat/wechat
openclaw plugins enable wechat
```

## Development Setup

### 1. Build

From the repo root:

```bash
pnpm install
pnpm build
```

This builds the shared package, CLI, and bundles the extension into `dist/index.js` via esbuild.

### 2. Deploy to OpenClaw

```bash
pnpm deploy:openclaw
```

This copies `dist/index.js`, `package.json`, and `openclaw.plugin.json` into `../openclaw/extensions/wechat/` (sibling to the agent-wechat repo). To deploy to a different location:

```bash
pnpm deploy:openclaw /path/to/openclaw/extensions/wechat
```

### 3. Enable the plugin

```bash
openclaw plugins enable wechat
```

### 4. Configure the channel

```bash
openclaw channels add --channel wechat --url http://localhost:6174
```

Or edit `~/.openclaw/openclaw.json` directly:

```json
{
  "channels": {
    "wechat": {
      "enabled": true,
      "serverUrl": "http://localhost:6174"
    }
  },
  "plugins": {
    "entries": {
      "wechat": { "enabled": true }
    }
  }
}
```

### 5. Run the gateway

```bash
openclaw gateway run --verbose
```

The WeChat monitor starts polling the agent-wechat server for new messages. Make sure the agent-wechat container is running (`pnpm cli up` / `pnpm cli status`).

### Iterating

After making changes to the extension source:

```bash
pnpm deploy:openclaw   # rebuilds + copies
# restart the gateway
```

## Docker Setup

The extension is bundled into a single `dist/index.js` with no runtime dependency on the shared package. Mount just the three required files:

```bash
mkdir -p /host/openclaw-ext/wechat/dist
cp packages/openclaw-extension/dist/index.js    /host/openclaw-ext/wechat/dist/index.js
cp packages/openclaw-extension/package.json     /host/openclaw-ext/wechat/package.json
cp packages/openclaw-extension/openclaw.plugin.json /host/openclaw-ext/wechat/openclaw.plugin.json

docker run \
  -v /host/openclaw-ext/wechat:/app/extensions/wechat \
  openclaw-image
```

Inside the container, configure `channels.wechat.serverUrl` to point at the agent-wechat server. If both containers are on the same Docker network:

```json
{
  "channels": {
    "wechat": {
      "enabled": true,
      "serverUrl": "http://agent-wechat:6174"
    }
  }
}
```

## Configuration Reference

All config lives under `channels.wechat` in OpenClaw's config file:

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | boolean | `false` | Enable the WeChat channel |
| `serverUrl` | string | — | agent-wechat REST API URL |
| `dmPolicy` | `"open" \| "allowlist" \| "disabled"` | `"disabled"` | Who can DM the bot |
| `allowFrom` | string[] | `[]` | wxid allowlist for DMs (when policy is `allowlist`) |
| `groupPolicy` | `"open" \| "allowlist" \| "disabled"` | `"disabled"` | Group message policy |
| `groupAllowFrom` | string[] | `[]` | wxid allowlist for group senders |
| `groups` | object | `{}` | Per-group overrides (e.g. `{ "id@chatroom": { "requireMention": false } }`) |
| `pollIntervalMs` | integer | `1000` | Message polling interval |
| `authPollIntervalMs` | integer | `30000` | Auth status check interval |

## Architecture

```
OpenClaw Gateway
  └── WeChat Monitor (polling loop)
        │
        │  GET /api/chats          (list chats with unreads)
        │  POST /api/chats/{id}/open  (open chat, clear unreads)
        │  GET /api/messages/{id}  (fetch new messages)
        │  GET /api/messages/{id}/media/{localId}  (download media)
        │  POST /api/messages/send (send reply)
        │
        ▼
  agent-wechat container (port 6174)
        │
        ▼
  WeChat Desktop (in Xvfb)
```

The monitor polls for chats with unread messages, fetches new messages, resolves routing/session via OpenClaw's runtime, and dispatches replies back through the agent-wechat API.
