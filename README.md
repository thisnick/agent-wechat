# agent-wechat

WeChat automation via deterministic FSM. Runs WeChat in a Docker container with UI automation.

## Install (Published CLI)

```bash
npm i -g @agent-wechat/cli
wx up
```

By default, `wx up` uses local images when available (`agent-wechat:arm64` / `agent-wechat:amd64`), then falls back to `ghcr.io/agent-wechat/agent-wechat:latest`.

Override image selection with:

- `wx up --image <image-ref>`
- `AGENT_WECHAT_IMAGE`
- `AGENT_WECHAT_IMAGE_REPO`
- `AGENT_WECHAT_IMAGE_TAG`

## Requirements

- Docker (via Colima on macOS, or Docker Desktop)
- Node.js >= 22 (for CLI)
- pnpm

## Quick Start

```bash
# Install dependencies
pnpm install

# Build CLI + shared types
pnpm build

# Build Docker image (choose your arch)
pnpm build:image:arm64   # Apple Silicon
pnpm build:image:amd64   # Intel

# Start container
pnpm cli up

# Connect VNC to see WeChat UI (optional)
# Open VNC viewer → localhost:5900

# Check status
pnpm cli status

# Login (shows QR code in terminal)
pnpm cli auth login

# List chats
pnpm cli chats list

# Stop container
pnpm cli down
```

## CLI Commands

| Command | Description |
|---------|-------------|
| `pnpm cli up` | Start the WeChat container |
| `pnpm cli down` | Stop and remove container |
| `pnpm cli logs` | Stream container logs |
| `pnpm cli status` | Show server and login status |
| `pnpm cli auth login` | Login flow (shows QR code) |
| `pnpm cli chats list` | List chats |
| `pnpm cli find <name>` | Find chat by name |
| `pnpm cli messages list <id>` | List messages |
| `pnpm cli messages send <id> --text <msg>` | Send message to chat |
| `pnpm cli messages media <id> <localId>` | Download media attachment |

## Architecture

- **Container**: WeChat + Xvfb + agent-server (Rust/Axum)
- **FSM Engine**: Deterministic state machine for UI automation
- **CLI**: HTTP/WebSocket client that talks to agent-server

See [CLAUDE.md](./CLAUDE.md) for technical details.

## Development

```bash
# Build CLI + shared types
pnpm build

# Deploy Rust server changes to running container
pnpm dev:deploy

# Type check Rust code
cd packages/agent-server-rust && cargo check
```

## OpenClaw Extension

Install the plugin with:

```bash
openclaw plugins install @agent-wechat/wechat
openclaw plugins enable wechat
```

For release instructions, see `docs/release.md`.

## Ports

- `6174` - Agent server API
- `5900` - VNC (view WeChat UI)
