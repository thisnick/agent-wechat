# @agent-wechat/wechat

OpenClaw channel plugin for WeChat. Connects your OpenClaw bot to WeChat using [agent-wechat](https://github.com/thisnick/agent-wechat).

## Prerequisites

- **A WeChat account logged in on your phone** — This account will be used by the bot. You must keep it logged in on your phone at all times. If you log out on the phone, the bot loses its connection.

- **A second screen** — WeChat login requires scanning a QR code with your phone camera. You'll need the QR code displayed on a screen separate from the phone running WeChat (e.g., a computer monitor, tablet, or another phone).

- **An agent-wechat server** — Either self-hosted via Docker or provided by someone else. You'll need the server URL and authentication token.

> **Note:** The agent-wechat container requires `SYS_PTRACE` and `seccomp=unconfined` (ptrace access to the WeChat desktop process). It cannot run in serverless or restricted container environments (AWS Fargate, Cloud Run, etc.) — use a VM or bare-metal Docker host.

## Setup

### 1. Install the extension

```bash
openclaw plugins install @agent-wechat/wechat
```

### 2. Configure the channel

```bash
# Uses defaults (localhost:6174, token from ~/.config/agent-wechat/token)
openclaw channels add --channel wechat

# Override server URL and token
openclaw channels add --channel wechat --url <url> --token <token>
```

Or edit `~/.openclaw/openclaw.json` directly:

```json
{
  "channels": {
    "wechat": {
      "enabled": true,
      "serverUrl": "http://localhost:6174",
      "dmPolicy": "open",
      "groupPolicy": "open"
    }
  }
}
```

For local setups, the token is automatically read from `~/.config/agent-wechat/token` (shared with the CLI and container), so you don't need to set it in the config. When connecting to a remote server, add the `token` field.

### 3. Restart the gateway

Restart your OpenClaw gateway so it picks up the new channel config:

```bash
openclaw gateway restart
```

### 4. Log in to WeChat

Ask your bot to log in to WeChat:

> "Log in to WeChat"

Your bot should generate a QR code image. Alternatively, use the CLI:

```bash
openclaw channels login --channel wechat
```

### 5. Scan the QR code

Display the QR code on a screen separate from the phone running WeChat. WeChat's login QR scanner uses the camera only — it cannot scan from the phone's photo gallery.

Scan the QR code using WeChat's built-in scanner (tap **+** > **Scan**) with the account you want the bot to use. Then tap **Login** on the confirmation screen.

You only need to do this once — the session persists across container restarts.

### 6. Configure DM and group policies

Once connected, configure how the bot handles direct messages and group chats. You can ask your bot to help you, or edit the config directly. See the Configuration Reference below.

## Limitations

- **The WeChat account must stay logged in on your phone.** Logging out on the phone disconnects the bot. One workaround: after the bot is logged in, you can uninstall and reinstall WeChat on your phone — the bot session persists. On Android, you can also run WeChat in a separate profile (Work Profile or Private Space) to keep the bot account isolated.

- **Only one desktop session.** Once the bot is logged in as a "desktop" client, you cannot simultaneously use WeChat on another computer or WeChat Web.

- **Infrastructure updates disconnect the bot.** If the agent-wechat server restarts or updates, the bot will be disconnected. When this happens, ask your bot to log in again. If the disconnection was brief, you may not need a new QR code scan.

## Starting the agent-wechat Server

If you need to run the server yourself:

**Option A: CLI** (quickest for local use)

```bash
npm install -g @agent-wechat/cli
wx up
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
      - "127.0.0.1:5900:5900"
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

## Configuration Reference

All config lives under `channels.wechat` in OpenClaw's config file:

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | boolean | `false` | Enable the WeChat channel |
| `serverUrl` | string | — | agent-wechat REST API URL |
| `token` | string | — | Auth token (auto-read from `~/.config/agent-wechat/token` for local setups) |
| `dmPolicy` | `"open" \| "allowlist" \| "disabled"` | `"disabled"` | Who can DM the bot |
| `allowFrom` | string[] | `[]` | wxid allowlist for DMs (when policy is `allowlist`) |
| `groupPolicy` | `"open" \| "allowlist" \| "disabled"` | `"disabled"` | Group message policy |
| `groupAllowFrom` | string[] | `[]` | Global allowlist of group sender IDs (`wxid_...`) |
| `groups` | object | `{}` | Per-group overrides (e.g. `{ "id@chatroom": { "requireMention": false, "enabled": true, "groupPolicy": "allowlist", "allowFrom": ["wxid_..."] } }`) |
| `pollIntervalMs` | integer | `1000` | Message polling interval |
| `authPollIntervalMs` | integer | `30000` | Auth status check interval |

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
