# @agent-wechat/client-ui

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
