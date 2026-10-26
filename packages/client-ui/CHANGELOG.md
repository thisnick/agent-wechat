# @agent-wechat/client-ui

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
