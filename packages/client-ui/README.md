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
controls** to inject incoming text, phone-originated self-sends, incoming image/file/video/voice or an animated sticker, an unavailable image, a rejection, or a lost send acknowledgment. Controls change synthetic
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
business backend. A production static host, bundled desktop hub, agent settings
and Tauri packaging remain follow-ups. The connection form uses the existing
bearer SDK and requires HTTPS or loopback HTTP; cross-origin remote hubs also
need an approved browser CORS arrangement. This work has not connected that form
to a live wx daemon.

## Attachments and stickers

Incoming images, files, videos and voice messages use real hub transfer jobs.
**Load attachment** retrieves the selected quality; progress, cancellation and
retry use the job API. Verified images render inline, videos and decoded voice
use browser playback controls, and files can be downloaded. Other clients see
ready assets through the hub event stream. Unsupported, pending and expired
assets keep their actual state; raw message XML is never a retrieved attachment.

Eligible incoming stickers load automatically when their message is displayed.
The browser renders the checked original image, including GIF animation. The
simulator provides an account-scoped two-frame GIF through its local adapter;
it never fetches the synthetic sticker URL. Native sticker downloads keep their
restricted source, integrity, size and frame checks. Sticker sending, packs,
favorites and an outgoing sticker picker are not implemented.

**Attach file** opens a review with the recipient and attachment type. The hub
accepts sources up to 25 MiB. Choose file, image, video or voice; voice preparation
and splitting happen in the existing Rust job pipeline. The client retains only
source IDs, filenames and job receipts across reload, never file bytes. Rejected,
partial and uncertain outcomes remain distinct. **Check status** only reads the
same receipt; uncertain sends are never retried automatically.

The simulator now supplies checked PNG, text, MP4 and SILK fixtures, and accepts
outgoing media through its decoded local transport. Production preparation,
readback, idempotency, cancellation, logout fences and official-format history
writes still run in the real hub. These checks do not establish live WeChat
acceptance or recipient playback.

## New chats and groups

**New conversation** opens a contact picker for a direct message. **New group**
lets you enter a name, select at least two other contacts and review the selected
count before creating a synthetic group. The current simulator accepts up to
39 invitees; this is a development limit, not a claim about WeChat's service limits.
The account's own contact and File Transfer are excluded from group selection.

Creation uses an admin-only operation and a stable request key. After a lost
response or reload, the client checks the same operation/key without creating
again. The store checks authorization and logout state atomically before its one
submission. Interrupted submissions become `needs_review`; queued requests that
never reached submission fail without being replayed. The created group, system
notice, roster and messages use the real hub profile and event pipeline.

This is enabled only for the simulator. Native capabilities report
`groupCreation: false` and reject creation before any service call. Creating a
real WeChat group, its per-member results and any required invitations still need
a native implementation and separately authorized final testing. The UI does
not present these as supported. System notices render as text and do not start
OpenClaw agent turns.

## Accounts and conversation details

Settings can rename the selected account and sign it out, with confirmation and
same-operation status recovery after a lost response or reload. Sign-out keeps
history. Conversation details provide **Mark as read** with the hub's version
check and phone-notification receipt, plus **Refresh avatar**. Cached avatars
replace initials; absent avatars keep initials. A successful receipt reports
server acceptance, not observation of a real phone badge. Group details load all
roster pages.

| Area             | Current browser behavior                                                                                                |
| ---------------- | ----------------------------------------------------------------------------------------------------------------------- |
| Hub and accounts | Simulator bootstrap, bearer connection, account creation/switching, rename and logout                                   |
| Sign-in          | QR PNG, countdown, automatic renewal, phone approval and automatic chat entry                                           |
| Conversations    | Contacts, thread filters, paginated history/search, full group roster, read-state, avatars and simulator group creation |
| Messaging        | Text and attachment composer, progress, receipt status, uncertainty, self-sends and SSE replay                          |
| Media            | Image/video/voice previews, downloads, incoming animated stickers and outgoing file/image/video/voice jobs              |
| Settings         | Appearance and account controls; agent configuration and desktop settings remain follow-ups                             |
| Distribution     | Browser development preview; production hosting and Tauri packaging remain follow-ups                                   |

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

The extended client journey runs in a non-root, read-only container with external
networking disabled. It includes incoming media integrity/ranges/account isolation,
animated GIF frames, outgoing preparation and readback, rejection and lost replies,
receipt recovery, account aliases, versioned read-state, cached avatars and logout.
Pinned Rust format/Clippy, native tests and generated contracts are required for
changes to the decoded transport. Browser checks exercise the same Rust simulator;
no live endpoint is used.

Linux x64 validation of this pass: 422 native tests across 20 executables passed
in a non-root, read-only container with external networking disabled. Rust doc
tests, pinned format/Clippy and generated contracts passed. The client passed
28 model tests, the SDK 30 and OpenClaw 211; workspace build/typecheck passed.
The 17-check client journey passed in the isolated container. Browser checks
confirmed automatic incoming sticker rendering, attachment review and an
accepted outgoing file appearing in canonical history. Desktop packaging and
live endpoint testing remain unverified.

After the group-creation addition and rebase onto `main`, 425 native tests passed
across the isolated workspace executables. The client passed 32 model tests,
the SDK 31 and OpenClaw 211. Both simulator journeys passed (18 check groups each),
including group identity/roster projection, account isolation and recovery of a
lost group response without another creation. Browser validation created a
synthetic group with two selected contacts, opened its three-member roster and
sent a synthetic group message. UI copy now says **agent-wechat hub** throughout.
