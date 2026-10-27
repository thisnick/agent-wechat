# @agent-wechat/shared

## 1.0.0-beta.6

### Minor Changes

- [#298](https://github.com/thisnick/agent-wechat/pull/298) [`acbc39d`](https://github.com/thisnick/agent-wechat/commit/acbc39d8039943de2f61054a249a0a0cbefb685c) Thanks [@thisnick](https://github.com/thisnick)! - Add backend thread pinning for ordinary direct chats and active groups. Chat responses include nullable saved pin state; new lists place pinned chats first. Add admin-only pin/unpin API, SDK and CLI operations using the existing idempotency and recovery journal. Recognized incoming pin changes update the official profile and emit thread.updated. Built-in peers and unknown updates remain unsupported; the frontend is unchanged.

## 1.0.0-beta.5

### Patch Changes

- [#294](https://github.com/thisnick/agent-wechat/pull/294) [`312ffc9`](https://github.com/thisnick/agent-wechat/commit/312ffc95c2a40fe32dee92c227decbf6e2b64c19) Thanks [@thisnick](https://github.com/thisnick)! - Speed up chat opening with reusable read-only database connections for daemon-owned profiles, batched attachment metadata and an immediate event-stream heartbeat. Keep offline inspection on source-preserving snapshots.

  Reuse authenticated conversation caches by last message ID with a five-minute history validation limit, retain hidden cached history across sign-out, catch up missed changes before restoring messages, and avoid redundant startup and foreground reloads. Cancel obsolete history requests when switching chats.

  Show contact photos and group member collages in the chat header on narrow screens, using the same avatar cache as the conversation list.

## 1.0.0-beta.4

### Patch Changes

- [#282](https://github.com/thisnick/agent-wechat/pull/282) [`3f55bd1`](https://github.com/thisnick/agent-wechat/commit/3f55bd14ff6522467a05c87535635ded52a95e21) Thanks [@thisnick](https://github.com/thisnick)! - Expose saved direct-chat and group notification preferences as nullable `muted` values on thread lists and detail, using batched local reads per page. Receive phone contact changes through the independent contact sync stream, retain unsupported controls, and preserve official cursor publication and restart fences. Profile exports also wait for the contact checkpoint receipt. Ignore unmapped contact notices without selectors so they cannot interrupt message catch-up.

  Add native desktop notifications for new incoming messages in known unmuted chats, including when the window is hidden. Suppress history, outgoing messages, duplicates, stale mute state and the currently focused conversation. Add a settings toggle and click-to-open navigation.

  Support direct-chat mute/unmute through the existing thread notification API and CLI alongside group controls. Add signed desktop update discovery, channel settings, verified downloads, graceful install/restart, and final-artifact update manifests in the release workflow.

## 1.0.0-beta.3

### Minor Changes

- [#279](https://github.com/thisnick/agent-wechat/pull/279) [`e31e6c4`](https://github.com/thisnick/agent-wechat/commit/e31e6c47a1c49bd1884403bc7acc1854c07bb43f) Thanks [@thisnick](https://github.com/thisnick)! - Add the AgentWechat desktop app for macOS, Windows and Ubuntu x64/ARM64, with a bundled local hub and CLI, system-keyring credentials, optional HTTPS network access and native file saving. Integrate signed Windows installers and notarized macOS packages into the prerelease release flow. Keep the standalone CLI independent, with protected credential files and a confirmed profile-removal command that retains local files.

  Add a browser chat client with account and group management, quoted replies, confirmed recalls, shared-link cards, searchable history, message previews and group avatars. Support photo/video viewing, zoom and swipe navigation, voice playback, original downloads, visible-avatar refresh, version-checked unread clearing and event-stream updates. Remember recent chats, drafts and verified media on the device without resending uncertain operations.

  Download received thumbnails and voice notes on the hub, and apply a per-account 50 MB default limit to automatic image, video, file and sticker downloads. Expose the policy through Settings, the API and CLI. Batch thread summaries from existing profile data without upstream requests or a separate stored preview model. Improve native profile initialization, group-operation locking and Windows credential/profile handling, with isolated simulator coverage for client workflows.

## 1.0.0-beta.2

### Minor Changes

- [#257](https://github.com/thisnick/agent-wechat/pull/257) [`2d0b57c`](https://github.com/thisnick/agent-wechat/commit/2d0b57c3f2f5141aa85515a29384e50668c8928d) Thanks [@thisnick](https://github.com/thisnick)! - Add admin-scoped native group creation, rename, membership, own display name, announcement clearing, leave and group notification operations with stable idempotency keys and retained uncertain outcomes. Split member list and exact lookup, refresh server group details, and expose matching API/SDK methods.

  Add the owner-only `wechat_group_info` OpenClaw tool. Recognized incoming group metadata updates are saved before events; unknown sync controls remain pending. Automatic propagation of every edit made on another device is not established.

- [#261](https://github.com/thisnick/agent-wechat/pull/261) [`e12e189`](https://github.com/thisnick/agent-wechat/commit/e12e1899910641730927fb8f5d603d2f3f2c61f5) Thanks [@thisnick](https://github.com/thisnick)! - Add native quoted text replies through the send API, TypeScript SDK and `wx messages send` quote flags. Resolve the original from saved history using its local message ID and exact server ID, retain encrypted reference material for idempotent sends, and write accepted replies into supported profiles.

  Resolve saved quote targets for voice, video, files, link cards and quoted replies in addition to text and images. Initial quote sending has static and synthetic coverage; live server acceptance and recipient rendering remain unverified.

### Patch Changes

- [#262](https://github.com/thisnick/agent-wechat/pull/262) [`c1147ed`](https://github.com/thisnick/agent-wechat/commit/c1147edca08fc2cdb126ac591a54b67b75077f37) Thanks [@thisnick](https://github.com/thisnick)! - Match official recall rows and notices, derive API sender and direction from existing message and receipt evidence without new author columns, and remove recalled entries from supported official message search indexes. Keep unknown authors unset and leave unsupported index layouts pending for repair.

## 1.0.0-beta.1

### Patch Changes

- [#223](https://github.com/thisnick/agent-wechat/pull/223) [`700f2c1`](https://github.com/thisnick/agent-wechat/commit/700f2c132eecd0fe16a80b5fee10efd1d2c3fdb9) Thanks [@thisnick](https://github.com/thisnick)! - Make whole-thread marking the only public read operation. Remove message-selection flags and request fields, keep expected-version checks, and mark a thread after confirmed OpenClaw model handoff unless reads are disabled. Add durable native cross-device read notifications that retain uncertain outcomes without automatic resubmission. Apply recognized incoming unread-clear controls while retaining unsupported session-exit behavior.

- [#228](https://github.com/thisnick/agent-wechat/pull/228) [`b4c9e8a`](https://github.com/thisnick/agent-wechat/commit/b4c9e8a1886abdf773d9cefff45474646fcef9d7) Thanks [@thisnick](https://github.com/thisnick)! - Receive incoming work over an authenticated persistent WeChat connection and sync immediately on native push notifications. Preserve server-directed heartbeats and transport keepalives, reconnect with bounded backoff, and fall back to heartbeat polling when the persistent connection is unavailable. Expose the active incoming transport in account status and retain durable sync, account isolation and logout fences.

  Bound stalled HTTP event streams so OpenClaw can reconnect and replay its durable cursor after a network interruption, including connections that remain open without receiving keepalives.

## 1.0.0-beta.0

### Major Changes

- [#215](https://github.com/thisnick/agent-wechat/pull/215) [`1e33596`](https://github.com/thisnick/agent-wechat/commit/1e335969911474870764926cf30f8c105c91107c) Thanks [@thisnick](https://github.com/thisnick)! - Replace the official-client container backend with the standalone native WeChat daemon and Rust `wx` CLI. This is a breaking release: remove `wx-legacy`, the UI server/image and Wechaty integrations; use native v1 HTTP/SSE and managed or external OpenClaw hosting. Existing container API configurations must be migrated explicitly.

  Take over a stopped, supported official Linux profile in place with `auth login --official[=/path]`, preserving account/device/database identity and saved-login material. Continue supported encrypted database, local read-state and media writes with durable sync, idempotent text/media jobs, independently encoded voice and explicit uncertainty handling. Add offline inspection, metadata catalogs and generated API contracts. Fresh-profile creation and copy import/export remain experimental extras.

  Distribute verified Linux x64 and ARM64 GNU executables (glibc 2.36+), an npm launcher and a native-only multi-architecture container. Add CI package/runtime tests, exact-source publication gates, codec notices and optional service/Compose examples. Deployment and publication remain separate operator actions. The shared TypeScript SDK remains private and is bundled into the OpenClaw package.

  Use OpenClaw environment/SecretRef credentials instead of `native.tokenFile`; owned daemon startup passes the token through a private stdin pipe without writing it or inheriting Gateway secrets. Clarify daemon-host profile paths and the unverified status of macOS/Windows profile compatibility in the published migration guide.

  Include the versioned Changesets package changelog entries in GitHub release notes, preserving Markdown and operator notes during release retries. Fail release preparation when exact-version changelogs are missing or entirely empty.

  Prepare the native rewrite for `1.0.0-beta.0` through Changesets prerelease mode and a CI-generated version PR. Keep beta npm/GitHub channels separate from stable, retain exact-version image/artifact identities and reject mismatched prerelease publication.

## 0.3.1

### Patch Changes

- [#195](https://github.com/thisnick/agent-wechat/pull/195) [`a71cf8b`](https://github.com/thisnick/agent-wechat/commit/a71cf8b66f99978b6c53bb43a7398f42692a9284) Thanks [@thisnick](https://github.com/thisnick)! - Accept valid phone images with recoverable metadata warnings and use best-available image retrieval by default in the HTTP API, CLI, and OpenClaw. The reported quality distinguishes original, standard, and thumbnail copies; callers can request strict full resolution with `quality=full` or `wx messages media --full`.

## 0.3.0

### Minor Changes

- [#193](https://github.com/thisnick/agent-wechat/pull/193) [`b288698`](https://github.com/thisnick/agent-wechat/commit/b2886984f0576e84d014a094620416165b91d852) Thanks [@thisnick](https://github.com/thisnick)! - Add queued voice-note sending with 50-second audio splitting, job status and cancellation, CLI support, and OpenClaw voice reply delivery.

## 0.2.0

### Minor Changes

- [#187](https://github.com/thisnick/agent-wechat/pull/187) [`93cdb04`](https://github.com/thisnick/agent-wechat/commit/93cdb0482629b90f48ade6aa7dc111586f8117c8) Thanks [@thisnick](https://github.com/thisnick)! - Return the requested image quality without silently substituting a thumbnail. The CLI requests full resolution by default; use --thumbnail for the cached preview. HTTP clients that omit quality retain the legacy thumbnail-first behavior.

  Validate cached file attachments against their message's size and content hash before returning them, and reject titles containing filesystem paths.
