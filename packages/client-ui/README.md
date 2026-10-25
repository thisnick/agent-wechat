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

The hub access key stays in memory. IndexedDB retains recent history, contacts,
verified media bytes, avatars, drafts and pending-operation receipts. Each record
is scoped to the hub address, its persistent server ID and the account. IDs and
cursors stay strings, including 64-bit server IDs. Pending sends are checked by
receipt after a reload and are never automatically resent. Small preferences,
the last account, conversation and scroll anchor use local storage.

Returning to a valid web session opens the last signed-in account and conversation
automatically. Authentication is checked with the hub before restoring its cache.
Credentials are not copied into either browser store. **Settings → Local cache**
shows usage and clears fetched data while preserving drafts and pending receipts.
If storage is unavailable, the client keeps working with memory and tab storage. The bootstrap secret
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

To connect the preview to an existing native daemon, select it deliberately with
the separate launcher:

```bash
pnpm --filter @agent-wechat/client-ui connect \
  --server http://127.0.0.1:6174 \
  --account ACCOUNT_ID \
  --token-file /absolute/private/browser-token \
  --state-dir /absolute/private/browser-preview
```

Use an owner-only file containing the existing daemon connection key. The key stays
in the launcher; the browser uses the private preview key. The launcher checks the
native daemon identity before listening. `--account` optionally checks that a
starting account exists; it does not restrict profile management. Account creation,
sign-in, sign-out and confirmed removal use the normal account API. The daemon's
existing token checks still apply; no frontend role or special management mode is
introduced. Token and daemon-management routes remain blocked by the proxy.

The launcher accepts `CLIENT_UI_PORT` and HTTPS `CLIENT_UI_ORIGIN`. Keep the printed
fragment link private. Closing the launcher leaves the external daemon running.
`node scripts/client-ui-connect-smoke.mjs` checks the proxy with a local HTTP fixture.

**Manage accounts** opens the chooser, where each profile has a settings button.
Signed-out profiles can sign in or be removed. Removal requires typing the profile's
current name and sends its canonical ID as confirmation. The backend rejects active
work and signed-in profiles, unregisters the profile, and retains all local files and
recovery records. Removed IDs and profile paths stay reserved; this is not a delete
of the stored WeChat data or a one-click restore feature.

The simulator launcher advances its clock once per second, so QR expiry
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
need an approved browser CORS arrangement. Prefer the same-origin proxy above
when connecting this preview to an explicitly selected native daemon.

## Attachments and stickers

Focusing or clicking the message composer clears the selected thread’s unread count
against its displayed read-state version. Concurrent focus and reply actions share
one request, and a conflict never clears a newer arrival automatically.

Replies show the sender and embedded excerpt from `message.quote` above the comment.
Rendering text or link quotes makes no quote-resolution request and never substitutes
a stored original's content. Image quotes show a small standard preview using the
parent reply's media identity. Recalled or unavailable quotes keep a short status
label. Forwarded history renders normalized text. Unknown message types
show an unsupported-content label instead of entering the media flow.

Images and stickers show their content without a file header or status text.
Timestamps beside the bubble and message actions above its right edge appear on
hover or keyboard focus. Sent messages have no sender label; received messages
keep the sender name. Timestamps sit to the left of sent bubbles and to the right
of received bubbles. Touch devices
keep the actions accessible. The menu offers copying text or opening a photo;
own messages also offer **Recall** with a distinct message-removal icon. It opens a
confirmation dialog showing the selected message; Cancel and Escape send no request.
Recall keeps its admission key and operation
receipt across reloads. Unknown outcomes offer a status check instead of starting
another attempt. The hub decides whether the exact message can be recalled.
Recalled bubbles use the API's derived original sender and direction for placement.
Missing sender or direction produces a neutral notice with no avatar. The text comes
from `recall.replacementText`, falling back to **Message recalled**; the UI does not
infer a recall initiator from the original author, a control sender, or notice text.
The sidebar uses the same notice. The merged native recall implementation owns
author recovery, notice storage and search cleanup; the browser adds no author ledger.
The **Reply** action selects a supported message for the text composer. Its preview
can jump to the original or be dismissed without losing the draft. Text and quote
selections are scoped to each account and chat. Sending passes only the stored
message ID and exact server ID to the native quoted-text API. Pending and uncertain
sends retain that reference; checking status does not submit another send. A known
recalled or changed source blocks sending until the quote is removed or replaced.
Attachments require cancelling the selected text reply. The simulator exercises the
same daemon admission, projection, event stream and embedded quote contracts; official endpoint
acceptance and recipient rendering remain separate final checks.

Incoming images automatically load standard quality. Clicking a photo or video opens a
shared viewer with previous/next controls, a thumbnail strip, and **Show message**.
Images zoom and pan on the full viewer canvas, behind the fixed navigation arrows
and thumbnail tray. Animated zoom, drag panning, wheel and pinch gestures use
`react-zoom-pan-pinch`; **Reset zoom** returns to the centered fit view. A single image
click toggles zoom; dragging pans without toggling. Enter or Space on the image canvas
also toggles zoom, and `+`, `-`, and `0` zoom in, out, and reset. Reduced-motion preferences
disable zoom animation and pan inertia. Videos have native playback controls. Video thumbnails
load separately from playback, so browsing the strip does not fetch full videos.
Earlier photos and videos load through the conversation's history cursor as needed. The
viewer keeps **Download** available beside **Get original** and its transfer progress.
**Get original** uses a cloud-download icon, then a spinner and **Getting original…**.
Its progress has a **Cancel** text action; the close icon closes the viewer.
Once the original is cached, **Download** saves that original to this device. Quality
and size appear only when the download control is hovered or focused, as
“Compressed” or “Original.” When no original source is provided, only standard can
be downloaded; the download tooltip explains this. Transfer progress
and cancellation appear while it runs. The conversation keeps displaying standard
quality even after an original is downloaded. The carousel first shows standard,
then replaces it with a verified cached original after its bytes load and decode.
A failed upgrade leaves the standard visible. Opening the carousel does not
request an original from WeChat. An unavailable image shows an icon.

Chat messages, the main viewer and its strip share one preview cache. Requests for the same account, asset, quality and source hash are deduplicated.
Idle previews are retained within a 64 MiB / 128-entry budget; visible previews stay
pinned until their consumers close. Reopening a chat or viewer reuses retained blob
URLs immediately. Recalls invalidate the message's preview entries, and sign-out or
disconnect clears the relevant account or connection. Verified preview bytes also survive reloads in IndexedDB, within a 256 MiB
least-recently-used budget shared with avatars. The client reads fresh asset metadata
from the hub before reusing bytes for the same account, message, quality and content
hash. Blob URLs and temporary content URLs are not persisted.

Files show their filename and an always-visible download control. **Get original**
makes the original available for download, then **Download** saves it to the device. Generic
files, including PDFs, do not render an image placeholder or an inline PDF viewer.

Clicking a quote finds its server ID in the same account and conversation, loading
older history pages only when needed, then scrolls to its source. This navigation
does not change the displayed embedded excerpt. Missing or recalled originals retain their state. Searches can
jump back into the conversation without losing its loaded history.

The wx daemon downloads new voice notes on receipt, independently of the browser.
Visible voice notes load those bytes into browser playback controls; older notes
still have a lazy download fallback. Files and
videos use the same compact download action and transfer jobs. Unsupported,
pending and expired assets retain their actual state; raw XML is never an
attachment. The conversation and viewer share transfer state, so opening a viewer
does not start another transfer.

One shared avatar component tracks visibility in lists, group mosaics, details
and messages. Missing visible avatars are fetched immediately; cached visible
avatars refresh after one hour since their last successful hub read. Reloading
restores the previous timestamp rather than restarting that hour.
Offscreen avatars do not refresh. Multiple views share each request, and queued
refreshes are skipped if the last visible view disappears. Group rows show at most
four member avatars and request only those faces. Refreshes pause while chat-open
read state is being admitted. Avatar reads and media discovery
start at the viewport so offscreen history does not flood the hub.

Eligible incoming stickers load automatically when their message is displayed.
The browser renders the checked original image, including GIF animation. The
simulator provides an account-scoped two-frame GIF through its local adapter;
it never fetches the synthetic sticker URL. Native sticker downloads keep their
restricted source, integrity, size and frame checks. Sticker sending, packs,
favorites and an outgoing sticker picker are not implemented.

**Attach file** opens a review with the recipient and attachment type. The browser preview
accepts sources up to 25 MiB; the native backend has separate per-kind limits. Choose file, image, video or voice; voice preparation
and splitting happen in the existing Rust job pipeline. The client retains only
source IDs, filenames and job receipts across reload, never file bytes. Rejected,
partial and uncertain outcomes remain distinct. **Check status** only reads the
same receipt; uncertain sends are never retried automatically.

The simulator now supplies checked PNG, text, MP4 and SILK fixtures, and accepts
outgoing media through its decoded local transport. Production preparation,
readback, idempotency, cancellation, logout fences and official-format history
writes still run in the real hub. These checks do not establish live WeChat
acceptance or recipient playback.

## Opening a conversation

Recent chat history stays cached across switches and reloads: up to 20 chats and
5,000 messages in the working set, evicting the least recently used chats. Cached
sidebars appear while the client checks fresh thread data. On return, it catches
up missed events before displaying cached message bodies and validates the first
history page against the current source generation. Source changes, expired event
cursors and a reset event journal discard old fetched data. Recalls remove cached
media and redact cached quote excerpts. Excessive catch-up falls back to fresh pages.

Within a continuous event stream, read cached chats reopen without another history
request. Returning to a foreground tab also refreshes the selected chat and thread
badges. Scroll positions restore when their message is still in the retained pages;
restoring a position does not fetch additional history. Draft text and quote
references survive reloads; quote references are revalidated before use.

The client marks unread history as read after it successfully loads the selected
chat. It fetches a fresh read version before loading; a conflict reloads both
version and history once before trying again. It never retries against unseen
messages by swapping in a newer version alone. Repeated conflicts report an error.
Failed loads and responses from a previously selected chat do not clear badges.
Text replies, quoted replies, and attachment sends also acknowledge the loaded chat's
current read version. That acknowledgment runs separately from sending; read failures
never resubmit a reply. A conflict refreshes the badge and keeps newer arrivals unread.
Cross-device read synchronization is handled by the hub.

Message events refresh the changed message and thread only. Read-state events
refresh that thread's badge, including remote reads, and group updates refresh
the affected group. Avatar metadata events do not reload contact inventories.
Contact/capability loading and group details do not block the initial history.
The first page contains 40 messages; older pages contain 100. The current hub
pagination builds a complete conversation snapshot before slicing a page, so
cold opens still include that server-side cost.

The conversation uses the headless shadcn Message Scroller. Opening a chat starts
at the bottom and follows images and other content as they load. Scrolling away
releases that follow; **Latest messages** resumes it. Loading older messages keeps
the visible message in place. Search uses a separate view.

Group avatars first use cached hub bytes. Opening a group lazily requests missing
avatars for visible message senders and the header collage; opening its member
list does the same for visible rows. These requests use the existing per-person
avatar refresh operation, with deduplication and at most four refreshes at once.
The client does not download the entire roster on group open. Missing or unavailable
photos keep initials.

## New chats and groups

The conversation list uses `lastMessage` and `avatarMembers` from the threads API.
Chats are sorted newest message first. Incoming and accepted outgoing updates
reorder the list through the event stream; equal or missing times retain their
existing relative order.
Direct chats show one avatar; groups show a collage of up to four saved members,
with a group icon when roster data is missing. The second line previews the latest
saved message, prefixes outgoing text with “You” and names incoming group senders.
Media gets a short label. Empty and unavailable local history have distinct states.
These summaries are read from the current profile snapshot after pagination; they
do not add stored tables, start network refreshes or fetch attachments.

**New conversation** opens a contact picker for a direct message. **New group**
lets you optionally enter a name and select 2–40 other contacts. Your own contact
and File Transfer are excluded. The UI uses the hub's `groupMutations`
capabilities and existing admin-scoped group API.

**Conversation details** shows saved group information and all saved member pages.
Use it to rename a group, edit your own display name, add/remove members,
mute/unmute notifications, clear an announcement or leave. Leaving asks for
confirmation, preserves cached history and disables new sends after membership
is confirmed ended. WeChat checks permissions and returns
member results separately. General announcement editing, ownership transfer and
invitations beyond the merged ordinary-group API are not offered.

Each change keeps a stable key and operation receipt, scoped to the server,
account and group. Reload and **Check status** only read that receipt/key. Unknown
or `needs_review` outcomes are retained and never resubmitted automatically.
Partial member results and unconfirmed members remain visible. Cancellation is
available only before the backend's first submission step.

**Refresh group** requests current group details and members, then reloads the
saved view. `group.updated` events also refresh it. Other-device updates are
partial, so automatic delivery of every phone edit is not promised. Unknown
membership stays unknown and edits require confirmed current membership.

The development preview implements these requests in the local simulator through
the same Core admission, one-attempt operation journal, codec validation and
profile projection as the native backend. Only local fixture replies are
substituted. No official endpoint has been used to validate this UI. Synthetic
notices make new mock groups visible as conversations; system notices do not
start OpenClaw agent turns.

## Accounts and conversation details

Settings can rename the selected account and sign it out, with confirmation and
same-operation status recovery after a lost response or reload. Sign-out keeps
history. Conversation details show the internal conversation ID with a hover label,
group settings, and members. Read acknowledgments and visible avatar refreshes run
automatically. Cached avatars replace initials; absent avatars keep initials.
Group details load all roster pages.

| Area             | Current browser behavior                                                                                      |
| ---------------- | ------------------------------------------------------------------------------------------------------------- |
| Hub and accounts | Simulator bootstrap, bearer connection, account creation/switching, rename and logout                         |
| Sign-in          | QR PNG, countdown, automatic renewal, phone approval and automatic chat entry                                 |
| Conversations    | Contacts, thread filters, paginated history/search, full group roster, read-state, avatars and group controls |
| Messaging        | Text and attachment composer, progress, receipt status, uncertainty, self-sends and SSE replay                |
| Media            | Image/video/voice previews, downloads, incoming animated stickers and outgoing file/image/video/voice jobs    |
| Settings         | Appearance and account controls; agent configuration and desktop settings remain follow-ups                   |
| Distribution     | Browser development preview; production hosting and Tauri packaging remain follow-ups                         |

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
changes to the decoded transport. Automated journeys use the Rust simulator and
do not contact a live endpoint.

Linux x64 validation including thread summaries: 527 Rust tests
passed across 20 executables in a non-root, read-only container with external
networking disabled. Rust doc tests, pinned format/Clippy and generated contracts
passed. The client passed 43 model tests, the SDK 41 and OpenClaw 239; workspace
build/typecheck passed. Both complete simulator journeys passed.

The group journey checks create/rename, own display name, mute/unmute, add/remove,
announcement clearing, refresh and three-step leave with retained history. It also
checks partial member acceptance, service rejection, a lost admission response,
a lost upstream acknowledgment, same-key status recovery and account isolation.
The explicit-connect launcher additionally passes local HTTP fixture checks for
bootstrap authorization, origin checks, account-only proxying, blocked owner
routes and leaving the external daemon running. A read-only Linux x64 native
browser check confirmed account admission, saved threads, message previews,
cached avatars and group details. Browser mutation flows on a live account and
desktop packaging remain unverified.
