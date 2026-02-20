# @agent-wechat/cli

CLI for running and controlling the `agent-wechat` container.

## Install

```bash
npm i -g @agent-wechat/cli
```

## Quick Start

```bash
wx up
wx status
wx auth login
wx chats list
wx down
```

## Image Selection

`wx up` resolves images in this order:

1. `--image <ref>` option
2. `AGENT_WECHAT_IMAGE` env var
3. Local arch image (`agent-wechat:arm64` or `agent-wechat:amd64`)
4. Published image (`ghcr.io/agent-wechat/agent-wechat:latest`)

Override default remote repo/tag with:

- `AGENT_WECHAT_IMAGE_REPO`
- `AGENT_WECHAT_IMAGE_TAG`
