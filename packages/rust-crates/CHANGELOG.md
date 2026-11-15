# @agent-wechat/rust-crates

## 2.5.0

### Minor Changes

- [#525](https://github.com/thisnick/agent-wechat/pull/525) [`aca8954`](https://github.com/thisnick/agent-wechat/commit/aca8954edb2129f44142acea9b1e1990b5d7e4eb) Thanks [@thisnick](https://github.com/thisnick)! - Stickers are no longer limited to 240 pixels. Images and GIFs up to 1024 pixels, the largest sticker WeChat saves, keep their size and encoding, so larger stickers stay sharp on high-resolution screens. A larger file is refused unless you ask to shrink it: `fit` now shrinks to 1024 pixels, and the new `maxEdge` option (`--max-edge` in the CLI) shrinks the longest side to any size from 16 to 1024 pixels. Stickers made from video can set their size with `maxEdge` and their frame rate with the new `fps` option (`--fps`, 1–30); they default to 240 pixels at 10 fps as before. A trim now applies before the 200-frame and 10-second checks, so part of a longer GIF can be kept. Preparing a large animation no longer needs memory for every frame at once.

- [#525](https://github.com/thisnick/agent-wechat/pull/525) [`25442d7`](https://github.com/thisnick/agent-wechat/commit/25442d7bff89c8695abbbb2b346c76e14f0c16d3) Thanks [@thisnick](https://github.com/thisnick)! - Stickers can now be made from videos up to 512 MB; images and GIFs stay limited to 64 MB. Uploads are written to disk as they arrive, and sticker preparation reads the video from a temporary file, so a large source no longer needs its whole size in the daemon's memory. **Add to Saved** deletes each upload once its sticker is added, or when the dialog closes, and says separately whether a file is empty or too large. API clients can do the same with the new `DELETE /uploads/{id}`, which returns 409 `upload_in_use` while a send or preparation still needs the upload. The daemon now also deletes finished uploads every hour instead of only at startup. The OpenClaw sticker tool keeps its 64 MB limit, because OpenClaw's workspace bridge reads a whole file into memory.

### Patch Changes

- [#522](https://github.com/thisnick/agent-wechat/pull/522) [`4f43734`](https://github.com/thisnick/agent-wechat/commit/4f43734df53a0b1f9238ece77fffad15fb6f0069) Thanks [@thisnick](https://github.com/thisnick)! - Official sticker packs such as WeChat Bubble Pup now download. Pack archives are now requested from WeChat's CDN as storage downloads, the way the official client requests them. Before, they were requested as ordinary file downloads, and the CDN refused some packs' archives with HTTP 400, so a pack or sticker download failed with `media_service_rejected`.

- [#525](https://github.com/thisnick/agent-wechat/pull/525) [`945ccf6`](https://github.com/thisnick/agent-wechat/commit/945ccf63ee49315d03bc9fe57911682a95b3966e) Thanks [@thisnick](https://github.com/thisnick)! - Sticker sends are about four times faster. A sticker is sent in 8 KB pieces, and every piece used to start a new secure connection to WeChat; a 370 KB sticker took about 70 seconds. Pieces now go over the account's existing long-lived connection when it is connected, as the official client sends them, and otherwise reuse the account's current short-link ticket until it nears expiry. The same sticker now takes about 15 seconds. Other requests to WeChat also skip the per-request connection setup and are faster.

## 2.4.2

### Patch Changes

- [#514](https://github.com/thisnick/agent-wechat/pull/514) [`ccdaa7b`](https://github.com/thisnick/agent-wechat/commit/ccdaa7b2e467ae8456cbeb1ec9f3e863d87c0b07) Thanks [@HypoxanthineOvO](https://github.com/HypoxanthineOvO)! - Fix received normal-quality videos that failed to download with `media_integrity_failed` when WeChat served a different playback copy than the message advertised. The daemon now accepts that copy after checking it against the checksum WeChat sends with it, including for videos received before this release. A reply that marks a different copy but has no valid checksum still fails, and original-quality copies must still match the message exactly.

## 2.4.1

### Patch Changes

- [#495](https://github.com/thisnick/agent-wechat/pull/495) [`7bcef20`](https://github.com/thisnick/agent-wechat/commit/7bcef20d8c3c0e8fa7b57e438b32f7c929b9469a) Thanks [@thisnick](https://github.com/thisnick)! - Fix adding larger custom stickers by splitting encrypted upload requests across transport records. Preserve collection revision headers in the desktop app so moving and removing Saved stickers reaches the daemon with the required precondition.

  Make Add to Saved use one primary Add button, show saving progress, and return to Saved after successful imports. Keep partial failures open for retry without resubmitting completed items.

  Simplify import review to previews and file selection by removing fit, start and duration controls.

  Accept verified complete-upload acknowledgements when adding previously uploaded stickers again. Show sticker job failures separately from connection errors, and show the dragged sticker and its drop position while reordering Saved. Keep edit controls still instead of jiggling.

## 2.4.0

### Minor Changes

- [#489](https://github.com/thisnick/agent-wechat/pull/489) [`c765365`](https://github.com/thisnick/agent-wechat/commit/c7653653809aee3baf842825eac83f3d7e75df43) Thanks [@juangrukat](https://github.com/juangrukat)! - Exclude deleted rows from the saved contact catalog when the official profile exposes a deletion marker. Keep nullable markers and readable layouts without that column working, and preserve historical chat labels and group rosters.

  Add `ProfileReader::contact` for exact reads of retained contact and group rows. Group admission uses this lookup so hiding deleted catalog entries preserves its existing eligibility checks and errors. Ambiguous stored usernames are rejected.

- [#477](https://github.com/thisnick/agent-wechat/pull/477) [`28290b3`](https://github.com/thisnick/agent-wechat/commit/28290b31140fd9bf027bb3237d995fc98be8220b) Thanks [@thisnick](https://github.com/thisnick)! - Images can now be sent at full resolution. Pass `--image-quality original` to `wx messages send --image`, or `"imageQuality": "original"` to `POST /sends`. A JPEG within the supported source limits is then sent unchanged as the image's original copy, and other formats are converted to JPEG once at full resolution. The CLI and HTTP API default to `standard`, offering only compressed standard and thumbnail copies without an Original download option. Existing saved upload receipts retain their previous original availability. OpenClaw image sends automatically request original quality and allow large image transfers and delayed local receipts to finish without resubmitting the send.

  JPEG images up to 8,192 pixels per edge and 64 million pixels can now also be downloaded and read back; previously images wider or taller than 4,096 pixels were rejected.

  Rust image sends now use one `NativeClient::send_image` method for both qualities, matching video sends. `UploadedImage.original` is now `Option<UploadedMedia>`: use `None` for standard and thumbnail only, or `Some(original)` to include an original. This is a breaking Rust API change included in this minor release; callers constructing the struct must wrap their original in `Some(...)` and handle its possible absence when reading it. The account-scoped method, request builder and local metadata helper use the same optional-original model.

- [#481](https://github.com/thisnick/agent-wechat/pull/481) [`e9f2b12`](https://github.com/thisnick/agent-wechat/commit/e9f2b126cbf61869f8fe21214847742c4f276fa2) Thanks [@thisnick](https://github.com/thisnick)! - Add Saved sticker import, placement, removal, preparation and sends, plus official pack browsing, details, membership, media acquisition and one-pack GIF export. The composer gains a sticker tray, in-tray editing, import review and pack browsing. REST resources, generated contracts and account-bound Rust operations share durable jobs and official profile storage. OpenClaw manages Saved, sends existing references through the standard message action, delivers visual previews and transfers files through the host's execution-workspace bridge. Outbound media honors the host's remote file reader.

### Patch Changes

- [#481](https://github.com/thisnick/agent-wechat/pull/481) [`6098ebf`](https://github.com/thisnick/agent-wechat/commit/6098ebfa592b4695617f09b52f39297ed796e904) Thanks [@thisnick](https://github.com/thisnick)! - Share recent sticker catalog metadata across clients and release the account writer during sticker operations, contact and room lookups, group mutations and recalls. Preserve operation ordering, accepted receipts and session fences. Read added packs and already indexed message media on consistent snapshots. Show confirmed pack membership before refreshing previews.

- [#482](https://github.com/thisnick/agent-wechat/pull/482) [`527f766`](https://github.com/thisnick/agent-wechat/commit/527f766aff88e5c8fc946d3edbbfb73a713a51ad) Thanks [@juangrukat](https://github.com/juangrukat)! - Match catalog searches without regard to letter case, combining words across saved contact fields. Keep Unicode matching consistent between profile queries and the account catalog, including paginated contact and group-member results.

- [#487](https://github.com/thisnick/agent-wechat/pull/487) [`f62795a`](https://github.com/thisnick/agent-wechat/commit/f62795a1a2d7de4a56df9a9a37cef2d57c9e8903) Thanks [@juangrukat](https://github.com/juangrukat)! - Retrieve normal videos when WeChat selects a different verified response representation. Preserve strict original identity checks, range and response-metadata validation, transfer limits and unsupported representation rejection.

- [#469](https://github.com/thisnick/agent-wechat/pull/469) [`f74dd46`](https://github.com/thisnick/agent-wechat/commit/f74dd4668b359340f8eb374d18790626883aaf51) Thanks [@thisnick](https://github.com/thisnick)! - The Rust libraries now share one version with the CLI, OpenClaw extension and desktop app. Every release publishes all five crates under the release version, so `wechat-protocol` X.Y.Z comes from the same source as `wx` X.Y.Z. Library versions jump from 2.2.0 to the next application version; no API changes.

- [#481](https://github.com/thisnick/agent-wechat/pull/481) [`d0a391c`](https://github.com/thisnick/agent-wechat/commit/d0a391c29441600dbf5be82dbc3782e92d2aa07d) Thanks [@thisnick](https://github.com/thisnick)! - Preserve successful operations when concurrent requests re-encrypt identical session credentials. Continue to reject stale credential, cookie, connection and logout state.

## 2.2.0

### Minor Changes

- [#464](https://github.com/thisnick/agent-wechat/pull/464) [`7d3190b`](https://github.com/thisnick/agent-wechat/commit/7d3190b5c8b04e5b2a0a73912646ca2a05c6fc54) Thanks [@thisnick](https://github.com/thisnick)! - `wechat-protocol` now reports why a request failed as a type, so embedders no longer match error text. A non-zero service code carries `ServiceRejected { service, code }`; `is_session_rejected` tells you the account needs a new login or renewal. A stop, transfer or renewal that ends a request or a notification wait carries `AccountInterrupted`, so a push loop can end quietly on `Stopped` and reconnect on `CredentialsSuperseded`. Error messages are unchanged.

  Logout and shutdown no longer need a Tokio runtime: `PendingLogout::submit_blocking`, `PendingLogout::submit_guarded_blocking`, `NativeClient::logout_blocking` and `NativeClient::shutdown_blocking` run on the caller's own blocking worker, with the same at-most-once logout as the async methods.

### Patch Changes

- [#462](https://github.com/thisnick/agent-wechat/pull/462) [`e52cb77`](https://github.com/thisnick/agent-wechat/commit/e52cb7704fc82e072358e41fdc9c004e97cc0098) Thanks [@thisnick](https://github.com/thisnick)! - Fix large file downloads when the CDN omits redundant source-size metadata, while retaining transfer length, decrypted size, and checksum validation.

## 2.1.0

### Minor Changes

- [#441](https://github.com/thisnick/agent-wechat/pull/441) [`1b4c215`](https://github.com/thisnick/agent-wechat/commit/1b4c2152d4fcfa302c6bebc48a473eab97064f0f) Thanks [@Lazily01](https://github.com/Lazily01)! - Add a native OpenIM avatar lookup path and optional group context to avatar refresh. Keep server-returned OpenIM failure codes available in operation results. A controlled macOS ARM64 account accepted two group-member avatar downloads; broader account and platform acceptance remains separate.

## 2.0.0

### Patch Changes

- [#414](https://github.com/thisnick/agent-wechat/pull/414) [`147ccb2`](https://github.com/thisnick/agent-wechat/commit/147ccb2d9be71d556e2379d026deaa6120b1ef6e) Thanks [@thisnick](https://github.com/thisnick)! - Version the five published Rust libraries together through a private Changesets
  package, independently of application and npm releases. Library releases retain
  exact dependency versions and source verification, and use separate `crates-v`
  tags and library changelogs.
