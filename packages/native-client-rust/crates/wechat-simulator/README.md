# Local upstream simulator

The development simulator runs the actual Rust hub, authenticated API, account
workers, encrypted profiles, outbox, projection journals and durable event/SSE
stream. An injected upstream adapter exchanges local JSON with a separate Rust
scenario service. The scenario service owns a separate SQLite database.

This command provides an API/SDK development environment. For the connected
browser preview, use `pnpm client:dev`; see [the client preview guide](client-ui.md).
Production web hosting and the Tauri shell remain follow-up work.

## Start

Use the pinned Rust toolchain and the native compiler dependencies described in
the native README. Docker and production account credentials are unnecessary.

From the repository root:

```sh
pnpm native:dev
```

The equivalent command, without pnpm or Node:

```sh
cargo run --profile test --locked --manifest-path packages/native-client-rust/Cargo.toml -p wechat-simulator --bin wx-dev -- dev
```

The optimized test profile keeps debug assertions and makes encrypted-profile
initialization practical. The launcher creates private temporary state, starts
and health-checks the scenario service and hub, and prints their addresses and
private token-file paths. It prints no token values. This is the hub API URL;
the command does not yet serve a frontend.

Ctrl+C stops the owned children and removes disposable state. Use `--keep-state`
to retain the automatically allocated directory, or pass `--data-dir PATH` to
reuse a named development directory. For example:

```sh
pnpm native:dev --data-dir ./local-simulator
pnpm native:dev --data-dir ./local-simulator --reset
```

Only a new/empty directory or one marked by this development runtime is accepted.
Reset requires both services to be stopped and only removes the simulator's hub
and upstream databases. Token files are retained. The supported scenario is
`basic`; unknown scenario names are rejected.

## SDK and scenario controls

Use the existing `NativeWeChatClient` with the printed hub `endpoint` and token
read from `tokenFile`. Create a synthetic account such as `personal`, then start
its ordinary login operation with strategy `qr`.

The QR image is synthetic and has no real WeChat login URL. Scan/approval is
controlled using authenticated `POST /control` on the printed upstream address.
Read the separate `controlTokenFile` for that request's bearer token. These
controls are absent from the production hub router.

Representative JSON control bodies:

```json
{"op":"login","account":"personal","status":"scanned"}
{"op":"login","account":"personal","status":"approved"}
{"op":"inbound","account":"personal","peer":"sim_mei","body":"Synthetic incoming message"}
{"op":"inbound","account":"personal","peer":"sim_mei","body":"Synthetic account-originated message","self_sent":true}
{"op":"inbound","account":"personal","peer":"sim_mei","body":"Catch up after restart","notify":false}
{"op":"notify","account":"personal","count":2}
{"op":"echo_send","account":"personal","client_id":123}
{"op":"status_notification","account":"personal","operation_id":2}
{"op":"status_notification","account":"personal","operation_id":11}
{"op":"send_fault","account":"personal","fault":"reject"}
{"op":"send_fault","account":"personal","fault":"lost_acknowledgment"}
{"op":"advance","seconds":121}
{"op":"revoke","account":"personal"}
{"op":"unknown_sync","account":"personal"}
{"op":"advertise_image","account":"personal","peer":"sim_mei"}
{"op":"receive_media","account":"personal","peer":"sim_mei","kind":"sticker"}
```

Account IDs contain lowercase ASCII letters, digits, underscores or hyphens, up
to 32 characters. Synthetic peers are `sim_mei`, `sim_daniel` and `filehelper`.
The service seeds the account's contacts and initial conversation on first login.
Each account has independent sessions, changes, notification positions and send
receipts. `GET /state` returns a bounded scenario summary with no credentials.

`advance` controls upstream challenge expiry and synthetic incoming timestamps.
Hub deadlines, worker scheduling and SSE polling still use their normal clocks.
Login cancellation and operation deadlines remain enforced by the real hub.

Text sends use the real durable outbox. Accepted sends are stored in the
simulator's persistent remote message history and are **not echoed into this
hub session's sync stream by default**. The hub commits them from the send
receipt. `echo_send` explicitly injects an abnormal late/duplicate sync echo for
deduplication testing; supply the send receipt's numeric `clientId`.

A lost acknowledgment records upstream
acceptance and then returns a simulated failure; the hub retains `needs_review`
and does not resubmit the send. An outgoing sync echo does not by itself resolve
that uncertain receipt. Successful sends and echoed messages reconcile through
their canonical history identity. Queued text sends now publish `send.updated`
atomically with admission, so independent clients can observe them immediately.
Logout holds and interrupted-send recovery publish the corresponding state
changes atomically as well.

## Message events and self-sends

Clients render incoming and outgoing messages, including catch-up history. After
`message.committed`, fetch the message using its `messageId`; that event contains
no message body. `sync.received` can follow the message event and is not a
prerequisite for displaying it.

| Event classification | Meaning | Agent eligibility |
| --- | --- | --- |
| `incoming` + `live` | A new message from another account | Eligible, subject to existing access/mention rules |
| `outgoing` + `live` | A self-sent message arriving through upstream sync | Ignore |
| `outgoing` + `local_send` | A send accepted through this hub | Ignore |
| Any direction + `catchup` | History synced after reconnect | Ignore |

Both outgoing kinds have the account's own `senderId` on the fetched message.
A phone-originated message to `filehelper` is a note to self; it does not enable
a bot-command feature. `local_send` identifies hub-originated messages, without
identifying which connected UI or agent initiated the send. The existing
OpenClaw dispatch gate already requires `incoming` plus `live`.

`status_notification` injects currently unhandled type-51 operation 2 or 11.
These remain staged with projection diagnostics and block publication of a
complete official cursor. Later normal messages still commit and become
readable. Clients should keep such diagnostics separate from the message feed;
phone read-state synchronization for these controls is not implemented here.

## Isolation and current limits

- The development executable is `wx-dev`; normal `wx serve` has no simulator
  switch. Native startup refuses a marked simulator store.
- Simulator startup requires a marked, isolated store and managed profiles.
  Attaching official profiles and importing native credentials are rejected.
- Adapter addresses are numeric loopback socket addresses. Redirects and proxy
  forwarding are disabled for these local simulator requests. The service and
  hub bind only to loopback and use separate private bearer tokens.
- The mock uses decoded sync batches and existing command codecs. It does not
  implement WeChat wire encryption or establish protocol compatibility.
- Supported operations are synthetic QR/saved-session login, heartbeat, sync,
  text/media sends, push hints, read notifications, contact/avatar refresh and
  logout. Incoming fixture kinds are image, file, video, voice and animated sticker.
  Outgoing kinds are image, file, video and voice. Sources, preparation, jobs,
  readback and history still use the production hub pipeline. Media objects and
  send acceptance are account-scoped in the separate simulator database.
- Unsupported network operations fail closed, including native-client construction,
  HTTP link thumbnails, arbitrary CDN/URL retrieval, shares and recalls. Avatar
  and sticker retrieval accept only exact local fixture identities. The sticker
  URL is metadata only and is never resolved or fetched by the simulator.
  `advertise_image` continues to exercise an unavailable image with no owned bytes;
  `receive_media` instead supplies checked local bytes, without marking them
  available merely because a notification arrived.
- Build dependencies may need downloading. The running scenario uses only local
  services. The regression journey can also run in a container with networking
  disabled to verify runtime isolation.
- An outgoing message from another simulated device tests classification and
  projection. Live official-client self-send delivery, timing and coexistence
  require separate evidence.

## Verification

```sh
pnpm native:simulator:smoke
```

The smoke starts actual `wx-dev upstream` and `wx-dev hub` child processes. It
checks the actual `wx` CLI's status and message fetch, and uses the existing
TypeScript SDK to verify QR scan/approval, encrypted-profile
projection, two independent SSE consumers, queued/accepted/uncertain sends,
idempotency and explicit fault-injected sync-echo deduplication, self-sent live
messages to File Transfer, incoming push, cursor resume, hub restart
and catch-up, account isolation, session revocation/logout, virtual-clock QR
expiry, unavailable advertised media, status-control diagnostics without blocking
later messages, retention of unknown sync commands, and launcher cleanup.
`WX_DEV_BINARY` and `WX_BINARY` environment variables can select already built
development and CLI executables; by default both come from `target/debug`.

Rust tests cover persistent remote acceptance after a lost acknowledgment and
service restart, deterministic fixtures, clock controls and store/process
isolation. They complement the existing native protocol/core/profile/API tests.
No synthetic result establishes official-client rendering or live acceptance.

For an already built Linux workspace, Docker can verify that the journey works
with external networking disabled. Pull the Node image during environment setup,
then run:

```sh
docker run --rm --network none --read-only \
  --user "$(id -u):$(id -g)" --tmpfs /tmp:rw,nosuid,nodev,mode=1777 \
  --mount "type=bind,src=$PWD,dst=/app,readonly" --workdir /app \
  node:24-trixie-slim node scripts/native-simulator-smoke.mjs
```

Linux x64 validation on 2026-10-22 passed the full Rust workspace suite, pinned
format/Clippy checks, generated-contract checks, TypeScript checks, SDK tests,
and OpenClaw tests. The complete simulator journey also passed in a read-only,
non-root validation image with `--network none`; a connection-only system trace
recorded only loopback addresses. Linux ARM64, macOS ARM64 and Windows x64/ARM64
runtime checks are configured in CI but have not been run in this workspace.
