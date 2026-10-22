# Connected browser preview

`pnpm client:dev` starts the shared React client, its own real Rust hub and the
local synthetic upstream. No production endpoint, CLI configuration or official
profile is read. This is a development preview, not a packaged desktop client.

## Start

Install the pinned development prerequisites from `AGENTS.md`, then run:

```sh
pnpm install --frozen-lockfile
pnpm client:dev
```

The command builds `wx-dev` with the optimized test profile and prints a local
browser URL. Add an account, select **Simulate scan**, then **Simulate phone
approval**. The UI polls the real login operation and opens chats automatically.
A normal QR sign-in likewise requires only the phone's approval. The hub renews
unscanned codes automatically using its bounded refresh policy. The view reads
the login operation's QR generation and expiry timestamp, then fetches the new
PNG when that generation changes. The simulator PNG encodes a valid synthetic
`agentwechat-simulator:login:…` payload, never an official login URL.

The simulator gives each QR 120 seconds. The native protocol currently sets a
local 240-second QR expiry; this is not an expiry timestamp decoded from the
service response. The UI uses the hub's `expiresAt` for **Code refresh in …** and
requests up to three automatic renewals within a nine-minute login operation.
After scanning, the phone approval challenge keeps its own deadline. **Simulate
code expiry**, under **Simulator test controls**, exercises renewal early; normal
expiry requires no button press.

Reopening the same account after a page reload resumes its saved login operation,
including a QR already scanned on the phone. The tab stores only the login
request key and operation ID, scoped to the hub's server ID and account. A lost
login response retries the same key. A temporary lookup error does not start a
second login; an ended challenge can start a new attempt.

Open the URL in two browser tabs and choose the same account. Use **Simulator
controls** to inject incoming text, phone-originated self-sends, an unavailable
image, a rejection, or a lost send acknowledgment. Controls change synthetic
upstream state; messages pass through the actual hub sync/store/event pipeline.
No automatic fake replies are generated.

Text send requests use one stable key each. Queued, rejected and `needs_review`
receipts remain distinct. **Check status** reads the receipt and never resends.
Accepted history is fetched from the hub. The event stream resumes from the last
processed cursor; expired cursors rebuild views before continuing. Account and
conversation generations fence late requests.

**Find in conversation** searches that conversation's stored history on the hub,
including messages outside the browser's loaded page. **Load more matches**
continues the same query using the hub's cursor. Clearing the query restores the
chat; switching accounts or conversations discards any late search response.

The hub access key stays in memory. Pending send keys/text/receipts are retained
in this tab's session storage, scoped to the hub's server ID and account, so a
reload can check an uncertain send. Appearance preferences use local storage.
History and credentials are not copied into browser storage. The bootstrap secret
is saved privately inside the simulator workspace so a named preview can restart
without invalidating its preview link.

Ctrl+C stops only the children created by this command and removes disposable
state. To keep a named synthetic workspace:

```sh
pnpm client:dev --data-dir /path/to/new-simulator-data
```

For tests, `WX_DEV_BINARY` selects an already built `wx-dev`. `CLIENT_UI_PORT`
selects the local Vite port. `CLIENT_UI_ORIGIN` can declare an HTTPS reverse-proxy
origin. It changes the preview's allowed host/origin and browser connection;
it does not connect to another hub. Keep any proxy inside a trusted network.
The launcher refuses a hub whose identity does not say `simulator`.

The browser launcher advances the simulator clock once per second, so QR expiry
and renewal occur naturally. The standalone simulator retains its controlled
virtual clock for repeatable regression tests.

## Authentication and limits

The printed URL carries a random preview bootstrap secret in its fragment. The
browser removes it immediately and exchanges it for an HttpOnly, SameSite cookie
for development endpoints. Host/origin checks and that secret/cookie protect
bootstrap and simulator controls. The separate simulator control token remains
in the launcher. The fragment secret and access keys must not be checked in.

Vite serves development assets and proxies the existing API. There is no second
business backend. A production static host, bundled desktop hub, native file
picker, outgoing files/voice, media previews, agent settings and Tauri packaging
remain follow-ups. The connection form uses the existing bearer SDK and requires
HTTPS or loopback HTTP; cross-origin remote hubs also need an approved browser
CORS arrangement. This work has not connected that form to a live wx daemon.

Incoming media displays the API's available, pending, expired or unavailable
state. Only available bytes offer Download. No raw message XML is rendered as an
attachment. The simulator supplies metadata rather than media bytes, so outgoing
attachments are disabled. Phone read-state synchronization is not claimed.

## UI integration status

| Area             | Current browser behavior                                                                                        |
| ---------------- | --------------------------------------------------------------------------------------------------------------- |
| Hub and accounts | Simulator bootstrap, bearer connection form, account creation/selection and switching                           |
| Sign-in          | QR PNG, expiry countdown, automatic renewal, phone approval and automatic chat entry                            |
| Conversations    | Contacts, thread filters, paginated history, stored-history search and group member details                     |
| Messaging        | Text composer, stable send keys, receipt status, uncertain outcomes, self-sends and SSE replay                  |
| Media            | Incoming availability states and download action; preview rendering, outgoing files and voice remain follow-ups |
| Settings         | Appearance only; account logout, agent configuration and desktop settings remain follow-ups                     |
| Distribution     | Browser development preview; production hosting and Tauri packaging remain follow-ups                           |

## Verification

```sh
pnpm --filter @agent-wechat/client-ui test
pnpm --filter @agent-wechat/shared test
pnpm build
pnpm typecheck
pnpm native:simulator:smoke
pnpm client:smoke
```

The model tests cover late account/thread responses, paginated history and
inventories, canonical content events, send/commit races, stable uncertain sends,
first-frame connection status and cursor recovery. Browser validation uses the
real Rust hub and simulator. Synthetic results do not establish official-client
compatibility or live WeChat acceptance.

Linux x64 verification on 2026-10-22 passed 419 native tests across the compiled
workspace test executables in a non-root, read-only validation container with
external networking disabled. Rust doc tests, pinned format/Clippy and generated
contracts passed. The SDK passed 30 tests, the client models 19 and OpenClaw 211;
TypeScript build/typecheck passed. The 18-check native simulator journey and the
13-check real-session client journey also passed with external networking disabled.
Browser checks covered QR/phone approval, automatic chat entry, two tabs,
self-sends, uncertainty, media availability, automatic QR renewal, decoded QR
payloads, login recovery, stored-history search and viewport layout. This records Linux validation only.
