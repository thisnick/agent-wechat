# agent-wechat

A programmable WeChat interface. Controls a WeChat client running in a Docker container — receive and send messages, see chat heads, and more via API, CLI, Wechaty puppet, or OpenClaw plugin.

## Packages

| Package | npm | Description |
|---------|-----|-------------|
| [`@agent-wechat/cli`](./packages/cli) | [![npm](https://img.shields.io/npm/v/@agent-wechat/cli)](https://www.npmjs.com/package/@agent-wechat/cli) | CLI for managing the Docker container and interacting with WeChat |
| [`@agent-wechat/wechaty-puppet`](./packages/wechaty-puppet) | [![npm](https://img.shields.io/npm/v/@agent-wechat/wechaty-puppet)](https://www.npmjs.com/package/@agent-wechat/wechaty-puppet) | [Wechaty](https://wechaty.js.org) puppet for agent-wechat |
| [`@agent-wechat/wechat`](./packages/openclaw-extension) | [![npm](https://img.shields.io/npm/v/@agent-wechat/wechat)](https://www.npmjs.com/package/@agent-wechat/wechat) | [OpenClaw](https://openclaw.ai) extension for AI agent integration |

## What It Does

- **Read** chats, messages, and media (images, voice, files) via REST API
- **Send** text messages, images, and files
- **Login** via QR code displayed in your terminal
- **Monitor** for new messages in real-time

## Requirements

- Docker (Colima on macOS, or Docker Desktop)
- Node.js >= 22 (for CLI)
- pnpm (for development)
- **Not compatible with serverless environments** — requires ptrace capabilities

## Quick Start

```bash
# Install the CLI
npm install -g @agent-wechat/cli

# Start the container (auto-pulls Docker image)
wx up

# Login (displays QR code in terminal)
wx auth login

# List your chats
wx chats list

# Send a message
wx messages send <chatId> --text "Hello"

# Read messages
wx messages list <chatId>

# Stop the container
wx down
```

## CLI Commands

| Command | Description |
|---------|-------------|
| `wx up [--proxy user:pass@host:port]` | Start the WeChat container (auto-pulls image) |
| `wx down` | Stop and remove container |
| `wx logs` | Stream container logs |
| `wx status` | Show server and login status |
| `wx auth login` | Login flow (shows QR code) |
| `wx chats list` | List chats |
| `wx find <name>` | Find chat by name |
| `wx messages list <id>` | List messages in a chat |
| `wx messages send <id> --text <msg>` | Send text message |
| `wx messages send <id> --image <file>` | Send image |
| `wx messages media <id> <localId>` | Download media attachment |

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                Docker Container                     │
│                                                     │
│   WeChat Linux  ←──  Xvfb + AT-SPI (accessibility)  │
│        ↕                                            │
│   agent-server (Rust/Axum, port 6174)               │
│     - FSM engine for UI automation                  │
│     - REST + WebSocket API                          │
└──────────────────────┬──────────────────────────────┘
                       │ HTTP / WebSocket
                       ↓
               CLI or AI agent
```

- **UI automation**: Login, open chats, send messages — all via deterministic FSM (no LLM needed)
- **API**: REST endpoints for all operations, WebSocket for login flow and events

## Docker Setup

**Option A: Via CLI** (recommended)

```bash
wx up    # auto-pulls ghcr.io/thisnick/agent-wechat
```

**Option B: Docker Compose** (for custom networking)

See [`docker-compose.yml`](./docker-compose.yml) for a full example. Key points:

```yaml
# Generate a token first:
#   mkdir -p ~/.config/agent-wechat
#   openssl rand -hex 32 > ~/.config/agent-wechat/token
#   chmod 600 ~/.config/agent-wechat/token

services:
  agent-wechat:
    image: ghcr.io/thisnick/agent-wechat:latest
    security_opt:
      - seccomp=unconfined
    cap_add:
      - SYS_PTRACE
      - NET_ADMIN           # for transparent proxy (optional)
    ports:
      - "6174:6174"
    volumes:
      - agent-wechat-data:/data
      - agent-wechat-home:/home/wechat
      - ~/.config/agent-wechat/token:/data/auth-token:ro
    environment:
      - PROXY=${PROXY:-}    # optional: user:pass@host:port
    restart: unless-stopped

volumes:
  agent-wechat-data:
  agent-wechat-home:
```

## Development

```bash
pnpm install
pnpm build                   # Build CLI + shared types
pnpm dev:deploy              # Cross-compile Rust server + deploy to running container
pnpm build:image:arm64       # Build Docker image (Apple Silicon)
pnpm build:image:amd64       # Build Docker image (Intel)
```

See [CLAUDE.md](./CLAUDE.md) for full technical documentation.

## Ports

| Port | Service |
|------|---------|
| 6174 | Agent server REST API + VNC web viewer at `/vnc/` |
