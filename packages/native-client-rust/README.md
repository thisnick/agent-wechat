# Native client foundation

This independently built Rust workspace starts the native client. It provides a configurable foreground server, private registry, existing-profile registration, and byte-preserving profile copy import/export. It does **not yet implement WeChat login, synchronization, message reads/sends, or official database mutations**. Its status/version responses advertise those limits explicitly. The existing npm `wx` remains the official-client automation CLI.

The initial supported development target is Linux AMD64, Rust 1.93. Other targets and official profile schemas require separate verification. No official executable, container or desktop is needed to run this foundation. Docker can be used as a development compiler; it is not a runtime dependency.

## Build and verify

```bash
cargo build --manifest-path packages/native-client-rust/Cargo.toml --locked
cargo test --manifest-path packages/native-client-rust/Cargo.toml --workspace --locked
cargo fmt --manifest-path packages/native-client-rust/Cargo.toml --all --check
cargo clippy --manifest-path packages/native-client-rust/Cargo.toml --workspace --all-targets --locked -- -D warnings
python3 packages/native-client-rust/scripts/smoke.py packages/native-client-rust/target/debug/wx
```

The smoke check uses synthetic local files and loopback HTTP only. It never launches the official client, authenticates a WeChat account, sends messages, or touches production data.

## Configure the server

The default server file is `$XDG_CONFIG_HOME/agent-wechat/server/config.toml`, falling back to `~/.config/agent-wechat/server/config.toml`:

```toml
schemaVersion = 1
listen = "127.0.0.1:6174"
dataDir = "/srv/agent-wechat"
tokenFile = "/run/secrets/wechat-api-token"
```

All fields except `schemaVersion` are optional when the file exists. Without a config file, defaults use loopback port 6174 and `$XDG_DATA_HOME/agent-wechat` (fallback `~/.local/share/agent-wechat`). An explicitly selected missing file fails. Unknown keys or unsupported configuration versions fail before store initialization. Only loopback listeners are supported in this increment. Relative TOML file paths resolve against the config directory; command-line/environment paths resolve against the current directory.

Use the built executable's absolute path in the following examples, or `cargo run --manifest-path packages/native-client-rust/Cargo.toml --bin wx -- ...`:

```bash
./packages/native-client-rust/target/debug/wx --json version
./packages/native-client-rust/target/debug/wx --server-config /private/server.toml config show
./packages/native-client-rust/target/debug/wx --data-dir /private/native-data serve
```

`--server-config` overrides `AGENT_WECHAT_SERVER_CONFIG`; `--data-dir` overrides `AGENT_WECHAT_DATA_DIR`, then the selected file, then defaults. `--listen` and `--token-file` override their file values. Config inspection and version do not create files or start a server. `config show` reports paths, never token contents.

On first explicit `serve`, the implicit `<dataDir>/api-token` is generated privately and retained. An explicitly configured token file must already exist, contain at least 32 printable non-whitespace bytes and be owned by the service user with owner-only permissions. A missing/invalid token is not replaced silently. Keep the token out of command arguments/logs. Native login credentials and profile SQLCipher keys are not API tokens; this increment acquires neither.

The reusable `wechat-api::router` exposes authenticated `GET /v1/status` and `GET /v1/accounts`. Send `Authorization: Bearer <token>` through a protected HTTP client. Registered accounts report `schemaCompatibility: "unverified"`; registration is not a login or schema-validation result. No message, login or send routes are registered. `serve` retains its process/profile locks until graceful Ctrl-C/SIGTERM shutdown. Background `daemon start/stop`, client endpoint configuration and native release/npm installation remain future work.

## Select an existing official-format operating profile

```bash
./packages/native-client-rust/target/debug/wx --data-dir /private/native-data \
  store attach --account test-account --path /private/stopped-official-home --source-stopped
./packages/native-client-rust/target/debug/wx --data-dir /private/native-data store list
```

Direct attachment records the canonical external path, without copying or modifying it. Daemon-only files live at `<dataDir>/accounts/test-account/agent/`, separate from the selected profile. Subsequent server startup locks that profile against our other processes, including ones using another data directory. An unmanaged official app does not honor these locks; it must be stopped or controlled through an exclusive handoff. `--source-stopped` is the operator's explicit assertion, not automatic detection. Do not use it for an active profile.

The registry has stable local account IDs and server identity. It does not infer remote account identity from a directory name. Official read/write schema checks, remote identity/key validation, fresh-profile creation and native account actors are later implementation increments. Official tables remain untouched.

## Optional same-format import and export

Stop this daemon before these offline operations. Paths are local to the machine executing the command, not a remote server:

```bash
./packages/native-client-rust/target/debug/wx --data-dir /private/native-data \
  store import --account copied-account --source /private/stopped-official-home --source-stopped
./packages/native-client-rust/target/debug/wx --data-dir /private/native-data \
  store export --account copied-account --destination /private/new-official-home --source-stopped
```

Import creates `<dataDir>/accounts/copied-account/profile/` with the original file format. Export copies only the profile tree, excluding daemon sidecars. A full backup must include the durable root/sidecars as well. This first copy implementation does not merge, decrypt, re-encode, migrate schema, repair indexes or refresh login trust. It verifies opaque bytes and filesystem metadata; it does not establish that an unknown profile/build can be opened by an official client.

The destination must not exist and its parent must already exist. Copies retain regular files (including WAL/SHM and unknown files), empty directories, permission bits, modification times and supported relative internal symbolic links. External, absolute or dangling symlinks, non-UTF-8 paths and special files fail explicitly. Physical hardlink sharing, ownership/ACLs/xattrs and root-directory timestamps are not preserved. Limits are 100,000 entries, 512 GiB and 128 directory levels per copy. Sources must be stopped or consistent snapshots; before/after manifests detect observed changes but cannot prove an uncontrolled live writer is absent.

Copy stages on the destination filesystem, verifies content hashes/metadata, rechecks the source and publishes using Linux no-replace rename. Existing destinations/accounts are never overwritten. The private import receipt is retained in `agent/control.sqlite`. A failure after filesystem publication but before registry commit can leave an unregistered account directory; preserve it for recovery rather than deleting/retrying over it. Full automatic recovery and profile mutation journals remain future work. Export success is a filesystem copy result, not confirmation of official login or all application-level relationships.

## Library boundaries

- `wechat-config`: pure versioned settings resolution and precedence.
- `wechat-store`: exclusive durable-root ownership, sidecar registry, profile binding/copy and per-profile process leases.
- `wechat-api`: reusable authenticated foundation router and protected token provisioning.
- `wx-native`: the `wx` executable composing these libraries.

The network protocol/core and build-specific official-format writers will be added separately. Canonical user data remains in official files; our extra operational state stays in sidecars. Sidecar migrations have independent versions and must never be treated as official profile migrations.
