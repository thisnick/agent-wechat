# @agent-wechat/rust-crates

## 3.2.1

### Patch Changes

- [#737](https://github.com/thisnick/agent-wechat/pull/737) [`9cf4e29`](https://github.com/thisnick/agent-wechat/commit/9cf4e294fa92a818131115a23585350ee6655df1) Thanks [@thisnick](https://github.com/thisnick)! - Group member lists fill faster after setting up a new profile: background group refreshes now run in batches of 5 per pass, wake the account loop on enqueue and while due groups remain, yield the account writer lock around each metadata request, and back off individual failing groups so one failure does not block healthy ones.

- [#732](https://github.com/thisnick/agent-wechat/pull/732) [`45e9ace`](https://github.com/thisnick/agent-wechat/commit/45e9ace80a901c7e3448837c740dd6aa350ee187) Thanks [@thisnick](https://github.com/thisnick)! - Clear saved group member display names and opaque member fields in `chat_room.ext_buffer` when a roster refresh omits them for returned members, matching the official Linux client while preserving unchanged delta members and unknown extension chunks. Also omit field 2 locally when clearing your own group display name via `MyName`, and update `wechat_profile::NativeMemberPatch` fields to `NativeFieldPatch<T>` (`Keep`, `Clear`, `Set`).

- [#733](https://github.com/thisnick/agent-wechat/pull/733) [`52b49b9`](https://github.com/thisnick/agent-wechat/commit/52b49b97942dd222053b4cb49799932cbe90f933) Thanks [@thisnick](https://github.com/thisnick)! - Expire operational notes in `control.sqlite` that previously grew forever with traffic, and retire permanently superseded metadata receipts (#730).

  - Contact phase markers (`contact-notifications:`, `contact-pinning:`, `contact-pinning-time:`, `contact-privacy-observation:`), non-sync `friend-attention-stage:` markers, and finished or held group refresh markers (`incoming-group-done:`, `incoming-group-held:`) are now removed after 30 days once their owning sync batch, metadata receipt or contact change has finished. Held group refreshes keep their sealed diagnostic response bodies under `incoming-group-receipt:` for 7 days.
  - Login QR images (`qr:`) and orphaned `metadata-hydrated:` checkpoints are removed once their login or metadata request finishes. Login idempotency keys (`login-key:`), superseded saved-login journals (`saved-login-superseded:`) and raw bytes of applied sync controls (`sync-control:` not paired with `skipped-control:` or referenced by `recall-control:`) are removed after 7 days.
  - Skipped WAL conversion event receipts (`event:profile-wal-conversion-skipped:`) now hash the error text to bound key size and expire after 30 days.
  - Permanently superseded or unrequested metadata receipts are retired from `metadata-pending:` instead of looping on every recovery pass and blocking profile exports.

- [#726](https://github.com/thisnick/agent-wechat/pull/726) [`324b065`](https://github.com/thisnick/agent-wechat/commit/324b065a2d3652b4435239986b99ea444fa20a43) Thanks [@thisnick](https://github.com/thisnick)! - Stop a group refresh from repeating forever and stop interrupted starts from keeping database copies on a full disk (#722).

  - When a group changed again while its information was being refreshed, the daemon kept fetching that group's details from WeChat on every pass and wrote ever-longer rows into `control.sqlite`. It now fetches once more and stops. Rows earlier versions left behind are removed by the regular cleanup after seven days, and the database shrinks once there is room to compact it.
  - Copies that an interrupted start left in `.sqlite-admission-*` folders are now removed before the daemon copies a database again, not only after a start succeeds. A start that failed for lack of disk space used to keep them, so it could never free the space it needed. A copy left while the daemon was already opening the database is still kept until a start succeeds. When the disk is full, the error now says how much free space the check needs.

- [#736](https://github.com/thisnick/agent-wechat/pull/736) [`11d6ba6`](https://github.com/thisnick/agent-wechat/commit/11d6ba6f4da5f25c18453de452f412fa5f2a2787) Thanks [@thisnick](https://github.com/thisnick)! - Fix freezing and blurry incoming video in direct video calls.

  - Large keyframes now assemble. Phones send a big keyframe in several bursts, each with its own timestamp, and the daemon treated every new timestamp as a new frame, so these keyframes never completed and video froze until a smaller one arrived.
  - Lost video packets are now retransmitted. Our NACKs set byte 6 to a counter, which official senders treat as an unknown format and ignore, so it is now 0 as they expect. Retransmitted packets (`0x67`, sent as Live98 with extension byte 4) are accepted and added to their frame, and P2S reports count them in the ARQ field.
  - When the other side NACKs our video, the daemon resends the requested packets once from the last 300 ms of sent video, as the official client does.
  - Incomplete frames are held for 500 ms of sender time, instead of a fixed number of frames, so retransmissions have time to arrive. NACKs wait 20–100 ms depending on jitter, are sent once per gap, and stop after 300 ms.
  - Live98 NACKs and retransmissions carry a separate counter in the sequence field, so they no longer go through the video replay check, where a repeated value could close the media channel.

  `wechat_protocol::calls::video_packet::encode_video_nack` no longer takes an `arq_seq` argument, `VideoNack::arq_seq` is now `VideoNack::format`, `VideoChannelPayload` has a new `ArqResponse` variant, and the new `wechat_protocol::calls::video_arq` module provides the sender-side retransmission cache.

- [#731](https://github.com/thisnick/agent-wechat/pull/731) [`9076c39`](https://github.com/thisnick/agent-wechat/commit/9076c39d0a9f2a7fdcabeb6d0ddbe9edda73cdd2) Thanks [@thisnick](https://github.com/thisnick)! - Retention of the daemon's internal `control.sqlite` notes now comes from one list of every kind of note, and keeps every note for as long as before. The general way the daemon saves such a note now refuses keys over 16 KiB, which only a bug can produce (#722 grew keys to 86 KB).

## 3.2.0

### Minor Changes

- [#714](https://github.com/thisnick/agent-wechat/pull/714) [`388f275`](https://github.com/thisnick/agent-wechat/commit/388f27568eea6a15b616358513813ed0460f9ebb) Thanks [@thisnick](https://github.com/thisnick)! - Add async parity for the remaining blocking-only `wechat-protocol` paths: `NativeClient::request` with `AsyncAccountRequest` (the async counterpart of `request_blocking`, keeping a verified reply when the account commit fails), async `upload_sticker_chunk_raw`, `custom_stickers_raw`, `send_sticker_chunk_raw`, `sticker_pack_page_raw`, `sticker_pack_detail_raw`, `sticker_pack_captions_raw` and `sticker_pack_acquisition_raw`, and LAN risk-challenge support in async QR login (`poll_login_guarded_async`, `ProtocolTransport::with_lan_login`, enabled by `ProtocolTransport::native()`). The protocol examples now use the native async transport.

- [#714](https://github.com/thisnick/agent-wechat/pull/714) [`91f8056`](https://github.com/thisnick/agent-wechat/commit/91f8056a72a2b3a9858354663c9b73745918b5a0) Thanks [@thisnick](https://github.com/thisnick)! - The daemon's CDN uploads and downloads, file-backed uploads, and avatar, sticker, link-thumbnail, Enterprise WeChat and Channels video downloads now use the native async transport; message media URLs go through `NativeHttpTransport` with `DestinationPolicy::PublicOnly`. `wechat-protocol` adds `HttpTransport::send_streaming` (with `HttpBodySink` and `HttpResponseHead`) so long downloads report progress and stop per piece, streams Channels videos through it, and adds `TransportError::Interrupted` so CDN transfers retry the same address after a connection drops mid-request, as the blocking session does.

- [#714](https://github.com/thisnick/agent-wechat/pull/714) [`ce03c0c`](https://github.com/thisnick/agent-wechat/commit/ce03c0cd8b7a705033afa206ae9e0aa90a929486) Thanks [@thisnick](https://github.com/thisnick)! - Deprecate `wechat-protocol`'s blocking APIs ahead of their removal in 4.0: `mmtls::Session` and `short_exchange`, `AccountPushConnection` and `open_push_connection_blocking`, `AccountRequest` and `request_blocking`, `renew_session_blocking`, `logout_blocking`, `shutdown_blocking`, `PendingLogout::submit_blocking` / `submit_guarded_blocking`, the blocking QR and saved-device login methods, the blocking `CdnClient` transfer methods, `PreparedFileUpload`, and the blocking avatar, sticker, link-thumbnail, Enterprise WeChat and Channels downloaders. Each deprecation note names its async replacement.

- [#714](https://github.com/thisnick/agent-wechat/pull/714) [`3630475`](https://github.com/thisnick/agent-wechat/commit/3630475469f9778073a19bf6a6f9ba74de595142) Thanks [@thisnick](https://github.com/thisnick)! - Add default native async transports to `wechat-protocol`: `ProtocolTransport::native()`, `TokioStreamTransport`, `TokioSpawner` and `NativeHttpTransport` (pooled HTTP/1.1, no proxy or redirects, streamed receive budgets) with `DestinationPolicy::PublicOnly` for message-supplied media URLs. Add `TransportError::DestinationNotAllowed`, which the async standalone downloaders report with the codes their blocking versions use (`*_dns_not_public`, or `channels_download_failed` for Channels videos), plus `media::FileUploadSource`, `PreparedAsyncUpload::from_file` and an `AsyncDownloadSink` implementation for `tokio::fs::File`.

## 3.1.1

### Patch Changes

- [#709](https://github.com/thisnick/agent-wechat/pull/709) [`239a696`](https://github.com/thisnick/agent-wechat/commit/239a69674dda2e970cdf14f32fb3df9cad2492cb) Thanks [@thisnick](https://github.com/thisnick)! - Make large-file uploads (files and original videos over 25 MiB) handle CDN errors like the official client, and say why an upload stopped:

  - A CDN error that the reply marks as retryable (`isretry`), or a code the official client retries, is retried with a new sequence number on the next CDN address. The retry waits `retrysec` when given, otherwise the official backoff. At most 8 server errors are retried per upload, waiting 15 minutes in total at most. Before, any CDN error stopped the upload.
  - A successful reply's `isretry`, `retrysec`, `isoverload` and `isgetcdn` no longer stop the upload, as in the official client.
  - An empty or unreadable CDN reply is retried like a dropped connection on every CDN transfer, uploads and downloads, resending the same request.
  - A CDN rejection now sets `serviceStage: "large_file_upload"` and `serviceCode` on the job, with reason `media_service_rejected`, instead of `media_processing_failed` with no detail. A large-file job that stops during the transfer also includes `result.cdnUploadFailure` with the request (`apply`, `uploadblock`, `fin` or `complete`), the block, the attempts for that request, the server errors retried and a fixed cause.

## 3.1.0

### Minor Changes

- [#697](https://github.com/thisnick/agent-wechat/pull/697) [`30df28c`](https://github.com/thisnick/agent-wechat/commit/30df28cc1f9c7263910f69288ce3b57bc9efc73f) Thanks [@thisnick](https://github.com/thisnick)! - Align incoming voice message downloads and decoding with official WeChat clients to fix intermittent `media_processing_failed` and `media_integrity_failed` errors (#681).

  - Support SILK DTX silence frames (`0x0000` via `lostFlag = 1`), `0xFFFF` EOF trailers, headerless `#!SILK_V3` and relaxed leading bytes, and incoming voice notes over 60 seconds in `wechat-protocol::silk`.
  - Add `#!OPUS_RAW_V1` decoding (`decode_raw_opus_v1`) in `wechat-calls::opus`, plus AMR/WAV/MP3 FFmpeg fallback for playable WAV conversion.
  - Store raw downloaded voice bytes on `--quality encoded` without forcing SILK decode, matching official clients, and add additive CDN voice download support (`VoiceCdnDescriptor` / `CdnClient::download_voice_with_progress`, `filetype = 15`, 16 MiB cap) with fallback to CGI 128 (`downloadvoice`).
  - Relax CGI 128 response validation to tolerate server offset/buffer-length drift, shorter transcoded responses (via 3 consecutive empty chunks), and single-chunk overflow past the advertised XML length; handle `cancelflag=1` and streaming `endflag=0 -> 1` in-place updates (`wechat_profile::apply_voice_message_update`); and emit download progress after each chunk.
  - Record non-secret `voiceDownloadFailure` diagnostics on failed voice jobs, allow explicit re-download of failed voice assets, and map `media_job_not_retryable` (`409`), `media_queue_full` (`503`), and `voice_cancelled` / `media_cancelled` (`410`) instead of `500 operation_failed`.

- [#700](https://github.com/thisnick/agent-wechat/pull/700) [`fcb3e7e`](https://github.com/thisnick/agent-wechat/commit/fcb3e7ea97af9fae7b92fa2e4d639fad7f76c748) Thanks [@thisnick](https://github.com/thisnick)! - Add asynchronous CDN and standalone media transfer APIs (`CdnClient::*_async` including `download_voice_with_progress_async`, `AsyncDownloadSink`, `PreparedAsyncUpload` / `AsyncUploadSource`, `download_advertised_avatar_async`, `fetch_sticker_async`, `fetch_sticker_original_async`, `fetch_link_thumbnail_async`, `fetch_tp_media_async`, `fetch_tp_media_to_async`, `fetch_channels_video_async`, `Clock::sleep`, `HttpResponse::header_values`, and `TransportError::ResponseTooLarge`) backed by `HttpTransport` and `Clock` for native and `wasm32-unknown-unknown` targets.

- [#698](https://github.com/thisnick/agent-wechat/pull/698) [`6056766`](https://github.com/thisnick/agent-wechat/commit/605676647fb91272bc882fe7bcc020644a313251) Thanks [@thisnick](https://github.com/thisnick)! - Add pluggable async transport abstractions (`wechat_protocol::transport`) and `wasm32-unknown-unknown` target compatibility to `wechat-protocol` and `wechat-media`.

  - Add `transport::{AsyncByteStream, Clock, HttpMethod, HttpOverStream, HttpRequest, HttpResponse, HttpTransport, MaybeSend, MaybeSync, ProtocolTransport, ReadOutcome, StreamTransport, SystemClock, TransportError, TransportFuture}`.
  - Add `mmtls::AsyncSession`, `mmtls::short_exchange_async`, `AsyncPushConnection`, `NativeClient::with_transport`, `NativeClient::from_account_with_transport`, `NativeClient::from_account_with_transport_and_persistence`, async login methods (`*_login_async`), and async `lookup_openim_contact` / `lookup_openim_contact_raw` / `channels_*` methods.
  - Gate blocking OS-socket, `reqwest`, `wechat-platform`, and native FFmpeg/WXGF helpers behind `cfg(not(target_arch = "wasm32"))` so `wechat-media` and `wechat-protocol --no-default-features --features pure-rust-crypto` compile cleanly for `wasm32-unknown-unknown` without adding Cargo features or changing native builds.

- [#692](https://github.com/thisnick/agent-wechat/pull/692) [`4718a4d`](https://github.com/thisnick/agent-wechat/commit/4718a4d972fd3893ff83834fac0872b12a100ab5) Thanks [@thisnick](https://github.com/thisnick)! - Add an opt-in `pure-rust-crypto` Cargo feature to `wechat-protocol` alongside the default-enabled `openssl-crypto` backend so consumers can build without vendored OpenSSL (`default-features = false, features = ["pure-rust-crypto"]`), and switch `wechat-calls` (`native-runtime`), the shipped `wx` CLI/daemon, and the desktop sidecar build to use `wechat-protocol`'s `pure-rust-crypto` backend (SQLCipher profile and store encryption continue to use OpenSSL). Add portable `crypto::Curve`, `crypto::Ecdh::generate_for`, `crypto::Ecdh::from_private_for`, `crypto::cbc_encrypt`, and `crypto::cbc_decrypt` APIs; OpenSSL-typed `Ecdh::generate`, `Ecdh::from_private`, and `crypto::cbc` remain available when `openssl-crypto` is enabled. Callers setting `default-features = false` must select at least one crypto backend (`openssl-crypto` or `pure-rust-crypto`).

- [#688](https://github.com/thisnick/agent-wechat/pull/688) [`75ad10b`](https://github.com/thisnick/agent-wechat/commit/75ad10b38bd0c85d4c46e68de2916d1c533ff12e) Thanks [@thisnick](https://github.com/thisnick)! - Gate the bundled C SILK voice encoder and decoder (`wechat_protocol::silk::encode` and `wechat_protocol::silk::decode`, plus the `cc` build step) behind a default-enabled `voice-silk` Cargo feature on `wechat-protocol`. Building `wechat-protocol` with `default-features = false` skips compiling C SILK while keeping SILK framing validation, WAV formatting, voice size/duration constants, and all protocol codecs. Callers using `default-features = false` must explicitly enable `voice-silk` to call `silk::encode` or `silk::decode`.

### Patch Changes

- [#695](https://github.com/thisnick/agent-wechat/pull/695) [`92e543e`](https://github.com/thisnick/agent-wechat/commit/92e543e3bac1305213dccebd5ed02340f9ab7433) Thanks [@thisnick](https://github.com/thisnick)! - Make native CDN chunked uploads and downloads reuse a pooled HTTP/1.1 connection per transfer instead of creating a new client with `Connection: close` per chunk, retry transient transport drops and 5xx/408/429 responses with bounded backoff while honoring cancellation/logout fences, and fail over across authenticated CDN server IPs.

## 3.0.0

### Major Changes

- [#685](https://github.com/thisnick/agent-wechat/pull/685) [`fced109`](https://github.com/thisnick/agent-wechat/commit/fced109a37a0dd73ae8e768d916f4f57ab55a39c) Thanks [@thisnick](https://github.com/thisnick)! - Fix missing incoming video on calls with official Android clients when the opening keyframe arrives before the relay connection is ready. Video P2S reports are now framed and filled as official clients send them, which lets WeChat's server adapt the sender's stream; when the server switches a sender to WeChat's Vcodec1 packaging, the daemon rewraps it as standard H.264. A stream still waiting for a keyframe asks for a smaller picture size, which makes the sender restart its encoder. Group calls also acknowledge in-band `0x50` messages.

- [#666](https://github.com/thisnick/agent-wechat/pull/666) [`ee575c1`](https://github.com/thisnick/agent-wechat/commit/ee575c14f2813cd8b01db5bcffcd1f38ea8a81e7) Thanks [@thisnick](https://github.com/thisnick)! - Fix VoIP relay check-in failing on IPv6 addresses and add dual-stack IPv4/IPv6 relay support across direct audio, direct video, and conference calls (#662).

  - Accept IPv4, IPv6, or empty strings (up to 45 bytes) in `RelayS2CCheckinResp` `clientIpStr` (field 12) and `svrIpStr` (field 13), preserving the learned text verbatim and setting the numeric IPv4 fields to `0` for IPv6/empty values matching the official client while keeping UDP pinned to the actual relay endpoint.
  - Add end-to-end dual-stack IPv4/IPv6 UDP relay selection and transport (`SelectedRelay::endpoint`, `TwoRttRelay::new`, `TwoRttRelay::receive` now take/expose `std::net::SocketAddr`), strategy candidate enumeration with local route probing and legacy `Strategy.udp_addrs` fallback, and 1-RTT `merge_mode` (`is_merged_response`) support with 2-RTT fallback.
  - Classify relay handshake errors as `relay_setup_failed` and record `call-media-diagnostic` for incoming direct calls as well as outgoing direct calls.
  - Breaking Rust crate change in `wechat-protocol`: `SelectedRelay::endpoint`, `TwoRttRelay::new`, and `TwoRttRelay::receive` now use `std::net::SocketAddr` instead of `std::net::SocketAddrV4`.

- [#635](https://github.com/thisnick/agent-wechat/pull/635) [`d74e89f`](https://github.com/thisnick/agent-wechat/commit/d74e89f2f5ed5258eba62aa2288059a911edf0d7) Thanks [@thisnick](https://github.com/thisnick)! - Add native direct and conference video calling and move call WebSockets to protocol version 3.

  - Upgrade the call control and media WebSocket protocol from version 2 to version 3. Media WebSockets now carry binary `WXCM` v3 frames (36-byte big-endian header) for both PCM audio and video (`h265`, `h264`, `jpeg`) instead of base64 JSON audio frames, and support single-use `?ticket=` authentication from `POST /v1/accounts/{account}/calls/tickets` alongside `Authorization: Bearer`. Older v2 clients are rejected with `unsupported_protocol_version`; upgrade the daemon, `wx` CLI, `@agent-wechat/shared` SDK, and OpenClaw plugin together.
  - Add direct (`voip-2p`) and conference (`voip-mt`) video calling to the native Rust daemon (`wechat-protocol`, `wechat-calls`, `wechat-core`, `wechat-api`), including standard H.265/H.264 pass-through and FFmpeg transcoding fallback, mid-call camera toggling, automatic conference video subscription, RSM video packetization/reassembly with GF(2^8) Reed-Solomon FEC recovery, and orientation metadata propagation.
  - Breaking Rust crate change in `wechat-protocol`: add `CallOperation::Subscribe` to the exhaustive `CallOperation` enum.
  - Extend `wx calls` (`dial`, `answer`, `monitor`, `conference start/join`, `devices`, `status`) with `--video`, `--camera`, `--camera-off`, and `--muted`, live mid-call camera toggling (`v`), inline 24-bit TrueColor half-block terminal video preview, FFmpeg camera capture (`auto`, `off`, `test`, `file:<path>`, or a device path/name), and smoother microphone/speaker playout jitter handling.
  - Update `@agent-wechat/shared` with Calls Protocol v3 (`WXCM` binary audio/video codecs, `NativeWeChatClient.issueCallTicket()`, `NativeCallsClient.setCamera()`, `NativeCallMediaClient.requestKeyframe()`, and video capabilities), and upgrade the OpenClaw plugin's voice calling client to protocol v3 (audio-only).

- [#682](https://github.com/thisnick/agent-wechat/pull/682) [`d8ec7f2`](https://github.com/thisnick/agent-wechat/commit/d8ec7f221cce4e5f844062c93472ad861664c18c) Thanks [@thisnick](https://github.com/thisnick)! - Clean up the calling packet/runtime boundary between `wechat-protocol` and `wechat-calls`.

  - Expose canonical RSM kind-3 header parsing (`RsmPacketHeader`, `parse_rsm_packet_header`, `RsmVideoReassembler::ingest_with_header`), `advance_seq16`, and Annex-B NALU helpers (`is_annexb_keyframe`, `annexb_nalu_types`) in `wechat-protocol::calls::video_packet` so callers never slice raw `EncFecHeader` or NALU bytes.
  - Add deterministic, clock-injected (`now_ms: u64`) `VideoNackTracker`, `VideoP2sAccumulator`, and `VideoKeyframePolicy` state machines under `wechat-protocol::calls::video_qos` for direct reuse without `wechat-calls` runtime or codec dependencies.
  - Unify direct and conference video receive recovery and outbound video burst pacing inside `wechat-calls` so both paths share the same state machines.
  - Mark the low-level `wechat_calls::conference_media` and `wechat_calls::conference_send` host-integration modules `#[doc(hidden)]` alongside `admission`, `session`, and `native_media` (breaking change to the `wechat-calls` public API listing).

### Minor Changes

- [#653](https://github.com/thisnick/agent-wechat/pull/653) [`e70b27a`](https://github.com/thisnick/agent-wechat/commit/e70b27ae1286fcdae13a79cce1b54908e8acacf1) Thanks [@thisnick](https://github.com/thisnick)! - Expand oversized merged-forward chat history cards arriving as app message subtype `40` (`49:40`) in place, and show an explicit unavailable fallback when expansion cannot complete.

  - Add CGI `221` (`downloadappattach`) chunked download support to `wechat-protocol` (`AppAttachRequest`, `AppAttachChunk`, `download_app_attach_chunk`, `Service::AppAttachment`) and in-place subtype-`40` expansion updates to `wechat-profile` (`AppMessageExpansion`, `apply_app_message_expansion`).
  - Automatically download and expand incoming `49:40` messages in `wechat-core`, rewriting the official `Msg_*` row to its inner subtype (such as `49:19`) and emitting `message.updated` if a later retry succeeds after an initial failure.
  - Render unexpanded `49:40` forwarded cards with an explicit `[Chat History unavailable]` fallback instead of `body: null` across the daemon API, OpenClaw extension, and browser client UI, and allow quoting expanded `49:19` cards up to the 1 MiB forwarded-XML limit.

- [#669](https://github.com/thisnick/agent-wechat/pull/669) [`72c6055`](https://github.com/thisnick/agent-wechat/commit/72c6055abac1eaff737cc431f9e4288d4aa7a3a4) Thanks [@thisnick](https://github.com/thisnick)! - Fix incoming direct video calls failing to receive and decode remote video.

  - Send `mode.av_status()` (`3` for video, `1` for voice) in `AckReq` field 13, set `group` field 7 (`member_flag`) to `1` after `AckRsp` on incoming joins, and send `network_type = 4` in `RelayCheckIn` field 13 so peers stream video on incoming direct video calls.
  - Buffer non-check-in datagrams arriving during the UDP relay handshake (`connect_relay_with_early`) and match the 200 ms check-in retransmit interval so incoming video frame #0 IDR bursts are preserved instead of discarded.
  - Stop dropping real RSM video packets whose `EncFecHeader` byte 13 bit 4 (`0x10`) bandwidth-probe flag is set (mobile H.264 IDR keyframe packets carry this bit alongside valid SPS/PPS/IDR bytes), while keeping `f_tot_pkts == 0 || f_pkt_idx == 0 || s_per_group == 0` filtering for degenerate/padding-only headers.
  - Advertise `HW265` (`0x04` and fields 21/22) plus standard video feature bits (`0x3f9d`, excluding proprietary `Vcodec1`/`MixVcodec1`), accept `extension_byte <= 2` on `0x20`/`0x40` video packets, send the initial `0x70` control probe after relay Ready, align `encode_p2s` with the nested `inner.f1` + `inner.f3.f1` wire layout, and encode 64-bit `lost_point_seq` and `force` in `encode_irequest_with_force`.

- [#660](https://github.com/thisnick/agent-wechat/pull/660) [`7c2fa97`](https://github.com/thisnick/agent-wechat/commit/7c2fa97ac5569d66336e87a5280eafe536a69067) Thanks [@thisnick](https://github.com/thisnick)! - Support downloading Enterprise WeChat (WeCom) encrypted HTTP media (`tpurl`, `tphdurl`, `tpthumburl`, `tpvideourl`) for images, files, videos, stickers, quoted WeCom stickers, and `@im.chatroom` external groups (including live sync projection).

### Patch Changes

- [#683](https://github.com/thisnick/agent-wechat/pull/683) [`33f4494`](https://github.com/thisnick/agent-wechat/commit/33f449407450d3b2488e4e3c6940f26852981290) Thanks [@thisnick](https://github.com/thisnick)! - Fix audio in both directions on video calls with official Android clients: video offers and answers no longer advertise bit 17 of the audio extended-capability field, which made Android peers stop sending and playing call audio. Acknowledge the in-band `0x50` reliable messages official clients send during a call, so they stop retransmitting them.

- [#674](https://github.com/thisnick/agent-wechat/pull/674) [`a59d3ab`](https://github.com/thisnick/agent-wechat/commit/a59d3ab67274c74070f49f926b8b74443cb1f36c) Thanks [@thisnick](https://github.com/thisnick)! - Improve direct video call startup and keyframe recovery, including camera-off answers.

  - Cache the latest contiguous inbound video GOP (or latest JPEG frame) per active receive track in `CallService` and replay it immediately when a client attaches its media socket (`attach_media`), eliminating the early-keyframe drop race between call connection and media WebSocket setup; if pre-attach frames arrived without a contiguous keyframe, trigger `request_video_keyframe` immediately on attach.
  - Apply receive P-frame loss gating and cached VPS/SPS/PPS parameter-set prepending to passthrough (`H.264`/`H.265`) tracks as well as transcoded tracks, so passthrough WebCodecs/H.264 decoders do not stall on unreferenced P-frames or bare recovery IDRs.
  - Join and invite direct video calls with the video status bit set (`av_status = 3`) even when `cameraEnabled` is `false` (while keeping local camera send inactive) so the relay forwards initial keyframe requests, then send `SwitchAV` to clear the camera bit once the first remote frame arrives or the startup grace window elapses.
  - Send unforced (`force = false`) direct-call `0x66` `IRequest` packets by default so remote hardware encoders run their keyframe-request callback, escalating to `force = true` after unanswered retries and pacing retries at 1000 ms.

- [#676](https://github.com/thisnick/agent-wechat/pull/676) [`9b47c28`](https://github.com/thisnick/agent-wechat/commit/9b47c28ecd88826b013e33c4ffbf6f7eb51b8de8) Thanks [@thisnick](https://github.com/thisnick)! - Fix incoming MP/iLink RSM video keyframe parity decoding and out-of-order FEC reassembly.

  - Sum the three wire RS parity fields (`r_lo2 + r_mid2 + r_hi4`, max 21) additively instead of bit-shifting them (`r_lo2 | (r_mid2 << 2) | (r_hi4 << 4)`), matching `s_per_group` and `pkg_idx_in_group`. Bit-shifting inflated high-parity keyframes (`r >= 10`) past `MAX_FEC_PARITY (64)` so every keyframe packet was silently dropped even without packet loss, and corrupted `r = 4..9` RS erasure recovery.
  - Hold completed newer frames in a bounded in-order ready queue while an older incomplete keyframe (up to 4 frames) or FEC-protected P-frame (up to 2 frames) is still receiving paced packets or parity, instead of immediately evicting the older frame when a smaller subsequent P-frame completes first, and expose `RsmVideoReassembler::pop_ready` / `ConferenceReceiver::pop_ready_video` so callers drain all newly unblocked frames without pipeline lag. Completed recovery keyframes continue to evict strictly older incomplete assemblies immediately.
  - Expand bounded keyframe, FEC recovery, drop, and ingest-error logging under `WX_CALL_VIDEO_TRACE=1`.

- [#679](https://github.com/thisnick/agent-wechat/pull/679) [`e195410`](https://github.com/thisnick/agent-wechat/commit/e1954100cb43ed991081730a7c716d52da809057) Thanks [@thisnick](https://github.com/thisnick)! - Improve incoming video call keyframe recovery under packet loss, add direct ringing relay pre-connect, and clean up conference remote-end teardown.

  - Track missing 16-bit `pkg_seq` ranges on direct and conference incoming video streams and send bounded `0x06` ARQ/NACK retransmission requests matched to the active `Live96`/`Live98` relay framing, report live `0x0b` MP P2S video/link telemetry (separating received `recv_fps` from playable `play_fps` and reporting heavy freeze while awaiting initial or post-drop keyframe sync so QoS servers trigger `u8IRequestFlag` on hardware H.264 peers), and send immediate forced `0x66` `IRequest` recovery before the first self-contained keyframe arrives.
  - Reassemble 1D multi-group RSM video frames whose trailing FEC group carries fewer source packets than group 0, and recover 2D multi-group frames across full-group loss via vertical Reed-Solomon parity.
  - Advertise the video transport capability bit (`1 << 17`) in `Cap11` field 7 on video offers while leaving audio-only offers unchanged.
  - Bound mid-stream post-drop P-frame holds to 1200 ms on initialized video streams so playback resumes with periodic background keyframe requests if a peer drops recovery IDRs, while still requiring a fresh IDR after a transcoder reset.
  - Pre-connect the incoming direct UDP relay during the ringing phase when `ConfAck` returns relay candidates, buffering early post-Ready packets (such as initial `seq = 0` keyframes) and handing off a fresh matching socket to the media session on `Join`.
  - Require a startup grace window before resetting an FFmpeg receive transcoder for zero output, drain transcoder output during synchronous early-conference replay, and clear stale conference cache entries when a track ends.
  - Reconcile early-announced conference tracks, exit the conference receive control loop promptly when all remote members leave, and extend the CLI media-closed-before-control-end grace window so remote conference teardown reports clean completion instead of `termination outcome unknown`.

- [#661](https://github.com/thisnick/agent-wechat/pull/661) [`c38356d`](https://github.com/thisnick/agent-wechat/commit/c38356da36129e6dc5fc8323d1afc5c81575d00c) Thanks [@thisnick](https://github.com/thisnick)! - Fix video calling orientation, conference join/create reliability, local preview smoothness, incoming CLI/transcoded video recovery, and OpenClaw call-end media teardown:

  - Pre-rotate outgoing call video 90° CCW across native capture (`LocalVideoCapture`), browser WebCodecs H.264, and daemon TX transcoding while signaling wire `orientation = 1` (+90° CW) in both `EncFecHeader` and `workMode = 3` virtual-frame metadata packets (`pkg_seq = 0`, `hdr[12] = 0x07`, `hdr[15] = 0x00`, `VideoPktExtHeader` fields `1..=9` and `16` with `f2 = 0`), with desktop `SelfViewDecoder` undoing the rotation for upright local self-view so outgoing video displays upright on Android, iOS/iPadOS, Official Linux, and native clients.
  - Fix conference media mutex starvation by releasing the conference roster lock before media encoding/decoding and UDP reads, allow `connected()` to race ahead of `Admitted` without regressing state, accept initial `sequence = 0` and proto3-omitted zero values in `AVMemberList`, preserve conference `video_flags = 100`, add periodic UDP relay check-in retransmits and bounded `SubscribeVideo` retries, and hide the desktop self-view corner tile until the call is connected.
  - Reduce desktop camera preview and in-call self-view lag with event-driven drop-oldest MJPEG capture (`480x360 @ 24 fps`, `-fflags nobuffer -flags low_delay`), low-latency single-threaded `SelfViewDecoder` without `fps=10` rate-conversion buffering, and single-in-flight `createImageBitmap` canvas coalescing.
  - Prevent incoming transcoded video (`wx calls monitor` / JPEG clients) from stalling on `Waiting for video...` by caching codec parameter sets (`VPS`/`SPS`/`PPS`), requiring parameter sets before latching the keyframe gate, resetting exited or stalled FFmpeg transcoders, upgrading `wechat-calls` JPEG extraction to segment-aware marker scanning, tracking the peer's observed `stream_id` for `IRequest`, and sending bounded initial keyframe requests.
  - Avoid spurious `needs_review` escalation in OpenClaw `CallsService` when `media_closed` races slightly ahead of a concurrent `call.state(ended)` control event.

- [#663](https://github.com/thisnick/agent-wechat/pull/663) [`a36a58f`](https://github.com/thisnick/agent-wechat/commit/a36a58fbc79e51ca3c54320c0e557294a2b86aef) Thanks [@thisnick](https://github.com/thisnick)! - Improve native video calling across desktop UI, `wx calls`, and daemon transcoding:

  - Render local self-capture directly inside the local participant's conference grid tile (with local speaking, mute, and camera indicators), hide the redundant corner self-view overlay when the self tile is visible, avoid duplicating the local participant in the conference roster drawer, and raise desktop self-view resolution to 640x480 without `-fflags nobuffer` IDR drops.
  - Fix incoming transcoded MJPEG video stalling on `Waiting for video...` under FFmpeg 7.1/8.0 (such as Homebrew FFmpeg on macOS) by explicitly converting MJPEG encoder frames to full-range YUV (`out_range=full`, `-strict unofficial -pix_fmt yuv420p -color_range pc`), forwarding control-socket track events into `wx calls` video reception, and using per-track latest-frame decoding. Also normalize WebSocket close teardown errors so `wx calls` does not print raw `Sending after closing is not allowed` errors.
  - Reduce desktop video lag, choppiness, and macroblocking by dropping unreferenced P-frames after packet/queue loss until the next IDR keyframe (including carrying eviction drop flags across multi-packet RSM reassembly), replacing the 16-frame FIFO JPEG transcoder buffer with a latest-frame slot and AUD frame flushing, keeping receive queues shallow for independent JPEG frames (2) while preserving deeper bursts for inter-frame H.264/H.265 tracks (16), skipping SQLite `spawn_blocking` grant lookups for owner bearer tokens on media frames, pacing desktop WebView JPEG IPC with `requestAnimationFrame` acknowledgements, and deduplicating steady-state audio level updates.

- [#650](https://github.com/thisnick/agent-wechat/pull/650) [`514baf5`](https://github.com/thisnick/agent-wechat/commit/514baf59fc2bc9c08c33ad90719e1a99a623f57c) Thanks [@thisnick](https://github.com/thisnick)! - Open received stickers served by the Tencent `vweixinf.tc.qq.com` sticker download endpoint.

## 3.0.0-beta.6

### Major Changes

- [#682](https://github.com/thisnick/agent-wechat/pull/682) [`d8ec7f2`](https://github.com/thisnick/agent-wechat/commit/d8ec7f221cce4e5f844062c93472ad861664c18c) Thanks [@thisnick](https://github.com/thisnick)! - Clean up the calling packet/runtime boundary between `wechat-protocol` and `wechat-calls`.

  - Expose canonical RSM kind-3 header parsing (`RsmPacketHeader`, `parse_rsm_packet_header`, `RsmVideoReassembler::ingest_with_header`), `advance_seq16`, and Annex-B NALU helpers (`is_annexb_keyframe`, `annexb_nalu_types`) in `wechat-protocol::calls::video_packet` so callers never slice raw `EncFecHeader` or NALU bytes.
  - Add deterministic, clock-injected (`now_ms: u64`) `VideoNackTracker`, `VideoP2sAccumulator`, and `VideoKeyframePolicy` state machines under `wechat-protocol::calls::video_qos` for direct reuse without `wechat-calls` runtime or codec dependencies.
  - Unify direct and conference video receive recovery and outbound video burst pacing inside `wechat-calls` so both paths share the same state machines.
  - Mark the low-level `wechat_calls::conference_media` and `wechat_calls::conference_send` host-integration modules `#[doc(hidden)]` alongside `admission`, `session`, and `native_media` (breaking change to the `wechat-calls` public API listing).

### Patch Changes

- [#676](https://github.com/thisnick/agent-wechat/pull/676) [`9b47c28`](https://github.com/thisnick/agent-wechat/commit/9b47c28ecd88826b013e33c4ffbf6f7eb51b8de8) Thanks [@thisnick](https://github.com/thisnick)! - Fix incoming MP/iLink RSM video keyframe parity decoding and out-of-order FEC reassembly.

  - Sum the three wire RS parity fields (`r_lo2 + r_mid2 + r_hi4`, max 21) additively instead of bit-shifting them (`r_lo2 | (r_mid2 << 2) | (r_hi4 << 4)`), matching `s_per_group` and `pkg_idx_in_group`. Bit-shifting inflated high-parity keyframes (`r >= 10`) past `MAX_FEC_PARITY (64)` so every keyframe packet was silently dropped even without packet loss, and corrupted `r = 4..9` RS erasure recovery.
  - Hold completed newer frames in a bounded in-order ready queue while an older incomplete keyframe (up to 4 frames) or FEC-protected P-frame (up to 2 frames) is still receiving paced packets or parity, instead of immediately evicting the older frame when a smaller subsequent P-frame completes first, and expose `RsmVideoReassembler::pop_ready` / `ConferenceReceiver::pop_ready_video` so callers drain all newly unblocked frames without pipeline lag. Completed recovery keyframes continue to evict strictly older incomplete assemblies immediately.
  - Expand bounded keyframe, FEC recovery, drop, and ingest-error logging under `WX_CALL_VIDEO_TRACE=1`.

- [#679](https://github.com/thisnick/agent-wechat/pull/679) [`e195410`](https://github.com/thisnick/agent-wechat/commit/e1954100cb43ed991081730a7c716d52da809057) Thanks [@thisnick](https://github.com/thisnick)! - Improve incoming video call keyframe recovery under packet loss, add direct ringing relay pre-connect, and clean up conference remote-end teardown.

  - Track missing 16-bit `pkg_seq` ranges on direct and conference incoming video streams and send bounded `0x06` ARQ/NACK retransmission requests matched to the active `Live96`/`Live98` relay framing, report live `0x0b` MP P2S video/link telemetry (separating received `recv_fps` from playable `play_fps` and reporting heavy freeze while awaiting initial or post-drop keyframe sync so QoS servers trigger `u8IRequestFlag` on hardware H.264 peers), and send immediate forced `0x66` `IRequest` recovery before the first self-contained keyframe arrives.
  - Reassemble 1D multi-group RSM video frames whose trailing FEC group carries fewer source packets than group 0, and recover 2D multi-group frames across full-group loss via vertical Reed-Solomon parity.
  - Advertise the video transport capability bit (`1 << 17`) in `Cap11` field 7 on video offers while leaving audio-only offers unchanged.
  - Bound mid-stream post-drop P-frame holds to 1200 ms on initialized video streams so playback resumes with periodic background keyframe requests if a peer drops recovery IDRs, while still requiring a fresh IDR after a transcoder reset.
  - Pre-connect the incoming direct UDP relay during the ringing phase when `ConfAck` returns relay candidates, buffering early post-Ready packets (such as initial `seq = 0` keyframes) and handing off a fresh matching socket to the media session on `Join`.
  - Require a startup grace window before resetting an FFmpeg receive transcoder for zero output, drain transcoder output during synchronous early-conference replay, and clear stale conference cache entries when a track ends.
  - Reconcile early-announced conference tracks, exit the conference receive control loop promptly when all remote members leave, and extend the CLI media-closed-before-control-end grace window so remote conference teardown reports clean completion instead of `termination outcome unknown`.

## 3.0.0-beta.5

### Patch Changes

- [#674](https://github.com/thisnick/agent-wechat/pull/674) [`a59d3ab`](https://github.com/thisnick/agent-wechat/commit/a59d3ab67274c74070f49f926b8b74443cb1f36c) Thanks [@thisnick](https://github.com/thisnick)! - Improve direct video call startup and keyframe recovery, including camera-off answers.

  - Cache the latest contiguous inbound video GOP (or latest JPEG frame) per active receive track in `CallService` and replay it immediately when a client attaches its media socket (`attach_media`), eliminating the early-keyframe drop race between call connection and media WebSocket setup; if pre-attach frames arrived without a contiguous keyframe, trigger `request_video_keyframe` immediately on attach.
  - Apply receive P-frame loss gating and cached VPS/SPS/PPS parameter-set prepending to passthrough (`H.264`/`H.265`) tracks as well as transcoded tracks, so passthrough WebCodecs/H.264 decoders do not stall on unreferenced P-frames or bare recovery IDRs.
  - Join and invite direct video calls with the video status bit set (`av_status = 3`) even when `cameraEnabled` is `false` (while keeping local camera send inactive) so the relay forwards initial keyframe requests, then send `SwitchAV` to clear the camera bit once the first remote frame arrives or the startup grace window elapses.
  - Send unforced (`force = false`) direct-call `0x66` `IRequest` packets by default so remote hardware encoders run their keyframe-request callback, escalating to `force = true` after unanswered retries and pacing retries at 1000 ms.

## 3.0.0-beta.4

### Minor Changes

- [#669](https://github.com/thisnick/agent-wechat/pull/669) [`72c6055`](https://github.com/thisnick/agent-wechat/commit/72c6055abac1eaff737cc431f9e4288d4aa7a3a4) Thanks [@thisnick](https://github.com/thisnick)! - Fix incoming direct video calls failing to receive and decode remote video.

  - Send `mode.av_status()` (`3` for video, `1` for voice) in `AckReq` field 13, set `group` field 7 (`member_flag`) to `1` after `AckRsp` on incoming joins, and send `network_type = 4` in `RelayCheckIn` field 13 so peers stream video on incoming direct video calls.
  - Buffer non-check-in datagrams arriving during the UDP relay handshake (`connect_relay_with_early`) and match the 200 ms check-in retransmit interval so incoming video frame #0 IDR bursts are preserved instead of discarded.
  - Stop dropping real RSM video packets whose `EncFecHeader` byte 13 bit 4 (`0x10`) bandwidth-probe flag is set (mobile H.264 IDR keyframe packets carry this bit alongside valid SPS/PPS/IDR bytes), while keeping `f_tot_pkts == 0 || f_pkt_idx == 0 || s_per_group == 0` filtering for degenerate/padding-only headers.
  - Advertise `HW265` (`0x04` and fields 21/22) plus standard video feature bits (`0x3f9d`, excluding proprietary `Vcodec1`/`MixVcodec1`), accept `extension_byte <= 2` on `0x20`/`0x40` video packets, send the initial `0x70` control probe after relay Ready, align `encode_p2s` with the nested `inner.f1` + `inner.f3.f1` wire layout, and encode 64-bit `lost_point_seq` and `force` in `encode_irequest_with_force`.

## 3.0.0-beta.3

### Major Changes

- [#666](https://github.com/thisnick/agent-wechat/pull/666) [`ee575c1`](https://github.com/thisnick/agent-wechat/commit/ee575c14f2813cd8b01db5bcffcd1f38ea8a81e7) Thanks [@thisnick](https://github.com/thisnick)! - Fix VoIP relay check-in failing on IPv6 addresses and add dual-stack IPv4/IPv6 relay support across direct audio, direct video, and conference calls (#662).

  - Accept IPv4, IPv6, or empty strings (up to 45 bytes) in `RelayS2CCheckinResp` `clientIpStr` (field 12) and `svrIpStr` (field 13), preserving the learned text verbatim and setting the numeric IPv4 fields to `0` for IPv6/empty values matching the official client while keeping UDP pinned to the actual relay endpoint.
  - Add end-to-end dual-stack IPv4/IPv6 UDP relay selection and transport (`SelectedRelay::endpoint`, `TwoRttRelay::new`, `TwoRttRelay::receive` now take/expose `std::net::SocketAddr`), strategy candidate enumeration with local route probing and legacy `Strategy.udp_addrs` fallback, and 1-RTT `merge_mode` (`is_merged_response`) support with 2-RTT fallback.
  - Classify relay handshake errors as `relay_setup_failed` and record `call-media-diagnostic` for incoming direct calls as well as outgoing direct calls.
  - Breaking Rust crate change in `wechat-protocol`: `SelectedRelay::endpoint`, `TwoRttRelay::new`, and `TwoRttRelay::receive` now use `std::net::SocketAddr` instead of `std::net::SocketAddrV4`.

## 3.0.0-beta.2

### Patch Changes

- [#663](https://github.com/thisnick/agent-wechat/pull/663) [`a36a58f`](https://github.com/thisnick/agent-wechat/commit/a36a58fbc79e51ca3c54320c0e557294a2b86aef) Thanks [@thisnick](https://github.com/thisnick)! - Improve native video calling across desktop UI, `wx calls`, and daemon transcoding:

  - Render local self-capture directly inside the local participant's conference grid tile (with local speaking, mute, and camera indicators), hide the redundant corner self-view overlay when the self tile is visible, avoid duplicating the local participant in the conference roster drawer, and raise desktop self-view resolution to 640x480 without `-fflags nobuffer` IDR drops.
  - Fix incoming transcoded MJPEG video stalling on `Waiting for video...` under FFmpeg 7.1/8.0 (such as Homebrew FFmpeg on macOS) by explicitly converting MJPEG encoder frames to full-range YUV (`out_range=full`, `-strict unofficial -pix_fmt yuv420p -color_range pc`), forwarding control-socket track events into `wx calls` video reception, and using per-track latest-frame decoding. Also normalize WebSocket close teardown errors so `wx calls` does not print raw `Sending after closing is not allowed` errors.
  - Reduce desktop video lag, choppiness, and macroblocking by dropping unreferenced P-frames after packet/queue loss until the next IDR keyframe (including carrying eviction drop flags across multi-packet RSM reassembly), replacing the 16-frame FIFO JPEG transcoder buffer with a latest-frame slot and AUD frame flushing, keeping receive queues shallow for independent JPEG frames (2) while preserving deeper bursts for inter-frame H.264/H.265 tracks (16), skipping SQLite `spawn_blocking` grant lookups for owner bearer tokens on media frames, pacing desktop WebView JPEG IPC with `requestAnimationFrame` acknowledgements, and deduplicating steady-state audio level updates.

## 3.0.0-beta.1

### Minor Changes

- [#653](https://github.com/thisnick/agent-wechat/pull/653) [`e70b27a`](https://github.com/thisnick/agent-wechat/commit/e70b27ae1286fcdae13a79cce1b54908e8acacf1) Thanks [@thisnick](https://github.com/thisnick)! - Expand oversized merged-forward chat history cards arriving as app message subtype `40` (`49:40`) in place, and show an explicit unavailable fallback when expansion cannot complete.

  - Add CGI `221` (`downloadappattach`) chunked download support to `wechat-protocol` (`AppAttachRequest`, `AppAttachChunk`, `download_app_attach_chunk`, `Service::AppAttachment`) and in-place subtype-`40` expansion updates to `wechat-profile` (`AppMessageExpansion`, `apply_app_message_expansion`).
  - Automatically download and expand incoming `49:40` messages in `wechat-core`, rewriting the official `Msg_*` row to its inner subtype (such as `49:19`) and emitting `message.updated` if a later retry succeeds after an initial failure.
  - Render unexpanded `49:40` forwarded cards with an explicit `[Chat History unavailable]` fallback instead of `body: null` across the daemon API, OpenClaw extension, and browser client UI, and allow quoting expanded `49:19` cards up to the 1 MiB forwarded-XML limit.

- [#660](https://github.com/thisnick/agent-wechat/pull/660) [`7c2fa97`](https://github.com/thisnick/agent-wechat/commit/7c2fa97ac5569d66336e87a5280eafe536a69067) Thanks [@thisnick](https://github.com/thisnick)! - Support downloading Enterprise WeChat (WeCom) encrypted HTTP media (`tpurl`, `tphdurl`, `tpthumburl`, `tpvideourl`) for images, files, videos, stickers, quoted WeCom stickers, and `@im.chatroom` external groups (including live sync projection).

### Patch Changes

- [#661](https://github.com/thisnick/agent-wechat/pull/661) [`c38356d`](https://github.com/thisnick/agent-wechat/commit/c38356da36129e6dc5fc8323d1afc5c81575d00c) Thanks [@thisnick](https://github.com/thisnick)! - Fix video calling orientation, conference join/create reliability, local preview smoothness, incoming CLI/transcoded video recovery, and OpenClaw call-end media teardown:

  - Pre-rotate outgoing call video 90° CCW across native capture (`LocalVideoCapture`), browser WebCodecs H.264, and daemon TX transcoding while signaling wire `orientation = 1` (+90° CW) in both `EncFecHeader` and `workMode = 3` virtual-frame metadata packets (`pkg_seq = 0`, `hdr[12] = 0x07`, `hdr[15] = 0x00`, `VideoPktExtHeader` fields `1..=9` and `16` with `f2 = 0`), with desktop `SelfViewDecoder` undoing the rotation for upright local self-view so outgoing video displays upright on Android, iOS/iPadOS, Official Linux, and native clients.
  - Fix conference media mutex starvation by releasing the conference roster lock before media encoding/decoding and UDP reads, allow `connected()` to race ahead of `Admitted` without regressing state, accept initial `sequence = 0` and proto3-omitted zero values in `AVMemberList`, preserve conference `video_flags = 100`, add periodic UDP relay check-in retransmits and bounded `SubscribeVideo` retries, and hide the desktop self-view corner tile until the call is connected.
  - Reduce desktop camera preview and in-call self-view lag with event-driven drop-oldest MJPEG capture (`480x360 @ 24 fps`, `-fflags nobuffer -flags low_delay`), low-latency single-threaded `SelfViewDecoder` without `fps=10` rate-conversion buffering, and single-in-flight `createImageBitmap` canvas coalescing.
  - Prevent incoming transcoded video (`wx calls monitor` / JPEG clients) from stalling on `Waiting for video...` by caching codec parameter sets (`VPS`/`SPS`/`PPS`), requiring parameter sets before latching the keyframe gate, resetting exited or stalled FFmpeg transcoders, upgrading `wechat-calls` JPEG extraction to segment-aware marker scanning, tracking the peer's observed `stream_id` for `IRequest`, and sending bounded initial keyframe requests.
  - Avoid spurious `needs_review` escalation in OpenClaw `CallsService` when `media_closed` races slightly ahead of a concurrent `call.state(ended)` control event.

## 3.0.0-beta.0

### Major Changes

- [#635](https://github.com/thisnick/agent-wechat/pull/635) [`d74e89f`](https://github.com/thisnick/agent-wechat/commit/d74e89f2f5ed5258eba62aa2288059a911edf0d7) Thanks [@thisnick](https://github.com/thisnick)! - Add native direct and conference video calling and move call WebSockets to protocol version 3.

  - Upgrade the call control and media WebSocket protocol from version 2 to version 3. Media WebSockets now carry binary `WXCM` v3 frames (36-byte big-endian header) for both PCM audio and video (`h265`, `h264`, `jpeg`) instead of base64 JSON audio frames, and support single-use `?ticket=` authentication from `POST /v1/accounts/{account}/calls/tickets` alongside `Authorization: Bearer`. Older v2 clients are rejected with `unsupported_protocol_version`; upgrade the daemon, `wx` CLI, `@agent-wechat/shared` SDK, and OpenClaw plugin together.
  - Add direct (`voip-2p`) and conference (`voip-mt`) video calling to the native Rust daemon (`wechat-protocol`, `wechat-calls`, `wechat-core`, `wechat-api`), including standard H.265/H.264 pass-through and FFmpeg transcoding fallback, mid-call camera toggling, automatic conference video subscription, RSM video packetization/reassembly with GF(2^8) Reed-Solomon FEC recovery, and orientation metadata propagation.
  - Breaking Rust crate change in `wechat-protocol`: add `CallOperation::Subscribe` to the exhaustive `CallOperation` enum.
  - Extend `wx calls` (`dial`, `answer`, `monitor`, `conference start/join`, `devices`, `status`) with `--video`, `--camera`, `--camera-off`, and `--muted`, live mid-call camera toggling (`v`), inline 24-bit TrueColor half-block terminal video preview, FFmpeg camera capture (`auto`, `off`, `test`, `file:<path>`, or a device path/name), and smoother microphone/speaker playout jitter handling.
  - Update `@agent-wechat/shared` with Calls Protocol v3 (`WXCM` binary audio/video codecs, `NativeWeChatClient.issueCallTicket()`, `NativeCallsClient.setCamera()`, `NativeCallMediaClient.requestKeyframe()`, and video capabilities), and upgrade the OpenClaw plugin's voice calling client to protocol v3 (audio-only).

### Patch Changes

- [#650](https://github.com/thisnick/agent-wechat/pull/650) [`514baf5`](https://github.com/thisnick/agent-wechat/commit/514baf59fc2bc9c08c33ad90719e1a99a623f57c) Thanks [@thisnick](https://github.com/thisnick)! - Open received stickers served by the Tencent `vweixinf.tc.qq.com` sticker download endpoint.

## 2.9.0

### Minor Changes

- [#630](https://github.com/thisnick/agent-wechat/pull/630) [`997b28e`](https://github.com/thisnick/agent-wechat/commit/997b28e934afacbdcdbea8f79c790090b3d90689) Thanks [@thisnick](https://github.com/thisnick)! - Acknowledge sync updates whose follow-up is local work, and finish them in the background, so they no longer hold back the sync position.

  - **Group changes** are acknowledged as soon as their fields are saved. The group refresh that fetches the group's members and announcement runs separately and writes them when it succeeds. A refresh that has given up no longer holds the update forever.
  - **Contact changes** with fields whose order the daemon can't establish, or with field controls it can't read, are acknowledged. One exact lookup of the contact is queued; it refreshes the contact's other fields and a controlled remark. A controlled flag keeps its saved value, and uncertain mute and pin state still keeps notifications suppressed until a newer known update arrives.
  - **An older pinned-chat change** that a newer one superseded is acknowledged without applying it, and the contact is looked up again.
  - **Sticker sync notices** are acknowledged at once. Sticker reconciliation still runs after the sync.
  - **A message already saved under a different time** is finished without writing a second row (label `message_already_in_profile`).
  - **A business-account record the profile writer refuses** (for example no contact type for a new row) is skipped with its raw bytes kept (label `business_contact_unwritable`).
  - **Diagnostics.** A `projection.blocked` entry for a plain error now includes `errorCode` when the error is an identifier-shaped code.

- [#623](https://github.com/thisnick/agent-wechat/pull/623) [`265bd45`](https://github.com/thisnick/agent-wechat/commit/265bd4554c03e1436cc0c07c7e3de94aaafa1271) Thanks [@thisnick](https://github.com/thisnick)! - Stop letting commands the daemon can't apply hold back the sync position.

  - **Skipped, not blocked.** A sync command this build can't apply no longer keeps its batch blocked. This covers status and system controls with no supported effect (including session exit, status opcode 5), updates to your own profile, unknown commands, and message types the profile writer doesn't support, such as contact cards. Deleting an existing contact through an unsupported path, and a recall in a form the daemon doesn't accept, still wait, so neither leaves stale content behind. The daemon applies whatever it supports first; a chat-open or chat-exit notice still clears unread state. Then it keeps the command's raw bytes, emits `account.control_observed` with `projection: "skipped"` and a label of its kind, and lets the batch finish, so the profile's saved sync position moves on. The raw bytes and a note of what was skipped are kept, so a later version can apply them; `wx store usage` lists them under `skippedControls`.
  - **Still waiting.** A command that waits on other local work, such as a group refresh, sticker reconciliation or a contact update with unresolved fields, still holds its batch and emits `projection.blocked`.
  - **Old blocked batches retry on start.** When the daemon starts, it replays batches an earlier version left blocked, the same way `wx store recover-projection` does, so they finish without a manual step.

- [#616](https://github.com/thisnick/agent-wechat/pull/616) [`b7ff268`](https://github.com/thisnick/agent-wechat/commit/b7ff268f2ece94c7cb04a8f459addc9ec0f2259a) Thanks [@thisnick](https://github.com/thisnick)! - Apply status controls the official client has no handler for, and stop keeping expired sticker store previews.

  - **Status controls without a handler.** A type-51 status control now completes when every operation in it uses an ID the official Linux client has no handler for (anything other than 1, 2, 4, 5, 7, 9, 11, 13, 14 and 21), whatever the operation contains or who sent it. Before, only the recorded forms of opcodes 3, 15, 16 and 23 completed, so other forms such as opcode 24 kept their sync batch blocked. A control that also carries an operation with a handler still blocks.
  - **Sticker store previews.** Pack icons, covers and sticker thumbnails listed from the sticker store are removed from `control.sqlite` once their one-day window ends, unless a download for them is queued or running. Reading a removed preview returns `not_found` (404), as an expired one could no longer be downloaded or read anyway; browsing the store again lists new ones. Before, each day's listing added rows that were never removed.

### Patch Changes

- [#620](https://github.com/thisnick/agent-wechat/pull/620) [`c27fe1c`](https://github.com/thisnick/agent-wechat/commit/c27fe1cad90158e36d4c1a777b0410acb4568c1b) Thanks [@thisnick](https://github.com/thisnick)! - Complete the recorded WeChatOut, UpdateWxaUsageListNotify and deleteMessages sync controls so they no longer hold a sync batch after the other commands have applied.

  After recovery, `wx store usage` counts only the commands that still block a batch.

## 2.8.0

### Minor Changes

- [#602](https://github.com/thisnick/agent-wechat/pull/602) [`62df973`](https://github.com/thisnick/agent-wechat/commit/62df9734eb4fcfaf7b294a933b6d62eab879833a) Thanks [@thisnick](https://github.com/thisnick)! - You can now set or clear your private name for a contact (its remark): `wx contacts set CONTACT_ID --remark "Name"` or `--clear-remark`, or `PATCH contacts/CONTACT_ID` with `{"remark": "Name", "expectedVersion": "…"}` (`null` clears it). The name shows on your phone, and as the contact's `displayName`, a few seconds after WeChat accepts it; until then the contact reports `remarkPending: true`. Change privacy and the remark in separate requests. Friends whose privacy can't be edited yet report why in `privacyUnavailable`. The protocol crate adds `contact_remark_request`.

- [#598](https://github.com/thisnick/agent-wechat/pull/598) [`eabe990`](https://github.com/thisnick/agent-wechat/commit/eabe990b84a677d7c2faab7aa6aac30a710b8ec9) Thanks [@thisnick](https://github.com/thisnick)! - Read relationship privacy with contact details and lists using account read access. Open contact cards using those settings, refresh details and privacy together, and preserve unsaved changes when a newer saved version arrives. Privacy edits still require account administrator access.

### Patch Changes

- [#602](https://github.com/thisnick/agent-wechat/pull/602) [`e6b9c42`](https://github.com/thisnick/agent-wechat/commit/e6b9c4261a95bffedcc9a412498fb322905a42ad) Thanks [@thisnick](https://github.com/thisnick)! - A remark change, from this account or another device, is now confirmed within moments instead of at the account's next scheduled sync, which could be up to 30 seconds away. Queueing the remark lookup wakes the account's worker.

- [#602](https://github.com/thisnick/agent-wechat/pull/602) [`57b8e9b`](https://github.com/thisnick/agent-wechat/commit/57b8e9b0f38cd3ef32939ec71a5476e2e610941e) Thanks [@thisnick](https://github.com/thisnick)! - The simulator now accepts contact privacy and remark edits, so a simulated account can change both end to end. Contact edits go through the account's upstream, like group edits.

## 2.7.1

### Patch Changes

- [#592](https://github.com/thisnick/agent-wechat/pull/592) [`cb35c93`](https://github.com/thisnick/agent-wechat/commit/cb35c93f20b07a9faa1c7d017b0d2c6c656e33e3) Thanks [@thisnick](https://github.com/thisnick)! - Fix listing friend requests failing with `profile_unavailable` (409) on profiles the official client wrote. Requests stored with empty (SQL NULL) columns, such as the remark and labels, couldn't be read, so `wx contacts requests list`, `GET contact-requests` and the OpenClaw `wechat_contact_request` tool failed for every request. Empty columns now read as empty text or zero, and accepting, updating or deleting a request keeps an empty column empty unless the change gives it a value, so the stored row keeps its form. New request rows are written as before.

## 2.7.0

### Minor Changes

- [#578](https://github.com/thisnick/agent-wechat/pull/578) [`0627dcf`](https://github.com/thisnick/agent-wechat/commit/0627dcf0a1904251731591bcda479cd7d7fd64fc) Thanks [@thisnick](https://github.com/thisnick)! - Keep blocked sync batches and the event log from growing, and apply more session-entry controls.

  - **Session entry.** A type-51 session-entry control (status opcode 2) now completes for a group or an ordinary contact once its unread clear is written, and for the built-in session holders the official Linux client ignores. Service accounts the official client's account classifier recognizes, `notifymessage`-style aggregates and hidden sessions still keep their batch blocked, because the official client does more for them. Session exit (opcode 5) and the other unmapped controls stay blocked.
  - **Blocked batches.** A blocked batch outside the newest 256 keeps only the commands not yet written to the profile, plus contact records. Each blocked status or system control leaves a small record of its kind and peer type (never the peer), and `wx store usage` lists them under `blockedControls`.
  - **Attachments never downloaded.** Message attachment rows that were listed but never downloaded, and aren't queued for automatic download, are removed after 7 days. Voice rows are kept. Reading one returns `operation_expired` (410) until listing the message's media again recreates it with the same ID.
  - **Event log.** Consecutive `job.updated` events for the same job that differ only in transfer progress replace each other, so the 10,000-event log covers more time. No state is lost: the newest event for each state a job passed through stays.

- [#562](https://github.com/thisnick/agent-wechat/pull/562) [`cc1f5f1`](https://github.com/thisnick/agent-wechat/commit/cc1f5f1b51e04547cb4e2652ee9189fd3432c998) Thanks [@thisnick](https://github.com/thisnick)! - Add relationship privacy reads and versioned contact updates, optional initial privacy for friend requests, and a shared contact detail card with privacy and contact removal. Add CLI contact updates that preserve omitted fields and retain the original merged input for recovery.

- [#570](https://github.com/thisnick/agent-wechat/pull/570) [`3f4247c`](https://github.com/thisnick/agent-wechat/commit/3f4247ce1b1cf648082860a02b598a121ee0344d) Thanks [@thisnick](https://github.com/thisnick)! - Bound the growth of each account's `agent/control.sqlite`. Maintenance now reduces finished sync batches, text and media sends, group and contact changes, recalls and call actions to small receipts. Download records, which hold no payload, are kept. It removes finished metadata receipts, expired pagination views and old duplicate guards, and gives freed space back to the file system. Unfinished, blocked and still-uncertain work keeps its full records.

  Uncertain (`needs_review`) records expire after 7 days, or once more than 100 newer ones exist in the same family. Expiry keeps the record and its unknown outcome but drops its sealed request and reply bytes; nothing is resent. Old receipts are removed after 90 days (text and media sends) or 365 days (group and contact changes and recalls). Their idempotency keys are kept as short digests, so reusing one returns `idempotency_key_expired` (409) instead of starting new work, and reading a removed send, media send job or operation returns `operation_expired` (410).

  The voice fallback now reads the same newest 256 retained message batches as recall evidence, instead of up to 10,000. Add `wx store usage` to show a stopped account's sidecar size, its largest record families and the unfinished work that keeps records whole.

- [#562](https://github.com/thisnick/agent-wechat/pull/562) [`8208b47`](https://github.com/thisnick/agent-wechat/commit/8208b474e2794561f74dafd9d58088c120adedb0) Thanks [@thisnick](https://github.com/thisnick)! - Add account-bound contact discovery and friend request history, sending, acceptance, local history deletion and contact removal through the Rust libraries, HTTP API and CLI. Add optional private-owner OpenClaw contact tools and unified contact, group and message search in the client UI.

- [#573](https://github.com/thisnick/agent-wechat/pull/573) [`6eac2a5`](https://github.com/thisnick/agent-wechat/commit/6eac2a5512864bc837a064c93ccea5aa1aae403e) Thanks [@thisnick](https://github.com/thisnick)! - Shrink two kinds of encrypted plans in each account's `agent/control.sqlite` that repeated bytes the account already keeps.

  - **Voice notes.** A voice attachment's plan no longer keeps the note's encoded audio when the official profile holds the note; a download reads the profile's copy first and otherwise fetches by server ID as before. Storage maintenance rewrites existing plans the same way, one row at a time, only after the profile returns identical bytes. Media cache repair is unchanged: it uses only owned bytes and never starts a transfer.
  - **Sticker jobs.** A finished pack acquisition among the newest five of its pack keeps only the pack, item IDs, captions and archive locator, which is what adding the pack again reads to reuse its owned archive. Older and failed acquisitions and finished pack, Saved and send jobs drop their plans once nothing reads them: after a day, and only when they are not waiting for a profile write. Their job records and idempotency keys stay. New sticker job plans store raw bytes as base64 text instead of JSON number arrays; earlier plans still read.

  An earlier version can't read sticker job plans written or reduced by this one. After downgrading, an unfinished sticker job or one awaiting a profile write fails, and adding a pack again doesn't reuse an earlier download. Voice plans stay readable.

### Patch Changes

- [#562](https://github.com/thisnick/agent-wechat/pull/562) [`6de620e`](https://github.com/thisnick/agent-wechat/commit/6de620ee73cc3fad4d2e974f8cb448e4facbe509) Thanks [@thisnick](https://github.com/thisnick)! - Show contact, group and message search results as each source responds, default Contacts search to its contact scope, and simplify the shared contact card with icon details actions, collapsed history and a single contextual footer.

  Report stored contact membership evidence in the existing relation field. Keep unknown people out of the saved-contact list and offer a separate lookup and friend-request review from their contact cards.

  Contact lists now default to confirmed saved friends across the API and CLI. Use the API relation filter or `wx contacts list --all` for other cached people. Exact reads and server discovery are unchanged.

  Local contact search defaults to saved friends. When a query is entered, show other cached people in a separate Other people section. Show server discovery separately under Add someone on WeChat, label friends Contact and confirmed non-contacts Not friends yet, and omit a relationship label when membership is unknown.

  Separate Friends and Other people search tabs, route Contacts + to Other people, page each section independently, and show original dates and times on message matches.

  Limit message-search snippets to two lines while keeping the conversation name, original timestamp and View action visible.

## 2.6.0

### Minor Changes

- [#548](https://github.com/thisnick/agent-wechat/pull/548) [`74d4fcb`](https://github.com/thisnick/agent-wechat/commit/74d4fcb1c0eb63a1446e667fae3d86115a06cc1c) Thanks [@thisnick](https://github.com/thisnick)! - Sending, downloading and serving large files and original-quality videos no longer loads them into the daemon's memory (#529). A send reads its upload from a private temporary file and encrypts each CDN chunk as it goes; a download decrypts each piece into a temporary file; both write the official profile and the daemon's encrypted copy from that file. `GET /media/{id}/content` streams the file, and a `Range` request reads only its range once the whole file was checked. A 512 MiB send or download now stays within a small, fixed amount of memory instead of needing several hundred MB.

  `wechat-protocol` adds `PreparedFileUpload` with `CdnClient::upload_file_with_progress`, `upload_large_file_from_file_with_progress` and `upload_video_original_from_file_with_progress`, plus `download_to_with_progress`, `DownloadReceipt` and `DownloadedMedia::into_parts` for downloads into a writer. `wechat-profile` adds `MediaPayload`, `cache_file_projection_payload`, `cache_video_projection_payload`, `publish_cache_payload` and `ProfileReader::verified_hardlink_file`. Existing APIs are unchanged.

### Patch Changes

- [#545](https://github.com/thisnick/agent-wechat/pull/545) [`b2016e6`](https://github.com/thisnick/agent-wechat/commit/b2016e6ea1ce9810b202c7c689c30f7374bac001) Thanks [@thisnick](https://github.com/thisnick)! - Renaming a contact on the phone now updates their remark in profiles the daemon created (#483). The daemon no longer drops remarks that WeChat sends with a version marker: when a remark change arrives it looks the contact up again and saves the current remark, and `wx contacts refresh` saves it too. Remark changes now also arrive while the daemon is connected instead of only after it reconnects. To fill in remarks that are missing from before this update, run `wx contacts sync` or refresh the contact.

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
