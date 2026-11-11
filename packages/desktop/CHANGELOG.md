# @agent-wechat/desktop

## 2.3.1

### Patch Changes

- [#453](https://github.com/thisnick/agent-wechat/pull/453) [`0cfedb4`](https://github.com/thisnick/agent-wechat/commit/0cfedb40c49b4899f47bd26ac4266ca6e541930b) Thanks [@thisnick](https://github.com/thisnick)! - Chinese: call a WeChat profile 账号数据 throughout the app, instead of a mix of 配置 and 资料.

- Updated dependencies [[`0cfedb4`](https://github.com/thisnick/agent-wechat/commit/0cfedb40c49b4899f47bd26ac4266ca6e541930b)]:
  - @agent-wechat/client-ui@0.4.1

## 2.3.0

### Minor Changes

- [#443](https://github.com/thisnick/agent-wechat/pull/443) [`68f3958`](https://github.com/thisnick/agent-wechat/commit/68f3958a0c559bc8ee86008770a2cce8290e25b0) Thanks [@thisnick](https://github.com/thisnick)! - Show the desktop app and web UI in Simplified Chinese as well as English. The language follows your system by default; change it under Settings → Language or on the sign-in screen. The tray menu, notification previews and notification actions follow the same choice. A desktop settings file saved by this version cannot be opened by an older version of the desktop app.

### Patch Changes

- [#439](https://github.com/thisnick/agent-wechat/pull/439) [`7921590`](https://github.com/thisnick/agent-wechat/commit/7921590128af8bad472051d129697487be5dc340) Thanks [@thisnick](https://github.com/thisnick)! - Filter the Messages sidebar by multiple conversation categories, see unread counts, and return to the full inbox with All.

- [#441](https://github.com/thisnick/agent-wechat/pull/441) [`1b4c215`](https://github.com/thisnick/agent-wechat/commit/1b4c2152d4fcfa302c6bebc48a473eab97064f0f) Thanks [@Lazily01](https://github.com/Lazily01)! - Expose the sender's supplied display name as optional `quote.senderName` in received quoted replies, including replies quoting OpenIM members. The name comes from the delivered reply payload.

  Desktop quoted replies show the supplied author name, with cached-name fallback for older replies.

- Updated dependencies [[`68f3958`](https://github.com/thisnick/agent-wechat/commit/68f3958a0c559bc8ee86008770a2cce8290e25b0)]:
  - @agent-wechat/client-ui@0.4.0

## 2.2.1

### Patch Changes

- [#436](https://github.com/thisnick/agent-wechat/pull/436) [`d4bd0cc`](https://github.com/thisnick/agent-wechat/commit/d4bd0cc1a6304c3651f556e1f2fcb2c937dc0397) Thanks [@thisnick](https://github.com/thisnick)! - Show unnamed group chats with up to three known member names, or a short ID suffix when member names are unavailable.

  Load the room avatar for visible groups while their member list is unavailable.

- [#435](https://github.com/thisnick/agent-wechat/pull/435) [`072f7fb`](https://github.com/thisnick/agent-wechat/commit/072f7fb529e26b1e3f87b4d5606c1198a7177f86) Thanks [@thisnick](https://github.com/thisnick)! - Send `.opus`, `.oga`, `.caf` and `.amr` audio as voice notes. OpenClaw text-to-speech replies, which are written as `.opus`, no longer fail with "Voice sending requires an explicit audio attachment", and a remote file served with a generic content type is classified by its name. The daemon now also converts CAF and AMR sources. An unsupported local voice source is now rejected before OpenClaw records the send as dispatched.

## 2.2.0

### Minor Changes

- [#428](https://github.com/thisnick/agent-wechat/pull/428) [`8fbdc98`](https://github.com/thisnick/agent-wechat/commit/8fbdc98a1e310787699a5a57eec954bdcc39e114) Thanks [@thisnick](https://github.com/thisnick)! - On macOS, message notifications are grouped by chat in Notification Center and offer **Mark as Read** and **Reply**. Mark as Read marks the chat read unless a newer message has arrived since. Reply sends your text as a quote reply to that message and marks the chat read; it appears only when the message can be quoted. Opening a chat clears its notifications, and a notification still opens its chat after the app restarts. The first notification after updating may ask again to allow notifications.

### Patch Changes

- [#431](https://github.com/thisnick/agent-wechat/pull/431) [`82938af`](https://github.com/thisnick/agent-wechat/commit/82938affc9db1faf110639dd0bc4f4fb272bc840) Thanks [@thisnick](https://github.com/thisnick)! - Resume only the original group update after refreshing group metadata, instead of retrying every blocked sync batch. Unsupported commands remain retained for local repair.

- [#426](https://github.com/thisnick/agent-wechat/pull/426) [`62b93b1`](https://github.com/thisnick/agent-wechat/commit/62b93b101f39f7cc3c058a70b2fefd5beb1d5781) Thanks [@thisnick](https://github.com/thisnick)! - On macOS, clicking a message notification now opens its chat, whether you click the banner or the notification later in Notification Center. Previously a click only brought the app to the front.

## 2.1.0

### Minor Changes

- [#423](https://github.com/thisnick/agent-wechat/pull/423) [`4bfc492`](https://github.com/thisnick/agent-wechat/commit/4bfc492675cb8228abe1cf45a71fb624f63d7bff) Thanks [@thisnick](https://github.com/thisnick)! - Every view in the chat window now has its own address. Reloading keeps you on the same chat, Back and Forward move between chats, the account list, settings and the photo viewer, and a link to a chat or message opens it once the account is signed in. A link to a chat or message that no longer exists opens the chat list or the conversation with a note. Clicking a message notification now opens its chat scrolled to that message and highlights it.

## 2.0.0

### Minor Changes

- [#403](https://github.com/thisnick/agent-wechat/pull/403) [`635fbc9`](https://github.com/thisnick/agent-wechat/commit/635fbc912fe1890dd4779c414a080746815888b1) Thanks [@thisnick](https://github.com/thisnick)! - Video sends now accept sources up to 512 MiB, matching ordinary files, instead of 50 MiB. The CLI, HTTP API capabilities, desktop and web composer, and OpenClaw extension all use the new limit; OpenClaw still applies `channels.agent-wechat.mediaMaxMb`, which defaults to 50. Original-quality videos of 25 MiB and above upload in blocks through the same path as large files.

### Patch Changes

- [#398](https://github.com/thisnick/agent-wechat/pull/398) [`1bc8f55`](https://github.com/thisnick/agent-wechat/commit/1bc8f550f710319c9ab5cda507c07d5d899ed9ac) Thanks [@thisnick](https://github.com/thisnick)! - Add `wx config servers remove NAME` to remove a saved server entry. Removing the selected server clears the CLI's active server selection.

- Updated dependencies []:
  - @agent-wechat/client-ui@0.3.4

## 2.0.0-beta.1

## 2.0.0-beta.0

### Minor Changes

- [#403](https://github.com/thisnick/agent-wechat/pull/403) [`635fbc9`](https://github.com/thisnick/agent-wechat/commit/635fbc912fe1890dd4779c414a080746815888b1) Thanks [@thisnick](https://github.com/thisnick)! - Video sends now accept sources up to 512 MiB, matching ordinary files, instead of 50 MiB. The CLI, HTTP API capabilities, desktop and web composer, and OpenClaw extension all use the new limit; OpenClaw still applies `channels.agent-wechat.mediaMaxMb`, which defaults to 50. Original-quality videos of 25 MiB and above upload in blocks through the same path as large files.

### Patch Changes

- [#398](https://github.com/thisnick/agent-wechat/pull/398) [`1bc8f55`](https://github.com/thisnick/agent-wechat/commit/1bc8f550f710319c9ab5cda507c07d5d899ed9ac) Thanks [@thisnick](https://github.com/thisnick)! - Add `wx config servers remove NAME` to remove a saved server entry. Removing the selected server clears the CLI's active server selection.

- Updated dependencies []:
  - @agent-wechat/client-ui@0.3.4-beta.0

## 1.5.1

## 1.5.0

## 1.4.0

### Minor Changes

- [#366](https://github.com/thisnick/agent-wechat/pull/366) [`76c3ac5`](https://github.com/thisnick/agent-wechat/commit/76c3ac55198ce43bccd249cd8a7774b4b9369b9f) Thanks [@thisnick](https://github.com/thisnick)! - Show readable CLI results by default, including tables for chats and message history. The message table shows the sender, a preview, the timestamp and a shortened ID. Use `--json` for full results, event streams and daemon startup records in scripts.

### Patch Changes

- [#363](https://github.com/thisnick/agent-wechat/pull/363) [`476a041`](https://github.com/thisnick/agent-wechat/commit/476a041e7616bcb1485393facc715f69270baaab) Thanks [@thisnick](https://github.com/thisnick)! - Include local microphone/speaker support in released executables, add local audio-device discovery, and show live signal-level meters during interactive calls. Keep passive/JSON output free of terminal meters and retain separate daemon calling permissions and native transmission guards. Linux executables now require libpulse0; daemon operation does not require audio devices or a sound server.

  Include the call API, native worker, codec and device adapters in normal Cargo builds. Use the same complete default features for development, CI and release artifacts, without feature-selection flags. Verify calling routes and a passive control connection in the packaged executable and image; runtime calling authorization remains separate.

- [#366](https://github.com/thisnick/agent-wechat/pull/366) [`76c3ac5`](https://github.com/thisnick/agent-wechat/commit/76c3ac55198ce43bccd249cd8a7774b4b9369b9f) Thanks [@thisnick](https://github.com/thisnick)! - Use `accounts`, `profiles` and `config servers` as the canonical CLI groups and `select` to save account, profile and server choices. Keep previous singular names and selection verbs as hidden compatibility aliases. Alphabetize subcommands at every help level and add command descriptions, selection guidance and examples. Update bundled instructions and repository callers to use the canonical names.

  Use `wx auth logout --local` to sign out on the daemon and keep saved credentials without contacting WeChat. The old `logout-local` command remains available as a hidden alias.

  Use `wx chats list` to list chats. The previous `wx threads` command remains available as a hidden compatibility command.

  Use `wx contacts lookup` and `wx groups lookup` for lookups by ID. The previous top-level lookup commands remain available as hidden compatibility commands.

  Contact lookup skips blank remarks and nicknames when choosing a display name.

- [#366](https://github.com/thisnick/agent-wechat/pull/366) [`76c3ac5`](https://github.com/thisnick/agent-wechat/commit/76c3ac55198ce43bccd249cd8a7774b4b9369b9f) Thanks [@thisnick](https://github.com/thisnick)! - Shorten the interactive call prompts by removing the audio device startup message and the microphone status from the waiting prompt. Show which call controls require Enter and let users redisplay the keys with `?`.

  Set `WX_CALL_DEBUG=1` to trace call events, audio frame writes and WebSocket closure details on stderr.

## 1.4.0-beta.6

## 1.4.0-beta.5

## 1.4.0-beta.4

### Minor Changes

- [#366](https://github.com/thisnick/agent-wechat/pull/366) [`76c3ac5`](https://github.com/thisnick/agent-wechat/commit/76c3ac55198ce43bccd249cd8a7774b4b9369b9f) Thanks [@thisnick](https://github.com/thisnick)! - Show readable CLI results by default, including tables for chats and message history. The message table shows the sender, a preview, the timestamp and a shortened ID. Use `--json` for full results, event streams and daemon startup records in scripts.

### Patch Changes

- [#366](https://github.com/thisnick/agent-wechat/pull/366) [`76c3ac5`](https://github.com/thisnick/agent-wechat/commit/76c3ac55198ce43bccd249cd8a7774b4b9369b9f) Thanks [@thisnick](https://github.com/thisnick)! - Use `accounts`, `profiles` and `config servers` as the canonical CLI groups and `select` to save account, profile and server choices. Keep previous singular names and selection verbs as hidden compatibility aliases. Alphabetize subcommands at every help level and add command descriptions, selection guidance and examples. Update bundled instructions and repository callers to use the canonical names.

  Use `wx auth logout --local` to sign out on the daemon and keep saved credentials without contacting WeChat. The old `logout-local` command remains available as a hidden alias.

  Use `wx chats list` to list chats. The previous `wx threads` command remains available as a hidden compatibility command.

  Use `wx contacts lookup` and `wx groups lookup` for lookups by ID. The previous top-level lookup commands remain available as hidden compatibility commands.

  Contact lookup skips blank remarks and nicknames when choosing a display name.

- [#366](https://github.com/thisnick/agent-wechat/pull/366) [`76c3ac5`](https://github.com/thisnick/agent-wechat/commit/76c3ac55198ce43bccd249cd8a7774b4b9369b9f) Thanks [@thisnick](https://github.com/thisnick)! - Shorten the interactive call prompts by removing the audio device startup message and the microphone status from the waiting prompt. Show which call controls require Enter and let users redisplay the keys with `?`.

  Set `WX_CALL_DEBUG=1` to trace call events, audio frame writes and WebSocket closure details on stderr.

## 1.4.0-beta.3

## 1.4.0-beta.2

## 1.4.0-beta.1

### Patch Changes

- [#363](https://github.com/thisnick/agent-wechat/pull/363) [`476a041`](https://github.com/thisnick/agent-wechat/commit/476a041e7616bcb1485393facc715f69270baaab) Thanks [@thisnick](https://github.com/thisnick)! - Include local microphone/speaker support in released executables, add local audio-device discovery, and show live signal-level meters during interactive calls. Keep passive/JSON output free of terminal meters and retain separate daemon calling permissions and native transmission guards. Linux executables now require libpulse0; daemon operation does not require audio devices or a sound server.

  Include the call API, native worker, codec and device adapters in normal Cargo builds. Use the same complete default features for development, CI and release artifacts, without feature-selection flags. Verify calling routes and a passive control connection in the packaged executable and image; runtime calling authorization remains separate.

## 1.4.0-beta.0

## 1.3.3

## 1.3.2

## 1.3.1

### Patch Changes

- [#345](https://github.com/thisnick/agent-wechat/pull/345) [`956459a`](https://github.com/thisnick/agent-wechat/commit/956459a75980a6099b1472bdceb1cdd72a4c1f47) Thanks [@thisnick](https://github.com/thisnick)! - Allow group mentions in quoted text replies, retaining the original mention names and quote reference across recovery. Add quotes for ordinary/shared stickers, music/video cards, shared contacts, static locations and forwarded-message summaries while keeping unsupported or recalled targets blocked.

- [#348](https://github.com/thisnick/agent-wechat/pull/348) [`0a20c2b`](https://github.com/thisnick/agent-wechat/commit/0a20c2b6b15e5e73cc1cbe4e08752970ef6ce472) Thanks [@thisnick](https://github.com/thisnick)! - Keep following new messages after scrolling downward at the end of a conversation. Load older history automatically when scrolling upward near the beginning, preserving the reading position and a manual retry button.

- [#349](https://github.com/thisnick/agent-wechat/pull/349) [`5ab40ed`](https://github.com/thisnick/agent-wechat/commit/5ab40ede5f43427ad25e9f157747f573c79ffe9e) Thanks [@thisnick](https://github.com/thisnick)! - Stage attached, pasted and dropped files in the message composer with previews and remove controls. The composer's Send action queues text and each selected file as separate messages, retaining each chat's unsent attachments until sent or removed.

  Use the hub's per-type source limits for larger attachments and display local validation errors accurately. Upload browser File/Blob sources without first copying the whole file into memory. Show optional keyboard and original media quality guidance on demand.

- Updated dependencies [[`139e9e8`](https://github.com/thisnick/agent-wechat/commit/139e9e85a9180fac8e40a4f139bafacbbd3cd396), [`956459a`](https://github.com/thisnick/agent-wechat/commit/956459a75980a6099b1472bdceb1cdd72a4c1f47), [`0a20c2b`](https://github.com/thisnick/agent-wechat/commit/0a20c2b6b15e5e73cc1cbe4e08752970ef6ce472), [`5ab40ed`](https://github.com/thisnick/agent-wechat/commit/5ab40ede5f43427ad25e9f157747f573c79ffe9e)]:
  - @agent-wechat/client-ui@0.3.3

## 1.3.1-beta.0

### Patch Changes

- [#345](https://github.com/thisnick/agent-wechat/pull/345) [`956459a`](https://github.com/thisnick/agent-wechat/commit/956459a75980a6099b1472bdceb1cdd72a4c1f47) Thanks [@thisnick](https://github.com/thisnick)! - Allow group mentions in quoted text replies, retaining the original mention names and quote reference across recovery. Add quotes for ordinary/shared stickers, music/video cards, shared contacts, static locations and forwarded-message summaries while keeping unsupported or recalled targets blocked.

- [#348](https://github.com/thisnick/agent-wechat/pull/348) [`0a20c2b`](https://github.com/thisnick/agent-wechat/commit/0a20c2b6b15e5e73cc1cbe4e08752970ef6ce472) Thanks [@thisnick](https://github.com/thisnick)! - Keep following new messages after scrolling downward at the end of a conversation. Load older history automatically when scrolling upward near the beginning, preserving the reading position and a manual retry button.

- Updated dependencies [[`139e9e8`](https://github.com/thisnick/agent-wechat/commit/139e9e85a9180fac8e40a4f139bafacbbd3cd396), [`956459a`](https://github.com/thisnick/agent-wechat/commit/956459a75980a6099b1472bdceb1cdd72a4c1f47), [`0a20c2b`](https://github.com/thisnick/agent-wechat/commit/0a20c2b6b15e5e73cc1cbe4e08752970ef6ce472)]:
  - @agent-wechat/client-ui@0.3.3-beta.0

## 1.3.0

### Minor Changes

- [#328](https://github.com/thisnick/agent-wechat/pull/328) [`28f8ea8`](https://github.com/thisnick/agent-wechat/commit/28f8ea85f08d2811b162dc84b228a3ab6c25f690) Thanks [@thisnick](https://github.com/thisnick)! - Receive and read static location pins from profile history as structured coordinates, place names and addresses, with a readable text body. Location cards provide a place search near the shared coordinates, a direct Google Maps pin and coordinate copying, without converting coordinates. Preserve the supplied map type and leave the coordinate system unknown. Show location-sharing notices (application subtype 17) as supplied text without joining live tracking. OpenClaw applies its existing sender, mention and recall rules. Retained static pins can recover into ordinary profile history; live-tracking controls remain separate and unsupported. Location thumbnails and official preview-cache compatibility are not implemented.

### Patch Changes

- [#327](https://github.com/thisnick/agent-wechat/pull/327) [`199a33a`](https://github.com/thisnick/agent-wechat/commit/199a33a8e510d6de0885f710b589cd23f9810f8e) Thanks [@thisnick](https://github.com/thisnick)! - Prevent a queued window reload from restarting the local hub while the desktop app quits.

- [#331](https://github.com/thisnick/agent-wechat/pull/331) [`623cd81`](https://github.com/thisnick/agent-wechat/commit/623cd81f52c636e2ffc9ee6e58269bf6c3f04dbf) Thanks [@thisnick](https://github.com/thisnick)! - Detect image, MP4 video and file attachments automatically. Paste or drop files into either message editor, preview them before sending, and preserve text drafts when cancelling or changing conversations.

  Keep desktop file uploads working on WebKit versions that cannot stream FormData directly.

- [#329](https://github.com/thisnick/agent-wechat/pull/329) [`f94a66a`](https://github.com/thisnick/agent-wechat/commit/f94a66a714f5492b4032b392e2ded9d54d7611ff) Thanks [@thisnick](https://github.com/thisnick)! - Display recognized WeChat face aliases as Unicode emoji in messages, quotes, chat previews and desktop notifications. Preserve original text for sending, copying, search and storage.

- [#332](https://github.com/thisnick/agent-wechat/pull/332) [`a064296`](https://github.com/thisnick/agent-wechat/commit/a064296263ddf11f0c26c30ef40d70f280bccdac) Thanks [@thisnick](https://github.com/thisnick)! - Restrict group-wide mentions to the group owner and administrators. Refresh the administrator list from WeChat into the official profile and expose administrator IDs and mention eligibility through group info. Check permission before queuing and submitting a new message, while retaining existing send receipts and idempotency.

  Show Everyone in the composer when the server permits it, preserve drafts after permission changes, and retain the existing behavior with older servers that omit the permission field.

- [`7527cfc`](https://github.com/thisnick/agent-wechat/commit/7527cfc455be74b258cba34f75f38523b59ba1c7) Thanks [@thisnick](https://github.com/thisnick)! - Declare the project MIT license in package metadata and include its text with npm packages, native release notices, desktop resources, and the native container. Preserve all existing third-party license notices.

- [#328](https://github.com/thisnick/agent-wechat/pull/328) [`28f8ea8`](https://github.com/thisnick/agent-wechat/commit/28f8ea85f08d2811b162dc84b228a3ab6c25f690) Thanks [@thisnick](https://github.com/thisnick)! - Show plain system notices, including location-sharing end notices, with their supplied text and without attributing the transport peer as their author. Desktop and web clients display them as centered notices without an avatar or sender name. Recall messages retain their separate author-evidence handling, and raw system XML is not exposed as message text.

- [#328](https://github.com/thisnick/agent-wechat/pull/328) [`28f8ea8`](https://github.com/thisnick/agent-wechat/commit/28f8ea85f08d2811b162dc84b228a3ab6c25f690) Thanks [@thisnick](https://github.com/thisnick)! - Correct the cancellation and codec-format fields in outgoing voice uploads so SILK notes declare format 4 in the transmitted request, including continuation and final ranges.

- Updated dependencies [[`623cd81`](https://github.com/thisnick/agent-wechat/commit/623cd81f52c636e2ffc9ee6e58269bf6c3f04dbf), [`f94a66a`](https://github.com/thisnick/agent-wechat/commit/f94a66a714f5492b4032b392e2ded9d54d7611ff), [`a064296`](https://github.com/thisnick/agent-wechat/commit/a064296263ddf11f0c26c30ef40d70f280bccdac), [`bb2c31b`](https://github.com/thisnick/agent-wechat/commit/bb2c31bbe003647bbacbd9da03161c950281bc88)]:
  - @agent-wechat/client-ui@0.3.2

## 1.3.0-beta.1

### Patch Changes

- Updated dependencies [[`bb2c31b`](https://github.com/thisnick/agent-wechat/commit/bb2c31bbe003647bbacbd9da03161c950281bc88)]:
  - @agent-wechat/client-ui@0.3.2-beta.1

## 1.3.0-beta.0

### Minor Changes

- [#328](https://github.com/thisnick/agent-wechat/pull/328) [`28f8ea8`](https://github.com/thisnick/agent-wechat/commit/28f8ea85f08d2811b162dc84b228a3ab6c25f690) Thanks [@thisnick](https://github.com/thisnick)! - Receive and read static location pins from profile history as structured coordinates, place names and addresses, with a readable text body. Location cards provide a place search near the shared coordinates, a direct Google Maps pin and coordinate copying, without converting coordinates. Preserve the supplied map type and leave the coordinate system unknown. Show location-sharing notices (application subtype 17) as supplied text without joining live tracking. OpenClaw applies its existing sender, mention and recall rules. Retained static pins can recover into ordinary profile history; live-tracking controls remain separate and unsupported. Location thumbnails and official preview-cache compatibility are not implemented.

### Patch Changes

- [#327](https://github.com/thisnick/agent-wechat/pull/327) [`199a33a`](https://github.com/thisnick/agent-wechat/commit/199a33a8e510d6de0885f710b589cd23f9810f8e) Thanks [@thisnick](https://github.com/thisnick)! - Prevent a queued window reload from restarting the local hub while the desktop app quits.

- [#331](https://github.com/thisnick/agent-wechat/pull/331) [`623cd81`](https://github.com/thisnick/agent-wechat/commit/623cd81f52c636e2ffc9ee6e58269bf6c3f04dbf) Thanks [@thisnick](https://github.com/thisnick)! - Detect image, MP4 video and file attachments automatically. Paste or drop files into either message editor, preview them before sending, and preserve text drafts when cancelling or changing conversations.

  Keep desktop file uploads working on WebKit versions that cannot stream FormData directly.

- [#329](https://github.com/thisnick/agent-wechat/pull/329) [`f94a66a`](https://github.com/thisnick/agent-wechat/commit/f94a66a714f5492b4032b392e2ded9d54d7611ff) Thanks [@thisnick](https://github.com/thisnick)! - Display recognized WeChat face aliases as Unicode emoji in messages, quotes, chat previews and desktop notifications. Preserve original text for sending, copying, search and storage.

- [#332](https://github.com/thisnick/agent-wechat/pull/332) [`a064296`](https://github.com/thisnick/agent-wechat/commit/a064296263ddf11f0c26c30ef40d70f280bccdac) Thanks [@thisnick](https://github.com/thisnick)! - Restrict group-wide mentions to the group owner and administrators. Refresh the administrator list from WeChat into the official profile and expose administrator IDs and mention eligibility through group info. Check permission before queuing and submitting a new message, while retaining existing send receipts and idempotency.

  Show Everyone in the composer when the server permits it, preserve drafts after permission changes, and retain the existing behavior with older servers that omit the permission field.

- [`7527cfc`](https://github.com/thisnick/agent-wechat/commit/7527cfc455be74b258cba34f75f38523b59ba1c7) Thanks [@thisnick](https://github.com/thisnick)! - Declare the project MIT license in package metadata and include its text with npm packages, native release notices, desktop resources, and the native container. Preserve all existing third-party license notices.

- [#328](https://github.com/thisnick/agent-wechat/pull/328) [`28f8ea8`](https://github.com/thisnick/agent-wechat/commit/28f8ea85f08d2811b162dc84b228a3ab6c25f690) Thanks [@thisnick](https://github.com/thisnick)! - Show plain system notices, including location-sharing end notices, with their supplied text and without attributing the transport peer as their author. Desktop and web clients display them as centered notices without an avatar or sender name. Recall messages retain their separate author-evidence handling, and raw system XML is not exposed as message text.

- [#328](https://github.com/thisnick/agent-wechat/pull/328) [`28f8ea8`](https://github.com/thisnick/agent-wechat/commit/28f8ea85f08d2811b162dc84b228a3ab6c25f690) Thanks [@thisnick](https://github.com/thisnick)! - Correct the cancellation and codec-format fields in outgoing voice uploads so SILK notes declare format 4 in the transmitted request, including continuation and final ranges.

- Updated dependencies [[`623cd81`](https://github.com/thisnick/agent-wechat/commit/623cd81f52c636e2ffc9ee6e58269bf6c3f04dbf), [`f94a66a`](https://github.com/thisnick/agent-wechat/commit/f94a66a714f5492b4032b392e2ded9d54d7611ff), [`a064296`](https://github.com/thisnick/agent-wechat/commit/a064296263ddf11f0c26c30ef40d70f280bccdac)]:
  - @agent-wechat/client-ui@0.3.2-beta.0

## 1.2.0

### Minor Changes

- [#319](https://github.com/thisnick/agent-wechat/pull/319) [`1f8701d`](https://github.com/thisnick/agent-wechat/commit/1f8701decd8def7371a67fa2c8f0e669e613f057) Thanks [@thisnick](https://github.com/thisnick)! - Allow message notifications to be turned off for individual accounts on this device, while retaining the desktop-wide master switch and each chat's WeChat mute setting. Remove the sound-preview button and duplicate settings divider. Let a private web preview select between configured hubs through separate authenticated same-origin routes.

### Patch Changes

- Updated dependencies [[`1f8701d`](https://github.com/thisnick/agent-wechat/commit/1f8701decd8def7371a67fa2c8f0e669e613f057)]:
  - @agent-wechat/client-ui@0.3.1

## 1.1.0

### Minor Changes

- [#316](https://github.com/thisnick/agent-wechat/pull/316) [`8e007d7`](https://github.com/thisnick/agent-wechat/commit/8e007d70f04db207f9be34c913bceb759ca633fc) Thanks [@thisnick](https://github.com/thisnick)! - Add inline group mentions to the message composer, with member search, removable mentions and saved recipient identities. Keep attachment and mention actions in the bottom toolbar. Extend the native API to preserve inline positions using shared member names, and expose group mentions and quoted text replies through OpenClaw's existing message tool, including queued-send recovery.

### Patch Changes

- [#311](https://github.com/thisnick/agent-wechat/pull/311) [`5aaa239`](https://github.com/thisnick/agent-wechat/commit/5aaa239418b111b4f437b9200b4d4b860255eeb8) Thanks [@thisnick](https://github.com/thisnick)! - Show group notifications with the sender as the title, the group on a separate line and the message as the body. macOS and Windows use native subtitles; Linux places the group above the message in the body. Use names already saved on the hub, with “Member” as the fallback when the sender's name is unavailable. Direct-chat notifications keep their existing format.

- [#309](https://github.com/thisnick/agent-wechat/pull/309) [`f839413`](https://github.com/thisnick/agent-wechat/commit/f83941360c8866782c1441ffd0e6b79ef4fdb608) Thanks [@thisnick](https://github.com/thisnick)! - Keep IME candidate confirmation and cancellation inside the message input instead of sending a message or removing a quoted reply. Track composition events, handle WebKit's IME key code, and ignore repeated Enter presses when sending.

  Remove the duplicate browser tooltip from conversation previews, keeping the conversation hover card.

  Add cancellation for queued text messages and dismissal for uncertain or failed send notices. Dismissal keeps the receipt for recovery and does not claim to cancel a message that may already have been accepted.

- [#314](https://github.com/thisnick/agent-wechat/pull/314) [`34b0204`](https://github.com/thisnick/agent-wechat/commit/34b02044bd53357559a5d1fd125e9b3ef602399f) Thanks [@thisnick](https://github.com/thisnick)! - Send ordinary group-text mentions through repeated CLI --mention flags, --mention-all, and structured API/SDK options. Resolve visible names from the group profile and retain generated text/source metadata across send replay and official-format writeback. Recognize received all-members mentions and apply them to OpenClaw's group mention gate. Reject unsupported quote/media combinations and ambiguous member lists.

- [#313](https://github.com/thisnick/agent-wechat/pull/313) [`16934c3`](https://github.com/thisnick/agent-wechat/commit/16934c3bfb25118b819476d5c93ed715570547df) Thanks [@thisnick](https://github.com/thisnick)! - Display shared text and URLs, WeChat Channels card metadata, plain group announcements, and file-upload notices instead of unsupported messages. Support quoted text/card/announcement replies and received notice quotes, while preserving recall suppression and stored source bytes. Upload notices do not start media transfers or OpenClaw model work.

- Updated dependencies [[`f839413`](https://github.com/thisnick/agent-wechat/commit/f83941360c8866782c1441ffd0e6b79ef4fdb608), [`8e007d7`](https://github.com/thisnick/agent-wechat/commit/8e007d70f04db207f9be34c913bceb759ca633fc), [`16934c3`](https://github.com/thisnick/agent-wechat/commit/16934c3bfb25118b819476d5c93ed715570547df)]:
  - @agent-wechat/client-ui@0.3.0

## 1.0.0

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

- [#292](https://github.com/thisnick/agent-wechat/pull/292) [`0071e0e`](https://github.com/thisnick/agent-wechat/commit/0071e0e7b2ee16e0cdd537c06d3ebe46ab0cd6b1) Thanks [@thisnick](https://github.com/thisnick)! - Restore the hidden desktop window when reopening AgentWechat from the macOS Dock. Reopening the app or choosing Open AgentWechat from the tray also restores minimized windows. Closing the window continues to keep the hub and notifications running in the background.

- [#282](https://github.com/thisnick/agent-wechat/pull/282) [`3f55bd1`](https://github.com/thisnick/agent-wechat/commit/3f55bd14ff6522467a05c87535635ded52a95e21) Thanks [@thisnick](https://github.com/thisnick)! - Expose saved direct-chat and group notification preferences as nullable `muted` values on thread lists and detail, using batched local reads per page. Receive phone contact changes through the independent contact sync stream, retain unsupported controls, and preserve official cursor publication and restart fences. Profile exports also wait for the contact checkpoint receipt. Ignore unmapped contact notices without selectors so they cannot interrupt message catch-up.

  Add native desktop notifications for new incoming messages in known unmuted chats, including when the window is hidden. Suppress history, outgoing messages, duplicates, stale mute state and the currently focused conversation. Add a settings toggle and click-to-open navigation.

  Support direct-chat mute/unmute through the existing thread notification API and CLI alongside group controls. Add signed desktop update discovery, channel settings, verified downloads, graceful install/restart, and final-artifact update manifests in the release workflow.

- Updated dependencies [[`dae1c62`](https://github.com/thisnick/agent-wechat/commit/dae1c62d16620e99dc61f5898e623eb3d9203b3b), [`2c2a02c`](https://github.com/thisnick/agent-wechat/commit/2c2a02c704a563dafcdb9c616ce22ec70e620d4b), [`312ffc9`](https://github.com/thisnick/agent-wechat/commit/312ffc95c2a40fe32dee92c227decbf6e2b64c19), [`4fc4ae8`](https://github.com/thisnick/agent-wechat/commit/4fc4ae8aea10b0c54dd70016dea5bd77f6c05444), [`e31e6c4`](https://github.com/thisnick/agent-wechat/commit/e31e6c47a1c49bd1884403bc7acc1854c07bb43f), [`00ed529`](https://github.com/thisnick/agent-wechat/commit/00ed5292c541772683f7cabdd0c98c2f5a2856bd), [`3f55bd1`](https://github.com/thisnick/agent-wechat/commit/3f55bd14ff6522467a05c87535635ded52a95e21)]:
  - @agent-wechat/client-ui@0.2.0

## 1.0.0-beta.20

### Patch Changes

- [#300](https://github.com/thisnick/agent-wechat/pull/300) [`4fc4ae8`](https://github.com/thisnick/agent-wechat/commit/4fc4ae8aea10b0c54dd70016dea5bd77f6c05444) Thanks [@thisnick](https://github.com/thisnick)! - Add chat pinning through direct hover/focus actions and conversation details, with optimistic presentation, a shared pending receipt, compact recovery feedback and keyboard/touch access. Keep unread badges visible, replace status indicators with Pin/Unpin and Mute/Unmute actions on hover, and show chat information without extra server requests. Preserve server ordering, refresh pin changes from other devices, and keep the selected chat, drafts, history and unread state while rows move.

  Add optional native notification sounds with a saved toggle and preview: the bundled Soft pair chime on macOS/Linux and the system notification sound on Windows. Keep existing notification suppression and limit sound during message bursts.

- Updated dependencies [[`4fc4ae8`](https://github.com/thisnick/agent-wechat/commit/4fc4ae8aea10b0c54dd70016dea5bd77f6c05444)]:
  - @agent-wechat/client-ui@0.2.0-beta.4

## 1.0.0-beta.19

## 1.0.0-beta.18

### Patch Changes

- [#294](https://github.com/thisnick/agent-wechat/pull/294) [`312ffc9`](https://github.com/thisnick/agent-wechat/commit/312ffc95c2a40fe32dee92c227decbf6e2b64c19) Thanks [@thisnick](https://github.com/thisnick)! - Speed up chat opening with reusable read-only database connections for daemon-owned profiles, batched attachment metadata and an immediate event-stream heartbeat. Keep offline inspection on source-preserving snapshots.

  Reuse authenticated conversation caches by last message ID with a five-minute history validation limit, retain hidden cached history across sign-out, catch up missed changes before restoring messages, and avoid redundant startup and foreground reloads. Cancel obsolete history requests when switching chats.

  Show contact photos and group member collages in the chat header on narrow screens, using the same avatar cache as the conversation list.

- [#292](https://github.com/thisnick/agent-wechat/pull/292) [`0071e0e`](https://github.com/thisnick/agent-wechat/commit/0071e0e7b2ee16e0cdd537c06d3ebe46ab0cd6b1) Thanks [@thisnick](https://github.com/thisnick)! - Restore the hidden desktop window when reopening AgentWechat from the macOS Dock. Reopening the app or choosing Open AgentWechat from the tray also restores minimized windows. Closing the window continues to keep the hub and notifications running in the background.

- Updated dependencies [[`312ffc9`](https://github.com/thisnick/agent-wechat/commit/312ffc95c2a40fe32dee92c227decbf6e2b64c19)]:
  - @agent-wechat/client-ui@0.2.0-beta.3

## 1.0.0-beta.17

### Patch Changes

- [#286](https://github.com/thisnick/agent-wechat/pull/286) [`2c2a02c`](https://github.com/thisnick/agent-wechat/commit/2c2a02c704a563dafcdb9c616ce22ec70e620d4b) Thanks [@thisnick](https://github.com/thisnick)! - Render compact link cards, images and stickers inside quotes using the received reply content. Include structured quoted article, music and video cards in the API, and support eligible shared-sticker originals and thumbnails through the validated media pipeline. Preserve reply-owned media, account isolation and recall restrictions without fetching stored originals to fill quotes. Keep older OpenClaw queued excerpts compatible with the new display fields while retaining checks on their original content. Show the reply text in desktop notifications for quoted replies instead of labeling them as attachments.

- [#285](https://github.com/thisnick/agent-wechat/pull/285) [`00ed529`](https://github.com/thisnick/agent-wechat/commit/00ed5292c541772683f7cabdd0c98c2f5a2856bd) Thanks [@thisnick](https://github.com/thisnick)! - Keep cached avatars and media readable after restarting the desktop app. Recover unreadable older image entries from the hub without clearing conversation history. Refresh saved hubs after forgetting a connection, add Back when switching hubs in the browser, clarify optional private certificates, and keep desktop settings reachable in shorter windows.

- Updated dependencies [[`2c2a02c`](https://github.com/thisnick/agent-wechat/commit/2c2a02c704a563dafcdb9c616ce22ec70e620d4b), [`00ed529`](https://github.com/thisnick/agent-wechat/commit/00ed5292c541772683f7cabdd0c98c2f5a2856bd)]:
  - @agent-wechat/client-ui@0.2.0-beta.2

## 1.0.0-beta.16

### Patch Changes

- [#282](https://github.com/thisnick/agent-wechat/pull/282) [`3f55bd1`](https://github.com/thisnick/agent-wechat/commit/3f55bd14ff6522467a05c87535635ded52a95e21) Thanks [@thisnick](https://github.com/thisnick)! - Expose saved direct-chat and group notification preferences as nullable `muted` values on thread lists and detail, using batched local reads per page. Receive phone contact changes through the independent contact sync stream, retain unsupported controls, and preserve official cursor publication and restart fences. Profile exports also wait for the contact checkpoint receipt. Ignore unmapped contact notices without selectors so they cannot interrupt message catch-up.

  Add native desktop notifications for new incoming messages in known unmuted chats, including when the window is hidden. Suppress history, outgoing messages, duplicates, stale mute state and the currently focused conversation. Add a settings toggle and click-to-open navigation.

  Support direct-chat mute/unmute through the existing thread notification API and CLI alongside group controls. Add signed desktop update discovery, channel settings, verified downloads, graceful install/restart, and final-artifact update manifests in the release workflow.

- Updated dependencies [[`3f55bd1`](https://github.com/thisnick/agent-wechat/commit/3f55bd14ff6522467a05c87535635ded52a95e21)]:
  - @agent-wechat/client-ui@0.2.0-beta.1

## 1.0.0-beta.15

## 1.0.0-beta.14

### Minor Changes

- [#279](https://github.com/thisnick/agent-wechat/pull/279) [`e31e6c4`](https://github.com/thisnick/agent-wechat/commit/e31e6c47a1c49bd1884403bc7acc1854c07bb43f) Thanks [@thisnick](https://github.com/thisnick)! - Add the AgentWechat desktop app for macOS, Windows and Ubuntu x64/ARM64, with a bundled local hub and CLI, system-keyring credentials, optional HTTPS network access and native file saving. Integrate signed Windows installers and notarized macOS packages into the prerelease release flow. Keep the standalone CLI independent, with protected credential files and a confirmed profile-removal command that retains local files.

  Add a browser chat client with account and group management, quoted replies, confirmed recalls, shared-link cards, searchable history, message previews and group avatars. Support photo/video viewing, zoom and swipe navigation, voice playback, original downloads, visible-avatar refresh, version-checked unread clearing and event-stream updates. Remember recent chats, drafts and verified media on the device without resending uncertain operations.

  Download received thumbnails and voice notes on the hub, and apply a per-account 50 MB default limit to automatic image, video, file and sticker downloads. Expose the policy through Settings, the API and CLI. Batch thread summaries from existing profile data without upstream requests or a separate stored preview model. Improve native profile initialization, group-operation locking and Windows credential/profile handling, with isolated simulator coverage for client workflows.

### Patch Changes

- Updated dependencies [[`e31e6c4`](https://github.com/thisnick/agent-wechat/commit/e31e6c47a1c49bd1884403bc7acc1854c07bb43f)]:
  - @agent-wechat/client-ui@0.2.0-beta.0
