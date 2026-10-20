# Standalone native client

This experimental Rust workspace implements the native `wx` executable, reusable protocol/profile libraries and an authenticated multi-account daemon. It neither loads nor launches an official WeChat executable. Docker is optional as a compiler or deployment wrapper.

The supported development and release baseline is Linux x64, glibc 2.36+, Rust 1.93.1. Compatibility has been investigated against a specific official Linux profile family; another build or architecture needs separate validation.

## Implemented scope

| Area | Implemented behavior | Validation / remaining limits |
| --- | --- | --- |
| Native authentication | Pinned MMTLS/Hybrid transport, QR and saved-device approval operations, active-session check and one AutoAuth renewal on authenticated expiry | Live Rust renewal + heartbeat + sync passed. Fresh QR/saved-device approval are wire/unit tested and still require a fresh phone-assisted end-to-end run. No automatic QR after transport failure. |
| Incoming sync | Serialized account workers, heartbeat selector checks, bounded reconnect, original command staging and separate projection receipts | Live protocol and assembled daemon passed. This actor uses heartbeat polling, capped at 30 seconds; unsolicited account-associated push is unverified. Sync is not complete server history. |
| Local reads | SQLCipher snapshots, scoped message IDs, cursor pagination, contacts, threads, groups, room membership, avatars and voice bytes | Real stopped encrypted snapshot read validated; all ordinary message shards are discovered. Schema reports include fingerprints and coverage. |
| Local images | V2 DAT encryption/decryption and exact-message resource/hardlink lookup | Original quality needs content-MD5 proof. Unknown cached role is not labeled standard/original. Standard/thumbnail requests without a verified role fail. |
| Official-format writes | Incoming direct plain text in an existing initialized supported chat; exact selected desktop read IDs | Real encrypted clone and synthetic end-to-end recovery tests passed. Strict table/index/map/collision checks; no generic schema migration, new chat allocation, group/media projection or fresh DB initialization. |
| Text send | Durable idempotent intent, exactly one native submission, correlated server acceptance, uncertain outcome retained for review | Wire/crypto tests and outbox recovery tests passed. Live assembled-daemon File Transfer text send accepted; repeating the logical key returned the same intent. Phone rendering was not rechecked in this run. Accepted sends are not yet fully projected into official files. |
| Media protocol library | Bounded CDN download/upload and file/image/video/pre-encoded voice final-send codecs | Offline vectors and synthetic peer tests. Not exposed as working daemon send/media-download jobs. No transcoding or general large-file support. |
| Other clients | Account-scoped HTTP API, durable events/SSE, scoped revocable tokens, native TypeScript SDK | API/security tests and process smoke checks. The legacy API is a separate backend. |
| OpenClaw | Explicit native backend, separately provisioned `wx`, managed child/external server, live committed text dispatch and durable custody | Real process tests and model-handoff harness pass. Released OpenClaw hosts inspected lack the prepared callback, so read state remains unchanged with `handoffUnconfirmed`; native media forwarding is unsupported. |

An empty native account slot can log in and stage original sync commands. It does not yet create an official database set that a stock client can open. Use a compatible initialized profile for local history and the narrow projection path. Unsupported original commands remain encrypted in sidecars, not silently discarded or presented as committed messages. `profileProjection: pending` is material: the current official files do not contain every staged update.

## Build and verify

From the repository root:

```bash
cargo build --manifest-path packages/native-client-rust/Cargo.toml --workspace --locked
cargo test --manifest-path packages/native-client-rust/Cargo.toml --workspace --locked
cargo fmt --manifest-path packages/native-client-rust/Cargo.toml --all --check
cargo clippy --manifest-path packages/native-client-rust/Cargo.toml --workspace --all-targets --locked -- -D warnings
python3 packages/native-client-rust/scripts/smoke.py packages/native-client-rust/target/debug/wx
node scripts/native-openclaw-smoke.mjs /absolute/path/to/wx
```

The public smoke tests use synthetic data, loopback HTTP and owned child processes. They make no WeChat requests. Unit tests cover authenticated crypto vectors, malformed/cross-account data, unsigned 64-bit IDs, real SQLCipher databases, schema drift, copy preservation, projection retries, exact read sets, token scope/revocation, event custody and uncertain-send recovery. Native packaging and deployment preparation are described in [deploy/native](../../deploy/native/README.md). No test command deploys production services.

Private test-account validation additionally exercised native expiry/renewal, heartbeat/sync, copied official history through the assembled daemon, and a stopped encrypted-profile clone. Original source/checkpoint hashes were preserved. Private credentials, profiles, captures and messages are excluded from this repository. These checks do not establish compatibility with arbitrary accounts or official builds.

## Server and client configuration

Default server file: `$XDG_CONFIG_HOME/agent-wechat/server/config.toml`, fallback `~/.config/agent-wechat/server/config.toml`:

```toml
schemaVersion = 1
listen = "127.0.0.1:6174"
dataDir = "/srv/agent-wechat"
# Optional pre-provisioned owner-only references:
# tokenFile = "/run/secrets/wechat-api-token"
# storageKeyFile = "/run/secrets/wechat-storage-key"
```

All fields except `schemaVersion` are optional when the file exists. Missing default files use defaults; explicitly selected missing files, unknown keys and unsupported versions fail. Relative file references resolve against the selected config directory. `--server-config` overrides `AGENT_WECHAT_SERVER_CONFIG`; `--data-dir` overrides `AGENT_WECHAT_DATA_DIR`, file settings, then XDG defaults. `--listen` and `--token-file` override file values. This server binds loopback only; use an authenticated TLS reverse proxy or protected tunnel for remote clients.

The durable root defaults to `$XDG_DATA_HOME/agent-wechat`, fallback `~/.local/share/agent-wechat`:

```text
registry.sqlite
api-token
storage-key
accounts/<local-id>/profile/       # managed official-format tree
accounts/<local-id>/agent/control.sqlite
```

Directly bound official profiles stay at their selected external location. They are not imported implicitly. Credentials, raw apply batches and queued send payloads use authenticated encryption with a separate storage key. API tokens are not database or storage keys. Missing/wrong storage keys fail without replacing protected state. Back up the durable root and any external secret references together. A profile-only export excludes native credentials/sidecars.

`wx config show` resolves settings without initializing storage. First `wx serve` provisions implicit private token/key files; explicit references must already exist and satisfy private-file checks. Owner API tokens are loaded at startup. `serve` runs in the foreground until Ctrl-C/SIGTERM; the provided service unit or OpenClaw host owns background execution and graceful shutdown.

Client file: `~/.config/agent-wechat/client.toml` (XDG supported):

```toml
schemaVersion = 1
endpoint = "http://127.0.0.1:6174"
tokenFile = "/srv/agent-wechat/api-token"
account = "test"
```

Use `--client-config`, `--url`, `--token-file` or `--account` to override client selections. Without a client file, the CLI uses resolved local server defaults. No token value belongs in a URL or command argument. Plain HTTP is restricted to loopback clients; redirects are refused.

## Select an initialized official profile

Stop the official app and daemon before offline operations. Never operate them as concurrent writers.

```bash
wx --data-dir /private/native-data store attach --account test \
  --path /private/stopped-official-home --source-stopped
wx --data-dir /private/native-data store list
```

Direct mode can select a complete official home containing `xwechat_files/<account>/db_storage`, or an existing account directory containing `db_storage`. Managed copy/import should preserve the complete official home and original account directory name:

```bash
wx --data-dir /private/native-data store import --account copied \
  --source /private/stopped-official-home --source-stopped
wx --data-dir /private/native-data store export --account copied \
  --destination /private/new-official-home --source-stopped
```

The destination must not exist and its parent must exist. Copies retain opaque regular-file bytes, WAL/SHM, unknown files, empty directories, modes, mtimes and safe internal relative symlinks. Before/after manifests detect observed source changes; they cannot prove an uncontrolled writer was absent. Unsupported links/special files, oversized copies and overlaps fail. Ownership/ACLs/xattrs and physical hardlink sharing are not preserved. No database conversion or trust refresh occurs.

Exports with pending native projection fail by default. `--allow-incomplete` explicitly copies only the current official files and reports remaining batches. It is not a complete takeover export. Keep the whole native durable root for recovery. A failure between directory publication and registry commit can leave an unregistered directory; preserve it for inspection rather than deleting it or retrying over it.

Our profile leases exclude cooperating native operations across data roots. The unmanaged official app does not honor those leases. `--source-stopped` is an operator assertion, not automatic official-process detection.

Bind an authorized private key file, or derive per-file keys from an explicitly supplied private 32-byte account-password file:

```bash
wx --account test store bind-keys --keys-file /private/profile-keys.json
wx --account test store derive-keys --profile /private/account-directory \
  --password-file /private/account-password.bin --source-stopped
wx --account test store schema
```

Derived keys are written under the account's daemon sidecar, outside the selected official tree. The CLI does not acquire an account password or active credentials from a running process. Key-file format, legacy agent-key cache export library API and reader limits are in [wechat-profile](crates/wechat-profile/README.md). Database key availability and image key material do not prove every page/image decodes.

Official schemas have no established universal monotonic marker in the inspected family. SQLite `schema_version` is not an application compatibility version. Readers inventory schemas and fingerprints; writers validate their exact supported shape and refuse unknown fields/indexes/triggers/routing rather than migrating the official store. Sidecar migrations have independent versions.

## Login, sync and reading

```bash
wx serve
# In another terminal:
wx accounts list
wx --account test auth login --qr-output /absolute/new-login.png
wx --account test threads
wx --account test contacts --search Example
wx --account test groups
wx --account test lookup-contacts username
wx --account test lookup-group 'room@chatroom'
wx --account test messages list filehelper --limit 50
wx --account test messages get msg_OPAQUE_ID
wx --account test events --after 0 --watch
```

Login is an asynchronous operation. `--detach` returns its ID; `auth operation ID`, `auth qr ID --output PATH` and `auth cancel ID` inspect/control it. QR output is an opaque PNG on white, with a quiet border. New generations use separate output files. The challenge remains stable while scanned/awaiting approval, refreshes only before a scan within a bounded overall deadline, and completes the final auth exchange immediately on approval.

An accepted active heartbeat resumes the saved session; authenticated expiry permits one renewal. Saved-device approval and explicit QR are separate paths. Transport/integrity failures do not clear credentials or establish trust expiry. Automatic reconnect never opens a QR. Saved credentials restored offline remain unvalidated until an accepted native exchange in this process.

`auth disconnect` stops desired network activity while retaining trust. `auth logout-local` fences local active use but does **not** claim server revocation. A separately verified server-logout workflow remains outstanding. Native active secrets and cursors are stored in encrypted sidecars; native session persistence is not a claim that the official app can reuse them.

For operator-selected standalone credential backup/checkpoints only, offline `store restore-credentials --file /private/state.json [--cursor-file /private/raw-cursor.bin]` installs saved-unvalidated state. This is not an official-profile credential extractor. Raw cursors are opaque bytes, not message IDs or JSON; do not edit their values.

`messages sync` requests one native page. Original command bytes and the received cursor commit together in the native sidecar before projection. The received checkpoint means original commands are recoverable; it does not assert that official files have caught up. Message/session/MMKV databases cannot share one transaction. Exact apply receipts and idempotent reconciliation bridge supported writes; unknown commands remain pending. Offline `store recover-projection` or authenticated `POST /v1/accounts/{account}/projection/recover` retries retained commands without a new send.

Enable the narrow writers only after confirming the actual remote identity and exclusive profile ownership:

```bash
# Both daemon and official app stopped:
wx --account test store enable-writes --own-username OWN_USERNAME \
  --mmkv-store /private/official-home/xwechat_files/all_users/config/global_config \
  --format-key-file /private/mmkv-format-key.bin --official-client-stopped
```

Enabling requires an explicitly supplied private 16-byte MMKV format key and an identity store inside the selected profile. The bounded read-only v4 decoder verifies framing/CRC, then matches stored username and UIN to native account state and checks the selected account-directory binding. These checks run again before writes. No format key is shipped or extracted from a runtime binary. Unsupported/expiration-enabled layouts refuse writes. This verifies stored identity; it does not import official login trust or update MMKV/SyncKey. Enabling does not waive schema/index/map checks. Live incoming events are emitted only after a supported official projection completes; startup/catch-up observations are labeled separately and do not become new OpenClaw turns.

## Sends, reads and event custody

```bash
wx --account test messages send filehelper --text "A clearly labeled test" \
  --idempotency-key unique-logical-send-id
wx --account test messages send-status SEND_ID
wx --account test messages read THREAD_ID --message-id msg_OPAQUE_ID \
  --expected-version OBSERVED_READ_STATE_VERSION
```

Reuse an idempotency key only for the same destination and payload. A queued send is persisted before submission. Accepted results require matching destination/client identity and a full server ID. Restart during submission becomes `needs_review`; no automatic resend follows an uncertain result. The same logical key returns the same intent, or a conflict for a changed payload. Local profile-send projection and general media jobs remain incomplete.

Read-state changes remove only the exact source-qualified incoming IDs. They preserve other arrivals and existing unread counter/list discrepancies. The compare-and-swap version includes the observed profile generation, so an intervening source change causes a conflict. Event delivery ACKs never change read state, and desktop read state does not send a phone read receipt. A prepared-model handoff needs a positively correlated callback; accepted/enqueued tool execution is not evidence of that handoff.

The API requires Bearer headers. Main resources are under `/v1/accounts/{account}`: `threads`, `contacts`, `groups`, `groups/{room}/members`, `threads/{thread}/messages`, `messages/{id}`, cached `avatars/{id}` and `messages/{id}/image?quality=best|original|standard|thumbnail`. Native lookup observations use `contacts/lookup` and `groups/{room}/lookup`. `auth/login`, operation/QR/cancel routes, `sends`, `threads/{thread}/read-state`, `events`, `events/stream` and `consumers/{consumer}/ack` are separate operations. Cursor/ID fields are strings; message XML, CDN secrets and raw database fields are not returned as public message bodies.

Use `tokens create NAME --accounts ACCOUNT --scopes read` to issue a restricted token, and `tokens list|revoke ID` to manage it with the owner token. Available grants are `read`, `media`, `send`, `read-state`, `login`, `admin`. Resource authorization occurs before profile/network work; tokens are checked again during SSE polling. Event journals retain a bounded window, report gaps explicitly, and support replay after a durable decimal cursor. Event custody and message read projection are independent.

## Library boundaries

- `wechat-config`: versioned server/client paths and settings.
- `wechat-store`: private root/registry, profile copies/leases, encrypted record vault, outbox, events and apply intents.
- `wechat-profile`: official SQLCipher/schema/message/media readers and narrow checked writers.
- `wechat-protocol`: native cryptography, transport, auth/sync/contact/media wire codecs.
- `wechat-core`: serialized account workers and durable operation/projection coordination.
- `wechat-api`: account/scoped HTTP and SSE router.
- `wx-native`: CLI/server composition.

This is a tested experimental implementation, with explicit remaining gates. It is not ready to replace arbitrary production official profiles. No deployment, release publication or production profile handoff is part of the validation performed here.
