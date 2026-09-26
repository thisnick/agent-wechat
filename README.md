# agent-wechat

A programmable WeChat interface. Controls a WeChat client running in a Docker container — receive and send messages, see chat heads, and more via API, CLI, Wechaty puppet, or OpenClaw plugin.

**[Documentation](https://thisnick.github.io/agent-wechat/)**

## Packages

| Package | npm | Description |
|---------|-----|-------------|
| [`@agent-wechat/cli`](./packages/cli) | [![npm](https://img.shields.io/npm/v/@agent-wechat/cli)](https://www.npmjs.com/package/@agent-wechat/cli) | CLI for managing the Docker container and interacting with WeChat |
| [`@agent-wechat/wechaty-puppet`](./packages/wechaty-puppet) | [![npm](https://img.shields.io/npm/v/@agent-wechat/wechaty-puppet)](https://www.npmjs.com/package/@agent-wechat/wechaty-puppet) | [Wechaty](https://wechaty.js.org) puppet for agent-wechat |
| [`@agent-wechat/agent-wechat`](./packages/openclaw-extension) | [![npm](https://img.shields.io/npm/v/@agent-wechat/agent-wechat)](https://www.npmjs.com/package/@agent-wechat/agent-wechat) | [OpenClaw](https://openclaw.ai) extension for AI agent integration |

## What It Does

- **Read** chats, messages, and media (images, voice, files) via REST API
- **Send** text messages, images, files, and voice notes
- **Login** via QR code displayed in your terminal
- **Monitor** for new messages through client polling

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

# Record audio as a WeChat voice note
wx messages send <chatId> --voice ./reply.mp3

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
| `wx auth status` | Show login status |
| `wx auth logout` | Log out of WeChat |
| `wx chats list` | List chats |
| `wx chats get <id>` | Show one chat |
| `wx chats open <id>` | Select a chat in WeChat |
| `wx find <name>` | Find chat by name |
| `wx contacts list` / `wx contacts find <name>` | List or find contacts |
| `wx messages list <id>` | List messages in a chat |
| `wx messages send <id> --text <msg>` | Send text message |
| `wx messages send <id> --image <file>` | Send image |
| `wx messages send <id> --file <file>` | Send a file attachment |
| `wx messages send <id> --voice <audio>` | Send audio as one or more voice notes and wait for verification |
| `wx messages send <id> --voice <audio> --detach` | Start a voice job and return its ID immediately |
| `wx messages voice status <jobId>` | Check voice-job progress and sent message IDs |
| `wx messages voice cancel <jobId>` | Request cancellation of a voice job |
| `wx messages media <id> <localId> [-o <path>] [--full\|--thumbnail]` | Save an attachment; images use the best available copy by default |

Run `wx --help` or `wx <command> --help` for all commands and options.

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
- **API**: REST endpoints for supported operations and a WebSocket login flow

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

See [AGENTS.md](./AGENTS.md) for contributor guidance and implementation pointers.

## Ports

| Port | Service |
|------|---------|
| 6174 | Agent server REST API + VNC web viewer at `/vnc/` |
