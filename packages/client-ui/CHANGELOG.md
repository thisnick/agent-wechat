# @agent-wechat/client-ui

## 0.6.0

### Minor Changes

- [#602](https://github.com/thisnick/agent-wechat/pull/602) [`2b26e58`](https://github.com/thisnick/agent-wechat/commit/2b26e582f77af1f95fd3868bf449f5c4acaffd85) Thanks [@thisnick](https://github.com/thisnick)! - Contact cards show and edit your remark for a friend. The card names the contact by the remark, with their nickname and WeChat ID below it. Friend edits now save on their own: privacy right after a change, the remark once you stop typing or leave the field. Several changes in a row save one after another without Save buttons. Each field shows its own state beside its heading (saving, syncing with WeChat, saved, not saved with Retry, or not confirmed with Check) instead of a banner at the bottom of the card. The remark and privacy explanations open from info buttons. Editing needs account administrator access.

### Patch Changes

- [#607](https://github.com/thisnick/agent-wechat/pull/607) [`24ce64b`](https://github.com/thisnick/agent-wechat/commit/24ce64b39f00b681084fea8a949741480ec07054) Thanks [@thisnick](https://github.com/thisnick)! - The contact card keeps contact data and remark and privacy saves in TanStack Query. Behavior is unchanged: a reloading contact stays on screen, edits save one after another, and a save whose outcome is unknown is checked before it is retried.

- [#598](https://github.com/thisnick/agent-wechat/pull/598) [`eabe990`](https://github.com/thisnick/agent-wechat/commit/eabe990b84a677d7c2faab7aa6aac30a710b8ec9) Thanks [@thisnick](https://github.com/thisnick)! - Read relationship privacy with contact details and lists using account read access. Open contact cards using those settings, refresh details and privacy together, and preserve unsaved changes when a newer saved version arrives. Privacy edits still require account administrator access.

- Updated dependencies [[`eabe990`](https://github.com/thisnick/agent-wechat/commit/eabe990b84a677d7c2faab7aa6aac30a710b8ec9)]:
  - @agent-wechat/shared@2.2.0

## 0.5.0

### Minor Changes

- [#562](https://github.com/thisnick/agent-wechat/pull/562) [`cc1f5f1`](https://github.com/thisnick/agent-wechat/commit/cc1f5f1b51e04547cb4e2652ee9189fd3432c998) Thanks [@thisnick](https://github.com/thisnick)! - Add relationship privacy reads and versioned contact updates, optional initial privacy for friend requests, and a shared contact detail card with privacy and contact removal. Add CLI contact updates that preserve omitted fields and retain the original merged input for recovery.

- [#562](https://github.com/thisnick/agent-wechat/pull/562) [`8208b47`](https://github.com/thisnick/agent-wechat/commit/8208b474e2794561f74dafd9d58088c120adedb0) Thanks [@thisnick](https://github.com/thisnick)! - Add account-bound contact discovery and friend request history, sending, acceptance, local history deletion and contact removal through the Rust libraries, HTTP API and CLI. Add optional private-owner OpenClaw contact tools and unified contact, group and message search in the client UI.

### Patch Changes

- [#562](https://github.com/thisnick/agent-wechat/pull/562) [`6de620e`](https://github.com/thisnick/agent-wechat/commit/6de620ee73cc3fad4d2e974f8cb448e4facbe509) Thanks [@thisnick](https://github.com/thisnick)! - Show contact, group and message search results as each source responds, default Contacts search to its contact scope, and simplify the shared contact card with icon details actions, collapsed history and a single contextual footer.

  Report stored contact membership evidence in the existing relation field. Keep unknown people out of the saved-contact list and offer a separate lookup and friend-request review from their contact cards.

  Contact lists now default to confirmed saved friends across the API and CLI. Use the API relation filter or `wx contacts list --all` for other cached people. Exact reads and server discovery are unchanged.

  Local contact search defaults to saved friends. When a query is entered, show other cached people in a separate Other people section. Show server discovery separately under Add someone on WeChat, label friends Contact and confirmed non-contacts Not friends yet, and omit a relationship label when membership is unknown.

  Separate Friends and Other people search tabs, route Contacts + to Other people, page each section independently, and show original dates and times on message matches.

  Limit message-search snippets to two lines while keeping the conversation name, original timestamp and View action visible.

- Updated dependencies [[`cc1f5f1`](https://github.com/thisnick/agent-wechat/commit/cc1f5f1b51e04547cb4e2652ee9189fd3432c998), [`6de620e`](https://github.com/thisnick/agent-wechat/commit/6de620ee73cc3fad4d2e974f8cb448e4facbe509), [`8208b47`](https://github.com/thisnick/agent-wechat/commit/8208b474e2794561f74dafd9d58088c120adedb0)]:
  - @agent-wechat/shared@2.1.0

## 0.4.3

### Patch Changes

- [#553](https://github.com/thisnick/agent-wechat/pull/553) [`d0e0e7d`](https://github.com/thisnick/agent-wechat/commit/d0e0e7d483b1d7d78d1f0f152a17f05de2e3d963) Thanks [@thisnick](https://github.com/thisnick)! - The message box's **Original media quality** choice now also covers images. When it is checked, each staged image is sent with a full-resolution original as well as the compressed copy, as each staged video already includes its unchanged file. The choice appears whenever an image or video is staged, and its help explains what it adds for each. Standard image sends are unchanged.

## 0.4.2

### Patch Changes

- [#495](https://github.com/thisnick/agent-wechat/pull/495) [`7bcef20`](https://github.com/thisnick/agent-wechat/commit/7bcef20d8c3c0e8fa7b57e438b32f7c929b9469a) Thanks [@thisnick](https://github.com/thisnick)! - Fix adding larger custom stickers by splitting encrypted upload requests across transport records. Preserve collection revision headers in the desktop app so moving and removing Saved stickers reaches the daemon with the required precondition.

  Make Add to Saved use one primary Add button, show saving progress, and return to Saved after successful imports. Keep partial failures open for retry without resubmitting completed items.

  Simplify import review to previews and file selection by removing fit, start and duration controls.

  Accept verified complete-upload acknowledgements when adding previously uploaded stickers again. Show sticker job failures separately from connection errors, and show the dragged sticker and its drop position while reordering Saved. Keep edit controls still instead of jiggling.

## 0.4.1

### Patch Changes

- [#453](https://github.com/thisnick/agent-wechat/pull/453) [`0cfedb4`](https://github.com/thisnick/agent-wechat/commit/0cfedb40c49b4899f47bd26ac4266ca6e541930b) Thanks [@thisnick](https://github.com/thisnick)! - Chinese: call a WeChat profile 账号数据 throughout the app, instead of a mix of 配置 and 资料.

## 0.4.0

### Minor Changes

- [#443](https://github.com/thisnick/agent-wechat/pull/443) [`68f3958`](https://github.com/thisnick/agent-wechat/commit/68f3958a0c559bc8ee86008770a2cce8290e25b0) Thanks [@thisnick](https://github.com/thisnick)! - Show the desktop app and web UI in Simplified Chinese as well as English. The language follows your system by default; change it under Settings → Language or on the sign-in screen. The tray menu, notification previews and notification actions follow the same choice. A desktop settings file saved by this version cannot be opened by an older version of the desktop app.

## 0.3.4

### Patch Changes

- Updated dependencies [[`e4efdf3`](https://github.com/thisnick/agent-wechat/commit/e4efdf3e00757571c7d7f07718e71679688c25d2)]:
  - @agent-wechat/shared@2.0.0

## 0.3.4-beta.0

### Patch Changes

- Updated dependencies [[`e4efdf3`](https://github.com/thisnick/agent-wechat/commit/e4efdf3e00757571c7d7f07718e71679688c25d2)]:
  - @agent-wechat/shared@2.0.0-beta.0

## 0.3.3

### Patch Changes

- [#343](https://github.com/thisnick/agent-wechat/pull/343) [`139e9e8`](https://github.com/thisnick/agent-wechat/commit/139e9e85a9180fac8e40a4f139bafacbbd3cd396) Thanks [@thisnick](https://github.com/thisnick)! - Recognize MOV, 3GP, 3G2 and MJ2 attachments as videos and prepare them as MP4 playback, including older QuickTime movies without an `ftyp` header. Add an Original video choice in the desktop and web attachment review to include the unchanged source alongside playback. Use the verified container type for original-video download extensions and show a fallback when the browser cannot preview a selected video.

- [#345](https://github.com/thisnick/agent-wechat/pull/345) [`956459a`](https://github.com/thisnick/agent-wechat/commit/956459a75980a6099b1472bdceb1cdd72a4c1f47) Thanks [@thisnick](https://github.com/thisnick)! - Allow group mentions in quoted text replies, retaining the original mention names and quote reference across recovery. Add quotes for ordinary/shared stickers, music/video cards, shared contacts, static locations and forwarded-message summaries while keeping unsupported or recalled targets blocked.

- [#348](https://github.com/thisnick/agent-wechat/pull/348) [`0a20c2b`](https://github.com/thisnick/agent-wechat/commit/0a20c2b6b15e5e73cc1cbe4e08752970ef6ce472) Thanks [@thisnick](https://github.com/thisnick)! - Keep following new messages after scrolling downward at the end of a conversation. Load older history automatically when scrolling upward near the beginning, preserving the reading position and a manual retry button.

- [#349](https://github.com/thisnick/agent-wechat/pull/349) [`5ab40ed`](https://github.com/thisnick/agent-wechat/commit/5ab40ede5f43427ad25e9f157747f573c79ffe9e) Thanks [@thisnick](https://github.com/thisnick)! - Stage attached, pasted and dropped files in the message composer with previews and remove controls. The composer's Send action queues text and each selected file as separate messages, retaining each chat's unsent attachments until sent or removed.

  Use the hub's per-type source limits for larger attachments and display local validation errors accurately. Upload browser File/Blob sources without first copying the whole file into memory. Show optional keyboard and original media quality guidance on demand.

- Updated dependencies [[`956459a`](https://github.com/thisnick/agent-wechat/commit/956459a75980a6099b1472bdceb1cdd72a4c1f47)]:
  - @agent-wechat/shared@1.1.1

## 0.3.3-beta.0

### Patch Changes

- [#343](https://github.com/thisnick/agent-wechat/pull/343) [`139e9e8`](https://github.com/thisnick/agent-wechat/commit/139e9e85a9180fac8e40a4f139bafacbbd3cd396) Thanks [@thisnick](https://github.com/thisnick)! - Recognize MOV, 3GP, 3G2 and MJ2 attachments as videos and prepare them as MP4 playback, including older QuickTime movies without an `ftyp` header. Add an Original video choice in the desktop and web attachment review to include the unchanged source alongside playback. Use the verified container type for original-video download extensions and show a fallback when the browser cannot preview a selected video.

- [#345](https://github.com/thisnick/agent-wechat/pull/345) [`956459a`](https://github.com/thisnick/agent-wechat/commit/956459a75980a6099b1472bdceb1cdd72a4c1f47) Thanks [@thisnick](https://github.com/thisnick)! - Allow group mentions in quoted text replies, retaining the original mention names and quote reference across recovery. Add quotes for ordinary/shared stickers, music/video cards, shared contacts, static locations and forwarded-message summaries while keeping unsupported or recalled targets blocked.

- [#348](https://github.com/thisnick/agent-wechat/pull/348) [`0a20c2b`](https://github.com/thisnick/agent-wechat/commit/0a20c2b6b15e5e73cc1cbe4e08752970ef6ce472) Thanks [@thisnick](https://github.com/thisnick)! - Keep following new messages after scrolling downward at the end of a conversation. Load older history automatically when scrolling upward near the beginning, preserving the reading position and a manual retry button.

- Updated dependencies [[`956459a`](https://github.com/thisnick/agent-wechat/commit/956459a75980a6099b1472bdceb1cdd72a4c1f47)]:
  - @agent-wechat/shared@1.1.1-beta.0

## 0.3.2

### Patch Changes

- [#331](https://github.com/thisnick/agent-wechat/pull/331) [`623cd81`](https://github.com/thisnick/agent-wechat/commit/623cd81f52c636e2ffc9ee6e58269bf6c3f04dbf) Thanks [@thisnick](https://github.com/thisnick)! - Detect image, MP4 video and file attachments automatically. Paste or drop files into either message editor, preview them before sending, and preserve text drafts when cancelling or changing conversations.

  Keep desktop file uploads working on WebKit versions that cannot stream FormData directly.

- [#329](https://github.com/thisnick/agent-wechat/pull/329) [`f94a66a`](https://github.com/thisnick/agent-wechat/commit/f94a66a714f5492b4032b392e2ded9d54d7611ff) Thanks [@thisnick](https://github.com/thisnick)! - Display recognized WeChat face aliases as Unicode emoji in messages, quotes, chat previews and desktop notifications. Preserve original text for sending, copying, search and storage.

- [#332](https://github.com/thisnick/agent-wechat/pull/332) [`a064296`](https://github.com/thisnick/agent-wechat/commit/a064296263ddf11f0c26c30ef40d70f280bccdac) Thanks [@thisnick](https://github.com/thisnick)! - Restrict group-wide mentions to the group owner and administrators. Refresh the administrator list from WeChat into the official profile and expose administrator IDs and mention eligibility through group info. Check permission before queuing and submitting a new message, while retaining existing send receipts and idempotency.

  Show Everyone in the composer when the server permits it, preserve drafts after permission changes, and retain the existing behavior with older servers that omit the permission field.

- [#338](https://github.com/thisnick/agent-wechat/pull/338) [`bb2c31b`](https://github.com/thisnick/agent-wechat/commit/bb2c31bbe003647bbacbd9da03161c950281bc88) Thanks [@thisnick](https://github.com/thisnick)! - Prepare large JPEG photos with reduced-resolution decoding, including 50-megapixel camera images, while retaining source, dimension and output limits. Show an actionable error when an image exceeds processing limits or cannot be decoded, without retrying its send automatically.

## 0.3.2-beta.1

### Patch Changes

- [#338](https://github.com/thisnick/agent-wechat/pull/338) [`bb2c31b`](https://github.com/thisnick/agent-wechat/commit/bb2c31bbe003647bbacbd9da03161c950281bc88) Thanks [@thisnick](https://github.com/thisnick)! - Prepare large JPEG photos with reduced-resolution decoding, including 50-megapixel camera images, while retaining source, dimension and output limits. Show an actionable error when an image exceeds processing limits or cannot be decoded, without retrying its send automatically.

## 0.3.2-beta.0

### Patch Changes

- [#331](https://github.com/thisnick/agent-wechat/pull/331) [`623cd81`](https://github.com/thisnick/agent-wechat/commit/623cd81f52c636e2ffc9ee6e58269bf6c3f04dbf) Thanks [@thisnick](https://github.com/thisnick)! - Detect image, MP4 video and file attachments automatically. Paste or drop files into either message editor, preview them before sending, and preserve text drafts when cancelling or changing conversations.

  Keep desktop file uploads working on WebKit versions that cannot stream FormData directly.

- [#329](https://github.com/thisnick/agent-wechat/pull/329) [`f94a66a`](https://github.com/thisnick/agent-wechat/commit/f94a66a714f5492b4032b392e2ded9d54d7611ff) Thanks [@thisnick](https://github.com/thisnick)! - Display recognized WeChat face aliases as Unicode emoji in messages, quotes, chat previews and desktop notifications. Preserve original text for sending, copying, search and storage.

- [#332](https://github.com/thisnick/agent-wechat/pull/332) [`a064296`](https://github.com/thisnick/agent-wechat/commit/a064296263ddf11f0c26c30ef40d70f280bccdac) Thanks [@thisnick](https://github.com/thisnick)! - Restrict group-wide mentions to the group owner and administrators. Refresh the administrator list from WeChat into the official profile and expose administrator IDs and mention eligibility through group info. Check permission before queuing and submitting a new message, while retaining existing send receipts and idempotency.

  Show Everyone in the composer when the server permits it, preserve drafts after permission changes, and retain the existing behavior with older servers that omit the permission field.

## 0.3.1

### Patch Changes

- [#319](https://github.com/thisnick/agent-wechat/pull/319) [`1f8701d`](https://github.com/thisnick/agent-wechat/commit/1f8701decd8def7371a67fa2c8f0e669e613f057) Thanks [@thisnick](https://github.com/thisnick)! - Allow message notifications to be turned off for individual accounts on this device, while retaining the desktop-wide master switch and each chat's WeChat mute setting. Remove the sound-preview button and duplicate settings divider. Let a private web preview select between configured hubs through separate authenticated same-origin routes.

## 0.3.0

### Minor Changes

- [#316](https://github.com/thisnick/agent-wechat/pull/316) [`8e007d7`](https://github.com/thisnick/agent-wechat/commit/8e007d70f04db207f9be34c913bceb759ca633fc) Thanks [@thisnick](https://github.com/thisnick)! - Add inline group mentions to the message composer, with member search, removable mentions and saved recipient identities. Keep attachment and mention actions in the bottom toolbar. Extend the native API to preserve inline positions using shared member names, and expose group mentions and quoted text replies through OpenClaw's existing message tool, including queued-send recovery.

### Patch Changes

- [#309](https://github.com/thisnick/agent-wechat/pull/309) [`f839413`](https://github.com/thisnick/agent-wechat/commit/f83941360c8866782c1441ffd0e6b79ef4fdb608) Thanks [@thisnick](https://github.com/thisnick)! - Keep IME candidate confirmation and cancellation inside the message input instead of sending a message or removing a quoted reply. Track composition events, handle WebKit's IME key code, and ignore repeated Enter presses when sending.

  Remove the duplicate browser tooltip from conversation previews, keeping the conversation hover card.

  Add cancellation for queued text messages and dismissal for uncertain or failed send notices. Dismissal keeps the receipt for recovery and does not claim to cancel a message that may already have been accepted.

- [#313](https://github.com/thisnick/agent-wechat/pull/313) [`16934c3`](https://github.com/thisnick/agent-wechat/commit/16934c3bfb25118b819476d5c93ed715570547df) Thanks [@thisnick](https://github.com/thisnick)! - Display shared text and URLs, WeChat Channels card metadata, plain group announcements, and file-upload notices instead of unsupported messages. Support quoted text/card/announcement replies and received notice quotes, while preserving recall suppression and stored source bytes. Upload notices do not start media transfers or OpenClaw model work.

- Updated dependencies [[`8e007d7`](https://github.com/thisnick/agent-wechat/commit/8e007d70f04db207f9be34c913bceb759ca633fc), [`bafeb28`](https://github.com/thisnick/agent-wechat/commit/bafeb28e06ff91dce78fa030a3a08cbed6a83a3c)]:
  - @agent-wechat/shared@1.1.0

## 0.2.0

### Minor Changes

- [#279](https://github.com/thisnick/agent-wechat/pull/279) [`e31e6c4`](https://github.com/thisnick/agent-wechat/commit/e31e6c47a1c49bd1884403bc7acc1854c07bb43f) Thanks [@thisnick](https://github.com/thisnick)! - Add the AgentWechat desktop app for macOS, Windows and Ubuntu x64/ARM64, with a bundled local hub and CLI, system-keyring credentials, optional HTTPS network access and native file saving. Integrate signed Windows installers and notarized macOS packages into the prerelease release flow. Keep the standalone CLI independent, with protected credential files and a confirmed profile-removal command that retains local files.

  Add a browser chat client with account and group management, quoted replies, confirmed recalls, shared-link cards, searchable history, message previews and group avatars. Support photo/video viewing, zoom and swipe navigation, voice playback, original downloads, visible-avatar refresh, version-checked unread clearing and event-stream updates. Remember recent chats, drafts and verified media on the device without resending uncertain operations.

  Download received thumbnails and voice notes on the hub, and apply a per-account 50 MB default limit to automatic image, video, file and sticker downloads. Expose the policy through Settings, the API and CLI. Batch thread summaries from existing profile data without upstream requests or a separate stored preview model. Improve native profile initialization, group-operation locking and Windows credential/profile handling, with isolated simulator coverage for client workflows.

### Patch Changes

- [#303](https://github.com/thisnick/agent-wechat/pull/303) [`dae1c62`](https://github.com/thisnick/agent-wechat/commit/dae1c62d16620e99dc61f5898e623eb3d9203b3b) Thanks [@thisnick](https://github.com/thisnick)! - Focus the message box immediately when selecting a conversation, including while history is loading. Show the account's cached WeChat avatar in the account chooser, switcher and sidebar, with initials as a fallback.

- [#286](https://github.com/thisnick/agent-wechat/pull/286) [`2c2a02c`](https://github.com/thisnick/agent-wechat/commit/2c2a02c704a563dafcdb9c616ce22ec70e620d4b) Thanks [@thisnick](https://github.com/thisnick)! - Render compact link cards, images and stickers inside quotes using the received reply content. Include structured quoted article, music and video cards in the API, and support eligible shared-sticker originals and thumbnails through the validated media pipeline. Preserve reply-owned media, account isolation and recall restrictions without fetching stored originals to fill quotes. Keep older OpenClaw queued excerpts compatible with the new display fields while retaining checks on their original content. Show the reply text in desktop notifications for quoted replies instead of labeling them as attachments.

- [#294](https://github.com/thisnick/agent-wechat/pull/294) [`312ffc9`](https://github.com/thisnick/agent-wechat/commit/312ffc95c2a40fe32dee92c227decbf6e2b64c19) Thanks [@thisnick](https://github.com/thisnick)! - Speed up chat opening with reusable read-only database connections for daemon-owned profiles, batched attachment metadata and an immediate event-stream heartbeat. Keep offline inspection on source-preserving snapshots.

  Reuse authenticated conversation caches by last message ID with a five-minute history validation limit, retain hidden cached history across sign-out, catch up missed changes before restoring messages, and avoid redundant startup and foreground reloads. Cancel obsolete history requests when switching chats.

  Show contact photos and group member collages in the chat header on narrow screens, using the same avatar cache as the conversation list.

- [#300](https://github.com/thisnick/agent-wechat/pull/300) [`4fc4ae8`](https://github.com/thisnick/agent-wechat/commit/4fc4ae8aea10b0c54dd70016dea5bd77f6c05444) Thanks [@thisnick](https://github.com/thisnick)! - Add chat pinning through direct hover/focus actions and conversation details, with optimistic presentation, a shared pending receipt, compact recovery feedback and keyboard/touch access. Keep unread badges visible, replace status indicators with Pin/Unpin and Mute/Unmute actions on hover, and show chat information without extra server requests. Preserve server ordering, refresh pin changes from other devices, and keep the selected chat, drafts, history and unread state while rows move.

  Add optional native notification sounds with a saved toggle and preview: the bundled Soft pair chime on macOS/Linux and the system notification sound on Windows. Keep existing notification suppression and limit sound during message bursts.

- [#285](https://github.com/thisnick/agent-wechat/pull/285) [`00ed529`](https://github.com/thisnick/agent-wechat/commit/00ed5292c541772683f7cabdd0c98c2f5a2856bd) Thanks [@thisnick](https://github.com/thisnick)! - Keep cached avatars and media readable after restarting the desktop app. Recover unreadable older image entries from the hub without clearing conversation history. Refresh saved hubs after forgetting a connection, add Back when switching hubs in the browser, clarify optional private certificates, and keep desktop settings reachable in shorter windows.

- [#282](https://github.com/thisnick/agent-wechat/pull/282) [`3f55bd1`](https://github.com/thisnick/agent-wechat/commit/3f55bd14ff6522467a05c87535635ded52a95e21) Thanks [@thisnick](https://github.com/thisnick)! - Expose saved direct-chat and group notification preferences as nullable `muted` values on thread lists and detail, using batched local reads per page. Receive phone contact changes through the independent contact sync stream, retain unsupported controls, and preserve official cursor publication and restart fences. Profile exports also wait for the contact checkpoint receipt. Ignore unmapped contact notices without selectors so they cannot interrupt message catch-up.

  Add native desktop notifications for new incoming messages in known unmuted chats, including when the window is hidden. Suppress history, outgoing messages, duplicates, stale mute state and the currently focused conversation. Add a settings toggle and click-to-open navigation.

  Support direct-chat mute/unmute through the existing thread notification API and CLI alongside group controls. Add signed desktop update discovery, channel settings, verified downloads, graceful install/restart, and final-artifact update manifests in the release workflow.

- Updated dependencies [[`312ffc9`](https://github.com/thisnick/agent-wechat/commit/312ffc95c2a40fe32dee92c227decbf6e2b64c19), [`e31e6c4`](https://github.com/thisnick/agent-wechat/commit/e31e6c47a1c49bd1884403bc7acc1854c07bb43f), [`1e33596`](https://github.com/thisnick/agent-wechat/commit/1e335969911474870764926cf30f8c105c91107c), [`2d0b57c`](https://github.com/thisnick/agent-wechat/commit/2d0b57c3f2f5141aa85515a29384e50668c8928d), [`e12e189`](https://github.com/thisnick/agent-wechat/commit/e12e1899910641730927fb8f5d603d2f3f2c61f5), [`700f2c1`](https://github.com/thisnick/agent-wechat/commit/700f2c132eecd0fe16a80b5fee10efd1d2c3fdb9), [`b4c9e8a`](https://github.com/thisnick/agent-wechat/commit/b4c9e8a1886abdf773d9cefff45474646fcef9d7), [`c1147ed`](https://github.com/thisnick/agent-wechat/commit/c1147edca08fc2cdb126ac591a54b67b75077f37), [`3f55bd1`](https://github.com/thisnick/agent-wechat/commit/3f55bd14ff6522467a05c87535635ded52a95e21), [`acbc39d`](https://github.com/thisnick/agent-wechat/commit/acbc39d8039943de2f61054a249a0a0cbefb685c)]:
  - @agent-wechat/shared@1.0.0

## 0.2.0-beta.4

### Patch Changes

- [#300](https://github.com/thisnick/agent-wechat/pull/300) [`4fc4ae8`](https://github.com/thisnick/agent-wechat/commit/4fc4ae8aea10b0c54dd70016dea5bd77f6c05444) Thanks [@thisnick](https://github.com/thisnick)! - Add chat pinning through direct hover/focus actions and conversation details, with optimistic presentation, a shared pending receipt, compact recovery feedback and keyboard/touch access. Keep unread badges visible, replace status indicators with Pin/Unpin and Mute/Unmute actions on hover, and show chat information without extra server requests. Preserve server ordering, refresh pin changes from other devices, and keep the selected chat, drafts, history and unread state while rows move.

  Add optional native notification sounds with a saved toggle and preview: the bundled Soft pair chime on macOS/Linux and the system notification sound on Windows. Keep existing notification suppression and limit sound during message bursts.

- Updated dependencies [[`acbc39d`](https://github.com/thisnick/agent-wechat/commit/acbc39d8039943de2f61054a249a0a0cbefb685c)]:
  - @agent-wechat/shared@1.0.0-beta.6

## 0.2.0-beta.3

### Patch Changes

- [#294](https://github.com/thisnick/agent-wechat/pull/294) [`312ffc9`](https://github.com/thisnick/agent-wechat/commit/312ffc95c2a40fe32dee92c227decbf6e2b64c19) Thanks [@thisnick](https://github.com/thisnick)! - Speed up chat opening with reusable read-only database connections for daemon-owned profiles, batched attachment metadata and an immediate event-stream heartbeat. Keep offline inspection on source-preserving snapshots.

  Reuse authenticated conversation caches by last message ID with a five-minute history validation limit, retain hidden cached history across sign-out, catch up missed changes before restoring messages, and avoid redundant startup and foreground reloads. Cancel obsolete history requests when switching chats.

  Show contact photos and group member collages in the chat header on narrow screens, using the same avatar cache as the conversation list.

- Updated dependencies [[`312ffc9`](https://github.com/thisnick/agent-wechat/commit/312ffc95c2a40fe32dee92c227decbf6e2b64c19)]:
  - @agent-wechat/shared@1.0.0-beta.5

## 0.2.0-beta.2

### Patch Changes

- [#286](https://github.com/thisnick/agent-wechat/pull/286) [`2c2a02c`](https://github.com/thisnick/agent-wechat/commit/2c2a02c704a563dafcdb9c616ce22ec70e620d4b) Thanks [@thisnick](https://github.com/thisnick)! - Render compact link cards, images and stickers inside quotes using the received reply content. Include structured quoted article, music and video cards in the API, and support eligible shared-sticker originals and thumbnails through the validated media pipeline. Preserve reply-owned media, account isolation and recall restrictions without fetching stored originals to fill quotes. Keep older OpenClaw queued excerpts compatible with the new display fields while retaining checks on their original content. Show the reply text in desktop notifications for quoted replies instead of labeling them as attachments.

- [#285](https://github.com/thisnick/agent-wechat/pull/285) [`00ed529`](https://github.com/thisnick/agent-wechat/commit/00ed5292c541772683f7cabdd0c98c2f5a2856bd) Thanks [@thisnick](https://github.com/thisnick)! - Keep cached avatars and media readable after restarting the desktop app. Recover unreadable older image entries from the hub without clearing conversation history. Refresh saved hubs after forgetting a connection, add Back when switching hubs in the browser, clarify optional private certificates, and keep desktop settings reachable in shorter windows.

## 0.2.0-beta.1

### Patch Changes

- [#282](https://github.com/thisnick/agent-wechat/pull/282) [`3f55bd1`](https://github.com/thisnick/agent-wechat/commit/3f55bd14ff6522467a05c87535635ded52a95e21) Thanks [@thisnick](https://github.com/thisnick)! - Expose saved direct-chat and group notification preferences as nullable `muted` values on thread lists and detail, using batched local reads per page. Receive phone contact changes through the independent contact sync stream, retain unsupported controls, and preserve official cursor publication and restart fences. Profile exports also wait for the contact checkpoint receipt. Ignore unmapped contact notices without selectors so they cannot interrupt message catch-up.

  Add native desktop notifications for new incoming messages in known unmuted chats, including when the window is hidden. Suppress history, outgoing messages, duplicates, stale mute state and the currently focused conversation. Add a settings toggle and click-to-open navigation.

  Support direct-chat mute/unmute through the existing thread notification API and CLI alongside group controls. Add signed desktop update discovery, channel settings, verified downloads, graceful install/restart, and final-artifact update manifests in the release workflow.

- Updated dependencies [[`3f55bd1`](https://github.com/thisnick/agent-wechat/commit/3f55bd14ff6522467a05c87535635ded52a95e21)]:
  - @agent-wechat/shared@1.0.0-beta.4

## 0.2.0-beta.0

### Minor Changes

- [#279](https://github.com/thisnick/agent-wechat/pull/279) [`e31e6c4`](https://github.com/thisnick/agent-wechat/commit/e31e6c47a1c49bd1884403bc7acc1854c07bb43f) Thanks [@thisnick](https://github.com/thisnick)! - Add the AgentWechat desktop app for macOS, Windows and Ubuntu x64/ARM64, with a bundled local hub and CLI, system-keyring credentials, optional HTTPS network access and native file saving. Integrate signed Windows installers and notarized macOS packages into the prerelease release flow. Keep the standalone CLI independent, with protected credential files and a confirmed profile-removal command that retains local files.

  Add a browser chat client with account and group management, quoted replies, confirmed recalls, shared-link cards, searchable history, message previews and group avatars. Support photo/video viewing, zoom and swipe navigation, voice playback, original downloads, visible-avatar refresh, version-checked unread clearing and event-stream updates. Remember recent chats, drafts and verified media on the device without resending uncertain operations.

  Download received thumbnails and voice notes on the hub, and apply a per-account 50 MB default limit to automatic image, video, file and sticker downloads. Expose the policy through Settings, the API and CLI. Batch thread summaries from existing profile data without upstream requests or a separate stored preview model. Improve native profile initialization, group-operation locking and Windows credential/profile handling, with isolated simulator coverage for client workflows.

### Patch Changes

- Updated dependencies [[`e31e6c4`](https://github.com/thisnick/agent-wechat/commit/e31e6c47a1c49bd1884403bc7acc1854c07bb43f)]:
  - @agent-wechat/shared@1.0.0-beta.3
