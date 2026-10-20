# @agent-wechat/cli

`wx` is the native Rust client and foreground daemon. The npm package installs a matching release executable; it does not implement the native protocol in JavaScript. The previous Docker/UI client remains available explicitly as `wx-legacy`. There is no automatic fallback between the backends. On a platform without native release support, an explicit `AGENT_WECHAT_SKIP_NATIVE_INSTALL=1` installation can still provision the retained `wx-legacy` tool; it does not enable native commands.

The native implementation is under active validation. Use `wx --json version` and authenticated `/v1/status` to inspect actual capabilities. A registered official-format profile does not establish a native login or prove every file writer is compatible. Linux x64 with glibc 2.36 or newer is the only distribution target currently enabled; ARM64, macOS and musl are rejected until independently tested.

## Installation

After a matching release has been published:

```bash
npm install -g @agent-wechat/cli@VERSION
wx --json version
```

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

Official-format profiles can be registered directly or copied into managed storage:

```bash
# Only use an explicitly stopped/quiescent profile; never share a writer.
wx store attach --account example --path /absolute/official-profile --source-stopped
wx store import --account copied --source /absolute/stopped-profile --source-stopped
wx store export --account copied --destination /absolute/new-export --source-stopped
```

Direct mode keeps official profile bytes at the selected path and daemon-only state under `dataDir`. Import/export copy the same format; they do not convert schema or refresh login trust. Use `--help` for the implemented command surface. Never copy or attach a running production profile implicitly.

## Build and validate without deployment

From the repository root:

```bash
pnpm --filter @agent-wechat/cli... build
pnpm --filter @agent-wechat/cli test
pnpm native:build
pnpm native:test
pnpm native:release:build
node scripts/native-package-smoke.mjs artifacts/native
```

Source-workspace postinstall defers release installation; run the Rust binary from its target directory or use an explicit trusted executable override. The local release builder tests the Rust workspace, verifies executable identity and creates versioned assets; it does not publish, deploy or log an account in. The package smoke test packs/unpacks an actual npm tarball and installs from local artifacts into a temporary directory.

Deployment examples and the deliberate release preparation sequence are in [deploy/native](../../deploy/native/README.md). The separate [legacy client guide](LEGACY.md) covers container/UI behavior retained by `wx-legacy`.
