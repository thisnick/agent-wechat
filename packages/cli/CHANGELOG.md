# @agent-wechat/cli

## 1.0.0-beta.17

### Patch Changes

- [#286](https://github.com/thisnick/agent-wechat/pull/286) [`2c2a02c`](https://github.com/thisnick/agent-wechat/commit/2c2a02c704a563dafcdb9c616ce22ec70e620d4b) Thanks [@thisnick](https://github.com/thisnick)! - Render compact link cards, images and stickers inside quotes using the received reply content. Include structured quoted article, music and video cards in the API, and support eligible shared-sticker originals and thumbnails through the validated media pipeline. Preserve reply-owned media, account isolation and recall restrictions without fetching stored originals to fill quotes. Keep older OpenClaw queued excerpts compatible with the new display fields while retaining checks on their original content. Show the reply text in desktop notifications for quoted replies instead of labeling them as attachments.

## 1.0.0-beta.16

### Patch Changes

- [#282](https://github.com/thisnick/agent-wechat/pull/282) [`3f55bd1`](https://github.com/thisnick/agent-wechat/commit/3f55bd14ff6522467a05c87535635ded52a95e21) Thanks [@thisnick](https://github.com/thisnick)! - Expose saved direct-chat and group notification preferences as nullable `muted` values on thread lists and detail, using batched local reads per page. Receive phone contact changes through the independent contact sync stream, retain unsupported controls, and preserve official cursor publication and restart fences. Profile exports also wait for the contact checkpoint receipt. Ignore unmapped contact notices without selectors so they cannot interrupt message catch-up.

  Add native desktop notifications for new incoming messages in known unmuted chats, including when the window is hidden. Suppress history, outgoing messages, duplicates, stale mute state and the currently focused conversation. Add a settings toggle and click-to-open navigation.

  Support direct-chat mute/unmute through the existing thread notification API and CLI alongside group controls. Add signed desktop update discovery, channel settings, verified downloads, graceful install/restart, and final-artifact update manifests in the release workflow.

## 1.0.0-beta.15

## 1.0.0-beta.14

### Minor Changes

- [#279](https://github.com/thisnick/agent-wechat/pull/279) [`e31e6c4`](https://github.com/thisnick/agent-wechat/commit/e31e6c47a1c49bd1884403bc7acc1854c07bb43f) Thanks [@thisnick](https://github.com/thisnick)! - Add the AgentWechat desktop app for macOS, Windows and Ubuntu x64/ARM64, with a bundled local hub and CLI, system-keyring credentials, optional HTTPS network access and native file saving. Integrate signed Windows installers and notarized macOS packages into the prerelease release flow. Keep the standalone CLI independent, with protected credential files and a confirmed profile-removal command that retains local files.

  Add a browser chat client with account and group management, quoted replies, confirmed recalls, shared-link cards, searchable history, message previews and group avatars. Support photo/video viewing, zoom and swipe navigation, voice playback, original downloads, visible-avatar refresh, version-checked unread clearing and event-stream updates. Remember recent chats, drafts and verified media on the device without resending uncertain operations.

  Download received thumbnails and voice notes on the hub, and apply a per-account 50 MB default limit to automatic image, video, file and sticker downloads. Expose the policy through Settings, the API and CLI. Batch thread summaries from existing profile data without upstream requests or a separate stored preview model. Improve native profile initialization, group-operation locking and Windows credential/profile handling, with isolated simulator coverage for client workflows.

### Patch Changes

- [#275](https://github.com/thisnick/agent-wechat/pull/275) [`fd5b127`](https://github.com/thisnick/agent-wechat/commit/fd5b12739e6591f81223f2b25aa6bd4e54aa3c2e) Thanks [@thisnick](https://github.com/thisnick)! - Sign out locally when WeChat rejects the current push, heartbeat or sync session
  with an authentication-required error. Stop automatic reconnect and session-renewal
  attempts, retain profiles and history, and publish the failure source and service
  code in account events. Late failures cannot sign out a newer login.

## 1.0.0-beta.13

### Patch Changes

- [#271](https://github.com/thisnick/agent-wechat/pull/271) [`083d90d`](https://github.com/thisnick/agent-wechat/commit/083d90d40d47b4589e2e65b13770b95e6fc3b626) Thanks [@thisnick](https://github.com/thisnick)! - Batch local recall checks for quoted originals when reading history pages. Reuse
  one page-scoped journal lookup and grouped profile queries instead of repeating
  database reads for every quote. Preserve embedded excerpts, recall suppression,
  conversation boundaries and fresh checks on subsequent requests.

- [#268](https://github.com/thisnick/agent-wechat/pull/268) [`f31be38`](https://github.com/thisnick/agent-wechat/commit/f31be388fc17807657555a0130df72da85cc79e5) Thanks [@thisnick](https://github.com/thisnick)! - Reuse a history request's captured profile snapshot when checking quoted-message recalls, avoiding repeated full-profile copies and hashing. Capture unfiltered history membership without reading message bodies, then load and normalize only the delivered page. Preserve fresh reads between requests, content search and recall suppression.

- [#270](https://github.com/thisnick/agent-wechat/pull/270) [`621298c`](https://github.com/thisnick/agent-wechat/commit/621298c4143d87d554153587f14452acbb785589) Thanks [@thisnick](https://github.com/thisnick)! - Use only the quoted content embedded in replies delivered by WeChat's server protocol. Include parsed quote text, link-card fields, image availability and sender metadata in normal message responses without substituting a stored original. Remove `GET /messages/{id}/quote`, the SDK's `quotedMessage` method and the separate quote-resolution response types; read quotes through the normal message endpoint and message lists. Keep local recall checks and quoted-author admission. Retrieve images using quote-owned metadata; preserve comments and unavailable status for other quoted media. Sending quoted replies continues through the normal send endpoint with the stored target's message ID and server ID.

## 1.0.0-beta.12

### Minor Changes

- [#257](https://github.com/thisnick/agent-wechat/pull/257) [`2d0b57c`](https://github.com/thisnick/agent-wechat/commit/2d0b57c3f2f5141aa85515a29384e50668c8928d) Thanks [@thisnick](https://github.com/thisnick)! - Add admin-scoped native group creation, rename, membership, own display name, announcement clearing, leave and group notification operations with stable idempotency keys and retained uncertain outcomes. Split member list and exact lookup, refresh server group details, and expose matching API/SDK methods.

  Add the owner-only `wechat_group_info` OpenClaw tool. Recognized incoming group metadata updates are saved before events; unknown sync controls remain pending. Automatic propagation of every edit made on another device is not established.

- [#255](https://github.com/thisnick/agent-wechat/pull/255) [`673c2c3`](https://github.com/thisnick/agent-wechat/commit/673c2c3c41e96d5d76b42682ab88b79348963a70) Thanks [@thisnick](https://github.com/thisnick)! - Add native ordinary-file transfers up to 512 MiB, including large-file routing, upload completion receipts, verified download readback, and persisted file-message reservations. Apply the file limit consistently in the daemon, CLI, SDK, and OpenClaw while retaining the existing image, video, and voice limits. Stream large file retrievals in the CLI and OpenClaw without base64 conversion. Keep ordinary-file transfers from blocking account heartbeats and incoming sync. An operator can request full verification of a completed upload on the original job only when message submission has never started. Interrupted or uncertain message reservations require review before another send.

  Keep the submitted OpenClaw job ID and idempotency key available when status polling times out or fails, without repeating the send.

- [#261](https://github.com/thisnick/agent-wechat/pull/261) [`e12e189`](https://github.com/thisnick/agent-wechat/commit/e12e1899910641730927fb8f5d603d2f3f2c61f5) Thanks [@thisnick](https://github.com/thisnick)! - Add native quoted text replies through the send API, TypeScript SDK and `wx messages send` quote flags. Resolve the original from saved history using its local message ID and exact server ID, retain encrypted reference material for idempotent sends, and write accepted replies into supported profiles.

  Resolve saved quote targets for voice, video, files, link cards and quoted replies in addition to text and images. Initial quote sending has static and synthetic coverage; live server acceptance and recipient rendering remain unverified.

- [#255](https://github.com/thisnick/agent-wechat/pull/255) [`673c2c3`](https://github.com/thisnick/agent-wechat/commit/673c2c3c41e96d5d76b42682ab88b79348963a70) Thanks [@thisnick](https://github.com/thisnick)! - Add normal/original video sends for MP4 sources through 50 MiB. Prepare and verify a compressed playback copy before uploading; original mode also uploads and verifies the unchanged source. Save prepared bytes and separate upload identities for recovery, preserve uncertain send handling, and expose videoQuality through the HTTP API, SDK, CLI --video-quality and OpenClaw message channelData. Honor OpenClaw forceDocument/asDocument to send media as ordinary files.

### Patch Changes

- [#259](https://github.com/thisnick/agent-wechat/pull/259) [`940719e`](https://github.com/thisnick/agent-wechat/commit/940719e22847a953d5d45427a066df723821ce46) Thanks [@thisnick](https://github.com/thisnick)! - Accept 64-bit last-message timestamps in incoming read controls while retaining the notification timestamp as the unread cutoff. Preserve the once-only partial unread effect and the remaining session-exit checkpoint barrier.

  Apply the supported unread-clear portion of incoming opcode-2 chat-entry controls once for existing visible ordinary chats. Preserve newer unread messages, uncertain-attempt markers and the complete-control checkpoint barrier. Report successful entry read changes through `thread.read-state` with `origin: remote_entry_read` and `completeControl: false`.

  Recognize narrowly verified Linux control forms without local profile effects: status opcodes 3, 15 and 16, system ClientCheckGetExtInfo and mmchatroombarannouncememt entries, and self-to-self type-10002 appmsg subtype 35. Preserve encrypted original bytes and deduplicated classification receipts. Normalize validated incoming group sender prefixes before parsing supported recalls; unsupported recall fields and other controls remain pending.

- [#262](https://github.com/thisnick/agent-wechat/pull/262) [`c1147ed`](https://github.com/thisnick/agent-wechat/commit/c1147edca08fc2cdb126ac591a54b67b75077f37) Thanks [@thisnick](https://github.com/thisnick)! - Match official recall rows and notices, derive API sender and direction from existing message and receipt evidence without new author columns, and remove recalled entries from supported official message search indexes. Keep unknown authors unset and leave unsupported index layouts pending for repair.

## 1.0.0-beta.11

### Patch Changes

- [#253](https://github.com/thisnick/agent-wechat/pull/253) [`e259b46`](https://github.com/thisnick/agent-wechat/commit/e259b462a7dc7e6511a2680cb76c84b066229bb1) Thanks [@thisnick](https://github.com/thisnick)! - Correct metadata for voice notes uploaded in multiple requests by including the client-ID buffer only in the first request. Notes continue to support up to 60 seconds.

## 1.0.0-beta.10

### Patch Changes

- [#251](https://github.com/thisnick/agent-wechat/pull/251) [`7f85e93`](https://github.com/thisnick/agent-wechat/commit/7f85e93689fb94feb09adcc97ddad94cb018a523) Thanks [@thisnick](https://github.com/thisnick)! - Complete verified self-to-self `HandOffMaster` notifications with an empty argument or an empty handoff list using inner opcode 4 and the supported attributes, plus peer-directed `packagesUpdated`/`newPackage` notifications. These forms have no local profile effect in the Linux x64 4.1.13.26 client. Retain their original bytes and projection receipts so supported batches can finish, while handoff items, other inner opcodes, extra handoff attributes and unsupported controls continue to block the official profile checkpoint.

- [#248](https://github.com/thisnick/agent-wechat/pull/248) [`112ad51`](https://github.com/thisnick/agent-wechat/commit/112ad51a96491a060495bac661a77783ec16597c) Thanks [@thisnick](https://github.com/thisnick)! - Send outgoing voice as notes of up to 60 seconds, WeChat's single-message limit, instead of 10-second notes. Longer audio splits into the fewest equal-length notes and keeps per-note IDs, partial results and `needs_review` on uncertainty. Notes above 32 KiB of SILK (about 15 seconds) are uploaded as consecutive offset ranges of one voice message; that upload path has offline fixtures only and still requires live verification.

## 1.0.0-beta.9

### Minor Changes

- [#241](https://github.com/thisnick/agent-wechat/pull/241) [`6c16321`](https://github.com/thisnick/agent-wechat/commit/6c16321abd7a8554a291bac0b2f65e95a39e89db) Thanks [@thisnick](https://github.com/thisnick)! - Add durable own-message recalls through the native daemon and `wx messages recall`, using exact message/server identities and private submission receipts. Preserve uncertain outcomes without automatic resubmission, handle incoming recall controls with durable account-scoped markers and events, suppress recalled message/quote/media content, and report pending recall work in profile-copy completeness checks.

  Preserve existing native registry and control database families when admission or migration fails, including their WAL, shared-memory and rollback-journal companions.

- [#237](https://github.com/thisnick/agent-wechat/pull/237) [`4f82fd9`](https://github.com/thisnick/agent-wechat/commit/4f82fd96ffa30e87258df178feffb127e7a05393) Thanks [@thisnick](https://github.com/thisnick)! - Add macOS ARM64 and native Windows x64/ARM64 release targets, portable private-file and daemon ownership handling, and ownership-bound graceful shutdown. Extend verified npm provisioning and OpenClaw managed hosting to these targets. Build candidate artifacts on native CI workers; live account and official-app profile compatibility require separate platform acceptance and are not established by the offline smoke tests.

  Apply the same private directory/file protections to WXGF decoder and media encoder scratch data, including owner-only Windows ACLs.

  Create scratch files and directories with an explicit current-user owner and private permissions from the start, including when Windows runs with an elevated administrator token.

- [#243](https://github.com/thisnick/agent-wechat/pull/243) [`d123c6e`](https://github.com/thisnick/agent-wechat/commit/d123c6e12829ed64ad74ceca971b38952d8ba012) Thanks [@thisnick](https://github.com/thisnick)! - Restore received link/music/video share titles, descriptions and URLs with optional verified thumbnail previews. Add a durable `share` send type, CLI `--share` payload and current-conversation `wechat_send_share` tool. Share submission and thumbnail interoperability still require verification with live official installs.

### Patch Changes

- [#244](https://github.com/thisnick/agent-wechat/pull/244) [`c49f335`](https://github.com/thisnick/agent-wechat/commit/c49f335362a145b18fb10e876b5c240868af4c7f) Thanks [@thisnick](https://github.com/thisnick)! - Add experimental handling for the server-directed LAN challenge during desktop phone/QR login. Advertise local IPv4 addresses through the native login service and answer the phone's temporary TCP token exchange, with cancellation, expiry and one upload per attempt. Live challenge acceptance remains unverified.

## 1.0.0-beta.8

### Patch Changes

- [#236](https://github.com/thisnick/agent-wechat/pull/236) [`16824e7`](https://github.com/thisnick/agent-wechat/commit/16824e7bad780b18519c7f017a9ae05511ee3857) Thanks [@thisnick](https://github.com/thisnick)! - Accept complete images with trailing source data while preserving downloaded bytes and integrity hashes. Use format-specific JPEG, PNG, GIF and WebP boundaries, decode static WebP images correctly, retain animated frames, and report fixed image-validation reasons instead of a generic media-processing failure.

  Read supported incoming WXGF containers through a bounded FFmpeg decoder, retaining original bytes in the server and official DAT cache. Provide separately encrypted, source-bound JPEG still previews for OpenClaw vision and quote retrieval. Source animation timing/transparency and sticker sending remain outside this change.

  Download incoming files advertising attachment encryption version 0 through the verified native CDN path, preserving strict advertised size/checksum validation. Report unsupported or malformed attachment metadata as specific media errors instead of `unsupported_profile`; PDFs remain byte-preserving file-path references in OpenClaw.

## 1.0.0-beta.7

### Patch Changes

- [#234](https://github.com/thisnick/agent-wechat/pull/234) [`a59b59d`](https://github.com/thisnick/agent-wechat/commit/a59b59d0a0dd465d5991daa72f47a5f9ed9431fc) Thanks [@thisnick](https://github.com/thisnick)! - Support text and image quotes with parent-bound same-chat original lookup, bounded embedded-content fallback when the original is absent, and verified quoted-image delivery to OpenClaw. Preserve comments and explicit pending/expired status, keep image source identity, enforce quoted-author admission for default tools, and restore bounded forwarded chat bundle rendering from the stable release.

## 1.0.0-beta.6

### Minor Changes

- [#232](https://github.com/thisnick/agent-wechat/pull/232) [`b281c4c`](https://github.com/thisnick/agent-wechat/commit/b281c4cbc9e0d874bfb852bddf9668b8159249b6) Thanks [@thisnick](https://github.com/thisnick)! - Add optional account-wide archive tools for thread discovery, cross-thread search, selected-thread history, exact message lookup and attachment retrieval. OpenClaw tool policies control access without a second plugin permission list; default tools remain restricted to the current thread. Archive tools preserve verified media paths, image content and bounded download status handling.

  Add the account-scoped local message search endpoint with retained, query-bound pagination across threads, preserving peer identities, edits and deletion tombstones during continuation.

## 1.0.0-beta.5

## 1.0.0-beta.4

### Patch Changes

- [#223](https://github.com/thisnick/agent-wechat/pull/223) [`700f2c1`](https://github.com/thisnick/agent-wechat/commit/700f2c132eecd0fe16a80b5fee10efd1d2c3fdb9) Thanks [@thisnick](https://github.com/thisnick)! - Make whole-thread marking the only public read operation. Remove message-selection flags and request fields, keep expected-version checks, and mark a thread after confirmed OpenClaw model handoff unless reads are disabled. Add durable native cross-device read notifications that retain uncertain outcomes without automatic resubmission. Apply recognized incoming unread-clear controls while retaining unsupported session-exit behavior.

- [#228](https://github.com/thisnick/agent-wechat/pull/228) [`b4c9e8a`](https://github.com/thisnick/agent-wechat/commit/b4c9e8a1886abdf773d9cefff45474646fcef9d7) Thanks [@thisnick](https://github.com/thisnick)! - Receive incoming work over an authenticated persistent WeChat connection and sync immediately on native push notifications. Preserve server-directed heartbeats and transport keepalives, reconnect with bounded backoff, and fall back to heartbeat polling when the persistent connection is unavailable. Expose the active incoming transport in account status and retain durable sync, account isolation and logout fences.

  Bound stalled HTTP event streams so OpenClaw can reconnect and replay its durable cursor after a network interruption, including connections that remain open without receiving keepalives.

## 1.0.0-beta.3

### Patch Changes

- [#222](https://github.com/thisnick/agent-wechat/pull/222) [`dad905d`](https://github.com/thisnick/agent-wechat/commit/dad905dad01bc6b1c7237604244592be54e2c6a2) Thanks [@thisnick](https://github.com/thisnick)! - Validate supported official table columns by name instead of physical order. Previously, an unread table with `create_time` before `local_id` could let a new message reach history but fail the thread update, preventing OpenClaw from receiving its committed event. Preserve strict column attributes, keys and indexes, and recover retained messages without another network send or duplicate event.

## 1.0.0-beta.2

### Patch Changes

- [#220](https://github.com/thisnick/agent-wechat/pull/220) [`afbeddc`](https://github.com/thisnick/agent-wechat/commit/afbeddcfe83f5812f28581c66a08821bbc5f28a1) Thanks [@thisnick](https://github.com/thisnick)! - Resolve continued native message pages from their retained message IDs instead of decoding the entire thread again for every page. Preserve stable membership, current edits, deletion tombstones and account/query checks. Allow a bounded thirty-minute history view so large downloads can finish without the former five-minute cursor expiry.

## 1.0.0-beta.1

### Patch Changes

- [#217](https://github.com/thisnick/agent-wechat/pull/217) [`cc5a217`](https://github.com/thisnick/agent-wechat/commit/cc5a2175ad7fd5ae972f2126aaa1e2498575faf9) Thanks [@thisnick](https://github.com/thisnick)! - Keep generated release notes outside the build checkout so native release manifests correctly identify clean source. Reject dirty architecture fragments before publishing release assets, rather than waiting for the npm and image publication gates.

## 1.0.0-beta.0

### Major Changes

- [#215](https://github.com/thisnick/agent-wechat/pull/215) [`1e33596`](https://github.com/thisnick/agent-wechat/commit/1e335969911474870764926cf30f8c105c91107c) Thanks [@thisnick](https://github.com/thisnick)! - Replace the official-client container backend with the standalone native WeChat daemon and Rust `wx` CLI. This is a breaking release: remove `wx-legacy`, the UI server/image and Wechaty integrations; use native v1 HTTP/SSE and managed or external OpenClaw hosting. Existing container API configurations must be migrated explicitly.

  Take over a stopped, supported official Linux profile in place with `auth login --official[=/path]`, preserving account/device/database identity and saved-login material. Continue supported encrypted database, local read-state and media writes with durable sync, idempotent text/media jobs, independently encoded voice and explicit uncertainty handling. Add offline inspection, metadata catalogs and generated API contracts. Fresh-profile creation and copy import/export remain experimental extras.

  Distribute verified Linux x64 and ARM64 GNU executables (glibc 2.36+), an npm launcher and a native-only multi-architecture container. Add CI package/runtime tests, exact-source publication gates, codec notices and optional service/Compose examples. Deployment and publication remain separate operator actions. The shared TypeScript SDK remains private and is bundled into the OpenClaw package.

  Use OpenClaw environment/SecretRef credentials instead of `native.tokenFile`; owned daemon startup passes the token through a private stdin pipe without writing it or inheriting Gateway secrets. Clarify daemon-host profile paths and the unverified status of macOS/Windows profile compatibility in the published migration guide.

  Include the versioned Changesets package changelog entries in GitHub release notes, preserving Markdown and operator notes during release retries. Fail release preparation when exact-version changelogs are missing or entirely empty.

  Prepare the native rewrite for `1.0.0-beta.0` through Changesets prerelease mode and a CI-generated version PR. Keep beta npm/GitHub channels separate from stable, retain exact-version image/artifact identities and reject mismatched prerelease publication.

## 0.16.0

## 0.15.6

## 0.15.5

### Patch Changes

- [#206](https://github.com/thisnick/agent-wechat/pull/206) [`666deea`](https://github.com/thisnick/agent-wechat/commit/666deea513e22551b0fbf04b8a360c430892db9d) Thanks [@thisnick](https://github.com/thisnick)! - Allow releases to proceed after successful npm publication without waiting for registry propagation, and resume binary and Docker publication when npm packages already exist but the GitHub release is missing.

## 0.15.4

### Patch Changes

- [#204](https://github.com/thisnick/agent-wechat/pull/204) [`40d6dde`](https://github.com/thisnick/agent-wechat/commit/40d6dde0c8ac90e6e5fca4b6db8a2f73020215af) Thanks [@thisnick](https://github.com/thisnick)! - Fix GitHub OIDC release publishing by ensuring pnpm uses the pinned, OIDC-capable npm instead of Node's bundled npm. Add a publishing-subprocess dry-run check so an incorrect npm version fails before publication.

## 0.15.3

### Patch Changes

- [#202](https://github.com/thisnick/agent-wechat/pull/202) [`e321b5b`](https://github.com/thisnick/agent-wechat/commit/e321b5b9b3557e41e5b6b84a5a41f8dead0ee68b) Thanks [@thisnick](https://github.com/thisnick)! - Retrieve supported WeChat type-47 stickers through the existing media endpoint, preserving GIF animation. Download and validate original CDN bytes into a bounded, account-scoped server cache without opening chats or writing to WeChat's cache. Existing CLI media commands can save the returned sticker files; automatic OpenClaw sticker ingestion is unchanged.

## 0.15.2

### Patch Changes

- [#199](https://github.com/thisnick/agent-wechat/pull/199) [`b5ee9ac`](https://github.com/thisnick/agent-wechat/commit/b5ee9ac123d6fb2a3de6e3e54a2a7a5b233ba81d) Thanks [@thisnick](https://github.com/thisnick)! - Report explicitly expired, uncached media without retrying native downloads. Preserve cached attachments and show expiry in matching CLI and OpenClaw attachment feedback.

## 0.15.1

### Patch Changes

- [#195](https://github.com/thisnick/agent-wechat/pull/195) [`a71cf8b`](https://github.com/thisnick/agent-wechat/commit/a71cf8b66f99978b6c53bb43a7398f42692a9284) Thanks [@thisnick](https://github.com/thisnick)! - Accept valid phone images with recoverable metadata warnings and use best-available image retrieval by default in the HTTP API, CLI, and OpenClaw. The reported quality distinguishes original, standard, and thumbnail copies; callers can request strict full resolution with `quality=full` or `wx messages media --full`.

## 0.15.0

### Minor Changes

- [#193](https://github.com/thisnick/agent-wechat/pull/193) [`b288698`](https://github.com/thisnick/agent-wechat/commit/b2886984f0576e84d014a094620416165b91d852) Thanks [@thisnick](https://github.com/thisnick)! - Add queued voice-note sending with 50-second audio splitting, job status and cancellation, CLI support, and OpenClaw voice reply delivery.

## 0.14.0

## 0.13.0

### Minor Changes

- [#187](https://github.com/thisnick/agent-wechat/pull/187) [`93cdb04`](https://github.com/thisnick/agent-wechat/commit/93cdb0482629b90f48ade6aa7dc111586f8117c8) Thanks [@thisnick](https://github.com/thisnick)! - Return the requested image quality without silently substituting a thumbnail. The CLI requests full resolution by default; use --thumbnail for the cached preview. HTTP clients that omit quality retain the legacy thumbnail-first behavior.

  Validate cached file attachments against their message's size and content hash before returning them, and reject titles containing filesystem paths.

## 0.12.2

## 0.12.1

## 0.12.0

## 0.11.15

## 0.11.14

## 0.11.13

## 0.11.12

## 0.11.11

## 0.11.10

## 0.11.9

## 0.11.8

## 0.11.7

## 0.11.6

## 0.11.5

### Patch Changes

- [#118](https://github.com/thisnick/agent-wechat/pull/118) [`fe27f34`](https://github.com/thisnick/agent-wechat/commit/fe27f344a8dc7746ebcf2ea5bf2bd5e886e2dd4e) Thanks [@thisnick](https://github.com/thisnick)! - Replace x86_64 NativeFunction lookup with pure memory hashmap walk in chat-select to prevent crashes

## 0.11.4

### Patch Changes

- [#116](https://github.com/thisnick/agent-wechat/pull/116) [`42c80f6`](https://github.com/thisnick/agent-wechat/commit/42c80f6d050d0210402f0a9fa00bf107cadd6db1) Thanks [@thisnick](https://github.com/thisnick)! - Move WeChat restart logic from entrypoint bash loop into agent-server health monitor

## 0.11.3

### Patch Changes

- [#113](https://github.com/thisnick/agent-wechat/pull/113) [`55544aa`](https://github.com/thisnick/agent-wechat/commit/55544aa6320b9bed170fd6a614bde7f32ffe3c99) Thanks [@thisnick](https://github.com/thisnick)! - Fix verify_key to use immutable=1 to avoid acquiring locks on WeChat databases

- [`8f7c6c2`](https://github.com/thisnick/agent-wechat/commit/8f7c6c2abbc14895b584a04ea7f4edc9cf7a39eb) Thanks [@thisnick](https://github.com/thisnick)! - Log WeChat crash/recovery in health monitor instead of silently ignoring process disappearance

- [#111](https://github.com/thisnick/agent-wechat/pull/111) [`587994b`](https://github.com/thisnick/agent-wechat/commit/587994b98dc7bb7c272e133ad621989ca9513602) Thanks [@thisnick](https://github.com/thisnick)! - Add lazy key extraction to chat and contact query handlers so keys are extracted on demand when missing

## 0.11.2

### Patch Changes

- [#108](https://github.com/thisnick/agent-wechat/pull/108) [`97bc89d`](https://github.com/thisnick/agent-wechat/commit/97bc89db4ac279926be285651ad8452f0e95b1e8) Thanks [@thisnick](https://github.com/thisnick)! - Secure noVNC with full-token auth on the WebSocket proxy (no 8-char VNC limit). Opening /vnc/ shows a login prompt for your token. Direct access via ?token=xxx&autoconnect=true also works. VNC and websockify listen on localhost only.

## 0.11.1

### Patch Changes

- [#106](https://github.com/thisnick/agent-wechat/pull/106) [`66ee117`](https://github.com/thisnick/agent-wechat/commit/66ee117ad9f52c9adefd2cac21ed007828737c79) Thanks [@thisnick](https://github.com/thisnick)! - Fix SQLite migration error: use CREATE UNIQUE INDEX instead of UNIQUE column constraint in ALTER TABLE for novnc_port, since SQLite does not support adding UNIQUE columns via ALTER TABLE.

## 0.11.0

### Minor Changes

- [#101](https://github.com/thisnick/agent-wechat/pull/101) [`470a907`](https://github.com/thisnick/agent-wechat/commit/470a9076a32ea1a83a8901ff636be9531754415f) Thanks [@thisnick](https://github.com/thisnick)! - Encrypt agent.db at rest using SQLCipher with the auth token as the encryption key. Existing unencrypted databases are automatically migrated on startup. If decryption fails (e.g. token changed), the database is discarded and recreated fresh.

- [#104](https://github.com/thisnick/agent-wechat/pull/104) [`50aacf0`](https://github.com/thisnick/agent-wechat/commit/50aacf0c2d09983e75a8847fb394252d4a8548bc) Thanks [@thisnick](https://github.com/thisnick)! - Replace raw VNC port (5900) with noVNC browser-based viewer on port 6080. x11vnc now listens on 127.0.0.1 only (internal to the container), and websockify serves the noVNC web client. Access the desktop at `http://localhost:6080/vnc.html?autoconnect=true`. No VNC client installation needed.

## 0.10.2

## 0.10.1

### Patch Changes

- [#94](https://github.com/thisnick/agent-wechat/pull/94) [`83f3991`](https://github.com/thisnick/agent-wechat/commit/83f399199b4c01a4575b12cfc7900737b2694f0a) Thanks [@thisnick](https://github.com/thisnick)! - feat: add video media support (type 43)

## 0.10.0

### Minor Changes

- [#90](https://github.com/thisnick/agent-wechat/pull/90) [`20d16a1`](https://github.com/thisnick/agent-wechat/commit/20d16a18b0ec24e6d517224c9b644bab4a9bfa6e) Thanks [@thisnick](https://github.com/thisnick)! - Add WeChat health monitor to detect and kill unresponsive processes

## 0.9.5

### Patch Changes

- [#87](https://github.com/thisnick/agent-wechat/pull/87) [`191a7d4`](https://github.com/thisnick/agent-wechat/commit/191a7d43068530e9562f269f61193e6ad04ba2f9) Thanks [@thisnick](https://github.com/thisnick)! - Add mutex to ensure only one plan execution runs at a time

## 0.9.4

## 0.9.3

## 0.9.2

## 0.9.1

## 0.9.0

## 0.8.5

## 0.8.4

### Patch Changes

- [#70](https://github.com/thisnick/agent-wechat/pull/70) [`6c27002`](https://github.com/thisnick/agent-wechat/commit/6c27002c3356cd34e84da77b0a44aaf6556146ed) Thanks [@thisnick](https://github.com/thisnick)! - Harden Wechaty puppet login websocket handling and clarify CLI status behavior.

  - Treat websocket `error`/`close` callbacks as non-fatal once a terminal login event has been seen.
  - Normalize empty websocket error messages to a stable fallback.
  - Close the login subscription handle immediately after `login_success` to reduce late transport noise.
  - Make `wx status` report explicit container up/down state, and only show server/login details when the container is running.

## 0.8.3

## 0.8.2

## 0.8.1

## 0.8.0

### Minor Changes

- [#59](https://github.com/thisnick/agent-wechat/pull/59) [`eb95ac6`](https://github.com/thisnick/agent-wechat/commit/eb95ac6f6ac0bc072450a12f636ee19544201ae2) Thanks [@thisnick](https://github.com/thisnick)! - Add Wechaty puppet package and contacts API

  - New `@agent-wechat/wechaty-puppet` package: bridges Wechaty bots to WeChat via the agent-wechat server
  - New `GET /api/contacts` endpoint: queries contact.db for full address book
  - New CLI commands: `contacts list` and `contacts find`

## 0.7.10

## 0.7.9

## 0.7.8

## 0.7.7

## 0.7.6

## 0.7.5

## 0.7.4

### Patch Changes

- [`dc1e1c8`](https://github.com/thisnick/agent-wechat/commit/dc1e1c8342030c88b665a7b526eac96b75634b42) Thanks [@thisnick](https://github.com/thisnick)! - Fix `wx update`: use native fetch instead of gh CLI, chmod +x after docker cp, reliable arch detection via container uname

## 0.7.3

### Patch Changes

- [`6617da2`](https://github.com/thisnick/agent-wechat/commit/6617da2970b314e8f829587840b7b0764770bd54) Thanks [@thisnick](https://github.com/thisnick)! - Fix container architecture detection in `wx update` command

## 0.7.2

### Patch Changes

- [#39](https://github.com/thisnick/agent-wechat/pull/39) [`65289a7`](https://github.com/thisnick/agent-wechat/commit/65289a7ecd8f0107166fbe28dcd71352d7863d9f) Thanks [@thisnick](https://github.com/thisnick)! - Fix binary publish job in release workflow

  - Remove read-only flag from Docker source mount that prevented container startup (exit code 125)
  - Create GitHub Release before uploading binary assets (release not found error)

## 0.7.1

### Patch Changes

- [#37](https://github.com/thisnick/agent-wechat/pull/37) [`d043f9c`](https://github.com/thisnick/agent-wechat/commit/d043f9c7f1fd0ed8f9aa1081643540c0d9487f22) Thanks [@thisnick](https://github.com/thisnick)! - Fix binary publish job in release workflow

  - Remove read-only flag from Docker source mount that prevented container startup (exit code 125)

## 0.7.0

### Minor Changes

- [#35](https://github.com/thisnick/agent-wechat/pull/35) [`fa0d64a`](https://github.com/thisnick/agent-wechat/commit/fa0d64aa74af8b0e973984977d09dee27a0c96a7) Thanks [@thisnick](https://github.com/thisnick)! - Add binary artifact publishing and CLI update command

  - Release workflow now publishes standalone `agent-server` binaries (amd64/arm64) as GitHub Release assets alongside Docker images
  - New `wx update` command downloads the binary matching the CLI version and hot-swaps it into the running container via `docker cp` + process restart

## 0.6.0

### Minor Changes

- [#33](https://github.com/thisnick/agent-wechat/pull/33) [`0cdae88`](https://github.com/thisnick/agent-wechat/commit/0cdae8861159464ed4a64005ebda70616e3ba2a7) Thanks [@thisnick](https://github.com/thisnick)! - Add container-level transparent proxy via redsocks. Use `wx up --proxy user:pass@host:port` or set the `PROXY` env var to route all container traffic through an upstream HTTP CONNECT or SOCKS5 proxy. Removes app-level proxy UI automation from the login flow.

## 0.5.0

## 0.4.1

## 0.4.0

## 0.3.1

## 0.3.0

### Minor Changes

- [`3dba4d7`](https://github.com/thisnick/agent-wechat/commit/3dba4d7c3381fc73bd5e0732bdaf6f89341b480b) Thanks [@thisnick](https://github.com/thisnick)! - Add WeChat crash recovery and auth status enum

  - Auto-restart WeChat in entrypoint with crash-loop backoff (3s delay, 30s backoff after 5 rapid restarts)
  - Replace `isLoggedIn: boolean` with `status: "logged_in" | "logged_out" | "app_not_running" | "unknown"` in auth endpoint
  - Detect WeChat process not running via `find_wechat_pid()` check before a11y observation
  - Notify agent on auth state transitions (session lost, server unreachable, first-poll not authenticated)
  - Add `app_not_running` diagnostic in openclaw extension status checks

## 0.2.4

### Patch Changes

- [`09aa334`](https://github.com/thisnick/agent-wechat/commit/09aa334d9fef0a67ab092f5f68e10540bd8af9bf) Thanks [@thisnick](https://github.com/thisnick)! - Fix image media retrieval for newly received images by using message_resource.db as the primary file lookup instead of hardlink.db, which has an indexing delay.

## 0.2.3

### Patch Changes

- [`91d6750`](https://github.com/thisnick/agent-wechat/commit/91d67504ffc3965c046ea28e13e2d9d3d5fedaf3) Thanks [@thisnick](https://github.com/thisnick)! - - Use versioned Docker image tags matching CLI version, with fallback to latest
  - Inject version from package.json at build time
  - Fix release workflow Docker tag parsing for scoped packages
  - Increase media poll retries from 5 to 15
  - Add setup docs to both package READMEs

## 0.2.2

### Patch Changes

- [`32e6d04`](https://github.com/thisnick/agent-wechat/commit/32e6d04eb4aca78f6143feb3b0b4c86d08a39f44) Thanks [@thisnick](https://github.com/thisnick)! - Use versioned Docker image tags matching CLI version, fix release workflow version parsing

## 0.2.1

### Patch Changes

- [`ff4e228`](https://github.com/thisnick/agent-wechat/commit/ff4e2288b0f89d3f4ea8e78778a6f31f8d86352d) Thanks [@thisnick](https://github.com/thisnick)! - Auto-pull Docker image in `wx up` when not found locally, add README docs for both packages

## 0.2.0

### Minor Changes

- [`9f1911d`](https://github.com/thisnick/agent-wechat/commit/9f1911dfc80194330dc9e6c352b2c181515ce300) Thanks [@thisnick](https://github.com/thisnick)! - Initial public release

  - CLI (`wx`) for managing agent-wechat containers
  - OpenClaw WeChat channel extension with login, directory, and heartbeat adapters
  - Multi-arch Docker image (amd64/arm64)
