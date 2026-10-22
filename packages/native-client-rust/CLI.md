# Native CLI workflows

`wx` is the native executable and never starts the official app. Install/provision a matching native
release separately before configuring OpenClaw; invocation never downloads it.
See [installation](../cli/README.md) and [deployment preparation](../../deploy/native/README.md).

## Configuration and process ownership

Server settings and client selections are independent:

- Server: `$XDG_CONFIG_HOME/agent-wechat/server/config.toml`, falling back to
  `~/.config/agent-wechat/server/config.toml`.
- Client: `$XDG_CONFIG_HOME/agent-wechat/client.toml`, falling back to
  `~/.config/agent-wechat/client.toml`. `--config PATH` selects an explicit file.
- Registry and encrypted sidecars: server `dataDir`, defaulting to
  `$XDG_DATA_HOME/agent-wechat` or `~/.local/share/agent-wechat`. An official
  operating profile stays at the path selected with `auth login --official`.

```bash
wx config show
wx serve
# Or choose one background owner:
wx daemon start
wx daemon status
wx daemon logs --follow
wx daemon stop
```

`daemon start` launches the same installed executable, checks authenticated PID,
store identity and generation, and persists a protected process record. It
refuses occupied listeners and never adopts another daemon. `daemon stop`
signals only that verified process, releases locks and keeps login trust/history.
It does not submit native logout. `serve --token-stdin` accepts a bounded API
token through a private pipe without creating a token file; it cannot be
combined with `--token-file` or a configured `tokenFile`. OpenClaw managed mode
uses this foreground-only startup option. Use either this manager, systemd, Compose or
OpenClaw ownership for one process; do not nest owners.

```bash
wx config server add local --url http://127.0.0.1:6174 \
  --token-file /private/api-token
wx config server use local
wx config server list
wx accounts list
wx accounts default ACCOUNT_ID
wx accounts alias ACCOUNT_ID home
wx --account home account status
```

`--server-name`, `--server URL`, `--token-file` and `--account` override saved
selections. Environment selections include `AGENT_WECHAT_CONFIG`,
`AGENT_WECHAT_SERVER`, `AGENT_WECHAT_URL`, `AGENT_WECHAT_TOKEN_FILE`,
`AGENT_WECHAT_TOKEN` and `AGENT_WECHAT_ACCOUNT`. An explicit token-file flag wins
over environment token bytes. Saved account/profile defaults belong to their
endpoint; a new URL cannot inherit an unrelated default. An omitted account is
accepted only when exactly one account is available or a deliberate default
exists. Aliases resolve to canonical daemon account IDs.

## Login and logout

V1 takes over existing official files and continues supported writes in place.
Close the official app and use one switch:

```bash
wx auth login --official
# Or select a full HOME, xwechat_files directory, or its active account directory:
wx auth login --official=/absolute/path/to/profile
```

The path and automatic discovery refer to the **daemon host**. Discovery checks
its HOME for a single Linux profile; missing or multiple profiles require an
explicit path. macOS/Windows profile compatibility is unverified; an explicit
path does not convert their formats or grant OS permissions. See the public
[migration guide](https://thisnick.github.io/agent-wechat/setup/migrate/) for
platform paths and the separate copy/import workflow. Preparing the profile creates/selects a daemon account, binds its
stored identity and takes ownership without copying data or importing an active
session. No separate key export or identity-binding command is required. Saved
trust is tried when usable; QR remains available when trust is absent/rejected.
The full stored device ID, selected key-info and regional host redirects under
`.xwechat/net/host` are retained across that flow. Host selection follows the
saved INI selector and matching XML; only validated native login destinations
are used.
Successful authentication must match username, UIN, device and every encrypted
database before writes are enabled. An explicit `--account` must already refer
to that profile; an unrelated configured default cannot redirect attachment.

This selection is persisted as storage mode `official`, an in-place HOME binding
whose export includes only `xwechat_files` and `.xwechat`. Unrelated HOME files
and native sidecars are excluded. The official app must remain closed while the
daemon owns these files. Open-file detection and native ownership leases help
catch conflicts; they cannot prevent the official app being reopened. Login
without this flag continues using the selected managed/attached account.

After an exclusive handoff, refreshed official saved-login files take precedence
over an older sidecar grant. An unpublished accepted native generation remains
authoritative until its projection completes. No credential source can bypass
account/device/database validation or the explicit logout fence.

```bash
wx --account home auth login
wx --account home auth login --saved-only
wx --account home auth login --strategy saved-device
wx --account home auth login --qr --qr-refresh manual --qr-out /private/new-qr.png
wx --account home --json auth login --detach --idempotency-key deliberate-login
wx --account home auth attempts get OPERATION_ID
wx --account home auth qr OPERATION_ID --output /private/issued-qr.png
wx --account home auth attempts cancel OPERATION_ID
```

Automatic explicit login first tries eligible active/saved trust; QR is a
conditional server result. `--saved-only` forbids a fresh QR, and background
connect never creates one. `--strategy saved-device` deliberately exercises the
saved-device path rather than merely confirming an already active session.
QR refresh is bounded to at most three replacements and the operation deadline;
manual mode stops on expiry. A scanned/approval challenge is not replaced on a
local image timer. Transient polling timeouts/disconnections retain the same
challenge, cookie and QR generation until its expiry or the operation deadline;
protocol/integrity failures and final credential exchanges are not retried this way.

Attached login displays the issued PNG's module grid on white in a terminal.
Without a terminal it writes a private PNG and returns its path. `--json` emits
one result and keeps QR bytes out of JSON. Explicit output paths cannot be
overwritten; new generations get separate filenames. Ctrl-C in attached login
requests cancellation of the same operation and exits130; an unconfirmed cancel
response is labelled explicitly. Detach leaves the bounded attempt running. A private pre-request login receipt retains the request key and operation ID; reusing `--idempotency-key` resumes that operation rather than starting a replacement.

```bash
wx --account home account connect
wx --account home account disconnect
wx --account home auth logout --idempotency-key deliberate-logout
wx --account home auth forget --yes --idempotency-key deliberate-local-forget
```

Disconnect retains reusable credentials. Logout commits persistent local
inhibition before at most one authenticated native logout; remote acceptance,
rejection, no attempt and unknown outcome are separate results. It never renews
or logs in just to complete logout. Saved-device revocation remains unverified.
Forgetting is separately confirmed, removes local authentication and retains
history, device identity, media keys and uncertain intents. None proves deletion
from backups or the phone's device inventory.

## History, metadata and media

```bash
wx --account home chats list --limit 20
wx --account home chats get filehelper
wx --account home contacts find Example
wx --account home groups get 'room@chatroom'
wx --account home groups members 'room@chatroom'
wx --account home messages list filehelper --limit 50
wx --account home messages get filehelper MESSAGE_ID
wx --account home messages media filehelper MESSAGE_ID \
  --quality original --output /private/new-image.jpg
wx --account home contacts avatar USERNAME --output /private/new-avatar.jpg
```

Listing history never marks it read or opens a GUI. Online cursors bind the
account, query, ordering and a retained read view. Keep the original query and
cursor; an expired cursor requires an explicit new view. Deleted members remain
`tombstone` items (`deleted:true`) rather than shifting a page silently. Offline
profile cursors instead bind the source generation and expire after source
change.

Media distinguishes advertised and verified actual quality. `best` ranks cached
original, standard/normal, then thumbnail; an explicit original/standard request
cannot return a lower quality. `--cached-only` never queues a transfer. Otherwise
a durable job may remain pending; inspect `jobs get JOB_ID` rather than inventing
an attachment or repeatedly submitting work. Available content is bounded and
hash/size checked before a new private output file is published. Cached bytes
can remain available after remote expiry; a confirmed expiry is terminal.

```bash
wx --account home contacts refresh USERNAME
wx --account home groups refresh 'room@chatroom'
wx --account home contacts sync
wx --account home groups sync
wx --account home contacts avatar USERNAME --refresh
wx --account home sync --detach
wx --account home operations get OPERATION_ID
```

Metadata refreshes are durable known-ID operations; groups do not require a
message row. Sync coverage remains partial and is not a complete thread list.
Unknown/nonmember group IDs do not imply creation or joining. Check current
account capabilities and operation results rather than treating route presence
as successful server resolution.

Contact sync resumes its saved inventory after a time or page bound. Inspect
`catalogComplete` separately from `complete`: the catalog can finish while
`unavailableCount` reports contacts explicitly rejected by the current lookup.
Those entries do not block later contacts or imply deletion. A later explicit
sync retries a bounded batch; a successful exact refresh clears its rejection.
Unexplained empty records remain errors, with the verified reply retained in
the encrypted sidecar for diagnosis.

To reconcile media already retained by this daemon into supported official files:

```bash
wx --account home jobs repair-writeback --limit 25 --wait 30
wx --account home jobs repair-writeback --limit 100 --detach
wx --account home operations get OPERATION_ID
```

The limit is1–100 (default25); `--wait` is seconds (default30), and `--detach`
returns the durable operation immediately. Results report `scanned`, `repaired`,
`blocked` and `hasMore`. Repair uses only owned cached bytes and accepted-send
receipts: it never downloads or resends, and can run after logout. A blocked
unsupported binding remains an explicit export gap; inspect it rather than
assuming a successful operation means every candidate was repaired.

## Durable sends and reads

```bash
wx --account home messages send filehelper --text "A deliberate message" \
  --idempotency-key one-logical-intent
wx --account home messages send filehelper --image /private/photo.png --detach
wx --account home messages send filehelper --file /private/report.pdf
wx --account home messages send filehelper --video /private/clip.mp4
wx --account home messages send filehelper --voice /private/audio.wav
wx --account home messages sends lookup --idempotency-key one-logical-intent
wx --account home messages voice status JOB_ID
wx --account home jobs cancel JOB_ID
```

Each invocation is a new intent unless the caller deliberately reuses a key.
Before its first submission request, the CLI fsyncs a private receipt containing
endpoint, canonical account, destination, payload hash and key; message text is
excluded. Retain that receipt after a lost response. Reusing its key/input
returns the same resource; changed input is a conflict. Accepted means native
server acceptance, not recipient delivery. `needs_review`/partial voice outcomes
retain completed note IDs and must not automatically resend.

The current native source cap is25MiB−17bytes, below the separate128MiB cached
HTTP bound. Voice uses independent SILK encoding and splits at10seconds; video
requires supported MP4 input and server-side FFmpeg preparation. The runtime
image includes FFmpeg/FFprobe; direct installs must provision these tools for
source normalization. Large-file branches and sticker sending are unavailable.

```bash
wx --account home chats read filehelper --expected-version VERSION
wx --account home messages read filehelper --expected-version VERSION
wx --account home events watch --cursor-file /private/events.cursor.json
wx --account home events ack --consumer deliberate-consumer --through CURSOR
```

Read marks the whole thread through the latest observed local message. The
expected version prevents clearing a different snapshot, and rows with newer timestamps stay
unread. The native cutoff has one-second precision. The result distinguishes local projection from accepted, rejected or
uncertain cross-device notification. Uncertain notifications are not resent.
Watch emits NDJSON;
its cursor file records successful local emission, not model consumption.
Consumer ACK is a separate explicit operation. Interrupting a send watcher
leaves daemon work running; only attached login explicitly requests cancel.

## Offline profiles

These commands read files directly without a daemon, network or API bearer:

```bash
wx profile add archive --path /private/stopped-account \
  --keys-file /private/profile-keys.json
wx profile use archive
wx profile inspect
wx profile contacts --source-stopped
wx profile groups --id 'room@chatroom' --members --source-stopped
wx profile messages --chat filehelper --source-stopped
wx profile images --message OBSERVATION_ID --quality original \
  --output /private/new-image.jpg --source-stopped
wx profile avatars --username USERNAME --output /private/new-avatar.jpg --source-stopped
wx profile keys export archive --password-file /private/account-password.bin \
  --output /private/new-keys.json --source-stopped
```

Register one account directory containing `db_storage`, or supply an explicit
`--account-dir` inside a selected home. Missing key references may be registered
and show `keys_missing`; content reads fail explicitly until keys are available.
Keyless inspect reports bounded filesystem inventory only. Readers require a
stopped/consistent-source assertion and preserve source files. `--raw` is an
explicit private-data choice; default projections omit raw transport/XML fields.
Message observation IDs are bound to profile and source generation; they cannot
be reused after the view changes. Output derivatives/keys must remain outside
the source tree, and no output is overwritten.

For manual low-level attachment of an existing populated daemon profile, bind
its independent stored identity before login using offline `store bind-identity --mmkv-store PATH
--format-key-file PATH --official-client-stopped`. This is distinct from consent
to enable writers. Copy-only `store attach|import|export` and experimental fresh-profile
configuration are covered in [README](README.md). Fresh creation, export and
returning files to the official app are outside the v1 acceptance scope.

`store export` requires a stopped/consistent source and refuses pending sync,
cursor, saved-login, read, metadata and accepted-send projections, including cached media
whose official-file writeback is incomplete. `--allow-incomplete`
preserves the current official bytes and reports the remaining categories; it
does not drain or discard native work. Its checkpoint field reports a durable
publication receipt separately from complete takeover compatibility. Official
saved-login material may be copied, while daemon sidecars and queued jobs are
excluded. Preserve the complete durable root for a native backup.

## Machine results and verification

`--json` emits one finite structured result; watch emits one JSON event per line.
Exit0 is read/action success or durable acceptance with detach. Exit2 is invalid
input/configuration/selection;3 unavailable endpoint;4 API access;5 login needed;
6 human action;7 a local wait deadline;8 an uncertain outcome;9 a declared terminal
failure;130 interruption. IDs and receipts survive wait deadlines/interruption.
Inspect structured state/reasons; an exit number never permits resending.

Run the actual binary lifecycle and synthetic transport tests without credentials:

```bash
python3 scripts/native-cli-smoke.py /absolute/path/to/wx
python3 scripts/native-cli-fixture-test.py /absolute/path/to/wx
python3 scripts/native-export-smoke.py /absolute/path/to/wx
bash scripts/native-generate-types.sh --check
```

The shared TypeScript wire types come from Rust contracts, including actual
media serde types. Regenerate with `scripts/native-generate-types.sh`; CI rejects
stale generated output. These checks do not establish compatibility with an
arbitrary official client build or prove recipient playback.
