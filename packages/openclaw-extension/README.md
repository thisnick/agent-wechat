# Native and legacy WeChat backends

The plugin keeps the existing container backend as its default. Native mode is an explicit opt-in and uses an independently provisioned native `wx` or an existing authenticated native daemon. Plugin installation/startup never downloads, installs or repairs a binary. Capabilities are channel-global: mixed native/legacy accounts under one registered channel are unsupported in this increment, and changing backend requires a Gateway/plugin reload. Native registration advertises media unavailable. The native daemon and API capabilities are under active validation; do not infer media or profile compatibility from a configured route.

## Native external mode

Configure plugin defaults under `plugins.entries.agent-wechat.config` (or the same `backend`/`native` keys in the channel configuration):

```json
{
  "backend": "native",
  "native": {
    "mode": "external",
    "accountId": "example",
    "endpoint": "http://127.0.0.1:6174",
    "tokenFile": "/private/native-api-token",
    "consumerStateDir": "/private/openclaw-native-consumer",
    "readPolicy": "on_model_dispatch"
  }
}
```

The external service remains independently owned. Plugin stop closes its stream and never stops/logs out the daemon or cancels its sends. Remote endpoints require verified HTTPS or an encrypted tunnel terminating at loopback. Token files must be private regular files (0600), contain at least 32 bytes, and are references rather than inline credentials. The API token needs read/send/login for the selected account, plus read-state only when automatic local read projection is enabled; do not grant admin solely to run the plugin. Normal sender/DM/group/mention policy remains in `channels.agent-wechat`; defaults remain closed.

## Native managed mode

```json
{
  "backend": "native",
  "native": {
    "mode": "managed",
    "accountId": "example",
    "executable": "/opt/agent-wechat/bin/wx",
    "listen": "127.0.0.1:6174",
    "dataDir": "/private/native-store",
    "tokenFile": "/private/native-api-token",
    "consumerStateDir": "/private/openclaw-native-consumer",
    "readPolicy": "never"
  }
}
```

Provision the native executable and account registration separately. Omitting `executable` resolves `wx` once from absolute trusted PATH entries. The plugin supports the package's tested npm launcher layout by selecting its already installed checksum-verified ELF payload directly. It never invokes an installer or shell. Unknown wrappers/old container CLIs are rejected. Managed startup refuses an occupied port and requires authenticated readiness matching the owned child PID, store path, server identity and process generation; it never adopts or terminates another process. Stop sends a graceful termination to its own child with bounded escalation. An unexpected child/stream failure is reported; automatic process crash restart is not implemented in this increment.

Keep `dataDir` and `consumerStateDir` on separate explicit persistent mounts. The daemon owns official-format profile/sidecar data; the consumer directory holds private intake/deduplication/model-handoff/read receipts bound to server and account identities. Executable upgrades do not remove either. One configured OpenClaw account maps to one selected native daemon account in this increment, while the daemon itself supports independent account slots.

## Native messaging lifecycle and validation limits

The adapter consumes account-scoped SSE using bearer headers and decimal-string journal cursors. It persists custody before acknowledging a cursor; acknowledgement is independent of unread state. Only an explicitly live, incoming, committed type-1 text resource can enter the model. Catch-up/import/raw pending sync batches, unknown origin, outgoing messages and unsupported media do not become automatic model inputs. Group admission uses structured exact self-mention facts; unknown metadata or displayed `@names` never satisfy a mention gate. Contacts and groups use their separate catalogs.

The prepared-run callback synchronously writes a bounded durable receipt for the exact selected new message IDs before a separate retryable read projection. Read CAS retries preserve the same sparse IDs and never widen to the largest ID or clear concurrent arrivals. `readPolicy: "never"` disables automatic reads. Crashes during an unconfirmed dispatch retain `needs_review`; the plugin does not automatically duplicate model turns or uncertain outgoing sends.

**Released SDK limitation:** inspected OpenClaw2026.10.2 and2026.10.5 do not expose `onPreparedAgentRunStart`. The older `onAgentRunStart` can also report direct tool execution and early lifecycle phases, so it is not used as a substitute. On those hosts native text dispatch can run, but automatic read projection remains disabled and status reports `handoffUnconfirmed`. The prepared callback and failure/CAS behavior are validated in a model-runtime harness, not a live hosted model. A compatible released SDK and real model handoff remain rollout gates.

Native text sends retain server-side intent/idempotency and returned send IDs. Native media/voice send and inbound attachment forwarding are not implemented in this adapter; they fail explicitly with no legacy fallback. Native QR login uses a durable daemon operation and authenticated PNG retrieval. The channel logout hook and native login tool perform persistent local logout fencing; they preserve credential files and do not claim server-side revocation. Explicit disconnect instead retains reusable trust. The legacy login transport is not used for a native account.

See [native deployment examples](../../deploy/native/README.md) and [native CLI installation](../cli/README.md). The following guide describes the legacy container backend.

---

# @agent-wechat/agent-wechat

OpenClaw channel plugin for WeChat. Connects your OpenClaw bot to WeChat using [agent-wechat](https://github.com/thisnick/agent-wechat).

**[Documentation](https://thisnick.github.io/agent-wechat/integrations/openclaw/setup/)**

## Prerequisites

- **A WeChat account logged in on your phone** — This account will be used by the bot. You must keep it logged in on your phone at all times. If you log out on the phone, the bot loses its connection.

- **A second screen** — WeChat login requires scanning a QR code with your phone camera. You'll need the QR code displayed on a screen separate from the phone running WeChat (e.g., a computer monitor, tablet, or another phone).

- **An agent-wechat server** — Either self-hosted via Docker or provided by someone else. You'll need the server URL and authentication token.

> **Note:** The agent-wechat container requires `SYS_PTRACE` and `seccomp=unconfined` (ptrace access to the WeChat desktop process). It cannot run in serverless or restricted container environments (AWS Fargate, Cloud Run, etc.) — use a VM or bare-metal Docker host.

## Setup

### 1. Start the agent-wechat Server

If you need to run the server yourself:

**Option A: CLI** (quickest for local use)

```bash
npm install -g @agent-wechat/cli
wx-legacy up
```

**Option B: Docker Compose** (production / networked)

```yaml
services:
  agent-wechat:
    image: ghcr.io/thisnick/agent-wechat:latest
    container_name: agent-wechat
    security_opt:
      - seccomp=unconfined
    cap_add:
      - SYS_PTRACE
    ports:
      - "6174:6174"
    volumes:
      - agent-wechat-data:/data
      - agent-wechat-home:/home/wechat
      - ~/.config/agent-wechat/token:/data/auth-token:ro
    restart: unless-stopped

volumes:
  agent-wechat-data:
  agent-wechat-home:
```

Generate a token before starting:

```bash
mkdir -p ~/.config/agent-wechat
openssl rand -hex 32 > ~/.config/agent-wechat/token
chmod 600 ~/.config/agent-wechat/token
docker compose up -d
```

If running alongside OpenClaw on the same Docker network, set `serverUrl` to `http://agent-wechat:6174` in the channel config.

### 2. Install the extension

```bash
openclaw plugins install @agent-wechat/agent-wechat
```

### 3. Configure the channel

```bash
# Uses defaults (localhost:6174, token from ~/.config/agent-wechat/token)
openclaw channels add --channel agent-wechat

# Override server URL and token
openclaw channels add --channel agent-wechat --url <url> --token <token>
```

Or edit `~/.openclaw/openclaw.json` directly:

```json
{
  "channels": {
    "agent-wechat": {
      "enabled": true,
      "serverUrl": "http://localhost:6174",
      "dmPolicy": "open",
      "allowFrom": ["*"],
      "groupPolicy": "open"
    }
  }
}
```

> **Upgrading from ≤0.11.x?** The channel id changed from `wechat` to `agent-wechat` (OpenClaw's official plugin registry now reserves `wechat`/`weixin` as aliases of Tencent's `openclaw-weixin` plugin, which hijacked this channel's id). Rename the `channels.wechat` key to `channels.agent-wechat` and any `plugins.entries.wechat` key to `plugins.entries.agent-wechat` in `openclaw.json`, then restart the gateway. Allowlist entries and group settings carry over unchanged.

For local setups, the token is automatically read from `~/.config/agent-wechat/token` (shared with the CLI and container), so you don't need to set it in the config. When connecting to a remote server, add the `token` field.

### 4. Restart the gateway

Restart your OpenClaw gateway so it picks up the new channel config:

```bash
openclaw gateway restart
```

### 5. Log in to WeChat

Ask your bot to log in to WeChat:

> "Log in to WeChat"

Your bot should generate a QR code image. Alternatively, use the CLI:

```bash
openclaw channels login --channel agent-wechat
```

### 6. Scan the QR code

Display the QR code on a screen separate from the phone running WeChat. WeChat's login QR scanner uses the camera only — it cannot scan from the phone's photo gallery.

Scan the QR code using WeChat's built-in scanner (tap **+** > **Scan**) with the account you want the bot to use. Then tap **Login** on the confirmation screen.

You only need to do this once — the session persists across container restarts.

### 7. Configure DM and group policies

Once connected, configure how the bot handles direct messages and group chats. You can ask your bot to help you, or edit the config directly. See the Configuration Reference below.

## Limitations

- **The WeChat account must stay logged in on your phone.** Logging out on the phone disconnects the bot. One workaround: after the bot is logged in, you can uninstall and reinstall WeChat on your phone — the bot session persists. On Android, you can also run WeChat in a separate profile (Work Profile or Private Space) to keep the bot account isolated.

- **Only one desktop session.** Once the bot is logged in as a "desktop" client, you cannot simultaneously use WeChat on another computer or WeChat Web.

- **Infrastructure updates disconnect the bot.** If the agent-wechat server restarts or updates, the bot will be disconnected. When this happens, ask your bot to log in again. If the disconnection was brief, you may not need a new QR code scan.

## Configuration Reference

All config lives under `channels.agent-wechat` in OpenClaw's config file:

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | boolean | `false` | Enable the WeChat channel |
| `serverUrl` | string | — | agent-wechat REST API URL |
| `token` | string | — | Auth token (auto-read from `~/.config/agent-wechat/token` for local setups) |
| `dmPolicy` | `"open" \| "allowlist" \| "disabled"` | `"disabled"` | Who can DM the bot |
| `allowFrom` | string[] | `[]` | DM senders and command owners: `wxid_...`, `"*"` for everyone, or `accessGroup:<name>`. With `dmPolicy: "open"`, use `["*"]` |
| `groupPolicy` | `"open" \| "allowlist" \| "disabled"` | `"disabled"` | Group message policy |
| `groupAllowFrom` | string[] | `[]` | Group senders who pass `allowlist` and may run commands in groups; falls back to `allowFrom` when empty |
| `groups` | object | `{}` | Per-group overrides (e.g. `{ "id@chatroom": { "requireMention": false, "enabled": true, "groupPolicy": "allowlist", "allowFrom": ["wxid_..."] } }`) |
| `pollIntervalMs` | integer | `1000` | Message polling interval |
| `authPollIntervalMs` | integer | `30000` | Auth status check interval |

Access decisions (DM and group senders, per-group overrides, control commands, @mention
gating) are made by OpenClaw's channel ingress policy, so `accessGroup:<name>` entries from
the top-level `accessGroups` config work in every allowlist. The pairing store is not
used: WeChat has no pairing flow.

## Development

### Build from source

```bash
git clone https://github.com/thisnick/agent-wechat.git
cd agent-wechat
pnpm install && pnpm build
```

### Link for local development

```bash
openclaw plugins install -l ./packages/openclaw-extension
```

This symlinks the extension so changes are picked up without reinstalling. Rebuild with `pnpm build` after making changes, then restart the gateway.

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
