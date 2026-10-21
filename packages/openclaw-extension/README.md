# Native and legacy WeChat backends

The plugin keeps the existing container backend as its default. Native mode is an explicit opt-in and uses an independently provisioned native `wx` or an existing authenticated native daemon. Plugin installation/startup never downloads, installs or repairs a binary. Capabilities are channel-global: mixed native/legacy accounts under one registered channel are unsupported in this increment, and changing backend requires a Gateway/plugin reload. Native registration advertises media support through verified daemon assets/jobs. The native daemon and API capabilities are under active validation; do not infer media or profile compatibility from a configured route.

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

The external service remains independently owned. Plugin stop closes its stream and never stops/logs out the daemon or cancels its sends. Remote endpoints require verified HTTPS or an encrypted tunnel terminating at loopback. Token files must be private regular files (0600), contain at least 32 bytes, and are references rather than inline credentials. The API token needs read/media/send/login for the selected account, plus read-state only when automatic local read projection is enabled; do not grant admin solely to run the plugin. Normal sender/DM/group/mention policy remains in `channels.agent-wechat`; defaults remain closed.

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

The adapter consumes account-scoped SSE using bearer headers and decimal-string journal cursors. It persists custody before acknowledging a cursor; acknowledgement is independent of unread state. Only explicitly live, incoming, committed resources can enter the model. Supported images, short voice, video, files and eligible stickers pass through bounded media retrieval and hash verification into OpenClaw-managed storage. Pending media retains custody without model dispatch or a read receipt. Expired/unsupported attachments retain an explicit status; raw XML never becomes an attachment. Catch-up/import/raw pending sync batches, unknown origin and outgoing messages do not become automatic model inputs. Group admission uses structured exact self-mention facts; unknown metadata or displayed `@names` never satisfy a mention gate. Contacts and groups use their separate catalogs.

The prepared-run callback synchronously writes a bounded durable receipt for the exact selected new message IDs before a separate retryable read projection. Read CAS retries preserve the same sparse IDs and never widen to the largest ID or clear concurrent arrivals. `readPolicy: "never"` disables automatic reads. Crashes during an unconfirmed dispatch retain `needs_review`; the plugin does not automatically duplicate model turns or uncertain outgoing sends.

OpenClaw **2026.10.2** lacks `onPreparedAgentRunStart`, so its released public hooks provide a bounded compatibility path. Before dispatch, the adapter persists an immutable manifest binding the exact new message ID/version/content hash, policy hash, source and supplied asset hashes, and context roles. Text requires the exact current input and host-created message/account/session facts plus a matching `model_call_started`. Media additionally waits for the same run's `agent_end` snapshot and source-turn identity. Image bytes/MIME/length must match the frozen supplied representation; file/video/voice references must appear in the exact current body. Image count, model selection and reply delivery alone cannot mark a message read. Unknown, omitted, rewritten, merged or ambiguous inputs remain unconfirmed.

The compatibility path requires the operator's explicit host permission, `plugins.entries.agent-wechat.hooks.allowConversationAccess: true`. It observes only run IDs issued by this adapter and never enables that permission itself. Without the grant, or on an unsupported host, messages remain unread and status reports `handoffUnconfirmed`. The older `onAgentRunStart` also reports direct tools and early lifecycle phases and is never used as proof. `readPolicy: "never"` preserves the handoff receipt while disabling read projection. Late confirmed receipts retry their exact sparse reads independently of new SSE traffic.

Images exceeding the host's bounded representation, or formats requiring conversion, receive a separately stored JPEG still preview using the public SDK image processor. The original stays intact; both hashes are recorded and the model input labels the preview. Prepared images fit within 5 MiB and at most 1200 pixels per side (or a smaller configured limit). Files and videos can be supplied as available local references; this does not claim their contents were analyzed. Voice retains its verified file and includes configured transcription output or an explicit unavailable status. Pending, expired, unsupported and failed-integrity media never gains a read receipt from a placeholder.

Run `node scripts/native-model-smoke.mjs` and `node scripts/native-media-model-smoke.mjs` after installing workspace dependencies. They use the actual pinned Gateway, a local model and synthetic daemon/files. Checks cover current provider input, image hashes and prepared previews, reference/transcription delivery, exact sparse reads, lost-read-response/reopen recovery, duplicate prevention, omitted images on a text-only model, permission, never-read policy and group routing behavior. Transcription uses the real SDK with an explicitly configured local deterministic CLI; it does not test speech recognition accuracy. Concurrent text inputs were serialized into distinct model turns; merged-input refusal has unit coverage. These checks establish the prepared runtime boundary, not external provider acceptance, semantic analysis or consumption.

Native text/media sends retain daemon intent/job identity and caller idempotency. Text waits for accepted submission; media jobs preserve partial and uncertain outcomes rather than resending. Ordinary audio remains a file; only explicit `audioAsVoice` requests independently encoded voice. Sender/group/mention gates run before retrieval/transcription. Verified voice may use the configured OpenClaw transcription runtime after admission. Native QR login uses a durable daemon operation and authenticated PNG retrieval. Logout commits local inhibition before at most one authenticated native logout, reporting the remote outcome separately; saved-device revocation remains unverified. Explicitly confirmed forgetting removes local auth while retaining history; disconnect retains reusable trust.

## Optional Gateway facade

Set `native.gatewayFacade: {"enabled": true}` to register `/plugins/agent-wechat/native` on the Gateway. The default is disabled. A custom prefix must remain namespaced under `/plugins/...`; changing backend/configuration requires a Gateway reload.

The actual Gateway authenticates requests before the proxy runs. Its bearer is never forwarded to the daemon; the plugin uses its private daemon token reference. Routes are restricted to the configured account, with no daemon-wide token/account administration. Binary bodies, byte ranges, status codes and SSE are preserved within explicit size limits. Client disconnect/plugin shutdown closes upstream streaming work without cancelling durable sends.

`scripts/native-gateway-smoke.mjs` tests the built plugin on an isolated OpenClaw **2026.10.2** Gateway with synthetic loopback data: auth rejection, separate credentials, account boundaries, Range206, SSE and cancellation. This transport test is separate from the local model-boundary test above; neither uses an external model provider or a real WeChat account.

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
