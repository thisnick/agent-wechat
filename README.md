# agent-wechat

A programmable WeChat interface with an experimental standalone native daemon and the retained official-client container backend.

The native `wx` executable speaks the protocol directly and uses official-format local profiles. It needs no official WeChat executable or desktop at runtime. Its compatibility scope is still limited; see the [implemented capabilities and validation](packages/native-client-rust/README.md). The existing container client is available explicitly as `wx-legacy`.

| Package | Responsibility |
| --- | --- |
| [`@agent-wechat/cli`](packages/cli) | Native `wx`, explicit `wx-legacy`, verified native installer |
| [`@agent-wechat/shared`](packages/shared) | Native HTTP/SSE SDK and retained container client |
| [`@agent-wechat/agent-wechat`](packages/openclaw-extension) | OpenClaw integration with explicit backend selection |
| [`@agent-wechat/wechaty-puppet`](packages/wechaty-puppet) | Existing container-backed Wechaty adapter |

## Native client

The enabled distribution target is Linux x64 with glibc 2.36 or newer. Build from source with Rust 1.93.1, or install a matching native release once published:

```bash
npm install -g @agent-wechat/cli@VERSION
wx --json version
wx config show
wx serve
```

`serve` runs the authenticated daemon in the foreground. It creates private API-token and storage-key files under the configured durable root. It does not start Docker or automatically display a login QR. Other clients connect over the versioned HTTP/SSE API.

In another terminal:

```bash
wx accounts create test
wx --account test auth login --qr-output /absolute/new-login.png
wx --account test threads
wx --account test messages list filehelper
```

An empty account slot can authenticate and stage native sync. Reading and projecting official history currently requires an initialized compatible official profile and its authorized keys; creating the full official database set from scratch remains a compatibility gate. Group/media projection is also incomplete. The commands above are not a claim of a complete drop-in replacement.

Server configuration defaults to `~/.config/agent-wechat/server/config.toml`; data defaults to `~/.local/share/agent-wechat`. XDG paths and explicit overrides are supported. Client endpoint, token reference and selected account belong in a separate `~/.config/agent-wechat/client.toml`. Existing stopped profiles can be attached directly or copied through import/export. Daemon state stays in sidecars.

See the [native guide](packages/native-client-rust/README.md) for configuration, profile ownership, recovery, API commands and current limits. [Deployment templates](deploy/native/README.md) include a user service and an official-binary-free image. Building these assets does not deploy them.

## Retained container backend

The container backend controls an official Linux client, reads its local data and provides the previous HTTP API. It supports the existing text/media/voice workflows. It requires Docker and its own persistent volumes; the Wechaty integrations continue to use this backend. The OpenClaw extension defaults to this backend unless native mode is explicitly selected.

```bash
wx-legacy up
wx-legacy auth login
wx-legacy chats list
wx-legacy messages send <chatId> --text "Hello"
wx-legacy messages send <chatId> --voice ./reply.mp3
wx-legacy down
```

See the [legacy CLI guide](packages/cli/LEGACY.md), [container compose example](docker-compose.yml), and [public documentation](https://thisnick.github.io/agent-wechat/). Commands in the older container guide refer to `wx-legacy` in this source version; the native and container APIs are different.

## Development

```bash
pnpm install
pnpm build
pnpm typecheck
pnpm native:test
pnpm native:build
python3 packages/native-client-rust/scripts/smoke.py packages/native-client-rust/target/debug/wx
```

The native smoke check uses synthetic profiles and loopback HTTP. Native protocol/media tests use synthetic fixtures; live account validation is separate and must use an isolated test account. See [AGENTS.md](AGENTS.md) for contributor rules and the [native guide](packages/native-client-rust/README.md) for the exact test/build commands. Existing `pnpm dev:deploy` changes a running container and must only be invoked for an explicitly selected development target.
