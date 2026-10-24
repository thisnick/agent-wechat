# agent-wechat

A standalone native WeChat daemon, CLI and OpenClaw channel.

The native `wx` executable speaks the protocol directly and uses official-format local profiles. It needs no official WeChat executable or desktop at runtime. See the supported profile scope and remaining limits in [implemented capabilities and validation](packages/native-client-rust/README.md).

| Package | Responsibility |
| --- | --- |
| [`@agent-wechat/cli`](packages/cli) | Native `wx` and verified executable installer |
| [`@agent-wechat/shared`](packages/shared) | Private native HTTP/SSE SDK; bundled into OpenClaw |
| [`@agent-wechat/agent-wechat`](packages/openclaw-extension) | OpenClaw managed or external native daemon integration |

## Native client

The release target matrix includes Linux x64/ARM64 with glibc 2.36+, macOS ARM64 and Windows x64/ARM64. macOS/Windows are release candidates pending native-machine acceptance; official-app profile takeover remains Linux-only. Build from source with Rust 1.93.1, or install a matching native release once published:

```bash
npm install -g @agent-wechat/cli@VERSION
wx --json version
wx config show
wx serve
```

For the native beta series, select a published `1.0.0-beta.N` version explicitly or install `@agent-wechat/cli@beta`. The stable `latest` channel retains the old container CLI until stable promotion. Beta preparation uses a CI-generated version PR and separately approved publication; no native beta is implied to be published by this source change.

`serve` runs the authenticated daemon in the foreground. It creates private API-token and storage-key files under the configured durable root. It does not start Docker or automatically display a login QR. Other clients connect over the versioned HTTP/SSE API.

In another terminal:

```bash
# Close the official Linux app before taking over its files.
wx auth login --official
wx accounts list
wx threads
wx messages list filehelper
```

`--official` selects the existing files on the daemon host and prepares identity/key setup automatically. Use `--official=/absolute/path/to/profile` to select a specific profile. Saved-device login is attempted when usable; a white-background QR is available when approval is needed. The files are used in place; no import is required. With multiple accounts, select one explicitly with `--account`.

The initial scope is taking over an existing official profile and continuing supported database and media writes in place. Native history, text/media sends, contact/group lookup, rosters, avatars and downloads use that profile. Unsupported sync controls remain staged. Fresh-profile creation, copy export and returning a profile to the official app are experimental extras outside this scope. FTS/search-index maintenance is not supported. See the [capability checklist](packages/native-client-rust/FEATURES.md) for the tested scope.

Use `wx messages recall MESSAGE_ID --server-id SERVER_ID` to recall an own message with its exact stored identities. OpenClaw exposes the scoped `wechat_recall` tool. Incoming recalls become durable placeholders and block attachment access; uncertain outgoing outcomes retain their operation without automatic resubmission. See [recall behavior and storage](https://thisnick.github.io/agent-wechat/how-it-works/message-types/#recalls).

See [Send attachments](https://thisnick.github.io/agent-wechat/use/media/) for the initial 32 MiB ordinary-file limit, unchanged image/video/voice limits, current validation status and longer CLI/API waits.

On Windows, configuration defaults under `%APPDATA%/agent-wechat` and durable data under `%LOCALAPPDATA%/agent-wechat`; `USERPROFILE` supplies the home fallback. Explicit and XDG overrides take precedence. On Unix, server configuration defaults to `~/.config/agent-wechat/server/config.toml`; daemon data defaults to `~/.local/share/agent-wechat`. XDG paths and explicit overrides are supported. Client endpoint, token reference and selected account belong in a separate `~/.config/agent-wechat/client.toml`. The operating official profile stays at its selected path; daemon state stays in sidecars.

See the [native guide](packages/native-client-rust/README.md) for configuration, profile ownership, recovery, API commands and current limits. [Deployment templates](deploy/native/README.md) include a user service and an official-binary-free image. Building these assets does not deploy them.

## Distribution and migration

Linux x64/ARM64 is validated with glibc 2.36+. macOS ARM64 and Windows x64/ARM64 have candidate build/install/lifecycle workflows; see the [platform acceptance checklist](PLATFORM-ACCEPTANCE.md). The native-only container remains Linux. The npm CLI installs the matching executable; OpenClaw is a separate npm plugin. The Rust crates and TypeScript SDK are internal source libraries, not new registry publications. See the [distribution matrix and release sequence](deploy/native/README.md).

This major release retires the old UI server/image, `wx-legacy` and Wechaty puppet/gateway integrations. Existing installations and volumes are not removed. Stop the old owner, preserve a backup and select its complete official profile with the native login command. Old `/api` clients and inline-token OpenClaw configurations require migration to native `/v1`; see the [migration guide](https://thisnick.github.io/agent-wechat/setup/migrate/).

## Development

```bash
pnpm install
pnpm build
pnpm typecheck
pnpm native:test
pnpm native:build
python3 packages/native-client-rust/scripts/smoke.py packages/native-client-rust/target/debug/wx
```

The native smoke check uses synthetic profiles and loopback HTTP. Native protocol/media tests use synthetic fixtures; live account validation is separate and must use an isolated test account. See [AGENTS.md](AGENTS.md) for contributor rules and the [native guide](packages/native-client-rust/README.md) for the exact test/build commands. CI builds/tests both native architectures without publishing or deploying.

Start the local upstream simulator and real hub API with `pnpm native:dev`, or run
`pnpm native:simulator:smoke` for the SDK regression journey. Both use synthetic
accounts without connecting to WeChat. See the [simulator guide](packages/native-client-rust/crates/wechat-simulator/README.md)
for state controls, isolation and the current implementation scope.

Start the connected browser preview with `pnpm client:dev`. It starts its own
isolated simulator and hub, then prints a browser URL. See the [client preview
guide](packages/client-ui/README.md) for sign-in, scenario controls and current limits.
