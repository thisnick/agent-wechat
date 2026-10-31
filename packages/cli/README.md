# @agent-wechat/cli

[English](README.md) | [简体中文](README.zh-CN.md)

`wx` is the native Rust client and foreground daemon. The npm package installs a matching release executable; it does not implement the native protocol in JavaScript. Linux x64/ARM64 with glibc 2.36+ is validated. The release matrix also builds macOS ARM64 and native Windows x64/ARM64 candidates; native-machine account acceptance is pending. musl remains unsupported. Official-app profile takeover is Linux-only. Clients on other systems can use the HTTP API of a supported Linux daemon. `wx --json version` and `/v1/status` expose the runtime and account capabilities; unsupported official schemas fail explicitly.

## Installation

After a matching release has been published:

```bash
npm install -g @agent-wechat/cli@VERSION
wx --json version
```

During the native beta series, use a published exact version such as `1.0.0-beta.0`, or `@agent-wechat/cli@beta`. The stable `latest` channel retains the old container CLI until promotion. See the beta release instructions before assuming a beta has been published.

The explicit npm installation downloads one exact-version asset over verified HTTPS. Its size and SHA-256 must match `native-release.json` inside the npm package, then a bounded local identity check must report the same version, native backend, Rust target and serve capability before atomic installation. Normal command invocation never downloads a missing runtime. A missing manifest, unsupported platform, corrupt asset or wrong backend is an error.

Installers that disable lifecycle scripts are supported:

```bash
npm install -g --ignore-scripts @agent-wechat/cli@VERSION
wx-native-install
```

For offline deployment, provision the release binary and its `wx-vVERSION-checksums.json` into one local directory, then run:

```bash
AGENT_WECHAT_NATIVE_ARTIFACT_DIR=/absolute/release-assets wx-native-install
```

The offline manifest must agree with the npm-bundled manifest. Alternatively, provision a trusted executable yourself and set `AGENT_WECHAT_NATIVE_BINARY=/absolute/path/to/wx`; the launcher verifies its native identity/version rather than downloading. This explicit override trusts the operator-provisioned bytes and does not assert they are identical to a release asset. `AGENT_WECHAT_SKIP_NATIVE_INSTALL=1` defers installation during image/package provisioning; commands remain unavailable until the runtime is provided. Do not disable TLS verification.

The OpenClaw plugin uses an already installed global native `wx` or a configured absolute executable. It never downloads binaries or relies on its plugin installer running npm lifecycle scripts. For managed processes, prefer the actual release executable path to avoid an extra launcher process; the npm launcher also forwards arguments, standard streams, exit codes and termination signals.

## Configuration and storage

```bash
wx config show
wx serve --server-config /absolute/server/config.toml
```

The default server configuration is `$XDG_CONFIG_HOME/agent-wechat/server/config.toml`, or `~/.config/agent-wechat/server/config.toml`. The durable root defaults to `$XDG_DATA_HOME/agent-wechat`, or `~/.local/share/agent-wechat`. `wx serve --data-dir PATH` and `AGENT_WECHAT_DATA_DIR` override it. Server config can select `listen`, `dataDir` and `tokenFile`; relative references resolve against the config file. Paths are not credentials.

The initial workflow takes over an existing official profile in place. Close the official app, start `wx serve`, then use a second terminal:

```bash
wx auth login --official
# Or choose a specific path on the daemon host:
wx auth login --official=/absolute/official-profile
```

The daemon binds the stored identity, attempts saved-device login and derives verified database keys after authentication. The official app stays closed while this daemon owns the files.

Recall an own message using its opaque message ID and exact decimal-string
server ID from history or a verified send receipt:

```bash
wx --account example messages recall MESSAGE_ID --server-id SERVER_ID
wx --account example operations get OPERATION_ID
```

Use `--detach` to return the durable operation immediately. A private receipt is
written before submission; the same target uses a stable default idempotency
key. An uncertain result exits8 and must be inspected rather than automatically
resubmitted. WeChat decides eligibility, and stopping the watcher does not cancel
the recall. Recalled messages remain as placeholders; their media is unavailable
through the daemon. Raw journals, cached files and backups are retained.

Lower-level registration and experimental profile copies are also available:

```bash
# Only use an explicitly stopped/quiescent profile; never share a writer.
wx store attach --account example --path /absolute/official-profile --source-stopped
wx store import --account copied --source /absolute/stopped-profile --source-stopped
wx store export --account copied --destination /absolute/new-export --source-stopped
```

Direct mode keeps official profile bytes at the selected path and daemon-only state under `dataDir`. Fresh creation, export and returning files to the official app are outside the initial supported takeover scope. Import/export copy the same format; they do not convert schema or refresh login trust. Use `--help` for the implemented command surface. Never copy or attach a running production profile implicitly.

## Build and validate without deployment

From the repository root:

```bash
pnpm --filter @agent-wechat/cli... build
pnpm --filter @agent-wechat/cli test
pnpm native:build
pnpm native:test
pnpm native:release:build
node scripts/native-package-smoke.mjs artifacts/native
# Substitute the exact artifact version emitted by the build:
python3 scripts/native-cli-smoke.py artifacts/native/wx-vVERSION-linux-x64-gnu
python3 scripts/native-cli-fixture-test.py artifacts/native/wx-vVERSION-linux-x64-gnu
python3 scripts/native-export-smoke.py artifacts/native/wx-vVERSION-linux-x64-gnu
```

Source-workspace postinstall defers release installation; run the Rust binary from its target directory or use an explicit trusted executable override. The local release builder tests the Rust workspace, verifies executable identity and creates versioned assets; it does not publish, deploy or log an account in. The package smoke test packs/unpacks an actual npm tarball and installs from local artifacts into a temporary directory.

Deployment examples and the deliberate release preparation sequence are in [deploy/native](../../deploy/native/README.md). The previous container CLI and Wechaty packages are retired; see the [migration guide](https://thisnick.github.io/agent-wechat/setup/migrate/).

## Package helper scripts

`native-install.mjs` runs during npm installation, or through `wx-native-install` for offline/disabled-script installs. It selects the host architecture, checks the npm-bundled hash and verifies executable identity before atomic installation. `native-launcher.mjs` only runs an already installed executable.

`native-check-package.mjs` is the prepack/prepublish gate. It checks version/source/target/checksum metadata; before publication it additionally requires both architecture assets to exist and match the package's manifest. A local pack smoke can use one architecture fragment; publication requires the complete matrix. These helpers belong to the existing CLI package, not additional npm packages.

Candidate platform validation and remaining profile boundaries are tracked in [the acceptance checklist](../../PLATFORM-ACCEPTANCE.md). Windows executables are named `wx.exe`; the npm launcher chooses the OS/CPU-specific asset. A native ARM64 Node installation selects the native Windows ARM64 executable.
