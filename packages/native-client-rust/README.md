# Standalone native client

This experimental Rust workspace implements the native `wx` executable, reusable protocol/profile libraries and an authenticated multi-account daemon. It neither loads nor launches an official WeChat executable. Docker is optional as a compiler or deployment wrapper.

The supported development and release baseline is Linux x64, glibc 2.36+, Rust 1.93.1. Compatibility has been investigated against a specific official Linux profile family; another build or architecture needs separate validation.

The initial scope is taking over an existing stopped official profile and continuing supported writes in place. Creating a profile from scratch, exporting it and returning it to the official app are experimental extras, not requirements for this scope. Unknown files are preserved; official FTS/search indexes are not maintained.

## Implemented scope

| Area | Implemented behavior | Validation / remaining limits |
| --- | --- | --- |
| Native authentication | Pinned MMTLS/Hybrid transport, active validation/renewal, saved-device and conditional white QR workflows | Controlled native exchanges passed; trust lifetime and arbitrary-account compatibility remain separate checks. No QR fallback after transport/integrity failures. |
| Incoming sync | Independent serialized account workers, heartbeat polling, bounded reconnect, original-command staging and separate projection receipts | Polling is capped at30 seconds; unsolicited account-associated push remains unverified. Sync is not complete server history. |
| Local storage | SQLCipher snapshots, account-qualified IDs, contacts/threads/groups/rosters, media readers and schema fingerprints | Stopped encrypted snapshots validated; unknown official schemas fail explicitly rather than migrate automatically. |
| Official-profile takeover | One-switch selection, saved identity/key-info/routing, accepted-password admission of every encrypted database | Original-profile history and cold restart passed; a fresh sidecar then logged in from the official saved files without QR or further phone approval. The supported build's MMKV format constant is bundled. |
| Profile writeback | Schema-checked text, metadata, supported media and desktop-read writers with recoverable cross-file receipts | Accepted outgoing history rows and image/voice/file/video caches with supported resource joins are checked, including local repair. Unsupported commands stay staged. FTS/search indexes and arbitrary official schemas are outside the supported writers. |
| Text/media sends | Durable idempotent text intents and image/file/video/voice jobs, one native submission, partial/uncertain receipts | Controlled File Transfer accepted all media kinds; the account owner confirmed file/image opening and video/voice playback for the latest implementation. Voice is encoded independently using bundled SILK; FFmpeg/FFprobe normalize source media. Large-file branches and sticker sending are disabled. |
| Media reads | Separate advertised/actual variants, verified cache, strict quality requests, durable transfer jobs and range reads | Three JPEG qualities and normal/original H.264/HEVC videos downloaded, with hash/range or decoder checks as applicable. Pending, expired, unsupported and available states remain distinct. Cached bytes can outlive remote expiry; raw XML is not a retrieved attachment. |
| Metadata refresh | Contact/group lookup, independent catalogs/checkpoints, rosters and avatar cache | A group with no thread, seven-member roster and small avatar passed native retrieval. Native contact inventory resumed across checkpoints and drained its catalog, with three explicitly unavailable contacts reported separately as partial hydration. |
| Clients | Account-scoped API, authenticated resource cursors, replay/SSE, generated Rust→TS contracts, native CLI/SDK | CLI lifecycle and transport fixtures exercise real binaries without account credentials. Tombstones preserve deleted view positions. |
| OpenClaw | Managed/external native hosting, durable custody, admitted text/media, native sends and optional Gateway facade | Actual pinned Gateway/local-model tests verify correlated text, image bytes and file/video/voice references in direct and group turns. Explicit host permission is required; omitted, unsupported or ambiguous inputs remain unread. |

[FEATURES.md](FEATURES.md) separates required implementation, focused tests and remaining live validation. [CLI.md](CLI.md) covers configuration, background ownership, login, offline profiles, media, receipts and exit codes. Unknown commands remain encrypted in sidecars until a supported official projection succeeds. An accepted sync checkpoint does not imply that every official file has caught up.

## Build and verify

From the repository root:

```bash
cargo build --manifest-path packages/native-client-rust/Cargo.toml --workspace --locked
cargo test --manifest-path packages/native-client-rust/Cargo.toml --workspace --locked
cargo fmt --manifest-path packages/native-client-rust/Cargo.toml --all --check
cargo clippy --manifest-path packages/native-client-rust/Cargo.toml --workspace --all-targets --locked -- -D warnings
python3 packages/native-client-rust/scripts/smoke.py packages/native-client-rust/target/debug/wx
node scripts/native-openclaw-smoke.mjs /absolute/path/to/wx
python3 scripts/native-cli-smoke.py /absolute/path/to/wx
python3 scripts/native-cli-fixture-test.py /absolute/path/to/wx
python3 scripts/native-export-smoke.py /absolute/path/to/wx
bash scripts/native-generate-types.sh --check
# Build the extension and its shared dependency before these pinned-host tests:
node scripts/native-gateway-smoke.mjs
node scripts/native-model-smoke.mjs
node scripts/native-media-model-smoke.mjs
```

The public smoke tests use synthetic data, loopback HTTP and owned child processes. They make no WeChat requests; model-boundary tests use a local stub provider and a deterministic local transcription fixture. Unit tests cover authenticated crypto vectors, malformed/cross-account data, unsigned 64-bit IDs, real SQLCipher databases, schema drift, copy preservation, projection retries, exact read sets, token scope/revocation, event custody and uncertain-send recovery. Native packaging and deployment preparation are described in [deploy/native](../../deploy/native/README.md). No test command deploys production services.

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
# profileFormatKeyFile = "/run/secrets/wechat-profile-format-key"
```

All fields except `schemaVersion` are optional when the file exists. `profileFormatKeyFile` overrides the supported build's bundled MMKV format constant; it is separate from account authorization, database encryption and the daemon storage key. Missing default files use defaults; explicitly selected missing files, unknown keys and unsupported versions fail. Relative file references resolve against the selected config directory. `--server-config` overrides `AGENT_WECHAT_SERVER_CONFIG`; `--data-dir` overrides `AGENT_WECHAT_DATA_DIR`, file settings, then XDG defaults. `--listen` and `--token-file` override file values. This server binds loopback only; use an authenticated TLS reverse proxy or protected tunnel for remote clients.

The durable root defaults to `$XDG_DATA_HOME/agent-wechat`, fallback `~/.local/share/agent-wechat`:

```text
registry.sqlite
api-token
storage-key
accounts/<local-id>/profile/       # experimental managed/copy profile
accounts/<local-id>/agent/control.sqlite
```

Directly bound official profiles stay at their selected external location. They are not imported implicitly. Credentials, raw apply batches and queued send payloads use authenticated encryption with a separate storage key. API tokens are not database or storage keys. Missing/wrong storage keys fail without replacing protected state. Back up the durable root and any external secret references together. A profile-only export excludes the daemon sidecar and its active authorization coordinator. Official login material already written into the selected profile is copied with that profile, so exported trees remain private account data.

`wx config show` resolves settings without initializing storage. First `wx serve` provisions implicit private token/key files; explicit references must already exist and satisfy private-file checks. Owner API tokens are loaded at startup. `serve` runs in the foreground until Ctrl-C/SIGTERM. `wx daemon start|status|stop|logs`, a supplied service unit, or OpenClaw may own its background lifecycle; choose one owner. Stop flushes durable work and releases ownership without native logout.

Client file: `~/.config/agent-wechat/client.toml` (XDG supported):

```toml
schemaVersion = 1
activeServer = "local"

[servers.local]
url = "http://127.0.0.1:6174"
tokenFile = "/srv/agent-wechat/api-token"
defaultAccountId = "example"
```

Use `--config`, `--server-name`, `--server URL`, `--token-file` or `--account` to override selections. Legacy single-endpoint client files remain readable. Saved defaults belong to their endpoint. Multiple accounts without a deliberate default fail before any operation. API bearer bytes may come from `AGENT_WECHAT_TOKEN`; explicit `--token-file` wins. No token belongs in a URL or command argument. HTTP is restricted to loopback; redirects are refused. Offline `profile` commands do not resolve a token or contact a daemon.

## Select an initialized official profile

With the daemon running, close the official app and use one switch. Never operate them as concurrent writers:

```bash
wx auth login --official
# Explicit path on the daemon host (HOME, xwechat_files, or active account root):
wx auth login --official=/absolute/path/to/profile
```

This discovers or selects the operating files, registers the account, binds its
identity, attempts usable saved-device trust and conditionally presents a QR.
After account/device/password validation, it derives local keys and enables
supported writeback automatically. `--official` keeps data in place; the official
app must stay closed. Discovery refuses multiple profiles. The persisted
`official` storage mode scopes exports to `xwechat_files` and `.xwechat`, excluding
unrelated HOME files and native sidecars. Other profiles are never silently
replaced. See [login workflows](CLI.md#login-and-logout).

Existing installation key-info and saved regional host redirects under
`.xwechat/net/host` are restored for saved-device approval and QR recovery. The complete stored device ID stays unchanged; native requests use
its inspected sixteen-byte prefix. A scanned approval remains bounded by the
login timeout rather than the QR image's display expiry. Login failures separate
key validation from database-open, filesystem and snapshot errors. These guards
do not establish successful takeover of an arbitrary official profile.

## Experimental profile copies and initialization

Import/export and fresh creation are available for experiments; they are outside the initial takeover scope. Export is a byte copy with reported projection gaps, not a schema converter or proof of official-app compatibility.

For lower-level offline registration, stop both writers:

```bash
wx --data-dir /private/native-data store attach --account test \
  --path /private/stopped-official-home --source-stopped
wx --data-dir /private/native-data store list
```

The lower-level direct mode can select a complete official home containing `xwechat_files/<account>/db_storage`, or an existing account directory containing `db_storage`. Managed copy/import should preserve the complete official home and original account directory name:

```bash
wx --data-dir /private/native-data store import --account copied \
  --source /private/stopped-official-home --source-stopped
wx --data-dir /private/native-data store export --account copied \
  --destination /private/new-official-home --source-stopped
```

The destination must not exist and its parent must exist. Copies retain opaque regular-file bytes, WAL/SHM, unknown files, empty directories, modes, mtimes and safe internal relative symlinks. Before/after manifests detect observed source changes; they cannot prove an uncontrolled writer was absent. Unsupported links/special files, oversized copies and overlaps fail. Ownership/ACLs/xattrs and physical hardlink sharing are not preserved. No database conversion or trust refresh occurs.

Exports refuse pending sync batches, cursor/saved-login publication, read updates, metadata receipts and accepted text/media projections. Saved-login receipts must match the exact accepted encrypted authorization generation. Available cached media whose ordinary-file writeback is partial, unsupported or unknown also requires an override, even when its history row is complete. `--allow-incomplete` explicitly copies only the current official files and reports each pending category with `incompleteCopy: true`; media cache gaps appear as `incompleteMediaWriteback`. A received cursor lacking a publication receipt for its exact sealed generation also requires this override. `syncCheckpointProjected: true` records that matching native publication receipt with no pending sync/cursor work; it does not prove stock-client acceptance or the state of separate metadata/read projections. Without that evidence the field is false and `syncCheckpointEvidence` distinguishes pending from unverified. Copying does not restore queued jobs, consumer receipts or the active coordinator. Use `jobs repair-writeback --limit 25` to reconcile already accepted/cached media into supported ordinary files without network transfers or resending; inspect blocked results before export. Keep the whole native durable root for recovery. A failure between import publication and registry commit can leave an unregistered directory; preserve it for inspection rather than deleting it or retrying over it.

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

To experiment with native-only fresh creation, use `wx accounts create --alias test` followed by `wx --account test auth login`. Accepted login material initializes seven supported encrypted databases and saved-login files. Complete official first-run initialization, search indexes and official UI rendering are unverified. Keep existing profiles and their backups.

## Login, sync and reading

```bash
wx serve
# In another terminal:
wx auth login --official=/absolute/stopped-official-home
wx accounts list
wx accounts alias ACCOUNT_ID test
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

Saved-login writeback includes the accepted regional business hosts in the
official routing XML and selector. Reopened profiles therefore retain their route
through `--official`. The recoverable credential journal
also covers these files; interrupted publication remains pending until routing
and credentials both pass readback. Existing unrelated mappings and settings
are preserved, and changed source files are refused.

An accepted active heartbeat resumes the saved session; authenticated expiry permits one renewal. Saved-device approval and explicit QR are separate paths. Transport/integrity failures do not clear credentials or establish trust expiry. Automatic reconnect never opens a QR. Saved credentials restored offline remain unvalidated until an accepted native exchange in this process.

`auth disconnect` stops desired network activity while retaining trust. `auth logout` commits durable local inhibition before one optional authenticated native logout. It reports local commitment, credential retention, remote confirmation/rejection/unknown outcome and unverified saved-device revocation separately. `auth forget --yes` is a separate confirmed local action that retains history. Legacy `auth logout-local` performs only local inhibition. Native active secrets and cursors are stored in encrypted sidecars; native session persistence is not a claim that the official app can reuse them.

For operator-selected standalone credential backup/checkpoints only, offline `store restore-credentials --file /private/state.json [--cursor-file /private/raw-cursor.bin]` installs saved-unvalidated state. This is not an official-profile credential extractor. Raw cursors are opaque bytes, not message IDs or JSON; do not edit their values.

`messages sync` requests one native page. Original command bytes and the received cursor commit together in the native sidecar before projection. The received checkpoint means original commands are recoverable; it does not assert that official files have caught up. Message/session/MMKV databases cannot share one transaction. Exact apply receipts and idempotent reconciliation bridge supported writes; unknown commands remain pending. Offline `store recover-projection` or authenticated `POST /v1/accounts/{account}/projection/recover` retries retained commands without a new send.

Enable the narrow writers only after confirming the actual remote identity and exclusive profile ownership:

```bash
# Both daemon and official app stopped:
wx --account test store enable-writes --own-username OWN_USERNAME \
  --mmkv-store /private/official-home/xwechat_files/all_users/config/global_config \
  --format-key-file /private/mmkv-format-key.bin --official-client-stopped
```

The explicit offline binding commands require a private 16-byte MMKV format-key reference and an identity store inside the selected profile. The bounded read-only v4 decoder verifies framing/CRC, then matches stored username and UIN to native account state and checks the selected account-directory binding. These checks run again before writes. The bundled profile format constant is not an account credential or a security boundary; account material and file permissions remain protected separately. Unsupported layouts refuse writes. Identity binding is separate from consent to write. Accepted saved-device/QR credentials and supported storage projections have their own checked MMKV/SyncKey paths; generic trust extraction and arbitrary-build migration are not implied. Enabling does not waive schema/index/map checks. Live incoming events are emitted only after a supported official projection completes; startup/catch-up observations are labeled separately and do not become new OpenClaw turns.

## Sends, reads and event custody

```bash
wx --account test messages send filehelper --text "A clearly labeled test" \
  --idempotency-key unique-logical-send-id
wx --account test messages send-status SEND_ID
wx --account test messages read THREAD_ID --message-id msg_OPAQUE_ID \
  --expected-version OBSERVED_READ_STATE_VERSION
```

Reuse an idempotency key only for the same destination and payload. A queued send is persisted before submission. Accepted results require matching destination/client identity and a full server ID. Restart during submission becomes `needs_review`; no automatic resend follows an uncertain result. The same logical key returns the same intent, or a conflict for a changed payload. Supported accepted-send rows and media caches use durable projection receipts; unsupported bindings remain explicit gaps.

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
