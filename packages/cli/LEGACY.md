# @agent-wechat/cli

Command-line tool for managing agent-wechat containers and interacting with WeChat.

**[Documentation](https://thisnick.github.io/agent-wechat/getting-started/cli/commands/)**

## Install

```bash
# Explicitly install the retained container client without a native payload.
AGENT_WECHAT_SKIP_NATIVE_INSTALL=1 npm install -g @agent-wechat/cli
wx-legacy --help
```

The current package also preserves the earlier container client as `wx-legacy`.

## Prerequisites

- [Docker](https://docs.docker.com/get-docker/) installed and running

`wx-legacy up` automatically pulls the Docker image from ghcr.io if it isn't found locally.

> **Note:** agent-wechat requires `SYS_PTRACE` and `seccomp=unconfined` to interact with the WeChat desktop process. It cannot run in serverless or restricted container environments (AWS Fargate, Cloud Run, etc.). Use a VM or bare-metal Docker host.

## Quick Start

```bash
# Start the container
wx-legacy up

# Check status
wx-legacy status

# Log in (displays QR code in terminal)
wx-legacy auth login

# List chats
wx-legacy chats list

# Send a message
wx-legacy messages send <chatId> --text "Hello"
```

## Commands

### Container

| Command | Description |
|---------|-------------|
| `wx-legacy up [--proxy user:pass@host:port]` | Start the agent-wechat container |
| `wx-legacy down` | Stop and remove the container |
| `wx-legacy logs` | Tail container logs |
| `wx-legacy status` | Show container up/down status and login status (when available) |

### Auth

| Command | Description |
|---------|-------------|
| `wx-legacy auth login` | Log in to WeChat (shows QR code) |
| `wx-legacy auth logout` | Log out of WeChat |
| `wx-legacy auth status` | Check login status |
| `wx-legacy auth token` | Show current auth token |
| `wx-legacy auth token --regenerate` | Generate a new auth token |

Login options:
- `--timeout <seconds>` — login timeout (default: 300)
- `--new` — switch to a new account instead of existing

### Chats

| Command | Description |
|---------|-------------|
| `wx-legacy chats list` | List chats from WeChat |
| `wx-legacy chats get <chatId>` | Get details for a specific chat |
| `wx-legacy chats find <name>` | Search chats by display name |
| `wx-legacy chats open <chatId>` | Open a chat in the WeChat UI |

Options: `--limit <n>`, `--offset <n>`, `--json`

### Messages

| Command | Description |
|---------|-------------|
| `wx-legacy messages list <chatId>` | List messages in a chat |
| `wx-legacy messages send <chatId>` | Send a message |
| `wx-legacy messages media <chatId> <localId>` | Download a media attachment |

Send options:
- `--text "message"` — send text
- `--image photo.jpg` — send an image
- `--file document.pdf` — send a file

Media options:
- `--output <path>` — save to a specific file path
- `--full` — require an original-resolution image
- `--thumbnail` — save the cached preview
- `--best` — explicitly choose the default best-available image behavior

Media retrieval does not open a chat. Use `wx-legacy chats open <chatId>` separately
if WeChat needs to cache a regular photo's standard-size copy.

### Sessions

| Command | Description |
|---------|-------------|
| `wx-legacy session list` | List all sessions |
| `wx-legacy session create <name>` | Create a new session |
| `wx-legacy session start <id>` | Start a session |
| `wx-legacy session stop <id>` | Stop a session |
| `wx-legacy session delete <id>` | Delete a session |

### Debug

| Command | Description |
|---------|-------------|
| `wx-legacy screenshot [file]` | Save a screenshot (default: screenshot.png) |
| `wx-legacy a11y` | Dump the accessibility tree (`--format json|aria`) |

## Global Options

| Option | Description |
|--------|-------------|
| `-s, --session <name>` | Use a specific session (default: "default") |
| `-V, --version` | Show version |
| `-h, --help` | Show help |

## Configuration

The CLI reads configuration from environment variables and a local token file:

| Source | Description |
|--------|-------------|
| `AGENT_WECHAT_URL` | Server URL (default: `http://localhost:6174`) |
| `AGENT_WECHAT_TOKEN` | Auth token (overrides token file) |
| `~/.config/agent-wechat/token` | Auto-generated auth token |

The auth token is generated automatically on first run and shared with the container via a read-only volume mount.

## Running the Container

There are two ways to run the agent-wechat container.

> **Note:** agent-wechat requires `SYS_PTRACE` and `seccomp=unconfined` because it uses ptrace to interact with the WeChat desktop process. It cannot run in serverless or restricted container environments (AWS Fargate, Cloud Run, Azure Container Instances, etc.). Use a VM or bare-metal Docker host.

### Option 1: `wx-legacy up` (local development)

The simplest way. `wx-legacy up` pulls/starts the container with the right flags, volume mounts, and auth token:

```bash
wx-legacy up
```

This starts a container named `agent-wechat` with:
- **Port 6174** — REST API + VNC web viewer at `/vnc/` (exposed to all interfaces)
- Persistent volumes for data and WeChat home directory
- Auth token from `~/.config/agent-wechat/token` (auto-generated on first run)

To route all container traffic through a proxy:

```bash
wx-legacy up --proxy user:pass@host:port
```

This sets up a transparent proxy (redsocks + iptables) inside the container — invisible to WeChat. Prefix with `socks5://` for SOCKS5 proxies.

### Option 2: Docker Compose (production / networked)

For production or when running alongside other services (e.g., OpenClaw), use the `docker-compose.yml` in the repo root as a reference:

```yaml
services:
  agent-wechat:
    image: ghcr.io/thisnick/agent-wechat:latest
    container_name: agent-wechat
    security_opt:
      - seccomp=unconfined
    cap_add:
      - SYS_PTRACE
      - NET_ADMIN
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

Generate a token before starting:

```bash
mkdir -p ~/.config/agent-wechat
openssl rand -hex 32 > ~/.config/agent-wechat/token
chmod 600 ~/.config/agent-wechat/token
```

If running alongside OpenClaw on the same Docker network, set `serverUrl` to `http://agent-wechat:6174` in your OpenClaw config.

### Building locally

To build the Docker image from source instead of pulling from ghcr.io:

```bash
# From the repo root — auto-detects host architecture
pnpm build:image

# Or specify architecture
pnpm build:image:arm64
pnpm build:image:amd64
```
